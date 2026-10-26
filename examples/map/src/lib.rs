//! An offline navigator over real OpenStreetMap data: the base map, place
//! search and routing all run on the device from three files that
//! `viso-map-build` makes from an OSM extract.
//!
//! ```sh
//! curl -LO https://download.geofabrik.de/europe/luxembourg-latest.osm.pbf
//! cargo run --release --manifest-path ../viso-ext/Cargo.toml -p viso-map-build -- \
//!     build luxembourg-latest.osm.pbf -o luxembourg
//! VISO_MAP_DATA=$PWD/luxembourg cargo run --release --manifest-path examples/map/Cargo.toml
//! ```
//!
//! Search for a place and pick it, or long-press anywhere on the map, to
//! plan a route there from the blue puck; Car, Bike and Walk plan it again
//! for that mode. Drive simulates the trip along the route at four times
//! its planned pace, the camera following the car tilted toward the road
//! ahead, its buildings standing, and the banner showing the next turn and
//! the distance to it, as `NavSession` matches each position onto the
//! route.
//!
//! Map data © OpenStreetMap contributors, available under the Open
//! Database Licence.

pub mod data;
pub mod drive;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use viso::prelude::*;
use viso::render::Rgba;
use viso::ui::context::UpdateCx;
use viso::ui::{
    Alignment2D, Axis, BoxStyle, Component, Continuation, EventCx, FontSize, FontWeight,
    FrameClient, Inset, Key, Length, Size, StackStyle, StateStore, StateWriter, TextStyle,
};
use viso_map::style::builtin;
use viso_map::{
    Camera, Insets, LngLat, LngLatBounds, MapHandleSlot, Marker, MemorySource, Polyline, Puck,
    ShapeId, map_view,
};
use viso_map_nav::{
    Fix, LonLat, Mode, NavSession, NavState, Route, RouteGraph, SearchHit, SearchIndex,
    SearchOptions,
};

/// Where the trip starts: Luxembourg's central station.
const START: LngLat = LngLat::new(6.1335, 49.6000);
/// How much faster than planned the simulated car drives.
const SPEEDUP: f64 = 4.0;
/// How far the camera tilts toward the road ahead while driving, degrees.
const DRIVE_PITCH: f64 = 50.0;
/// The destination marker's id.
const DESTINATION: u64 = 1;

fn ll(p: LonLat) -> LngLat {
    LngLat::new(p.lon, p.lat)
}

fn lonlat(p: LngLat) -> LonLat {
    LonLat::new(p.lon, p.lat)
}

/// What the navigator knows and shows.
struct Nav {
    map: MapHandleSlot,
    search: Option<Arc<SearchIndex>>,
    graph: Option<Arc<RouteGraph>>,
    here: LngLat,
    mode: Mode,
    destination: Option<(LngLat, String)>,
    route: Option<Route>,
    line: Option<ShapeId>,
    traveled: f64,
    session: Option<NavSession>,
    driving: bool,
    /// Seconds into the route the simulated car is.
    clock: f64,
    hits: Vec<SearchHit>,
    banner: String,
    detail: String,
    summary: String,
    /// The cell the text hook reads, bumped when the words change.
    words: StateId,
    rev: i32,
    /// Bumped by each plan, so a route that comes back after a newer plan
    /// is dropped.
    plans: u64,
}

type Shared = Rc<RefCell<Nav>>;

impl Nav {
    /// Asks the text hook to show the words again.
    fn show(&mut self, w: &mut dyn StateWriter) {
        self.rev += 1;
        w.set(self.words, StateValue::Int(self.rev));
    }

    fn say(&mut self, w: &mut dyn StateWriter, banner: &str, detail: &str) {
        self.banner = banner.to_owned();
        self.detail = detail.to_owned();
        self.show(w);
    }

    /// Plans a route from the puck to `to`, called `name`, off the loop.
    fn plan(nav: &Shared, ev: &mut EventCx<'_>, to: LngLat, name: String) {
        let mut n = nav.borrow_mut();
        n.stop(ev);
        n.plans += 1;
        let plan = n.plans;
        n.destination = Some((to, name.clone()));
        n.route = None;
        n.summary.clear();
        let line = n.line.take();
        if let Some(map) = n.map.borrow().as_ref() {
            if let Some(id) = line {
                map.remove_shape(ev, id);
            }
            map.set_marker(ev, Marker::new(DESTINATION, to, pin).unclustered());
        }
        let Some(graph) = n.graph.clone() else {
            n.say(ev, &name, "The road network is still loading…");
            return;
        };
        let (from, mode) = (n.here, n.mode);
        n.say(ev, &name, "Finding a route…");
        drop(n);
        let routed = data::off_loop(move || {
            graph
                .route(lonlat(from), lonlat(to), mode)
                .ok_or_else(|| "No route there".to_owned())
        });
        let nav = Rc::clone(nav);
        ev.spawn_then(routed, move |cx, route| {
            let mut n = nav.borrow_mut();
            if n.plans == plan {
                n.routed(cx, route);
            }
        });
    }

