//! The map view: a component that shows a tile source's map, moves with
//! the hand, the wheel and the keys, and tells the app where it was tapped
//! and where it came to rest.
//!
//! ```no_run
//! use viso_map::{MemorySource, map_view, style::builtin};
//! # fn build(cx: &mut viso_ui::BuildCx<'_>) {
//! map_view(MemorySource::new())
//!     .center(13.40, 52.52)
//!     .zoom(12.0)
//!     .style(builtin::openmaptiles_light())
//!     .on_tap(|_, at| println!("tapped {:?}", at.place))
//!     .build(cx);
//! # }
//! ```
//!
//! A one-finger or mouse drag pans and coasts on when let go; two fingers
//! pinch to zoom about their middle and turn the map once they have turned
//! past a few degrees, settling north when let go near it; a trackpad's
//! pinch does the same, a wheel's notch zooms about the pointer and a
//! trackpad's two-finger scroll pans. A double tap zooms in about where it
//! lands, a two-finger tap zooms out, and a drag with Alt held turns the
//! map. Focused, arrow keys pan, `+` and `-` zoom, Shift with Left and Right
//! turns and `N` faces north again.

mod core;
mod overlays;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use viso_ui::{
    Alignment2D, BoxStyle, BuildCx, Component, DispatchPhase, EventCx, FrameClient, Key,
    NoticeKinds, PinchPhase, PointerPhase, ScrollPhase, Size, StackStyle, StateValue, StateWriter,
};

pub use self::core::CameraCells;
use self::core::{Core, CoreNodes, Told, band_of};
use self::overlays::Pressed;
pub use self::overlays::{Cluster, ClusterContent, Marker, MarkerContent, ShapeId};
use crate::camera::{Camera, Insets, Limits, View};
use crate::geo::{LngLat, LngLatBounds};
use crate::geojson::GeoJson;
use crate::overlay::{Puck, Shape};
use crate::pyramid::Slot;
use crate::query::QueriedFeature;
use crate::source::TileSource;
use crate::style::{Style, builtin};

/// How far one key press pans, in points.
const KEY_PAN: f64 = 100.0;
/// How far one key press turns, in degrees.
const KEY_TURN: f64 = 15.0;
/// How long a key's or a double tap's move takes, in seconds.
const STEP_EASE: f64 = 0.25;
/// How far a wheel turns (pt) to zoom one level.
const WHEEL_LEVEL: f64 = 240.0;
/// How long a wheel's zoom eases, in seconds.
const WHEEL_EASE: f64 = 0.15;
/// How far (radians) two fingers turn before the map turns with them.
const TURN_START: f32 = 0.17;
/// Within this many degrees of north a turn let go settles north.
const NORTH_SNAP: f64 = 7.0;
/// Degrees an Alt-drag turns per point across.
const DRAG_TURN: f64 = 0.4;

/// A point of the map an input landed on: the place and the view point.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MapPoint {
    pub place: LngLat,
    /// Points from the view's top-left corner.
    pub point: [f64; 2],
}

/// What a tap or a long press tells the app.
pub type PlaceHandler = Rc<dyn Fn(&mut EventCx<'_>, MapPoint)>;
/// What a tap on a marker tells the app: the marker's id.
pub type MarkerHandler = Rc<dyn Fn(&mut EventCx<'_>, u64)>;
/// What the camera coming to rest tells the app.
pub type ViewportHandler = Rc<dyn Fn(&mut dyn StateWriter, &Camera)>;

/// A slot an app passes to [`MapView::handle`] and reads after building for
/// the view's [`MapHandle`].
pub type MapHandleSlot = Rc<RefCell<Option<MapHandle>>>;

/// Moves a built map view's camera and reads it, from a handler or a task.
/// Cheap to clone.
#[derive(Clone)]
pub struct MapHandle {
    core: Rc<RefCell<Core>>,
}

impl std::fmt::Debug for MapHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MapHandle").finish_non_exhaustive()
    }
}

