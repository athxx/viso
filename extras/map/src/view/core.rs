//! The live state of a built map view: its camera, the motion it is in,
//! its tiles and the nodes that draw them.
//!
//! Gestures, keys, a handle and the frame clock change the camera here; a
//! structure hook ([`Core::apply`]) then writes it into the tree. The tiles
//! sit in one transform root, the camera node: a pan sets its translate, a
//! zoom within a level its scale and a turn its rotation, so a steady
//! camera move only marks that node's transform. Tiles are placed in the
//! points of the current zoom level relative to an anchor near the camera,
//! so their offsets stay small whatever the zoom; the anchor moves, and the
//! tiles with it, only when the camera strays far or changes level.

use std::collections::HashMap;
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use std::cell::RefCell;

use viso_render::Rgba;
use viso_ui::context::UpdateCx;
use viso_ui::{
    BuildCx, Continuation, NodeId, Semantics, Size, StateId, StateStore, StateValue, StateWriter,
    StructureCx, TaskId, Turn, Vec2,
};

use crate::camera::{Camera, Insets, Limits, Motion, TILE_SIZE, View};
use crate::geo::LngLat;
use crate::load::{Built, Input, build_off_loop};
use crate::pyramid::{BuildKey, Plan, Pyramid, Slot, covering};
use crate::source::{TileId, TileSource};
use crate::style::Style;

/// How far (band points) the camera strays from the tiles' anchor before
/// the anchor follows.
const REBASE: f64 = 8192.0;
/// How far past the view's edges tiles are fetched ahead, in points.
const PREFETCH: f64 = 256.0;
/// How fast a fling slows: its speed falls by `e` every this many seconds.
const FLING_DECAY: f64 = 0.325;
/// Below this speed (pt/s) a fling has stopped.
const FLING_STOP: f64 = 20.0;
/// The fastest a fling starts, in pt/s.
const FLING_MAX: f64 = 6000.0;

/// The zoom level a camera at `zoom` draws tiles for: the nearest whole one.
pub(crate) fn band_of(zoom: f64) -> u8 {
    (zoom + 0.5).floor().clamp(0.0, 30.0) as u8
}

/// The cells a view writes its camera into as it moves, for a view
/// description to bind; writing one moves the camera there.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CameraCells {
    pub lon: StateId,
    pub lat: StateId,
    pub zoom: StateId,
    pub bearing: StateId,
}

impl CameraCells {
    /// Fresh cells holding `camera`.
    pub fn new(cx: &mut BuildCx<'_>, camera: &Camera) -> Self {
        Self {
            lon: cx.state(StateValue::Float(camera.center.lon as f32)),
            lat: cx.state(StateValue::Float(camera.center.lat as f32)),
            zoom: cx.state(StateValue::Float(camera.zoom as f32)),
            bearing: cx.state(StateValue::Float(camera.bearing as f32)),
        }
    }

    fn read(&self, states: &StateStore) -> [f32; 4] {
        let f = |id| match states.get(id) {
            Some(StateValue::Float(v)) => v,
            _ => f32::NAN,
        };
        [f(self.lon), f(self.lat), f(self.zoom), f(self.bearing)]
    }

    fn write(&self, states: &mut StateStore, v: [f32; 4]) {
        states.set(self.lon, StateValue::Float(v[0]));
        states.set(self.lat, StateValue::Float(v[1]));
        states.set(self.zoom, StateValue::Float(v[2]));
        states.set(self.bearing, StateValue::Float(v[3]));
    }
}

fn camera_values(c: &Camera) -> [f32; 4] {
    [
        c.center.lon as f32,
        c.center.lat as f32,
        c.zoom as f32,
        c.bearing as f32,
    ]
}

/// What a ready tile carries in the pyramid.
pub(crate) type Ready = Rc<Built>;

/// A request in flight: the task awaiting it and the flag that stops its
/// build.
struct Request {
    task: Option<TaskId>,
    stop: Option<Arc<AtomicBool>>,
}

/// One tile drawn: its slot, the build it shows and its node.
struct TileNode {
    slot: Slot,
    built: Ready,
    node: NodeId,
}

/// A fling coasting on: its velocity in view points a second.
#[derive(Debug, Clone, Copy)]
struct Fling {
    v: [f64; 2],
}