    /// A route was found, or not.
    fn routed(&mut self, w: &mut dyn StateWriter, route: Result<Route, String>) {
        let Some((_, name)) = self.destination.clone() else {
            return;
        };
        let route = match route {
            Ok(r) => r,
            Err(e) => {
                self.say(w, &name, &e);
                return;
            }
        };
        let map = self.map.borrow().clone();
        if let Some(map) = &map {
            let points: Vec<LngLat> = route.points.iter().copied().map(ll).collect();
            self.line = Some(map.add_shape(w, Polyline::route(points.clone())));
            if let Some(bounds) = LngLatBounds::around(points) {
                // Clear of the banner above and the bar below.
                let padding = Insets {
                    top: 170.0,
                    bottom: 90.0,
                    left: 40.0,
                    right: 40.0,
                };
                map.fit_bounds(w, bounds, padding, 0.0, 17.0);
            }
        }
        self.traveled = 0.0;
        self.summary = format!(
            "{} · {} by {}",
            drive::distance_text(route.distance()),
            drive::duration_text(route.duration()),
            match route.mode {
                Mode::Car => "car",
                Mode::Bike => "bike",
                Mode::Foot => "foot",
            }
        );
        let first = route
            .maneuvers
            .first()
            .map(|m| m.instruction())
            .unwrap_or_default();
        self.route = Some(route);
        self.say(w, &name, &first);
    }

    /// Starts the simulated drive along the route.
    fn start(&mut self, w: &mut dyn StateWriter) {
        let Some(route) = self.route.clone() else {
            self.say(
                w,
                "No route yet",
                "Long-press the map or search for a place",
            );
            return;
        };
        self.session = Some(NavSession::new(route));
        self.clock = 0.0;
        self.driving = true;
        if let Some(map) = self.map.borrow().as_ref() {
            map.set_padding(
                w,
                Insets {
                    top: 220.0,
                    ..Insets::all(0.0)
                },
            );
        }
        self.step(w);
    }

    /// Stops driving where the car is.
    fn stop(&mut self, w: &mut dyn StateWriter) {
        if !self.driving {
            return;
        }
        self.driving = false;
        self.session = None;
        if let Some(map) = self.map.borrow().as_ref() {
            map.set_padding(w, Insets::all(0.0));
            let flat = map.camera().with_bearing(0.0).with_pitch(0.0);
            map.ease_to(w, flat, 0.4);
        }
    }

    /// Forgets the route and the destination.
    fn clear(&mut self, w: &mut dyn StateWriter) {
        self.stop(w);
        self.route = None;
        self.destination = None;
        if let Some(map) = self.map.borrow().as_ref() {
            if let Some(id) = self.line.take() {
                map.remove_shape(w, id);
            }
            map.remove_marker(w, DESTINATION);
        }
        self.summary.clear();
        self.say(
            w,
            "Where to?",
            "Search for a place, or long-press the map to go there",
        );
    }