impl MapHandle {
    fn with<R>(&self, w: &mut dyn StateWriter, f: impl FnOnce(&mut Core) -> R) -> R {
        let mut core = self.core.borrow_mut();
        let out = f(&mut core);
        core.poke(w);
        out
    }

    /// The camera as it is now.
    pub fn camera(&self) -> Camera {
        self.core.borrow().camera()
    }

    /// The camera in its view: the math between places and view points.
    pub fn view(&self) -> View {
        self.core.borrow().view
    }

    /// The view point of `place`, from the view's top-left corner.
    pub fn project(&self, place: LngLat) -> [f64; 2] {
        self.core.borrow().view.project(place)
    }

    /// The place under view point `point`.
    pub fn unproject(&self, point: [f64; 2]) -> LngLat {
        self.core.borrow().view.unproject(point)
    }

    /// The cells the view writes its camera into.
    pub fn cells(&self) -> CameraCells {
        self.core.borrow().cells
    }

    /// Moves the camera to `camera` at once.
    pub fn jump_to(&self, w: &mut dyn StateWriter, camera: Camera) {
        self.with(w, |c| {
            c.halt();
            c.jump(camera);
            c.settle_due = true;
        });
    }

    /// Moves the camera to `camera` over `duration` seconds, easing.
    pub fn ease_to(&self, w: &mut dyn StateWriter, camera: Camera, duration: f64) {
        self.with(w, |c| c.ease_to(camera, duration));
    }

    /// Flies the camera to `camera`: out, across and back in, over
    /// `duration` seconds or as long as the distance asks.
    pub fn fly_to(&self, w: &mut dyn StateWriter, camera: Camera, duration: Option<f64>) {
        self.with(w, |c| c.fly_to(camera, duration));
    }

    /// Eases to show `bounds` whole inside the view kept `padding` clear,
    /// facing `bearing`, no closer than `max_zoom`.
    pub fn fit_bounds(
        &self,
        w: &mut dyn StateWriter,
        bounds: LngLatBounds,
        padding: Insets,
        bearing: f64,
        max_zoom: f64,
    ) {
        self.with(w, |c| {
            let mut view = c.view;
            view.padding = padding;
            let to = view.fit(bounds, bearing, max_zoom.min(c.limits.max_zoom));
            c.ease_to(to, 0.5);
        });
    }

    /// Pans the view by `by` points, as a drag by it would.
    pub fn pan_by(&self, w: &mut dyn StateWriter, by: [f64; 2]) {
        self.with(w, |c| {
            let f = c.view.focus();
            let mut view = c.view;
            view.camera = c.aim();
            let to = view.panned(f, [f[0] - by[0], f[1] - by[1]]);
            c.ease_to(to, STEP_EASE);
        });
    }

    /// Zooms by `delta` levels about view point `about`, the focus for
    /// `None`.
    pub fn zoom_by(&self, w: &mut dyn StateWriter, delta: f64, about: Option<[f64; 2]>) {
        self.with(w, |c| zoom_eased(c, delta, about, STEP_EASE));
    }

    /// Turns the map to face `bearing`.
    pub fn rotate_to(&self, w: &mut dyn StateWriter, bearing: f64) {
        self.with(w, |c| {
            let to = c.aim().with_bearing(bearing);
            c.ease_to(to, STEP_EASE * 1.5);
        });
    }

    /// Draws the map with `style` from now on, rebuilding its tiles from
    /// what they hold rather than fetching them again.
    pub fn set_style(&self, w: &mut dyn StateWriter, style: Style) {
        self.with(w, |c| c.set_style(Arc::new(style)));
    }

    /// Keeps `padding` clear at the view's edges; the camera centers in
    /// what is left.
    pub fn set_padding(&self, w: &mut dyn StateWriter, padding: Insets) {
        self.with(w, |c| {
            c.view.padding = padding;
            c.moved = true;
        });
    }

    /// Shows `source`'s tiles from now on, over zooms `zooms`.
    pub fn set_source(&self, w: &mut dyn StateWriter, source: impl TileSource, zooms: (u8, u8)) {
        self.with(w, |c| c.set_source(Rc::new(source), zooms));
    }

