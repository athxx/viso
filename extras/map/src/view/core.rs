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
    BuildCx, Continuation, DirtyClass, Instant, NodeId, Semantics, Sizing, StateId, StateStore,
    StateValue, StateWriter, StructureCx, TaskId, Turn, Vec2,
};

use super::labels::Labels;
use super::overlays::Overlays;
use super::rasters::Rasters;
use super::tilted::{Frame, Tilted};
use crate::camera::{Camera, Limits, MAX_PITCH, Motion, Padding, TILE_SIZE, View};
use crate::geo::LngLat;
use crate::geojson::DataLayer;
use crate::load::{Built, Data, Input, Recipe, build_off_loop};
use crate::mvt::VectorTile;
use crate::pyramid::{BuildKey, Plan, Pyramid, Slot, covering};
use crate::query::{QueriedFeature, query_tile};
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
    pub pitch: StateId,
}

impl CameraCells {
    /// Fresh cells holding `camera`.
    pub fn new(cx: &mut BuildCx<'_>, camera: &Camera) -> Self {
        Self {
            lon: cx.state(StateValue::Float(camera.center.lon as f32)),
            lat: cx.state(StateValue::Float(camera.center.lat as f32)),
            zoom: cx.state(StateValue::Float(camera.zoom as f32)),
            bearing: cx.state(StateValue::Float(camera.bearing as f32)),
            pitch: cx.state(StateValue::Float(camera.pitch as f32)),
        }
    }

    /// Every cell, for a hook to depend on.
    pub fn ids(&self) -> [StateId; 5] {
        [self.lon, self.lat, self.zoom, self.bearing, self.pitch]
    }

    fn read(&self, states: &StateStore) -> [f32; 5] {
        let f = |id| match states.get(id) {
            Some(StateValue::Float(v)) => v,
            _ => f32::NAN,
        };
        self.ids().map(f)
    }

    fn write(&self, states: &mut StateStore, v: [f32; 5]) {
        for (id, v) in self.ids().into_iter().zip(v) {
            states.set(id, StateValue::Float(v));
        }
    }
}