    /// Moves the simulated car to where the clock says, and tells the
    /// session.
    fn step(&mut self, w: &mut dyn StateWriter) {
        let (Some(route), true) = (self.route.as_ref(), self.driving) else {
            return;
        };
        let (pos, heading) = drive::pose(route, self.clock);
        let fix = Fix {
            position: pos,
            accuracy: 5.0,
            heading: Some(heading),
            speed: None,
            time: self.clock / SPEEDUP,
        };
        let total = route.distance().max(1.0);
        let Some(session) = self.session.as_mut() else {
            return;
        };
        let update = session.update(fix);
        self.here = ll(update.matched);
        let map = self.map.borrow().clone();
        if let Some(map) = &map {
            map.set_puck(w, Some(Puck::new(self.here, 5.0).heading(update.bearing)));
            map.jump_to(
                w,
                Camera::new(self.here, 16.5)
                    .with_bearing(update.bearing)
                    .with_pitch(DRIVE_PITCH),
            );
            let traveled = update.along / total;
            if (traveled - self.traveled).abs() > 0.002
                && let (Some(id), Some(route)) = (self.line, self.route.as_ref())
            {
                self.traveled = traveled;
                let points: Vec<LngLat> = route.points.iter().copied().map(ll).collect();
                map.set_shape(w, id, Polyline::route(points).traveled(traveled));
            }
        }
        let route = self.route.as_ref().expect("a route");
        match update.state {
            NavState::Arrived => {
                let name = self
                    .destination
                    .as_ref()
                    .map(|d| d.1.clone())
                    .unwrap_or_default();
                self.summary = format!("Arrived · {} driven", drive::distance_text(total));
                self.stop(w);
                self.say(w, "◉ You have arrived", &name);
                return;
            }
            NavState::OffRoute => {
                if let (Some(graph), Some(s)) = (self.graph.clone(), self.session.as_mut()) {
                    s.reroute(&graph, &fix);
                }
            }
            _ => {}
        }
        if let Some(m) = update.next.and_then(|i| route.maneuvers.get(i)) {
            self.banner = format!(
                "{}  {}",
                drive::arrow(m.kind),
                drive::distance_text(update.distance_to_next)
            );
            self.detail = match &update.announcement {
                Some(a) => a.text.clone(),
                None => m.instruction(),
            };
        }
        self.summary = format!(
            "{} · {} to go",
            drive::distance_text(update.remaining_distance),
            drive::duration_text(update.remaining_time),
        );
        self.show(w);
    }
}

/// The destination's pin.
fn pin(cx: &mut BuildCx<'_>) {
    cx.leaf(LeafStyle {
        size: Size::fixed(22.0, 22.0),
        style: BoxStyle {
            border: viso::render::Border {
                width: 3.0,
                color: Rgba::new(1.0, 1.0, 1.0, 1.0),
            },
            ..BoxStyle::solid(Rgba::new(0.85, 0.12, 0.15, 1.0)).with_radius(11.0)
        },
    });
}

/// Runs the simulated drive's clock.
struct Driver {
    nav: Shared,
    due: bool,
}

impl FrameClient for Driver {
    fn frame(&mut self, dt: f64) {
        if let Ok(mut nav) = self.nav.try_borrow_mut()
            && nav.driving
        {
            nav.clock += dt * SPEEDUP;
            self.due = true;
        }
    }

    fn key(&mut self, _key: Key, _pressed: bool) -> bool {
        false
    }

    fn release_input(&mut self) {}

    fn wants_frame(&self) -> bool {
        self.nav.try_borrow().is_ok_and(|n| n.driving)
    }