    /// What the view holds now: the tiles it draws, in paint order, the
    /// requests in flight and the builds it started.
    /// The features drawn under view point `point`, within `radius` points
    /// of it, topmost first: what a tap landed on.
    pub fn features_at(&self, point: [f64; 2], radius: f64) -> Vec<QueriedFeature> {
        self.core.borrow().features_at(point, radius)
    }

    /// Draws `shape` over the tiles and the shapes added before it.
    pub fn add_shape(&self, w: &mut dyn StateWriter, shape: impl Into<Shape>) -> ShapeId {
        self.with(w, |c| c.overlays.add_shape(shape.into()))
    }

    /// Replaces shape `id`; whether it was there.
    pub fn set_shape(&self, w: &mut dyn StateWriter, id: ShapeId, shape: impl Into<Shape>) -> bool {
        self.with(w, |c| c.overlays.set_shape(id, shape.into()))
    }

    /// Takes shape `id` away; whether it was there.
    pub fn remove_shape(&self, w: &mut dyn StateWriter, id: ShapeId) -> bool {
        self.with(w, |c| c.overlays.remove_shape(id))
    }

    /// Shape `id` as it is.
    pub fn shape(&self, id: ShapeId) -> Option<Shape> {
        self.core.borrow().overlays.shape(id).cloned()
    }

    /// Pins `marker`, or replaces the one of its id.
    pub fn set_marker(&self, w: &mut dyn StateWriter, marker: Marker) {
        self.with(w, |c| c.overlays.set_marker(marker));
    }

    /// Moves marker `id` to `at`; whether it was there.
    pub fn move_marker(&self, w: &mut dyn StateWriter, id: u64, at: LngLat) -> bool {
        self.with(w, |c| c.overlays.move_marker(id, at))
    }

    /// Takes marker `id` away; whether it was there.
    pub fn remove_marker(&self, w: &mut dyn StateWriter, id: u64) -> bool {
        self.with(w, |c| c.overlays.remove_marker(id))
    }

    /// The markers pinned, by id, where they are.
    pub fn markers(&self) -> Vec<(u64, LngLat)> {
        self.core.borrow().overlays.markers().collect()
    }

    /// The user's position as shown.
    pub fn puck(&self) -> Option<Puck> {
        self.core.borrow().overlays.puck()
    }

    /// The markers shown on their own, and each cluster's size and place.
    pub fn markers_shown(&self) -> (Vec<u64>, Vec<(usize, LngLat)>) {
        self.core.borrow().overlays.shown()
    }

    /// Gathers nearby markers by `cluster`, or no longer with `None`.
    pub fn set_cluster(&self, w: &mut dyn StateWriter, cluster: Option<Cluster>) {
        self.with(w, |c| c.overlays.set_cluster(cluster));
    }

    /// Shows the user's position, or no longer with `None`.
    pub fn set_puck(&self, w: &mut dyn StateWriter, puck: Option<Puck>) {
        self.with(w, |c| c.overlays.set_puck(puck));
    }

    /// Shows `data` as the tile layer `name` the style's layers draw, or no
    /// longer with `None`.
    pub fn set_data(&self, w: &mut dyn StateWriter, name: &str, data: Option<&GeoJson>) {
        self.with(w, |c| c.set_data(name, data));
    }

    /// The tiles drawn and the work in flight.
    pub fn tiles(&self) -> MapTiles {
        let core = self.core.borrow();
        MapTiles {
            drawn: core.drawn().map(|(slot, _)| slot).collect(),
            in_flight: core.in_flight(),
            builds: core.builds,
            bytes: core.pyramid.bytes(),
        }
    }
}

/// A census of a view's tiles ([`MapHandle::tiles`]).
#[derive(Debug, Clone, PartialEq)]
pub struct MapTiles {
    pub drawn: Vec<Slot>,
    pub in_flight: usize,
    pub builds: u32,
    pub bytes: usize,
}

