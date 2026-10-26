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
}

struct MapApp;

impl Application for MapApp {
    fn new(_cx: &mut AppCx) -> Self {
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
                .handle(&s)
                .build(cx);
            }))
        });
        let app = drive_scripted::<MapApp>(redraws(16), STEP);
        let handle = slot.borrow().clone().expect("built");
        assert!(!handle.tiles().drawn.is_empty());
        picture(
            &app,
            &format!(
                "real-{name}-z{zoom}-b{bearing}-{}",
                if dark { "dark" } else { "light" }
            ),
        );
    }
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