    fn settle(&mut self, states: &mut StateStore) {
        if std::mem::take(&mut self.due)
            && let Ok(mut nav) = self.nav.try_borrow_mut()
        {
            nav.step(states);
        }
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

/// A run of text the words hook rewrites.
fn words(cx: &mut BuildCx<'_>, size: f32, bold: bool, text: &str) -> NodeId {
    let leaf = cx.leaf(LeafStyle {
        size: Size {
            width: Length::Fill { weight: 1.0 },
            height: Length::Fit,
        },
        style: BoxStyle::NONE,
    });
    cx.text_request(leaf, text_of(size, bold, text));
    // Read out as it changes: its accessible name follows its text.
    cx.semantics(leaf, Semantics::role(Role::Label).with_label(text));
    leaf.id()
}

fn text_of(size: f32, bold: bool, text: &str) -> TextRequest {
    TextRequest {
        text: text.to_owned(),
        font_size: FontSize::Fixed(size),
        color: Rgba::new(0.06, 0.07, 0.09, 1.0),
        soft_wrap: true,
        locale: None,
        style: TextStyle {
            weight: if bold {
                FontWeight::BOLD
            } else {
                FontWeight::REGULAR
            },
            ..TextStyle::default()
        },
        spans: Vec::new(),
    }
}

fn card() -> BoxStyle {
    BoxStyle::solid(Rgba::new(1.0, 1.0, 1.0, 0.94)).with_radius(12.0)
}

/// What a search hit reads as in the suggestions.
fn hit_text(hit: &SearchHit) -> String {
    if hit.address.is_empty() {
        hit.name.clone()
    } else {
        format!("{} · {}", hit.name, hit.address)
    }
}

/// The navigator app.
pub struct Navigator;

impl Application for Navigator {
    fn new(cx: &mut AppCx) -> Self {
        // A face of its own, so its text and the map's labels draw where the
        // system offers none, as on the web.
        cx.use_packaged_fonts(viso::packaged_fonts!());
        Navigator
    }

    fn window_config(&self) -> WindowConfig {
        WindowConfig {
            title: "Navigator".to_owned(),
            size: (1000.0, 720.0),
            ..Default::default()
        }
    }

    fn build(&mut self, cx: &mut BuildCx<'_>) {
        let words_cell = cx.state(StateValue::Int(0));
        let slot = MapHandleSlot::default();
        let nav: Shared = Rc::new(RefCell::new(Nav {
            map: Rc::clone(&slot),
            search: None,
            graph: None,
            here: START,
            mode: Mode::Car,
            destination: None,
            route: None,
            line: None,
            traveled: 0.0,
            session: None,
            driving: false,
            clock: 0.0,
            hits: Vec::new(),
            banner: "Where to?".to_owned(),
            detail: "Loading the map…".to_owned(),
            summary: String::new(),
            words: words_cell,
            rev: 0,
            plans: 0,
        }));
        let dark = std::env::var("VISO_MAP_DARK").is_ok_and(|v| v == "1");
        let style = if dark {
            builtin::shortbread_dark()
        } else {
            builtin::shortbread_light()
        };
        let mut texts = None;
        let root = cx.stack(
            StackStyle {
                size: Size::fill(),
                align: Alignment2D::TopStart,
                ..StackStyle::default()
            },
            |cx| {
                let long = Rc::clone(&nav);
                map_view(MemorySource::new())
                    .camera(Camera::new(START, 14.0))
                    .style(style)
                    .languages(&["en", "fr", "de"])
                    .puck(Puck::new(START, 10.0))
                    .size(Size::fill())
                    .label("Map of Luxembourg")
                    .handle(&slot)
                    .on_long_press(move |ev, p| {
                        Nav::plan(&long, ev, p.place, "Dropped pin".to_owned());
                    })
                    .build(cx);
                texts = Some(panels(cx, &nav));
            },
        );
        let [banner, detail, summary] = texts.expect("built");
        let hook_nav = Rc::clone(&nav);
        let load_nav = Rc::clone(&nav);
        let driver = Rc::new(RefCell::new(Driver {
            nav: Rc::clone(&nav),
            due: false,
        }));
        cx.structure(move |s| {
            s.store.add_structure_hook([words_cell], move |s, _| {
                let n = hook_nav.borrow();
                s.store.set_text(banner, text_of(20.0, true, &n.banner));
                s.store.set_text(detail, text_of(14.0, false, &n.detail));
                s.store.set_text(summary, text_of(14.0, true, &n.summary));
            });
            s.store.add_frame_client(root.id(), driver);
            load(s.store, root.id(), &load_nav);
        });
    }
}

/// Builds the search field and the banner over the map, and the bar of
/// actions under it; returns the banner's, its detail's and the summary's
/// text nodes.
fn panels(cx: &mut BuildCx<'_>, nav: &Shared) -> [NodeId; 3] {
    let mut ids = [None; 3];
    let column = cx.flex(
        FlexStyle {
            axis: Axis::Column,
            gap: 10.0,
            padding: Inset::all(14.0),
            size: Size::fill(),
            ..FlexStyle::default()
        },
        |cx| {
            let search_slot = SearchFieldHandleSlot::default();
            let (change_nav, submit_nav) = (Rc::clone(nav), Rc::clone(nav));
            let (suggest, submitted) = (Rc::clone(&search_slot), Rc::clone(&search_slot));
            search_field()
                .placeholder("Search places in Luxembourg")
                .label("Search places")
                .max_suggestions(6)
                .size(Size {
                    width: Length::Fixed(380.0),
                    height: Length::Fit,
                })
                .on_change(move |ev, query| {
                    let mut n = change_nav.borrow_mut();
                    let Some(index) = n.search.clone() else {
                        return;
                    };
                    let near = n.map.borrow().as_ref().map(|m| lonlat(m.camera().center));
                    n.hits = index.search(
                        query,
                        &SearchOptions {
                            near,
                            limit: 6,
                            ..SearchOptions::default()
                        },
                    );
                    let shown: Vec<String> = n.hits.iter().map(hit_text).collect();
                    drop(n);
                    if let Some(h) = suggest.borrow().as_ref() {
                        h.set_suggestions(ev, shown);
                    }
                })
                .on_submit(move |ev, text| {
                    let n = submit_nav.borrow();
                    let hit = n
                        .hits
                        .iter()
                        .find(|h| hit_text(h) == text)
                        .or(n.hits.first())
                        .cloned();
                    drop(n);
                    if let Some(h) = submitted.borrow().as_ref() {
                        h.set_suggestions(ev, Vec::<String>::new());
                    }
                    if let Some(hit) = hit {
                        Nav::plan(&submit_nav, ev, ll(hit.position), hit.name);
                    }
                })
                .handle(&search_slot)
                .build(cx);
            let banner = cx.flex(
                FlexStyle {
                    axis: Axis::Column,
                    gap: 4.0,
                    padding: Inset::all(12.0),
                    size: Size {
                        width: Length::Fixed(380.0),
                        height: Length::Fit,
                    },
                    style: card(),
                    ..FlexStyle::default()
                },
                |cx| {
                    ids[0] = Some(words(cx, 20.0, true, "Where to?"));
                    ids[1] = Some(words(cx, 14.0, false, "Loading the map…"));
                },
            );
            cx.semantics(
                banner,
                Semantics::role(Role::Status).with_label("Directions"),
            );
            let spacer = cx.leaf(LeafStyle {
                size: Size::fill(),
                style: BoxStyle::NONE,
            });
            let spacer = spacer.id();
            cx.flex(
                FlexStyle {
                    axis: Axis::Row,
                    gap: 8.0,
                    padding: Inset::all(10.0),
                    size: Size {
                        width: Length::Fill { weight: 1.0 },
                        height: Length::Fit,
                    },
                    style: card(),
                    ..FlexStyle::default()
                },
                |cx| {
                    ids[2] = Some(words(cx, 14.0, true, ""));
                    for (name, mode) in [
                        ("Car", Mode::Car),
                        ("Bike", Mode::Bike),
                        ("Walk", Mode::Foot),
                    ] {
                        let nav = Rc::clone(nav);
                        button(name)
                            .on_click(move |ev| {
                                let to = {
                                    let mut n = nav.borrow_mut();
                                    n.mode = mode;
                                    n.destination.clone()
                                };
                                if let Some((at, name)) = to {
                                    Nav::plan(&nav, ev, at, name);
                                }
                            })
                            .build(cx);
                    }
                    let go = Rc::clone(nav);
                    button("Drive")
                        .on_click(move |ev| go.borrow_mut().start(ev))
                        .build(cx);
                    let halt = Rc::clone(nav);
                    button("Stop")
                        .on_click(move |ev| halt.borrow_mut().stop(ev))
                        .build(cx);
                    let clear = Rc::clone(nav);
                    button("Clear")
                        .on_click(move |ev| clear.borrow_mut().clear(ev))
                        .build(cx);
                },
            );
            cx.structure(|s| s.store.set_hittable(spacer, false));
        },
    );
    let column = column.id();
    cx.structure(|s| s.store.set_hittable(column, false));
    ids.map(|id| id.expect("built"))
}

/// Starts loading the map, the search index and the road graph, each told
/// to the navigator as it arrives.
fn load(store: &mut viso::ui::NodeStore, owner: NodeId, nav: &Shared) {
    let prefix = data::prefix();
    let tiles_nav = Rc::clone(nav);
    let p = prefix.clone();
    store.spawn_task(
        owner,
        Box::pin(async move {
            let opened = data::tiles(&p).await;
            Some(Box::new(move |cx: &mut UpdateCx<'_>| {
                let mut n = tiles_nav.borrow_mut();
                match opened {
                    Ok(archive) => {
                        let info = viso_tiles::TileSource::info(&*archive);
                        let zooms = (info.min_zoom, info.max_zoom);
                        if let Some(map) = n.map.borrow().as_ref() {
                            map.set_source(cx, data::source(archive), zooms);
                        }
                        n.say(
                            cx,
                            "Where to?",
                            "Search for a place, or long-press the map to go there",
                        );
                    }
                    Err(e) => n.say(cx, "No map", &e),
                }
            }) as Continuation)
        }),
    );
    let search_nav = Rc::clone(nav);
    let p = prefix.clone();
    store.spawn_task(
        owner,
        Box::pin(async move {
            let index = data::search(&p).await;
            Some(Box::new(move |cx: &mut UpdateCx<'_>| {
                let mut n = search_nav.borrow_mut();
                match index {
                    Ok(index) => n.search = Some(Arc::new(index)),
                    Err(e) => n.say(cx, "No search", &e),
                }
            }) as Continuation)
        }),
    );
    let graph_nav = Rc::clone(nav);
    store.spawn_task(
        owner,
        Box::pin(async move {
            let graph = data::graph(&prefix).await;
            Some(Box::new(move |cx: &mut UpdateCx<'_>| {
                let mut n = graph_nav.borrow_mut();
                match graph {
                    Ok(graph) => n.graph = Some(Arc::new(graph)),
                    Err(e) => n.say(cx, "No routing", &e),
                }
            }) as Continuation)
        }),
    );
}