fn zoom_eased(c: &mut Core, delta: f64, about: Option<[f64; 2]>, duration: f64) {
    let mut view = c.view;
    view.camera = c.aim();
    let at = about.unwrap_or_else(|| view.focus());
    let z = (view.camera.zoom + delta).clamp(c.limits.min_zoom, c.limits.max_zoom);
    let to = view.zoomed_about(z - view.camera.zoom, at);
    c.ease_to(to, duration);
}

/// A map view of a tile source. Build one with [`map_view`].
pub struct MapView {
    source: Rc<dyn TileSource>,
    zooms: (u8, u8),
    camera: Camera,
    limits: Limits,
    padding: Insets,
    style: Arc<Style>,
    size: Size,
    label: String,
    slot: Option<MapHandleSlot>,
    cells: Option<CameraCells>,
    on_tap: Option<PlaceHandler>,
    on_long_press: Option<PlaceHandler>,
    on_viewport: Option<ViewportHandler>,
    on_marker_tap: Option<MarkerHandler>,
    shapes: Vec<Shape>,
    markers: Vec<Marker>,
    cluster: Option<Cluster>,
    puck: Option<Puck>,
    data: Vec<(String, GeoJson)>,
}

/// A map view of `source`, filling its parent, over the whole world in the
/// light OpenMapTiles style.
pub fn map_view(source: impl TileSource) -> MapView {
    MapView {
        source: Rc::new(source),
        zooms: (0, 14),
        camera: Camera::new(LngLat::default(), 1.0),
        limits: Limits::default(),
        padding: Insets::default(),
        style: Arc::new(builtin::openmaptiles_light()),
        size: Size::fill(),
        label: "Map".to_owned(),
        slot: None,
        cells: None,
        on_tap: None,
        on_long_press: None,
        on_viewport: None,
        on_marker_tap: None,
        shapes: Vec::new(),
        markers: Vec::new(),
        cluster: None,
        puck: None,
        data: Vec::new(),
    }
}

impl MapView {
    /// Centers the camera on longitude `lon`, latitude `lat`.
    pub fn center(mut self, lon: f64, lat: f64) -> Self {
        self.camera.center = LngLat::new(lon, lat);
        self
    }

    /// Opens at `zoom`.
    pub fn zoom(mut self, zoom: f64) -> Self {
        self.camera.zoom = zoom;
        self
    }

    /// Opens facing `bearing`, degrees clockwise from north.
    pub fn bearing(mut self, bearing: f64) -> Self {
        self.camera.bearing = bearing;
        self
    }

    /// Opens at `camera`.
    pub fn camera(mut self, camera: Camera) -> Self {
        self.camera = camera;
        self
    }

    /// Draws with `style`.
    pub fn style(mut self, style: Style) -> Self {
        self.style = Arc::new(style);
        self
    }