/// Something the view tells the app after a frame's apply.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Told {
    /// The camera came to rest.
    Settled(Camera),
}

/// The live state of a built map view.
pub(crate) struct Core {
    pub root: NodeId,
    pub camera_node: NodeId,
    #[allow(dead_code)]
    pub overlay: NodeId,
    /// The cell the view's structure hook depends on: poked to run it.
    pub wake: StateId,
    pokes: i32,
    pub cells: CameraCells,
    /// What the cells held after the last write, to tell an outside write.
    written: [f32; 4],
    pub view: View,
    pub limits: Limits,
    pub source: Rc<dyn TileSource>,
    pub style: Arc<Style>,
    style_rev: u64,
    pub pyramid: Pyramid<Ready>,
    requests: HashMap<TileId, Request>,
    plan: Plan,
    visible: Vec<Slot>,
    prefetch: Vec<Slot>,
    shown: Vec<Slot>,
    scratch: Vec<(f64, Slot)>,
    tiles: Vec<TileNode>,
    free_scratch: Vec<NodeId>,

    /// The zoom level and anchor (band points) the tiles are placed for.
    band: Option<u8>,
    anchor: [f64; 2],
    /// Tiles arrived, the style changed or the tiles must be placed anew.
    dirty_tiles: bool,
    /// The camera moved since the last apply.
    pub moved: bool,
    /// The camera came to rest and the app has not heard.
    pub settle_due: bool,
    /// The camera's motions.
    motion: Option<(Motion, f64)>,
    fling: Option<Fling>,
    /// Whether an input gesture holds the camera.
    pub held: bool,
    pub reduced_motion: bool,
    /// The seconds the frame clock has run.
    pub clock: f64,
    me: Weak<RefCell<Core>>,
    pub label: String,
    /// Tile builds started, for the counters a test reads.
    pub builds: u32,
}