fn camera_values(c: &Camera) -> [f32; 5] {
    [
        c.center.lon as f32,
        c.center.lat as f32,
        c.zoom as f32,
        c.bearing as f32,
        c.pitch as f32,
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

/// One tile drawn: its slot, the build it shows and its nodes beneath and
/// over the image layers.
struct TileNode {
    slot: Slot,
    built: Ready,
    nodes: [Option<NodeId>; 2],
    fresh: bool,
}

/// Places a tile's node `node` of `slot` for level `band` about `anchor`.
pub(crate) fn place_tile(
    s: &mut StructureCx<'_>,
    node: NodeId,
    slot: Slot,
    band: u8,
    anchor: [f64; 2],
) {
    let band_scale = TILE_SIZE * f64::from(band).exp2();
    let (nw, _) = slot.square();
    let tau = [nw.x * band_scale - anchor[0], nw.y * band_scale - anchor[1]];
    let k = (f64::from(band) - f64::from(slot.tile.z)).exp2();
    s.store
        .set_translate(node, Vec2::new(tau[0] as f32, tau[1] as f32));
    s.store.set_turn(
        node,
        Turn {
            scale: k as f32,
            rotation: 0.0,
            origin: viso_ui::Alignment2D::TopStart,
        },
    );
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

/// The nodes a view's parts go under.
pub(crate) struct CoreNodes {
    pub labels: NodeId,
    pub camera: NodeId,
    pub tiles: NodeId,
    pub rasters: NodeId,
    pub over: NodeId,
    pub shapes: NodeId,
    pub accuracy: NodeId,
    pub overlay: NodeId,
}

/// The live state of a built map view.
pub(crate) struct Core {
    pub root: NodeId,
    pub camera_node: NodeId,
    /// The tiles' parents under the camera node: beneath and over the
    /// image layers.
    tiles_node: NodeId,
    over_node: NodeId,
    pub overlays: Overlays,
    pub labels: Labels,
    pub rasters: Rasters,
    pub tilted: Tilted,
    /// The app's data layers, by name, and all of them for the builds.
    data_layers: Vec<(String, Arc<DataLayer>)>,
    data: Data,
    /// The languages labels are named in, expanded.
    languages: Vec<String>,
    recipe: Option<Arc<Recipe>>,
    /// The cell the view's structure hook depends on: poked to run it.
    pub wake: StateId,
    pokes: i32,
    pub cells: CameraCells,
    /// What the cells held after the last write, to tell an outside write.
    written: [f32; 5],
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
    /// The drawn tiles' slots that offer labels.
    labeled: Vec<(Slot, Ready)>,
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
    /// The revision of the relief the labels were placed over.
    relief: Option<u64>,
}

impl Core {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        root: NodeId,
        nodes: CoreNodes,
        wake: StateId,
        cells: CameraCells,
        camera: Camera,
        limits: Limits,
        padding: Padding,
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
                camera_node: nodes.camera,
                tiles_node: nodes.tiles,
                over_node: nodes.over,
                overlays: Overlays::new(nodes.shapes, nodes.accuracy, nodes.overlay, nodes.labels),
                labels: Labels::new(nodes.labels),
                rasters: Rasters::new(nodes.rasters),
                tilted: Tilted::new(root, nodes.overlay),
                data_layers: Vec::new(),
                data: Arc::new([]),
                languages: Vec::new(),
                recipe: None,
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
                labeled: Vec::new(),
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
                relief: None,
            })
        })
    }

    /// Asks for the hook to run, from a task's continuation.
    pub fn wake_with(&mut self, cx: &mut UpdateCx<'_>) {
        self.pokes = self.pokes.wrapping_add(1);
        cx.set(self.wake, StateValue::Int(self.pokes));
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

    /// Tilts by `degrees` about the focus, within `0` and [`MAX_PITCH`].
    pub fn tilt_by(&mut self, degrees: f64) {
        let c = &mut self.view.camera;
        c.pitch = (c.pitch + degrees).clamp(0.0, MAX_PITCH);
        self.moved = true;
    }

    /// Whether the frame clock should keep running.
    pub fn animating(&self) -> bool {
        self.moving() || self.labels.fading() || self.labels.measuring()
    }

    /// Whether the camera moves on its own.
    pub fn moving(&self) -> bool {
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
        if self.labels.fading() {
            self.labels.fade_dt += dt;
            return true;
        }
        if self.labels.measuring() {
            return true;
        }
        moved
    }

    /// Uses `style` from now on, rebuilding the tiles from what they hold.
    pub fn set_style(&mut self, style: Arc<Style>) {
        self.rasters.restyle(&self.style, &style);
        self.style = style;
        self.recipe = None;
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

    /// Shows `data` as the tile layer `name`, or no longer with `None`;
    /// the tiles are built again with it.
    pub fn set_data(&mut self, name: &str, data: Option<&crate::geojson::GeoJson>) {
        self.data_layers.retain(|(n, _)| n != name);
        if let Some(data) = data {
            self.data_layers
                .push((name.to_owned(), Arc::new(DataLayer::new(name, data))));
        }
        self.data = self
            .data_layers
            .iter()
            .map(|(_, d)| Arc::clone(d))
            .collect();
        self.pyramid.forget_missing();
        self.recipe = None;
        self.style_rev += 1;
        self.dirty_tiles = true;
    }

    /// Names labels in the first of `languages` a feature has a name in.
    pub fn set_languages(&mut self, languages: &[String]) {
        let expanded = crate::label::expand_languages(languages);
        if expanded != self.languages {
            self.languages = expanded;
            self.recipe = None;
            self.style_rev += 1;
            self.dirty_tiles = true;
        }
    }

    /// What tiles build with now.
    fn recipe(&mut self) -> Arc<Recipe> {
        Arc::clone(self.recipe.get_or_insert_with(|| {
            Arc::new(Recipe {
                style: Arc::clone(&self.style),
                data: Arc::clone(&self.data),
                languages: self.languages.clone(),
            })
        }))
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
        let recipe = self.recipe();
        let has_data = !recipe.data.is_empty();
        let me = self.me.clone();
        let wake = self.wake;
        let (future, stop): (std::pin::Pin<Box<dyn std::future::Future<Output = _>>>, _) =
            match rebuild {
                Some(decoded) => {
                    let (answer, stop) = build_off_loop(Input::Decoded(decoded), tile, recipe, key);
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
                            let input = match fetch.await {
                                Ok(Some(bytes)) => Input::Bytes(bytes),
                                // The app's data still draws where the
                                // source has nothing.
                                Ok(None) if has_data => {
                                    Input::Decoded(Arc::new(VectorTile::default()))
                                }
                                Ok(None) => return Ok(None),
                                Err(_) => return Err(()),
                            };
                            if stopped.load(Ordering::Relaxed) {
                                return Err(());
                            }
                            let (answer, flag) = build_off_loop(input, tile, recipe, key);
                            let _keep = StopWith(stopped, flag);
                            answer.await.map(Some).map_err(|_| ())
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
            if outside[4] != self.written[4] {
                next.pitch = f64::from(outside[4]);
            }
            self.halt();
            self.jump(next);
            self.settle_due = true;
        }
        let b = s.store.bounds(self.root);
        let size = [f64::from(b.w), f64::from(b.h)];
        if size != self.view.size {
            self.view.size = size;
            self.labels.due = true;
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
        self.rasters.apply(
            s,
            &self.style,
            &self.view,
            band,
            self.anchor,
            rebase,
            &self.me,
        );
        let lift = self.tilted.lift(&self.view, tz);
        self.overlays.apply(
            s,
            &self.view,
            band,
            self.anchor,
            rebase,
            &self.me,
            lift.as_ref(),
        );
        if self.view.tilt().is_some() {
            s.store.set_hidden(self.camera_node, true);
            let drawn: Vec<(Slot, Ready)> = self
                .tiles
                .iter()
                .map(|t| (t.slot, Rc::clone(&t.built)))
                .collect();
            let frame = Frame {
                view: &self.view,
                band,
                tile_zoom: tz,
                anchor: self.anchor,
                style: &self.style,
                sources: self.rasters.sources(),
                rasters: &self.rasters,
                me: &self.me,
            };
            let revision = self.overlays.revision;
            self.tilted
                .apply(s, &frame, &drawn, self.overlays.framed(), revision);
        } else if self.tilted.shown() {
            s.store.set_hidden(self.camera_node, false);
            self.tilted.hide(s);
        }
        let mut told = None;
        if self.moved {
            self.moved = false;
            self.labels.drifted = true;
            self.written = camera_values(&self.view.camera);
            self.cells.write(s.states, self.written);
        }
        // Labels: faded on, placed again as the camera moves and once it
        // rests, and moved with it.
        self.labels.reduced_motion = self.reduced_motion;
        if self.labels.fade_dt > 0.0 {
            let dt = std::mem::take(&mut self.labels.fade_dt);
            self.labels.fade(s, dt);
        }
        self.labels.measure(s);
        let lift = self.tilted.lift(&self.view, tz);
        let relief = lift.as_ref().map(|_| self.tilted.relief_revision());
        if relief != self.relief {
            self.relief = relief;
            self.labels.due = true;
        }
        let still = !self.held && !self.moving();
        if self.labels.wants_pass(still, Instant::now()) {
            let tiles: Vec<(Slot, &Built)> =
                self.labeled.iter().map(|(slot, b)| (*slot, &**b)).collect();
            self.labels.pass(s, &self.view, &tiles, lift.as_ref());
        }
        self.labels.follow(s, &self.view, lift.as_ref());
        if self.settle_due && !self.held && !self.moving() {
            self.labels.due = true;
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
                "Arrow keys pan, plus and minus zoom, Shift with Left and Right turns, Shift with Up and Down tilts, N faces north",
            )
            .with_value(if c.pitch > 0.0 {
                format!(
                    "{:.4}° {ns}, {:.4}° {ew}, zoom {:.1}, tilted {:.0}°",
                    c.center.lat.abs(),
                    c.center.lon.abs(),
                    c.zoom,
                    c.pitch
                )
            } else {
                format!(
                    "{:.4}° {ns}, {:.4}° {ew}, zoom {:.1}",
                    c.center.lat.abs(),
                    c.center.lon.abs(),
                    c.zoom
                )
            })
    }

    /// Brings the camera node's children to `draw`, in order, each placed
    /// for zoom level `band`.
    fn place_tiles(&mut self, s: &mut StructureCx<'_>, draw: &[Slot], band: u8, rebase: bool) {
        // The tiles whose labels count, drawing or not.
        let labeled = draw.iter().filter_map(|&slot| {
            self.pyramid
                .ready(slot.tile)
                .filter(|b| !b.labels.is_empty())
                .map(|b| (slot, b))
        });
        let same_labels = labeled.clone().count() == self.labeled.len()
            && labeled
                .clone()
                .zip(&self.labeled)
                .all(|((s, b), (t, c))| s == *t && Rc::ptr_eq(b, c));
        if !same_labels {
            self.labeled = labeled.map(|(s, b)| (s, Rc::clone(b))).collect();
            self.labels.due = true;
        }
        self.shown.clear();
        for &slot in draw {
            if self
                .pyramid
                .ready(slot.tile)
                .is_some_and(|b| b.drawing.is_some() || b.over.is_some())
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
        let parents = [self.tiles_node, self.over_node];
        if !same_set {
            // Free what is no longer drawn; keep and reorder the rest.
            let mut kept: Vec<TileNode> = Vec::with_capacity(self.shown.len());
            let mut old = std::mem::take(&mut self.tiles);
            let side = TILE_SIZE as f32;
            for &slot in &self.shown {
                let built = Rc::clone(self.pyramid.ready(slot.tile).expect("ready"));
                let parts = [built.drawing.clone(), built.over.clone()];
                let mut t = match old.iter().position(|t| t.slot == slot) {
                    Some(i) => old.swap_remove(i),
                    None => TileNode {
                        slot,
                        built: Rc::clone(&built),
                        nodes: [None, None],
                        fresh: true,
                    },
                };
                if t.fresh || !Rc::ptr_eq(&t.built, &built) {
                    for (p, part) in parts.into_iter().enumerate() {
                        match (t.nodes[p], part) {
                            (Some(node), Some(drawing)) => s.store.set_drawing(node, drawing),
                            (Some(node), None) => {
                                s.store.free_tree(node, s.effects, &mut self.free_scratch);
                                t.nodes[p] = None;
                            }
                            (None, Some(drawing)) => {
                                let mut cx =
                                    BuildCx::with_parent(s.store, s.states, s.bindings, parents[p]);
                                t.nodes[p] =
                                    Some(cx.drawing(Sizing::fixed(side, side), drawing).id());
                            }
                            (None, None) => {}
                        }
                    }
                    t.built = built;
                    t.fresh = false;
                }
                kept.push(t);
            }
            for t in old {
                for node in t.nodes.into_iter().flatten() {
                    s.store.free_tree(node, s.effects, &mut self.free_scratch);
                }
            }
            for (p, &parent) in parents.iter().enumerate() {
                s.store.mark_dirty(
                    parent,
                    DirtyClass::MEASURE | DirtyClass::LAYOUT | DirtyClass::PAINT,
                );
                // The order they paint in: ancestors beneath.
                let in_order = {
                    let mut children = s.store.arena().children(parent);
                    kept.iter()
                        .filter_map(|t| t.nodes[p])
                        .all(|n| children.next() == Some(n))
                };
                if !in_order {
                    for n in kept.iter().filter_map(|t| t.nodes[p]) {
                        s.store.arena_detach(n);
                    }
                    for n in kept.iter().filter_map(|t| t.nodes[p]) {
                        s.store.arena_insert_before(parent, n, None);
                    }
                }
            }
            self.tiles = kept;
        } else if !rebase {
            return;
        }
        for t in &self.tiles {
            for node in t.nodes.into_iter().flatten() {
                place_tile(s, node, t.slot, band, self.anchor);
            }
        }
    }

    /// The tiles drawn, in paint order.
    pub fn drawn(&self) -> impl Iterator<Item = Slot> + '_ {
        self.tiles.iter().map(|t| t.slot)
    }

    /// The features drawn under view point `p`, within `radius` points,
    /// topmost first.
    pub fn features_at(&self, p: [f64; 2], radius: f64) -> Vec<QueriedFeature> {
        let w = self.view.to_world(p);
        let mut out = Vec::new();
        // The deepest tile drawn there answers: children paint last.
        let Some(t) = self.tiles.iter().rev().find(|t| {
            let (nw, side) = t.slot.square();
            w.x >= nw.x && w.x < nw.x + side && w.y >= nw.y && w.y < nw.y + side
        }) else {
            return out;
        };
        let (nw, side) = t.slot.square();
        let extent = f64::from(crate::build::EXTENT);
        let at = [(w.x - nw.x) / side * extent, (w.y - nw.y) / side * extent];
        let zoom = self.view.camera.zoom;
        let tile_points = TILE_SIZE * (zoom - f64::from(t.slot.tile.z)).exp2();
        let units_per_point = extent / tile_points;
        let layers: Vec<&crate::mvt::Layer> =
            t.built.tile.layers.iter().chain(&t.built.data).collect();
        // The style's values are those the tile was built at.
        let (built_zoom, _) = crate::load::params_for(t.slot.tile, t.built.key);
        query_tile(
            &layers,
            &self.style,
            built_zoom,
            at,
            radius * units_per_point,
            units_per_point,
            &mut out,
        );
        out
    }

    /// The requests, raises and elevation loads in flight.
    pub fn in_flight(&self) -> usize {
        self.requests.len() + self.rasters.loading() + self.tilted.working()
    }
}

/// Raises the outer stop flag's state into the inner build's as the task
/// is dropped or finishes, so a cancelled fetch also stops its build.
pub(crate) struct StopWith(pub Arc<AtomicBool>, pub Arc<AtomicBool>);

impl Drop for StopWith {
    fn drop(&mut self) {
        if self.0.load(Ordering::Relaxed) {
            self.1.store(true, Ordering::Relaxed);
        }
    }
}
