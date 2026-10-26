//! The map view driven through the headless app loop: tiles load off the
//! loop and draw, the camera moves only its transform root, and the
//! gestures, the wheel and the keys move it. Pictures of each run are
//! written to `VISO_MAP_OUT` when it is set.

mod support;

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use viso::__test_support::{DrivenApp, drive_scripted, probe_frames};
use viso::platform::{
    GesturePhase, KeyCode, Modifiers, PointerButtons, PointerPhase, RawEvent, RawGesture, RawKey,
    RawPointer, RawScroll, ScrollPhase, WindowId,
};
use viso::prelude::*;
use viso::ui::{BuildCx, Component, WindowConfig};
use viso_map::style::builtin;
use viso_map::{Camera, LngLat, MapHandle, MapHandleSlot, MapView, TileId, map_view};

const STEP: Duration = Duration::from_millis(16);
const WIN: WindowId = WindowId(1);
const W: f64 = 640.0;
const H: f64 = 480.0;

type Setup = Box<dyn Fn(&mut BuildCx<'_>)>;

thread_local! {
    static SETUP: RefCell<Option<Setup>> = const { RefCell::new(None) };
    /// The faces the next app loads.
    static FONTS: RefCell<Vec<Vec<u8>>> = const { RefCell::new(Vec::new()) };
}

struct MapApp;

impl Application for MapApp {
    fn new(cx: &mut AppCx) -> Self {
        FONTS.with(|f| {
            for face in f.borrow().iter() {
                cx.load_font(&face[..]);
            }
        });
        MapApp
    }

    fn window_config(&self) -> WindowConfig {
        WindowConfig {
            caption: false,
            size: (W, H),
            ..Default::default()
        }
    }

    fn build(&mut self, cx: &mut BuildCx<'_>) {
        SETUP.with(|s| (s.borrow().as_ref().expect("a setup"))(cx));
    }
}

fn redraws(n: usize) -> Vec<RawEvent> {
    (0..n)
        .map(|_| RawEvent::RedrawRequested { window: WIN })
        .collect()
}

fn mouse(x: f64, y: f64, phase: PointerPhase, m: Modifiers) -> RawEvent {
    let buttons = if phase == PointerPhase::Up {
        PointerButtons::NONE
    } else {
        PointerButtons::PRIMARY
    };
    RawEvent::Pointer(RawPointer::mouse(WIN, x, y, buttons, m, phase))
}

/// Builds a map of synthetic tiles with `camera` and `dark` style, runs
/// `script` after enough frames to load, and returns the app and handle.
fn run(camera: Camera, dark: bool, script: Vec<RawEvent>) -> (DrivenApp<MapApp>, MapHandle) {
    run_with(camera, dark, script, |m| m)
}

/// [`run`] with the view configured further by `configure`.
fn run_with(
    camera: Camera,
    dark: bool,
    script: Vec<RawEvent>,
    configure: impl Fn(MapView) -> MapView + 'static,
) -> (DrivenApp<MapApp>, MapHandle) {
    let slot = MapHandleSlot::default();
    let s = Rc::clone(&slot);
    SETUP.with(|setup| {
        *setup.borrow_mut() = Some(Box::new(move |cx| {
            let style = if dark {
                builtin::openmaptiles_dark()
            } else {
                builtin::openmaptiles_light()
            };
            let view =
                map_view(|tile: TileId| viso_map::Reply::ready(Ok(Some(support::synthetic(tile)))))
                    .camera(camera)
                    .style(style)
                    .handle(&s);
            configure(view).build(cx);
        }))
    });
    let mut all = redraws(12);
    all.extend(script);
    all.extend(redraws(4));
    let app = drive_scripted::<MapApp>(all, STEP);
    let handle = slot.borrow().clone().expect("built");
    (app, handle)
}

fn picture(app: &DrivenApp<MapApp>, name: &str) -> (u32, u32, Vec<u8>) {
    let (w, h, px) = app.pixels().expect("a software surface");
    support::save(name, w, h, &px);
    (w, h, px)
}

fn berlin(zoom: f64, bearing: f64) -> Camera {
    Camera::new(LngLat::new(13.40, 52.52), zoom).with_bearing(bearing)
}

#[test]
fn tiles_load_off_the_loop_and_draw_at_several_zooms_and_turns() {
    for (zoom, bearing, dark) in [
        (12.0, 0.0, false),
        (12.4, 0.0, false),
        (13.6, 30.0, false),
        (9.3, -60.0, true),
        (14.0, 0.0, true),
    ] {
        let (app, handle) = run(berlin(zoom, bearing), dark, vec![]);
        let tiles = handle.tiles();
        assert!(!tiles.drawn.is_empty(), "z{zoom}: {tiles:?}");
        let band = (zoom + 0.5).floor() as u8;
        assert!(
            tiles.drawn.iter().all(|s| s.tile.z == band),
            "every tile loaded: {tiles:?}"
        );
        assert_eq!(tiles.in_flight, 0);
        let (w, h, px) = picture(
            &app,
            &format!(
                "view-z{zoom}-b{bearing}-{}",
                if dark { "dark" } else { "light" }
            ),
        );
        assert_eq!((w, h), (W as u32, H as u32));
        // More than the background shows.
        let first = &px[..4];
        assert!(px.as_chunks::<4>().0.iter().any(|p| p[..] != *first));
    }
}

#[test]
fn a_drag_pans_the_camera_by_what_it_moved_and_uploads_nothing() {
    let frames: Rc<RefCell<Vec<bool>>> = Rc::default();
    let log = Rc::clone(&frames);
    probe_frames(Some(Box::new(move |_, uploaded| {
        log.borrow_mut().push(uploaded);
    })));
    let none = Modifiers::default();
    let mut script = vec![mouse(320.0, 240.0, PointerPhase::Down, none)];
    for i in 1..=10 {
        script.push(mouse(
            320.0 + 6.0 * f64::from(i),
            240.0,
            PointerPhase::Moved,
            none,
        ));
        script.push(RawEvent::RedrawRequested { window: WIN });
    }
    let start = berlin(13.0, 0.0);
    let (app, handle) = run(start, false, script);
    probe_frames(None);
    let moved = handle.camera();
    // A 60-point drag right moves the camera 60 points west.
    let before = handle.view();
    let mut v = before;
    v.camera = start;
    let p = v.project(moved.center);
    assert!((p[0] - (320.0 - 60.0)).abs() < 1.0, "{p:?}");
    assert!((p[1] - 240.0).abs() < 1e-6);
    picture(&app, "view-dragged");
    let frames = frames.borrow();
    // The frames of the drag after the first move, tiles all loaded: no
    // primitive reached the renderer.
    let tail = &frames[frames.len().saturating_sub(8)..];
    assert!(frames.iter().any(|&u| u), "the tiles uploaded once");
    assert!(tail.iter().all(|&uploaded| !uploaded), "{frames:?}");
}

/// The middle of tile `(z, x, y)`.
fn middle(z: u8, x: u32, y: u32) -> LngLat {
    let n = f64::from(1u32 << z);
    viso_map::WorldPoint::new((f64::from(x) + 0.5) / n, (f64::from(y) + 0.5) / n).to_lng_lat()
}

#[test]
fn real_tiles_draw_through_the_view() {
    let Some(dir) = support::tiles_dir() else {
        return;
    };
    label_fonts();
    for (name, z, x, y, zoom, bearing, dark) in [
        ("berlin", 14, 8800, 5373, 14.0, 0.0, false),
        ("berlin", 12, 2200, 1343, 12.4, 25.0, true),
        ("berlin", 10, 550, 335, 10.3, 0.0, true),
        ("tokyo", 13, 7276, 3225, 13.2, -40.0, false),
        ("tokyo", 14, 14552, 6451, 14.4, 0.0, true),
    ] {
        let dir = dir.clone();
        let slot = MapHandleSlot::default();
        let s = Rc::clone(&slot);
        SETUP.with(|setup| {
            *setup.borrow_mut() = Some(Box::new(move |cx| {
                let dir = dir.clone();
                let style = if dark {
                    builtin::openmaptiles_dark()
                } else {
                    builtin::openmaptiles_light()
                };
                map_view(move |tile: TileId| {
                    let path = dir.join(format!("{}-{}-{}.pbf", tile.z, tile.x, tile.y));
                    viso_map::Reply::ready(Ok(std::fs::read(path).ok()))
                })
                .camera(Camera::new(middle(z, x, y), zoom).with_bearing(bearing))
                .style(style)
                .languages(&["en"])
                .handle(&s)
                .build(cx);
            }))
        });
        let app = drive_scripted::<MapApp>(redraws(40), STEP);
        let handle = slot.borrow().clone().expect("built");
        assert!(!handle.tiles().drawn.is_empty());
        assert!(!handle.labels().is_empty(), "{name} z{zoom}");
        picture(
            &app,
            &format!(
                "real-{name}-z{zoom}-b{bearing}-{}",
                if dark { "dark" } else { "light" }
            ),
        );
    }
    FONTS.with(|f| f.borrow_mut().clear());
}

fn touch(id: u64, x: f64, y: f64, phase: PointerPhase) -> RawEvent {
    let buttons = if phase == PointerPhase::Up {
        PointerButtons::NONE
    } else {
        PointerButtons::PRIMARY
    };
    RawEvent::Pointer(RawPointer {
        window: WIN,
        pointer: viso::platform::PointerId(id),
        kind: viso::platform::PointerKind::Touch,
        x,
        y,
        pressure: 0.5,
        buttons,
        modifiers: Modifiers::default(),
        phase,
    })
}

fn key(code: KeyCode, m: Modifiers) -> Vec<RawEvent> {
    [true, false]
        .map(|pressed| {
            RawEvent::Key(RawKey {
                window: WIN,
                code,
                pressed,
                repeat: false,
                modifiers: m,
            })
        })
        .into()
}

/// Where `place` showed in `view` before the gestures moved it.
fn shown(handle: &MapHandle, place: LngLat) -> [f64; 2] {
    handle.view().project(place)
}

fn near(a: [f64; 2], b: [f64; 2], tol: f64) -> bool {
    (a[0] - b[0]).abs() < tol && (a[1] - b[1]).abs() < tol
}

#[test]
fn two_fingers_zoom_about_their_middle_and_turn_with_it() {
    let start = berlin(12.0, 0.0);
    let view = viso_map::View::new(start, [W, H]);
    let held = view.unproject([320.0, 240.0]);
    // Spread from 100 to 200 points apart about the middle, then turn.
    let mut script = vec![
        touch(1, 270.0, 240.0, PointerPhase::Down),
        touch(2, 370.0, 240.0, PointerPhase::Down),
    ];
    for i in 1..=10 {
        let r = 50.0 + 5.0 * f64::from(i);
        script.push(touch(1, 320.0 - r, 240.0, PointerPhase::Moved));
        script.push(touch(2, 320.0 + r, 240.0, PointerPhase::Moved));
        script.push(RawEvent::RedrawRequested { window: WIN });
    }
    let (_, handle) = run(start, false, script.clone());
    let c = handle.camera();
    assert!((c.zoom - 13.0).abs() < 0.05, "{c:?}");
    assert!(near(shown(&handle, held), [320.0, 240.0], 1.0));
    // Turned 40 degrees clockwise as well, the map turns with them.
    for i in 1..=8 {
        let a = (5.0 * f64::from(i)).to_radians();
        let r = 100.0;
        script.push(touch(
            1,
            320.0 - r * a.cos(),
            240.0 - r * a.sin(),
            PointerPhase::Moved,
        ));
        script.push(touch(
            2,
            320.0 + r * a.cos(),
            240.0 + r * a.sin(),
            PointerPhase::Moved,
        ));
        script.push(RawEvent::RedrawRequested { window: WIN });
    }
    script.push(touch(1, 320.0 - 76.6, 240.0 - 64.3, PointerPhase::Up));
    script.push(touch(2, 320.0 + 76.6, 240.0 + 64.3, PointerPhase::Up));
    let (app, handle) = run(start, false, script);
    let c = handle.camera();
    assert!((c.bearing + 40.0).abs() < 1.5, "{c:?}");
    assert!(near(shown(&handle, held), [320.0, 240.0], 1.5));
    picture(&app, "gesture-pinch-turn");
}

#[test]
fn a_turn_let_go_near_north_settles_north() {
    let start = berlin(12.0, 0.0);
    let mut script = vec![
        touch(1, 220.0, 240.0, PointerPhase::Down),
        touch(2, 420.0, 240.0, PointerPhase::Down),
    ];
    // Past the turn threshold and back to five degrees.
    for deg in [4.0, 8.0, 12.0, 8.0, 5.0f64] {
        let a = deg.to_radians();
        script.push(touch(
            1,
            320.0 - 100.0 * a.cos(),
            240.0 - 100.0 * a.sin(),
            PointerPhase::Moved,
        ));
        script.push(touch(
            2,
            320.0 + 100.0 * a.cos(),
            240.0 + 100.0 * a.sin(),
            PointerPhase::Moved,
        ));
        script.push(RawEvent::RedrawRequested { window: WIN });
    }
    script.push(touch(1, 220.0, 240.0, PointerPhase::Up));
    script.push(touch(2, 420.0, 240.0, PointerPhase::Up));
    let (_, handle) = run(start, false, script);
    assert_eq!(handle.camera().bearing, 0.0);
}

#[test]
fn a_wheel_notch_zooms_about_the_pointer() {
    let start = berlin(12.0, 0.0);
    let at = [200.0, 150.0];
    let held = viso_map::View::new(start, [W, H]).unproject(at);
    let wheel = RawEvent::Scroll(RawScroll {
        window: WIN,
        x: at[0],
        y: at[1],
        delta_x: 0.0,
        delta_y: -240.0,
        modifiers: Modifiers::default(),
        phase: ScrollPhase::None,
    });
    let (_, handle) = run(start, false, vec![wheel]);
    let c = handle.camera();
    assert!((c.zoom - 13.0).abs() < 1e-6, "{c:?}");
    assert!(near(shown(&handle, held), at, 0.5));
}

#[test]
fn a_trackpad_scroll_pans_and_its_pinch_zooms() {
    let start = berlin(12.0, 0.0);
    let scroll = |dx: f64, phase| {
        RawEvent::Scroll(RawScroll {
            window: WIN,
            x: 320.0,
            y: 240.0,
            delta_x: dx,
            delta_y: 0.0,
            modifiers: Modifiers::default(),
            phase,
        })
    };
    let script = vec![
        scroll(10.0, ScrollPhase::Began),
        scroll(20.0, ScrollPhase::Changed),
        scroll(0.0, ScrollPhase::Ended),
    ];
    let (_, handle) = run(start, false, script);
    // Scrolling 30 points right reveals what lay 30 points to the right.
    let was = viso_map::View::new(start, [W, H]).unproject([350.0, 240.0]);
    assert!(near(shown(&handle, was), [320.0, 240.0], 0.5));
    let gesture = |scale: f64, phase| {
        RawEvent::Gesture(RawGesture {
            window: WIN,
            x: 320.0,
            y: 240.0,
            scale,
            rotation: 0.0,
            modifiers: Modifiers::default(),
            phase,
        })
    };
    let script = vec![
        gesture(1.0, GesturePhase::Began),
        gesture(2.0, GesturePhase::Changed),
        gesture(1.0, GesturePhase::Ended),
    ];
    let (_, handle) = run(start, false, script);
    assert!((handle.camera().zoom - 13.0).abs() < 1e-3);
}

#[test]
fn a_double_tap_zooms_in_and_a_two_finger_tap_zooms_out() {
    let start = berlin(12.0, 0.0);
    let none = Modifiers::default();
    let at = [400.0, 300.0];
    let held = viso_map::View::new(start, [W, H]).unproject(at);
    let click = |phase| mouse(at[0], at[1], phase, none);
    let script = vec![
        click(PointerPhase::Down),
        click(PointerPhase::Up),
        click(PointerPhase::Down),
        click(PointerPhase::Up),
    ];
    let (_, handle) = run(start, false, script);
    assert!((handle.camera().zoom - 13.0).abs() < 1e-6);
    assert!(near(shown(&handle, held), at, 0.5));
    let script = vec![
        touch(1, 300.0, 240.0, PointerPhase::Down),
        touch(2, 340.0, 240.0, PointerPhase::Down),
        touch(1, 300.0, 240.0, PointerPhase::Up),
        touch(2, 340.0, 240.0, PointerPhase::Up),
    ];
    let (_, handle) = run(start, false, script);
    assert!((handle.camera().zoom - 11.0).abs() < 1e-6);
}

#[test]
fn keys_pan_zoom_and_turn_the_focused_map() {
    let start = berlin(12.0, 0.0);
    let none = Modifiers::default();
    let shift = Modifiers {
        shift: true,
        ..Modifiers::default()
    };
    let focus = vec![
        mouse(320.0, 240.0, PointerPhase::Down, none),
        mouse(320.0, 240.0, PointerPhase::Up, none),
    ];
    let mut script = focus.clone();
    script.extend(key(KeyCode::Equal, none));
    script.extend(redraws(30));
    let (_, handle) = run(start, false, script);
    assert!((handle.camera().zoom - 13.0).abs() < 1e-6);
    let mut script = focus.clone();
    script.extend(key(KeyCode::Right, none));
    script.extend(redraws(30));
    let (_, handle) = run(start, false, script);
    let was = viso_map::View::new(start, [W, H]).unproject([420.0, 240.0]);
    assert!(near(shown(&handle, was), [320.0, 240.0], 0.5));
    let mut script = focus;
    script.extend(key(KeyCode::Right, shift));
    script.extend(redraws(30));
    script.extend(key(KeyCode::Right, shift));
    script.extend(redraws(30));
    let (_, handle) = run(start, false, script.clone());
    assert!((handle.camera().bearing - 30.0).abs() < 1e-6);
    script.extend(key(KeyCode::N, none));
    script.extend(redraws(30));
    let (_, handle) = run(start, false, script);
    assert_eq!(handle.camera().bearing, 0.0);
}

thread_local! {
    static HEARD: RefCell<Vec<(&'static str, LngLat)>> = const { RefCell::new(Vec::new()) };
}

#[test]
fn taps_long_presses_and_rests_reach_the_app() {
    HEARD.with(|h| h.borrow_mut().clear());
    let start = berlin(12.0, 0.0);
    let none = Modifiers::default();
    let at = [100.0, 100.0];
    let place = viso_map::View::new(start, [W, H]).unproject(at);
    let mut script = vec![
        mouse(at[0], at[1], PointerPhase::Down, none),
        mouse(at[0], at[1], PointerPhase::Up, none),
    ];
    // A press held still past the long-press time.
    script.push(touch(5, at[0], at[1], PointerPhase::Down));
    for _ in 0..50 {
        script.extend([RawEvent::Wakeup, RawEvent::RedrawRequested { window: WIN }]);
    }
    script.push(touch(5, at[0], at[1], PointerPhase::Up));
    // And a drag, which comes to rest.
    script.push(mouse(320.0, 240.0, PointerPhase::Down, none));
    script.push(mouse(360.0, 240.0, PointerPhase::Moved, none));
    script.push(mouse(360.0, 240.0, PointerPhase::Up, none));
    script.extend(redraws(30));
    let (_, handle) = run_with(start, false, script, |m| {
        m.on_tap(|_, p| HEARD.with(|h| h.borrow_mut().push(("tap", p.place))))
            .on_long_press(|_, p| HEARD.with(|h| h.borrow_mut().push(("long", p.place))))
            .on_viewport(|_, c| HEARD.with(|h| h.borrow_mut().push(("rest", c.center))))
    });
    let heard = HEARD.with(|h| h.borrow().clone());
    let kinds: Vec<_> = heard.iter().map(|h| h.0).collect();
    assert_eq!(kinds, ["tap", "long", "rest"], "{heard:?}");
    for (kind, p) in &heard[..2] {
        assert!(
            (p.lon - place.lon).abs() < 1e-9 && (p.lat - place.lat).abs() < 1e-9,
            "{kind}"
        );
    }
    assert_eq!(heard[2].1, handle.camera().center);
}

#[test]
fn a_new_style_rebuilds_the_loaded_tiles_without_fetching_them_again() {
    let fetched = Rc::new(std::cell::Cell::new(0u32));
    let slot = MapHandleSlot::default();
    let (s, f, tap) = (Rc::clone(&slot), Rc::clone(&fetched), Rc::clone(&slot));
    SETUP.with(|setup| {
        *setup.borrow_mut() = Some(Box::new(move |cx| {
            let f = Rc::clone(&f);
            let tap = Rc::clone(&tap);
            map_view(move |tile: TileId| {
                f.set(f.get() + 1);
                viso_map::Reply::ready(Ok(Some(support::synthetic(tile))))
            })
            .camera(berlin(12.0, 0.0))
            .handle(&s)
            .on_tap(move |cx, _| {
                let h = tap.borrow().clone().expect("built");
                h.set_style(cx, builtin::openmaptiles_dark());
            })
            .build(cx);
        }))
    });
    let none = Modifiers::default();
    let light = drive_scripted::<MapApp>(redraws(12), STEP);
    let before = slot.borrow().clone().expect("built").tiles();
    let (_, _, light) = light.pixels().expect("pixels");
    let first = fetched.get();
    fetched.set(0);
    let mut script = redraws(12);
    script.push(mouse(320.0, 240.0, PointerPhase::Down, none));
    script.push(mouse(320.0, 240.0, PointerPhase::Up, none));
    script.extend(redraws(12));
    let app = drive_scripted::<MapApp>(script, STEP);
    let after = slot.borrow().clone().expect("built").tiles();
    assert_eq!(fetched.get(), first, "each tile fetched once: {after:?}");
    assert_eq!(after.drawn, before.drawn);
    // The tiles shown are built again; those only fetched ahead wait until
    // they show.
    assert_eq!(
        after.builds as usize,
        before.builds as usize + before.drawn.len(),
        "{before:?} {after:?}"
    );
    let (_, _, dark) = picture(&app, "view-restyled-dark");
    assert_ne!(light, dark);
}

/// The color at view point `p` of a picture, as RGB.
fn rgb_at(px: &(u32, u32, Vec<u8>), p: [f64; 2]) -> [u8; 3] {
    let (w, _, px) = px;
    let i = ((p[1].round() as u32 * w + p[0].round() as u32) * 4) as usize;
    [px[i + 2], px[i + 1], px[i]]
}

fn reddish(c: [u8; 3]) -> bool {
    c[0] > 200 && c[1] < 60 && c[2] < 60
}

fn red_box(cx: &mut BuildCx<'_>) {
    cx.stack(
        viso::ui::StackStyle {
            size: viso::ui::Size::fixed(20.0, 30.0),
            style: viso::ui::BoxStyle::solid(viso::render::Rgba::new(1.0, 0.0, 0.0, 1.0)),
            ..Default::default()
        },
        |_| {},
    );
}

#[test]
fn shapes_markers_and_the_puck_draw_and_a_pan_moves_them_uploading_nothing() {
    use viso_map::{Circle, Marker, Polygon, Polyline, Puck, Stroke, style::Color};
    let frames: Rc<RefCell<Vec<bool>>> = Rc::default();
    let log = Rc::clone(&frames);
    let none = Modifiers::default();
    let start = berlin(13.0, 20.0);
    let view = viso_map::View::new(start, [W, H]);
    let pinned = view.unproject([420.0, 300.0]);
    let route: Vec<LngLat> = [
        [120.0, 380.0],
        [300.0, 330.0],
        [380.0, 150.0],
        [560.0, 120.0],
    ]
    .iter()
    .map(|&p| view.unproject(p))
    .collect();
    let mut script = vec![mouse(320.0, 240.0, PointerPhase::Down, none)];
    for i in 1..=10 {
        script.push(mouse(
            320.0 - 5.0 * f64::from(i),
            240.0 + 3.0 * f64::from(i),
            PointerPhase::Moved,
            none,
        ));
        script.push(RawEvent::RedrawRequested { window: WIN });
    }
    script.push(mouse(270.0, 270.0, PointerPhase::Up, none));
    probe_frames(Some(Box::new(move |_, uploaded| {
        log.borrow_mut().push(uploaded);
    })));
    let r = route.clone();
    let (app, handle) = run_with(start, false, script, move |m| {
        m.shape(
            Polygon::new(
                vec![
                    view.unproject([60.0, 60.0]),
                    view.unproject([220.0, 60.0]),
                    view.unproject([220.0, 180.0]),
                    view.unproject([60.0, 180.0]),
                ],
                Color::rgb(0x2e, 0xa0, 0x43).alpha(0.6),
            )
            .outline(Stroke::new(2.0, Color::rgb(0x1b, 0x5e, 0x20))),
        )
        .shape(
            Circle::new(
                view.unproject([520.0, 380.0]),
                600.0,
                Color::rgb(0xff, 0x98, 0).alpha(0.5),
            )
            .outline(Stroke::new(1.5, Color::rgb(0xe6, 0x51, 0))),
        )
        .shape(Polyline::route(r.clone()).traveled(0.35))
        .marker(Marker::new(1, pinned, red_box))
        .puck(Puck::new(view.unproject([200.0, 300.0]), 120.0).heading(45.0))
    });
    probe_frames(None);
    let px = picture(&app, "overlays-panned");
    // The marker's box stands on its place, wherever the pan left it.
    let at = handle.view().project(pinned);
    assert!(
        reddish(rgb_at(&px, [at[0], at[1] - 15.0])),
        "{:?}",
        rgb_at(&px, [at[0], at[1] - 15.0])
    );
    assert!(!reddish(rgb_at(&px, [at[0], at[1] + 6.0])));
    // The route passes through its points.
    let mid = handle.view().project(route[2]);
    let c = rgb_at(&px, mid);
    assert!(c[2] > 150 && c[0] < 120, "{c:?}");
    let frames = frames.borrow();
    let tail = &frames[frames.len().saturating_sub(8)..];
    assert!(tail.iter().all(|&u| !u), "{frames:?}");
}

thread_local! {
    static TAPPED: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

#[test]
fn a_tap_on_a_marker_tells_its_id_and_not_the_place() {
    use viso_map::Marker;
    TAPPED.with(|t| t.borrow_mut().clear());
    let start = berlin(12.0, 0.0);
    let view = viso_map::View::new(start, [W, H]);
    let none = Modifiers::default();
    let click = |x, y| {
        vec![
            mouse(x, y, PointerPhase::Down, none),
            mouse(x, y, PointerPhase::Up, none),
            RawEvent::RedrawRequested { window: WIN },
        ]
    };
    let mut script = click(300.0, 185.0);
    // Away from it, and past the double-click time.
    script.extend(redraws(40));
    script.extend(click(100.0, 400.0));
    run_with(start, false, script, move |m| {
        m.marker(Marker::new(7, view.unproject([300.0, 200.0]), red_box))
            .on_marker_tap(|_, id| TAPPED.with(|t| t.borrow_mut().push(format!("marker {id}"))))
            .on_tap(|_, _| TAPPED.with(|t| t.borrow_mut().push("map".into())))
    });
    assert_eq!(TAPPED.with(|t| t.borrow().clone()), ["marker 7", "map"]);
}

#[test]
fn near_markers_gather_into_a_cluster_that_opens_when_tapped() {
    use viso_map::{Cluster, Marker};
    let start = berlin(10.0, 0.0);
    let view = viso_map::View::new(start, [W, H]);
    let none = Modifiers::default();
    let script = vec![
        mouse(320.0, 240.0, PointerPhase::Down, none),
        mouse(320.0, 240.0, PointerPhase::Up, none),
    ];
    let (app, handle) = run_with(start, false, vec![], move |mut m| {
        for i in 0..5u32 {
            let p = view.unproject([310.0 + 5.0 * f64::from(i), 235.0 + 2.0 * f64::from(i)]);
            m = m.marker(Marker::new(u64::from(i), p, red_box));
        }
        m.marker(Marker::new(9, view.unproject([100.0, 100.0]), red_box))
            .cluster(Cluster::new(40.0, |cx, _| red_box(cx)))
    });
    picture(&app, "overlays-cluster");
    let (alone, clusters) = handle.markers_shown();
    assert_eq!(alone, [9]);
    assert_eq!(clusters.len(), 1);
    assert_eq!(clusters[0].0, 5);
    let (_, handle) = run_with(
        start,
        false,
        script.into_iter().chain(redraws(40)).collect(),
        move |mut m| {
            for i in 0..5u32 {
                let p = view.unproject([310.0 + 5.0 * f64::from(i), 235.0 + 2.0 * f64::from(i)]);
                m = m.marker(Marker::new(u64::from(i), p, red_box));
            }
            m.cluster(Cluster::new(40.0, |cx, _| red_box(cx)))
        },
    );
    let c = handle.camera();
    assert!(c.zoom >= 11.0, "{c:?}");
    let (alone, clusters) = handle.markers_shown();
    assert!(
        alone.len() > 1 || clusters.iter().all(|c| c.0 < 5),
        "{alone:?} {clusters:?}"
    );
}

#[test]
fn geojson_draws_through_the_style_even_where_the_source_has_nothing() {
    use viso_map::style::{Color, Layer};
    use viso_map::{GeoFeature, GeoJson, Geometry};
    let start = berlin(12.0, 0.0);
    let view = viso_map::View::new(start, [W, H]);
    let ring: Vec<LngLat> = [
        [200.0, 150.0],
        [440.0, 150.0],
        [440.0, 330.0],
        [200.0, 330.0],
    ]
    .iter()
    .map(|&p| view.unproject(p))
    .collect();
    let data = GeoJson::new(vec![
        GeoFeature::new(Geometry::Polygons(vec![vec![ring]])).with("kind", "zone"),
    ]);
    let slot = MapHandleSlot::default();
    let s = Rc::clone(&slot);
    SETUP.with(|setup| {
        let data = data.clone();
        *setup.borrow_mut() = Some(Box::new(move |cx| {
            let style = builtin::openmaptiles_light().with(Layer::fill(
                "zones",
                "zones",
                Color::rgb(0xff, 0, 0),
            ));
            map_view(viso_map::MemorySource::new())
                .camera(start)
                .style(style)
                .data("zones", data.clone())
                .handle(&s)
                .build(cx);
        }))
    });
    let app = drive_scripted::<MapApp>(redraws(16), STEP);
    let px = picture(&app, "overlays-geojson");
    assert!(
        reddish(rgb_at(&px, [320.0, 240.0])),
        "{:?}",
        rgb_at(&px, [320.0, 240.0])
    );
    assert!(!reddish(rgb_at(&px, [100.0, 100.0])));
    // What a tap there lands on.
    let handle = slot.borrow().clone().expect("built");
    let found = handle.features_at([320.0, 240.0], 4.0);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].layer, "zones");
    assert_eq!(
        found[0].properties,
        [("kind".to_owned(), viso_map::mvt::Value::from("zone"))]
    );
    assert!(handle.features_at([100.0, 100.0], 4.0).is_empty());
    assert_eq!(handle.features_at([196.0, 240.0], 6.0).len(), 1);
}

/// Places with names in several scripts, two of them almost on top of each
/// other, and a named road, as OpenMapTiles tiles.
fn named(tile: TileId) -> Vec<u8> {
    use viso_map::mvt::{GeomType, TileEncoder};
    let n = f64::from(1u32 << tile.z);
    let local = |p: LngLat| {
        let w = p.to_world();
        [
            ((w.x * n - f64::from(tile.x)) * 4096.0).round() as i32,
            ((w.y * n - f64::from(tile.y)) * 4096.0).round() as i32,
        ]
    };
    let places: [(f64, f64, &str, &str, &str, f64); 6] = [
        (13.400, 52.520, "city", "Berlin", "ベルリン", 1.0),
        (13.4005, 52.5201, "town", "Shadowed", "", 9.0),
        (13.380, 52.530, "town", "Αθήνα", "", 2.0),
        (13.420, 52.530, "town", "القاهرة", "", 3.0),
        (13.380, 52.508, "town", "Москва", "", 4.0),
        (13.420, 52.508, "town", "北京", "", 5.0),
    ];
    let mut t = TileEncoder::new();
    let mut l = t.layer("place", 4096);
    for (lon, lat, class, name, ja, rank) in places {
        let p = local(LngLat::new(lon, lat));
        if !(0..4096).contains(&p[0]) || !(0..4096).contains(&p[1]) {
            continue;
        }
        let mut props = vec![
            ("class", class.into()),
            ("name", name.into()),
            ("rank", rank.into()),
        ];
        if !ja.is_empty() {
            props.push(("name:ja", ja.into()));
        }
        l.feature(None, GeomType::Point, &[&[p]], &props);
    }
    l.finish();
    let mut l = t.layer("transportation_name", 4096);
    let a = local(LngLat::new(13.385, 52.514));
    let b = local(LngLat::new(13.415, 52.514));
    l.feature(
        None,
        GeomType::Line,
        &[&[a, b]],
        &[("name", "Unter den Linden".into())],
    );
    l.finish();
    t.finish()
}

/// A face for the scripts the labels are in: a system one with them all
/// where there is one, else one for Latin, Greek and Cyrillic.
fn label_fonts() -> bool {
    let wide = [
        "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
        "/Library/Fonts/Arial Unicode.ttf",
        "/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf",
    ]
    .iter()
    .find_map(|p| std::fs::read(p).ok());
    let all = wide.is_some();
    FONTS.with(|f| {
        *f.borrow_mut() = wide.into_iter().collect();
        f.borrow_mut().push(
            include_bytes!("../../../crates/text/tests/fixtures/DejaVuSans-subset.ttf").to_vec(),
        );
    });
    all
}

#[test]
fn labels_show_in_the_readers_language_without_overlapping_and_stay_upright() {
    let every_script = label_fonts();
    let start = berlin(13.0, 0.0);
    let slot = MapHandleSlot::default();
    let s = Rc::clone(&slot);
    SETUP.with(|setup| {
        *setup.borrow_mut() = Some(Box::new(move |cx| {
            map_view(|tile: TileId| viso_map::Reply::ready(Ok(Some(named(tile)))))
                .camera(start)
                .languages(&["ja-JP"])
                .handle(&s)
                .build(cx);
        }))
    });
    let app = drive_scripted::<MapApp>(redraws(30), STEP);
    picture(&app, "labels-z13");
    let handle = slot.borrow().clone().expect("built");
    let mut texts: Vec<String> = handle.labels().into_iter().map(|l| l.0).collect();
    texts.sort();
    texts.dedup();
    assert_eq!(
        texts,
        [
            "Unter den Linden",
            "Αθήνα",
            "Москва",
            "القاهرة",
            "ベルリン",
            "北京"
        ],
        "the shadowed town loses to the city over it"
    );
    // Turned past a quarter, the road's name turns back to read upright.
    let mut script = redraws(30);
    script.extend(key(
        KeyCode::Right,
        Modifiers {
            shift: true,
            ..Modifiers::default()
        },
    ));
    for _ in 0..8 {
        script.extend(key(
            KeyCode::Right,
            Modifiers {
                shift: true,
                ..Modifiers::default()
            },
        ));
        script.extend(redraws(20));
    }
    script.splice(
        0..0,
        [
            mouse(320.0, 240.0, PointerPhase::Down, Modifiers::default()),
            mouse(320.0, 240.0, PointerPhase::Up, Modifiers::default()),
        ],
    );
    let app = drive_scripted::<MapApp>(script, STEP);
    let handle = slot.borrow().clone().expect("built");
    assert!(
        (handle.camera().bearing - 135.0).abs() < 1e-6,
        "{:?}",
        handle.camera()
    );
    FONTS.with(|f| f.borrow_mut().clear());
    let px = picture(&app, "labels-z13-b135");
    assert!(handle.labels().iter().any(|l| l.0 == "Unter den Linden"));
    // Every label drew: dark text where each sits.
    if every_script {
        for (text, place) in handle.labels() {
            let p = handle.view().project(place);
            if p[0] < 10.0 || p[1] < 10.0 || p[0] > W - 10.0 || p[1] > H - 10.0 {
                continue;
            }
            let ink = (-6..=6)
                .flat_map(|dx| (-4..=4).map(move |dy| (dx, dy)))
                .filter(|&(dx, dy)| {
                    let c = rgb_at(&px, [p[0] + f64::from(dx), p[1] + f64::from(dy)]);
                    c.iter().all(|&v| v < 140)
                })
                .count();
            assert!(ink > 0, "{text} drew nothing at {p:?}");
        }
    }
}

#[test]
fn a_label_shows_its_svg_icon_over_its_text() {
    use viso_map::IconSet;
    use viso_map::mvt::{GeomType, TileEncoder};
    use viso_map::style::{Color, Layer, Prop, Style, Symbol};
    label_fonts();
    let start = berlin(16.0, 0.0);
    let cafe = start.center;
    let source = move |tile: TileId| {
        let n = f64::from(1u32 << tile.z);
        let w = cafe.to_world();
        let p = [
            ((w.x * n - f64::from(tile.x)) * 4096.0).round() as i32,
            ((w.y * n - f64::from(tile.y)) * 4096.0).round() as i32,
        ];
        let mut t = TileEncoder::new();
        let mut l = t.layer("poi", 4096);
        if (0..4096).contains(&p[0]) && (0..4096).contains(&p[1]) {
            l.feature(None, GeomType::Point, &[&[p]], &[("name", "Café".into())]);
        }
        l.finish();
        viso_map::Reply::ready(Ok(Some(t.finish())))
    };
    let square = br##"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><rect width="10" height="10" fill="#000"/></svg>"##;
    let slot = MapHandleSlot::default();
    let s = Rc::clone(&slot);
    SETUP.with(|setup| {
        *setup.borrow_mut() = Some(Box::new(move |cx| {
            let poi = Symbol {
                icon: Some(Prop::Const("cafe".to_owned())),
                icon_color: Prop::Const(Color::rgb(0xff, 0, 0)),
                icon_size: 16.0,
                ..Symbol::default()
            };
            map_view(source)
                .camera(start)
                .style(Style::new("icons", Color::WHITE).with(Layer::symbol("poi", "poi", poi)))
                .icons(IconSet::new().svg("cafe", square, true).unwrap())
                .handle(&s)
                .build(cx);
        }))
    });
    let app = drive_scripted::<MapApp>(redraws(30), STEP);
    FONTS.with(|f| f.borrow_mut().clear());
    let px = picture(&app, "labels-icon");
    let handle = slot.borrow().clone().expect("built");
    assert_eq!(handle.labels().len(), 1);
    // The icon is red on its place, the text dark under it.
    assert!(
        reddish(rgb_at(&px, [320.0, 240.0])),
        "{:?}",
        rgb_at(&px, [320.0, 240.0])
    );
    let ink = (248..270)
        .flat_map(|y| (300..340).map(move |x| [f64::from(x), f64::from(y)]))
        .filter(|&p| rgb_at(&px, p).iter().all(|&v| v < 120))
        .count();
    assert!(ink > 10, "{ink}");
}

/// A raster tile of solid color: blue on even tiles, green on odd ones.
fn checker(tile: TileId) -> Vec<u8> {
    let bgra = if (tile.x + tile.y).is_multiple_of(2) {
        [255, 0, 0, 255]
    } else {
        [0, 255, 0, 255]
    };
    support::png(256, 256, &bgra.repeat(256 * 256))
}

/// A terrain tile of a ridge running north and south through `ridge` (a
/// world x), Terrarium-encoded: 1 km high, falling a meter a meter.
fn ridge_tile(tile: TileId, ridge: f64) -> Vec<u8> {
    let n = 256u32;
    let span = f64::from(1u32 << tile.z);
    let mut bgra = Vec::with_capacity((n * n * 4) as usize);
    for _y in 0..n {
        for x in 0..n {
            let wx = (f64::from(tile.x) + (f64::from(x) + 0.5) / f64::from(n)) / span;
            // A world unit is about 24 500 km at Berlin's latitude.
            let meters = (1000.0 - (wx - ridge).abs() * 24_500_000.0).max(0.0);
            let v = meters + 32768.0;
            let r = (v / 256.0).floor();
            let g = (v - r * 256.0).floor();
            let b = ((v - r * 256.0 - g) * 256.0).round().min(255.0);
            bgra.extend_from_slice(&[b as u8, g as u8, r as u8, 255]);
        }
    }
    support::png(n, n, &bgra)
}

fn luma(c: [u8; 3]) -> u32 {
    u32::from(c[0]) + u32::from(c[1]) + u32::from(c[2])
}

#[test]
fn raster_and_hillshade_layers_draw_between_the_vector_layers() {
    use viso_map::RasterSource;
    use viso_map::style::{Color, DemEncoding, Layer};
    // Images of half opacity laid over the water, under the roads.
    let start = berlin(13.0, 0.0);
    let frames: Rc<RefCell<Vec<bool>>> = Rc::default();
    let log = Rc::clone(&frames);
    probe_frames(Some(Box::new(move |_, uploaded| {
        log.borrow_mut().push(uploaded);
    })));
    let none = Modifiers::default();
    let mut script = vec![mouse(320.0, 240.0, PointerPhase::Down, none)];
    for i in 1..=10 {
        script.push(mouse(
            320.0 + 2.0 * f64::from(i),
            240.0 + f64::from(i),
            PointerPhase::Moved,
            none,
        ));
        script.push(RawEvent::RedrawRequested { window: WIN });
    }
    let (app, handle) = run_with(start, false, script, |m| {
        let mut style = builtin::openmaptiles_light();
        let at = style
            .layers
            .iter()
            .position(|l| l.id.starts_with("road"))
            .unwrap_or(style.layers.len());
        style
            .layers
            .insert(at, Layer::raster("photo", "photo").opacity(0.5));
        m.style(style).raster_source(
            "photo",
            RasterSource::new(
                |tile: TileId| viso_map::Reply::ready(Ok(Some(checker(tile)))),
                0,
                18,
            ),
        )
    });
    probe_frames(None);
    let px = picture(&app, "raster-photo");
    // The images moved with a drag that brought no new tile into view,
    // and nothing reached the renderer.
    let frames = frames.borrow();
    let tail = &frames[frames.len().saturating_sub(8)..];
    assert!(tail.iter().all(|&uploaded| !uploaded), "{frames:?}");
    let tiles = handle.tiles();
    assert!(tiles.images >= 4, "{tiles:?}");
    assert_eq!(tiles.in_flight, 0);
    // Tinted blue or green everywhere, half way.
    let c = rgb_at(&px, [320.0, 240.0]);
    assert!(
        (c[2] > c[0] + 40 && c[2] > c[1] + 40) || (c[1] > c[0] + 40 && c[1] > c[2] + 40),
        "{c:?}"
    );
    assert!(c[0] > 40, "half of what is beneath shows: {c:?}");

    // A ridge north and south through the middle, lit from the north-west.
    let mid = start.center.to_world().x;
    let slot = MapHandleSlot::default();
    let s = Rc::clone(&slot);
    SETUP.with(|setup| {
        *setup.borrow_mut() = Some(Box::new(move |cx| {
            let style = viso_map::style::Style::new("terrain", Color::rgb(200, 200, 200))
                .with(Layer::hillshade("shade", "dem", DemEncoding::Terrarium));
            map_view(viso_map::MemorySource::new())
                .camera(start)
                .style(style)
                .raster_source(
                    "dem",
                    RasterSource::new(
                        move |tile: TileId| viso_map::Reply::ready(Ok(Some(ridge_tile(tile, mid)))),
                        0,
                        12,
                    ),
                )
                .handle(&s)
                .build(cx);
        }))
    });
    let app = drive_scripted::<MapApp>(redraws(16), STEP);
    let px = picture(&app, "raster-hillshade");
    let handle = slot.borrow().clone().expect("built");
    assert!(handle.tiles().images >= 1);
    let (west, east, far) = (
        rgb_at(&px, [300.0, 240.0]),
        rgb_at(&px, [340.0, 240.0]),
        rgb_at(&px, [20.0, 240.0]),
    );
    assert!(luma(west) > luma(far) + 15, "{west:?} {far:?}");
    assert!(luma(east) + 15 < luma(far), "{east:?} {far:?}");
}

/// A real Shortbread tile of Luxembourg City (see `fixtures/ATTRIBUTION`).
const LUXEMBOURG: &[u8] = include_bytes!("fixtures/luxembourg-14-8469-5582.mvt.gz");

#[test]
fn a_real_shortbread_tile_draws_light_and_dark_at_several_zooms_and_turns() {
    label_fonts();
    let tile = TileId::new(14, 8469, 5582);
    // Rollingergrund, in the tile's south-east, where its streets and
    // buildings are.
    let n = f64::from(1u32 << 14);
    let at = viso_map::WorldPoint::new((8469.0 + 0.72) / n, (5582.0 + 0.78) / n).to_lng_lat();
    for (zoom, bearing, dark) in [(14.6, 0.0, false), (15.4, 35.0, true), (16.3, 200.0, false)] {
        let slot = MapHandleSlot::default();
        let s = Rc::clone(&slot);
        SETUP.with(|setup| {
            *setup.borrow_mut() = Some(Box::new(move |cx| {
                let style = if dark {
                    builtin::shortbread_dark()
                } else {
                    builtin::shortbread_light()
                };
                map_view(viso_map::MemorySource::new().with(tile, LUXEMBOURG))
                    .camera(Camera::new(at, zoom).with_bearing(bearing))
                    .style(style)
                    .languages(&["en"])
                    .handle(&s)
                    .build(cx);
            }))
        });
        let app = drive_scripted::<MapApp>(redraws(30), STEP);
        let name = format!(
            "luxembourg-z{zoom}-b{bearing}-{}",
            if dark { "dark" } else { "light" }
        );
        let px = picture(&app, &name);
        let handle = slot.borrow().clone().expect("built");
        let tiles = handle.tiles();
        assert!(tiles.drawn.iter().any(|d| d.tile == tile), "{tiles:?}");
        // Streets, buildings and land: many colors, not one background.
        let mut seen = std::collections::HashSet::new();
        for y in (0..H as usize).step_by(8) {
            for x in (0..W as usize).step_by(8) {
                let c = rgb_at(&px, [x as f64, y as f64]);
                seen.insert([c[0] / 16, c[1] / 16, c[2] / 16]);
            }
        }
        assert!(seen.len() > 12, "{name}: {} colors", seen.len());
        assert!(!handle.labels().is_empty(), "{name}: no labels");
    }
}