impl Core {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        root: NodeId,
        camera_node: NodeId,
        overlay: NodeId,
        wake: StateId,
        cells: CameraCells,
        camera: Camera,
        limits: Limits,
        padding: Insets,
        source: Rc<dyn TileSource>,
        zooms: (u8, u8),
        style: Arc<Style>,
        label: String,
    ) -> Rc<RefCell<Core>> {
        let mut view = View::new(camera, [0.0, 0.0]);
        view.padding = padding;
        Rc::new_cyclic(|me| {
            RefCell::new(Core {
                root,
                camera_node,
                overlay,
                wake,
                pokes: 0,
                cells,
                written: camera_values(&camera),
                view,
                limits,
                source,
                style,
                style_rev: 1,
                pyramid: Pyramid::new(zooms.0, zooms.1),
                requests: HashMap::new(),
                plan: Plan::default(),
                visible: Vec::new(),
                prefetch: Vec::new(),
                shown: Vec::new(),
                scratch: Vec::new(),
                tiles: Vec::new(),
                free_scratch: Vec::new(),

                band: None,
                anchor: [0.0, 0.0],
                dirty_tiles: true,
                moved: true,
                settle_due: false,
                motion: None,
                fling: None,
                held: false,
                reduced_motion: false,
                clock: 0.0,
                me: me.clone(),
                label,
                builds: 0,
            })
        })
    }

    /// Asks for the hook to run this frame.
    pub fn poke(&mut self, w: &mut dyn StateWriter) {
        self.pokes = self.pokes.wrapping_add(1);
        w.set(self.wake, StateValue::Int(self.pokes));
    }

    /// The camera as it is.
    pub fn camera(&self) -> Camera {
        self.view.camera
    }

    /// Moves the camera to `camera` at once, within the limits.
    pub fn jump(&mut self, camera: Camera) {
        self.view.camera = camera;
        self.constrain();
        self.moved = true;
    }

    fn constrain(&mut self) {
        if self.view.size[0] > 0.0 && self.view.size[1] > 0.0 {
            self.view.camera = self.limits.constrain(&self.view);
        } else {
            let c = &mut self.view.camera;
            c.zoom = c.zoom.clamp(self.limits.min_zoom, self.limits.max_zoom);
        }
    }

    /// Stops whatever moves the camera on its own.
    pub fn halt(&mut self) {
        let was = self.motion.is_some() || self.fling.is_some();
        self.motion = None;
        self.fling = None;
        if was {
            self.settle_due = false;
        }
    }

    /// Moves the camera to `to` over `duration` seconds, easing; at once
    /// under reduced motion or for no duration.
    pub fn ease_to(&mut self, to: Camera, duration: f64) {
        self.halt();
        if self.reduced_motion || duration <= 0.0 {
            self.jump(to);
            self.settle_due = true;
            return;
        }
        self.motion = Some((Motion::ease(self.view.camera, to, duration), 0.0));
    }

    /// Flies the camera to `to`: out, across and back in.
    pub fn fly_to(&mut self, to: Camera, duration: Option<f64>) {
        self.halt();
        if self.reduced_motion {
            self.jump(to);
            self.settle_due = true;
            return;
        }
        let m = Motion::fly(self.view.camera, to, self.view.size, duration);
        self.motion = Some((m, 0.0));
    }

    /// The camera a running ease heads for, or where it is.
    pub fn aim(&self) -> Camera {
        self.motion
            .as_ref()
            .map_or(self.view.camera, |(m, _)| m.target())
    }

    /// Lets go of a pan moving at `v` points a second: it coasts on.
    pub fn fling(&mut self, v: [f64; 2]) {
        let speed = v[0].hypot(v[1]);
        if self.reduced_motion || speed < FLING_STOP * 4.0 {
            self.settle_due = true;
            return;
        }
        let k = (FLING_MAX / speed).min(1.0);
        self.fling = Some(Fling {
            v: [v[0] * k, v[1] * k],
        });
    }

    /// Pans the view so that what showed at view point `from` shows at `to`.
    pub fn pan(&mut self, from: [f64; 2], to: [f64; 2]) {
        self.view.camera = self.view.panned(from, to);
        self.constrain();
        self.moved = true;
    }

    /// Zooms by `delta` about view point `at`.
    pub fn zoom_about(&mut self, delta: f64, at: [f64; 2]) {
        let mut next = self.view;
        next.camera.zoom = (next.camera.zoom + delta)
            .clamp(self.limits.min_zoom, self.limits.max_zoom)
            - next.camera.zoom;
        self.view.camera = self.view.zoomed_about(next.camera.zoom, at);
        self.constrain();
        self.moved = true;
    }

    /// Turns by `degrees` clockwise about view point `at`.
    pub fn turn_about(&mut self, degrees: f64, at: [f64; 2]) {
        self.view.camera = self.view.turned_about(degrees, at);
        self.constrain();
        self.moved = true;
    }

    /// Whether the frame clock should keep running.
    pub fn animating(&self) -> bool {
        self.motion.is_some() || self.fling.is_some()
    }

    /// Advances the motions by `dt` seconds. Returns whether the camera
    /// moved.
    pub fn tick(&mut self, dt: f64) -> bool {
        self.clock += dt;
        let mut moved = false;
        if let Some((m, t)) = &mut self.motion {
            *t += dt;
            let (camera, done) = (m.at(*t), *t >= m.duration());
            self.view.camera = camera;
            self.constrain();
            moved = true;
            if done {
                self.motion = None;
                self.settle_due = true;
            }
        }
        if let Some(f) = &mut self.fling {
            let k = (-dt / FLING_DECAY).exp();
            // The distance the decaying speed covers over the step.
            let d = [
                f.v[0] * FLING_DECAY * (1.0 - k),
                f.v[1] * FLING_DECAY * (1.0 - k),
            ];
            f.v = [f.v[0] * k, f.v[1] * k];
            let stopped = f.v[0].hypot(f.v[1]) < FLING_STOP;
            let c = self.view.focus();
            self.pan(c, [c[0] + d[0], c[1] + d[1]]);
            moved = true;
            if stopped {
                self.fling = None;
                self.settle_due = true;
            }
        }
        self.moved |= moved;
        moved
    }

    /// Uses `style` from now on, rebuilding the tiles from what they hold.
    pub fn set_style(&mut self, style: Arc<Style>) {
        self.style = style;
        self.style_rev += 1;
        self.dirty_tiles = true;
    }

    /// Uses `source` from now on, forgetting every tile.
    pub fn set_source(&mut self, source: Rc<dyn TileSource>, zooms: (u8, u8)) {
        self.source = source;
        let stale = self.pyramid.clear();
        for tile in stale {
            self.stop_request(None, tile);
        }
        self.pyramid = Pyramid::new(zooms.0, zooms.1);
        self.dirty_tiles = true;
    }

    fn stop_request(&mut self, store: Option<&mut viso_ui::NodeStore>, tile: TileId) {
        if let Some(r) = self.requests.remove(&tile) {
            if let Some(stop) = r.stop {
                stop.store(true, Ordering::Relaxed);
            }
            if let (Some(store), Some(task)) = (store, r.task) {
                store.cancel_task(task);
            }
            self.source.cancel(tile);
        }
    }

    /// A tile arrived from its task.
    fn arrive(&mut self, tile: TileId, result: Result<Option<Built>, ()>) {
        let Some(request) = self.requests.remove(&tile) else {
            return;
        };
        drop(request);
        let result = result.map(|built| {
            built.map(|b| {
                let (bytes, key) = (b.bytes, b.key);
                (Rc::new(b), bytes, key)
            })
        });
        self.pyramid.arrive(tile, result);
        self.dirty_tiles = true;
    }

    /// Starts `tile`'s request: from the source, or a rebuild of what it
    /// holds.
    fn start(&mut self, store: &mut viso_ui::NodeStore, tile: TileId, key: BuildKey) {
        let rebuild = self.pyramid.ready(tile).map(|b| Arc::clone(&b.tile));
        let style = Arc::clone(&self.style);
        let me = self.me.clone();
        let wake = self.wake;
        let (future, stop): (std::pin::Pin<Box<dyn std::future::Future<Output = _>>>, _) =
            match rebuild {
                Some(decoded) => {
                    let (answer, stop) = build_off_loop(Input::Decoded(decoded), tile, style, key);
                    (
                        Box::pin(async move { answer.await.map(Some).map_err(|_| ()) }),
                        Some(stop),
                    )
                }
                None => {
                    let fetch = self.source.request(tile);
                    let stop = Arc::new(AtomicBool::new(false));
                    let stopped = Arc::clone(&stop);
                    (
                        Box::pin(async move {
                            match fetch.await {
                                Ok(Some(bytes)) => {
                                    if stopped.load(Ordering::Relaxed) {
                                        return Err(());
                                    }
                                    let (answer, flag) =
                                        build_off_loop(Input::Bytes(bytes), tile, style, key);
                                    let _keep = StopWith(stopped, flag);
                                    answer.await.map(Some).map_err(|_| ())
                                }
                                Ok(None) => Ok(None),
                                Err(_) => Err(()),
                            }
                        }),
                        Some(stop),
                    )
                }
            };
        self.builds += 1;
        let task = Box::pin(async move {
            let result = future.await;
            Some(Box::new(move |cx: &mut UpdateCx<'_>| {
                if let Some(core) = me.upgrade()
                    && let Ok(mut core) = core.try_borrow_mut()
                {
                    core.arrive(tile, result);
                    core.pokes = core.pokes.wrapping_add(1);
                    cx.set(wake, StateValue::Int(core.pokes));
                }
            }) as Continuation)
        });
        let task = store.spawn_task(self.root, task);
        self.requests.insert(tile, Request { task, stop });
    }

    /// Writes the camera and the tiles into the tree. Run by the view's
    /// structure hook; returns what the app is to be told.
    pub fn apply(&mut self, s: &mut StructureCx<'_>) -> Option<Told> {
        if !s.store.arena().is_live(self.root) {
            return None;
        }
        self.reduced_motion = s.store.reduced_motion();
        // A write to the cells from outside moves the camera there.
        let outside = self.cells.read(s.states);
        if outside != self.written && outside.iter().all(|v| v.is_finite()) {
            let c = self.view.camera;
            let mut next = c;
            if outside[0] != self.written[0] || outside[1] != self.written[1] {
                next.center = LngLat::new(f64::from(outside[0]), f64::from(outside[1]));
            }
            if outside[2] != self.written[2] {
                next.zoom = f64::from(outside[2]);
            }
            if outside[3] != self.written[3] {
                next.bearing = f64::from(outside[3]);
            }
            self.halt();
            self.jump(next);
            self.settle_due = true;
        }
        let b = s.store.bounds(self.root);
        let size = [f64::from(b.w), f64::from(b.h)];
        if size != self.view.size {
            self.view.size = size;
            self.constrain();
            self.moved = true;
            self.dirty_tiles = true;
        }
        if size[0] <= 0.0 || size[1] <= 0.0 {
            return None;
        }
        let camera = self.view.camera;
        let band = band_of(camera.zoom);
        let tz = self.pyramid.tile_zoom(band);
        let key = BuildKey {
            style: self.style_rev,
            band: band.max(tz),
        };
        let band_scale = TILE_SIZE * f64::from(band).exp2();
        let c = camera.center.to_world();
        let cam = [c.x * band_scale, c.y * band_scale];
        let rebase = self.band != Some(band)
            || (cam[0] - self.anchor[0]).abs() > REBASE
            || (cam[1] - self.anchor[1]).abs() > REBASE;
        if rebase {
            if self.band != Some(band) {
                let bg = self.style.background.at(f32::from(band)).linear(1.0);
                s.store
                    .set_fill(self.root, Rgba::new(bg[0], bg[1], bg[2], bg[3]));
            }
            self.band = Some(band);
            self.anchor = [
                (cam[0] / TILE_SIZE).round() * TILE_SIZE,
                (cam[1] / TILE_SIZE).round() * TILE_SIZE,
            ];
            self.dirty_tiles = true;
        }
        // The camera node: the tiles' anchor brought to the focus, scaled
        // within the level and turned.
        let scale = (camera.zoom - f64::from(band)).exp2();
        let r = (-camera.bearing).to_radians();
        let (cos, sin) = (r.cos(), r.sin());
        let d = [
            (self.anchor[0] - cam[0]) * scale,
            (self.anchor[1] - cam[1]) * scale,
        ];
        let f = self.view.focus();
        let t = [
            f[0] - size[0] / 2.0 + d[0] * cos - d[1] * sin,
            f[1] - size[1] / 2.0 + d[0] * sin + d[1] * cos,
        ];
        s.store
            .set_translate(self.camera_node, Vec2::new(t[0] as f32, t[1] as f32));
        s.store.set_turn(
            self.camera_node,
            Turn {
                scale: scale as f32,
                rotation: r as f32,
                origin: viso_ui::Alignment2D::TopStart,
            },
        );
        // The tiles the view needs.
        covering(&self.view, tz, 0.0, &mut self.scratch, &mut self.prefetch);
        let same = self.prefetch == self.visible;
        if !same {
            std::mem::swap(&mut self.visible, &mut self.prefetch);
            self.dirty_tiles = true;
        }
        if self.dirty_tiles {
            self.dirty_tiles = false;
            covering(
                &self.view,
                tz,
                PREFETCH,
                &mut self.scratch,
                &mut self.prefetch,
            );
            let mut plan = std::mem::take(&mut self.plan);
            self.pyramid
                .plan(&self.visible, &self.prefetch, key, &mut plan);
            for &tile in &plan.cancel {
                self.stop_request(Some(s.store), tile);
            }
            for &tile in plan.fetch.iter().chain(&plan.rebuild) {
                self.start(s.store, tile, key);
            }
            self.place_tiles(s, &plan.draw, band, rebase);
            self.plan = plan;
        }
        let mut told = None;
        if self.moved {
            self.moved = false;
            self.written = camera_values(&self.view.camera);
            self.cells.write(s.states, self.written);
        }
        if self.settle_due && !self.held && !self.animating() {
            self.settle_due = false;
            let c = self.view.camera;
            s.store.set_semantics(self.root, self.semantics());
            told = Some(Told::Settled(c));
        }
        told
    }

    /// What assistive technology reads of the view.
    pub fn semantics(&self) -> Semantics {
        let c = self.view.camera;
        let (ns, ew) = (
            if c.center.lat >= 0.0 { 'N' } else { 'S' },
            if c.center.lon >= 0.0 { 'E' } else { 'W' },
        );
        Semantics::role(viso_ui::Role::Image)
            .with_label(self.label.clone())
            .with_hint(
                "Arrow keys pan, plus and minus zoom, Shift with Left and Right turns, N faces north",
            )
            .with_value(format!(
                "{:.4}° {ns}, {:.4}° {ew}, zoom {:.1}",
                c.center.lat.abs(),
                c.center.lon.abs(),
                c.zoom
            ))
    }

    /// Brings the camera node's children to `draw`, in order, each placed
    /// for zoom level `band`.
    fn place_tiles(&mut self, s: &mut StructureCx<'_>, draw: &[Slot], band: u8, rebase: bool) {
        self.shown.clear();
        for &slot in draw {
            if self
                .pyramid
                .ready(slot.tile)
                .is_some_and(|b| b.drawing.is_some())
            {
                self.shown.push(slot);
            }
        }
        let same_set = self.shown.len() == self.tiles.len()
            && self.shown.iter().zip(&self.tiles).all(|(s, t)| {
                *s == t.slot
                    && self
                        .pyramid
                        .ready(s.tile)
                        .is_some_and(|b| Rc::ptr_eq(b, &t.built))
            });
        if !same_set {
            // Free what is no longer drawn; keep and reorder the rest.
            let mut kept: Vec<TileNode> = Vec::with_capacity(self.shown.len());
            let mut old = std::mem::take(&mut self.tiles);
            for &slot in &self.shown {
                let built = Rc::clone(self.pyramid.ready(slot.tile).expect("ready"));
                let drawing = built.drawing.clone().expect("drawn");
                match old.iter().position(|t| t.slot == slot) {
                    Some(i) => {
                        let mut t = old.swap_remove(i);
                        if !Rc::ptr_eq(&t.built, &built) {
                            s.store.set_drawing(t.node, drawing);
                            t.built = built;
                        }
                        kept.push(t);
                    }
                    None => {
                        let node = {
                            let mut cx = BuildCx::with_parent(
                                s.store,
                                s.states,
                                s.bindings,
                                self.camera_node,
                            );
                            let side = TILE_SIZE as f32;
                            cx.drawing(Size::fixed(side, side), drawing).id()
                        };
                        kept.push(TileNode { slot, built, node });
                    }
                }
            }
            for t in old {
                s.store.free_tree(t.node, s.effects, &mut self.free_scratch);
            }
            // The order they paint in: ancestors beneath.
            let in_order = {
                let mut children = s.store.arena().children(self.camera_node);
                kept.iter().all(|t| children.next() == Some(t.node))
            };
            if !in_order {
                for t in &kept {
                    s.store.arena_detach(t.node);
                }
                for t in &kept {
                    s.store.arena_insert_before(self.camera_node, t.node, None);
                }
            }
            self.tiles = kept;
        } else if !rebase {
            return;
        }
        let band_scale = TILE_SIZE * f64::from(band).exp2();
        for t in &self.tiles {
            let (nw, _) = t.slot.square();
            let tau = [
                nw.x * band_scale - self.anchor[0],
                nw.y * band_scale - self.anchor[1],
            ];
            let k = (f64::from(band) - f64::from(t.slot.tile.z)).exp2();
            s.store
                .set_translate(t.node, Vec2::new(tau[0] as f32, tau[1] as f32));
            s.store.set_turn(
                t.node,
                Turn {
                    scale: k as f32,
                    rotation: 0.0,
                    origin: viso_ui::Alignment2D::TopStart,
                },
            );
        }
    }

    /// The tiles drawn, in paint order, and their nodes.
    pub fn drawn(&self) -> impl Iterator<Item = (Slot, NodeId)> + '_ {
        self.tiles.iter().map(|t| (t.slot, t.node))
    }

    /// The requests in flight.
    pub fn in_flight(&self) -> usize {
        self.requests.len()
    }
}

/// Raises the outer stop flag's state into the inner build's as the task
/// is dropped or finishes, so a cancelled fetch also stops its build.
struct StopWith(Arc<AtomicBool>, Arc<AtomicBool>);

impl Drop for StopWith {
    fn drop(&mut self) {
        if self.0.load(Ordering::Relaxed) {
            self.1.store(true, Ordering::Relaxed);
        }
    }
}