    /// Keeps the camera within `limits`.
    pub fn limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self
    }

    /// Keeps `padding` clear at the edges.
    pub fn padding(mut self, padding: Insets) -> Self {
        self.padding = padding;
        self
    }

    /// The zooms the source has tiles for (default 0 to 14); past the
    /// deepest the tiles are drawn larger.
    pub fn source_zooms(mut self, min: u8, max: u8) -> Self {
        self.zooms = (min, max.max(min));
        self
    }

    /// Its own size within its parent.
    pub fn size(mut self, size: Size) -> Self {
        self.size = size;
        self
    }

    /// Names it to assistive technology (default "Map").
    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = label.into();
        self
    }

    /// Fills `slot` with its [`MapHandle`] as it builds.
    pub fn handle(mut self, slot: &MapHandleSlot) -> Self {
        self.slot = Some(Rc::clone(slot));
        self
    }

    /// Writes its camera into `cells`, and moves to what is written there.
    pub fn camera_cells(mut self, cells: CameraCells) -> Self {
        self.cells = Some(cells);
        self
    }

    /// Calls `f` with where a tap landed.
    pub fn on_tap(mut self, f: impl Fn(&mut EventCx<'_>, MapPoint) + 'static) -> Self {
        self.on_tap = Some(Rc::new(f));
        self
    }

    /// Calls `f` with where a long press held.
    pub fn on_long_press(mut self, f: impl Fn(&mut EventCx<'_>, MapPoint) + 'static) -> Self {
        self.on_long_press = Some(Rc::new(f));
        self
    }

    /// Calls `f` with a marker's id when it is tapped.
    pub fn on_marker_tap(mut self, f: impl Fn(&mut EventCx<'_>, u64) + 'static) -> Self {
        self.on_marker_tap = Some(Rc::new(f));
        self
    }

    /// Draws `shape` over the tiles.
    pub fn shape(mut self, shape: impl Into<Shape>) -> Self {
        self.shapes.push(shape.into());
        self
    }

    /// Pins `marker`.
    pub fn marker(mut self, marker: Marker) -> Self {
        self.markers.push(marker);
        self
    }

    /// Gathers nearby markers by `cluster`.
    pub fn cluster(mut self, cluster: Cluster) -> Self {
        self.cluster = Some(cluster);
        self
    }

    /// Shows the user's position.
    pub fn puck(mut self, puck: Puck) -> Self {
        self.puck = Some(puck);
        self
    }

    /// Shows `data` as the tile layer `name`, drawn by the style's layers
    /// of that source layer.
    pub fn data(mut self, name: &str, data: GeoJson) -> Self {
        self.data.push((name.to_owned(), data));
        self
    }

    /// Calls `f` with the camera each time it comes to rest.
    pub fn on_viewport(mut self, f: impl Fn(&mut dyn StateWriter, &Camera) + 'static) -> Self {
        self.on_viewport = Some(Rc::new(f));
        self
    }
}

/// The frame clock of one view: runs its motions and asks for its hook.
struct Clock {
    core: Rc<RefCell<Core>>,
    due: bool,
}

impl FrameClient for Clock {
    fn frame(&mut self, dt: f64) {
        if let Ok(mut core) = self.core.try_borrow_mut()
            && core.animating()
        {
            self.due |= core.tick(dt);
        }
    }

    fn key(&mut self, _key: Key, _pressed: bool) -> bool {
        false
    }

    fn release_input(&mut self) {}

    fn wants_frame(&self) -> bool {
        self.due || self.core.try_borrow().is_ok_and(|c| c.animating())
    }

    fn settle(&mut self, states: &mut viso_ui::StateStore) {
        if std::mem::take(&mut self.due)
            && let Ok(mut core) = self.core.try_borrow_mut()
        {
            core.poke(states);
        }
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

/// The gesture state a view's pointer handler keeps.
#[derive(Default)]
struct Hand {
    /// The pointers down, and whether a gesture used the current press.
    down: u8,
    used: bool,
    /// Two fingers landed together and have not moved apart as a pinch.
    two: bool,
    pinching: bool,
    turning: bool,
    /// A drag turning the map, not panning it.
    alt_drag: bool,
    /// The pan's velocity as the drag ended, to coast on with.
    fling: Option<[f64; 2]>,
}

impl Component for MapView {
    fn build(&self, cx: &mut BuildCx<'_>) {
        let bg = self.style.background.at(band_of(self.camera.zoom) as f32);
        let bg = bg.linear(1.0);
        let mut camera_node = None;
        let mut overlay = None;
        let mut parts = None;
        let root = cx.stack(
            StackStyle {
                align: Alignment2D::Center,
                size: self.size,
                style: BoxStyle::solid(viso_render::Rgba::new(bg[0], bg[1], bg[2], bg[3])),
                ..StackStyle::default()
            },
            |cx| {
                let sizeless = || StackStyle {
                    size: Size::fixed(0.0, 0.0),
                    align: Alignment2D::TopStart,
                    ..StackStyle::default()
                };
                camera_node = Some(cx.stack(sizeless(), |cx| {
                    parts = Some([
                        cx.stack(sizeless(), |_| {}).id(),
                        cx.stack(sizeless(), |_| {}).id(),
                        cx.stack(sizeless(), |_| {}).id(),
                    ]);
                }));
                overlay = Some(cx.stack(
                    StackStyle {
                        size: Size::fill(),
                        align: Alignment2D::TopStart,
                        ..StackStyle::default()
                    },
                    |_| {},
                ));
            },
        );
        let (camera_node, overlay) = (camera_node.unwrap(), overlay.unwrap());
        let parts = parts.expect("built");
        let nodes = CoreNodes {
            camera: camera_node.id(),
            tiles: parts[0],
            shapes: parts[1],
            accuracy: parts[2],
            overlay: overlay.id(),
        };
        cx.clip(root, true);
        cx.draggable(root, true);
        cx.pinchable(root, true);
        cx.long_pressable(root, true);
        cx.count_clicks(root);
        cx.takes_wheel(root, true);
        cx.focusable(root, true);
        let wake = cx.state(StateValue::Int(0));
        let cells = self
            .cells
            .unwrap_or_else(|| CameraCells::new(cx, &self.camera));
        let core = Core::new(
            root.id(),
            nodes,
            wake,
            cells,
            self.camera,
            self.limits,
            self.padding,
            Rc::clone(&self.source),
            self.zooms,
            Arc::clone(&self.style),
            self.label.clone(),
        );
        {
            let mut c = core.borrow_mut();
            for shape in &self.shapes {
                c.overlays.add_shape(shape.clone());
            }
            for m in &self.markers {
                c.overlays.set_marker(m.clone());
            }
            c.overlays.set_cluster(self.cluster.clone());
            c.overlays.set_puck(self.puck);
            for (name, data) in &self.data {
                c.set_data(name, Some(data));
            }
        }
        cx.semantics(root, core.borrow().semantics());
        if let Some(slot) = &self.slot {
            *slot.borrow_mut() = Some(MapHandle {
                core: Rc::clone(&core),
            });
        }
        let on_viewport = self.on_viewport.clone();
        let hook_core = Rc::clone(&core);
        let watch_core = Rc::clone(&core);
        cx.structure(|s| {
            s.store.add_structure_hook(
                [wake, cells.lon, cells.lat, cells.zoom, cells.bearing],
                move |s, _| {
                    let told = match hook_core.try_borrow_mut() {
                        Ok(mut core) => core.apply(s),
                        Err(_) => None,
                    };
                    if let (Some(Told::Settled(camera)), Some(f)) = (told, &on_viewport) {
                        f(s.states, &camera);
                    }
                },
            );
            s.store.add_frame_client(
                root.id(),
                Rc::new(RefCell::new(Clock {
                    core: Rc::clone(&core),
                    due: false,
                })),
            );
            s.store.watch(
                root.id(),
                NoticeKinds::SIZE,
                Box::new(move |_, states| {
                    if let Ok(mut core) = watch_core.try_borrow_mut() {
                        core.poke(states);
                    }
                }),
            );
        });
        let hand = Rc::new(RefCell::new(Hand::default()));
        let pc = Rc::clone(&core);
        let handlers = Handlers {
            tap: self.on_tap.clone(),
            long: self.on_long_press.clone(),
            marker: self.on_marker_tap.clone(),
        };
        cx.on_pointer(root, move |ev| {
            if ev.phase() == DispatchPhase::Capture {
                return;
            }
            pointer(ev, &pc, &mut hand.borrow_mut(), &handlers);
        });
        let kc = Rc::clone(&core);
        cx.on_key(root, move |ev| {
            if key(ev, &mut kc.borrow_mut()) {
                ev.stop_propagation();
            }
        });
    }
}

fn local(ev: &EventCx<'_>, x: f32, y: f32) -> [f64; 2] {
    let p = ev.local(x, y);
    [f64::from(p.x), f64::from(p.y)]
}

/// What a view tells the app of its taps and presses.
struct Handlers {
    tap: Option<PlaceHandler>,
    long: Option<PlaceHandler>,
    marker: Option<MarkerHandler>,
}

fn pointer(ev: &mut EventCx<'_>, core: &Rc<RefCell<Core>>, hand: &mut Hand, on: &Handlers) {
    let node = core.borrow().root;
    if let Some(w) = ev.wheel() {
        let at = local(ev, w.at.x, w.at.y);
        let mut c = core.borrow_mut();
        match w.phase {
            ScrollPhase::None => {
                let delta = -f64::from(if w.delta.y != 0.0 {
                    w.delta.y
                } else {
                    w.delta.x
                });
                zoom_eased(&mut c, delta / WHEEL_LEVEL, Some(at), WHEEL_EASE);
            }
            ScrollPhase::Began | ScrollPhase::Changed | ScrollPhase::Momentum => {
                if w.phase == ScrollPhase::Began {
                    c.halt();
                }
                let d = [f64::from(w.delta.x), f64::from(w.delta.y)];
                c.pan(at, [at[0] - d[0], at[1] - d[1]]);
                c.held = true;
            }
            ScrollPhase::Ended | ScrollPhase::MomentumEnded => {
                c.held = false;
                c.settle_due = true;
            }
        }
        c.poke(ev);
        ev.stop_propagation();
        return;
    }
    if let Some(p) = ev.pinch() {
        let at = local(ev, p.focal.x, p.focal.y);
        let mut c = core.borrow_mut();
        if p.phase == PinchPhase::Start {
            c.halt();
            c.held = true;
            hand.pinching = true;
            hand.turning = false;
            hand.two = false;
            hand.used = true;
        }
        // What the fingers did since they landed, or since the last sample.
        if matches!(p.phase, PinchPhase::Start | PinchPhase::Move) {
            let from = [at[0] - f64::from(p.pan.x), at[1] - f64::from(p.pan.y)];
            c.pan(from, at);
            if p.scale_delta > 0.0 {
                c.zoom_about(f64::from(p.scale_delta).log2(), at);
            }
            // The map turns with the fingers: clockwise on screen turns
            // the bearing back.
            if !hand.turning && p.rotation.abs() > TURN_START {
                hand.turning = true;
                c.turn_about(-f64::from(p.rotation).to_degrees(), at);
            } else if hand.turning {
                c.turn_about(-f64::from(p.rotation_delta).to_degrees(), at);
            }
        }
        if p.phase == PinchPhase::End {
            hand.pinching = false;
            c.held = false;
            let b = c.camera().bearing;
            if b != 0.0 && b.abs() < NORTH_SNAP {
                let to = c.camera().with_bearing(0.0);
                c.ease_to(to, STEP_EASE);
            } else {
                c.settle_due = true;
            }
        }
        c.poke(ev);
        ev.stop_propagation();
        return;
    }
    if let Some(lp) = ev.long_press() {
        hand.used = true;
        let at = local(ev, lp.at.x, lp.at.y);
        let place = core.borrow().view.unproject(at);
        core.borrow_mut().overlays.pressed = None;
        if let Some(f) = &on.long {
            f(ev, MapPoint { place, point: at });
        }
        return;
    }
    let Some(sample) = ev.pointer().copied() else {
        return;
    };
    if let Some(d) = ev.drag() {
        let at = local(ev, d.at.x, d.at.y);
        let from = local(ev, d.at.x - d.delta.x, d.at.y - d.delta.y);
        let mut c = core.borrow_mut();
        match d.phase {
            viso_ui::DragPhase::Start => {
                c.halt();
                c.held = true;
                hand.used = true;
                hand.alt_drag = sample.modifiers.alt;
                hand.fling = None;
            }
            viso_ui::DragPhase::Move => {}
            viso_ui::DragPhase::End => {
                c.held = false;
                hand.fling = Some([f64::from(d.velocity.x), f64::from(d.velocity.y)]);
            }
        }
        if hand.alt_drag {
            let f = c.view.focus();
            c.turn_about(-(at[0] - from[0]) * DRAG_TURN, f);
        } else if !hand.pinching {
            c.pan(from, at);
        }
        if d.phase == viso_ui::DragPhase::End {
            match hand.fling.take() {
                Some(v) if !hand.alt_drag && !hand.pinching => c.fling(v),
                _ => c.settle_due = true,
            }
        }
        c.poke(ev);
        return;
    }
    let at = local(ev, sample.x, sample.y);
    match sample.phase {
        PointerPhase::Down => {
            hand.down = hand.down.saturating_add(1);
            if hand.down == 1 {
                hand.used = false;
                hand.two = false;
                core.borrow_mut().halt();
                ev.request_focus(node);
            } else if hand.down == 2 && !hand.used {
                hand.two = true;
            }
            if ev.clicks() == 2 && hand.down == 1 {
                hand.used = true;
                let mut c = core.borrow_mut();
                let delta = if sample.modifiers.shift { -1.0 } else { 1.0 };
                zoom_eased(&mut c, delta, Some(at), STEP_EASE);
                c.poke(ev);
            }
        }
        PointerPhase::Up => {
            hand.down = hand.down.saturating_sub(1);
            if hand.down > 0 {
                return;
            }
            if hand.two && !hand.pinching {
                hand.two = false;
                hand.used = true;
                let mut c = core.borrow_mut();
                zoom_eased(&mut c, -1.0, None, STEP_EASE);
                c.poke(ev);
                return;
            }
            let pressed = core.borrow_mut().overlays.pressed.take();
            if hand.used {
                return;
            }
            match pressed {
                Some(Pressed::Marker(id)) => {
                    if let Some(f) = &on.marker {
                        f(ev, id);
                    }
                }
                Some(Pressed::Cluster(index)) => {
                    // A cluster opens: the camera fits its markers.
                    let mut c = core.borrow_mut();
                    let places = c.overlays.cluster_members(index);
                    if let Some(bounds) = LngLatBounds::around(places) {
                        let mut view = c.view;
                        view.padding = Insets::all(64.0);
                        let fit = view.fit(bounds, view.camera.bearing, c.limits.max_zoom);
                        let zoom = fit.zoom.max(f64::from(band_of(view.camera.zoom)) + 1.0);
                        let to = fit.with_zoom(zoom.min(c.limits.max_zoom));
                        c.ease_to(to, STEP_EASE * 2.0);
                        c.poke(ev);
                    }
                }
                None => {
                    let place = core.borrow().view.unproject(at);
                    if let Some(f) = &on.tap {
                        f(ev, MapPoint { place, point: at });
                    }
                }
            }
        }
        _ => {}
    }
}

/// A focused view's key; whether it took it.
fn key(ev: &mut EventCx<'_>, c: &mut Core) -> bool {
    let Some(k) = ev.key().copied() else {
        return false;
    };
    if !k.pressed {
        return false;
    }
    let m = k.modifiers;
    let f = c.view.focus();
    let pan = |c: &mut Core, d: [f64; 2]| {
        let mut view = c.view;
        view.camera = c.aim();
        let to = view.panned(f, [f[0] - d[0], f[1] - d[1]]);
        c.ease_to(to, STEP_EASE);
    };
    let turn = |c: &mut Core, by: f64| {
        let to = c.aim();
        let b = to.bearing + by;
        c.ease_to(to.with_bearing(b), STEP_EASE);
    };
    match k.key {
        Key::Left if m.shift => turn(c, -KEY_TURN),
        Key::Right if m.shift => turn(c, KEY_TURN),
        Key::Left => pan(c, [-KEY_PAN, 0.0]),
        Key::Right => pan(c, [KEY_PAN, 0.0]),
        Key::Up => pan(c, [0.0, -KEY_PAN]),
        Key::Down => pan(c, [0.0, KEY_PAN]),
        Key::Equal | Key::NumpadAdd => zoom_eased(c, 1.0, None, STEP_EASE),
        Key::Minus | Key::NumpadSubtract => zoom_eased(c, -1.0, None, STEP_EASE),
        Key::N => {
            let to = c.aim().with_bearing(0.0);
            c.ease_to(to, STEP_EASE * 1.5);
        }
        _ => return false,
    }
    c.poke(ev);
    true
}
