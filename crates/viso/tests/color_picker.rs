//! Colour pickers driven through the facade: a square over hue and opacity
//! bars and a wheel draw their tones, hues, checkerboard and swatch
//! matching a blessed golden; a press on the square picks its saturation and
//! brightness and a drag carries on, a press on the ring or the hue bar
//! picks the hue, the opacity bar the opacity; the arrows step the focused
//! part; typed hex sets the colour and settles to its full form; dragging to
//! black and back keeps the hue.
#![cfg(feature = "headless")]

use std::cell::RefCell;
use std::path::PathBuf;
use std::time::Duration;

use viso::__test_support::{DrivenApp, drive_scripted};
use viso::gpu::{CpuRaster, GpuBackend, RawWindowHandle};
use viso::platform::{
    KeyCode as Key, Modifiers as RawModifiers, PointerButtons as RawButtons,
    PointerPhase as RawPhase, RawEvent, RawKey, RawPointer, RawText, WindowId,
};
use viso::prelude::*;
use viso::render::{Rect, Renderer, Rgba};
use viso::ui::color_plane::{Hsv, PlanePart};
use viso::ui::{
    Axis, BindingTable, BoxStyle, Component, FlexStyle, Inset, NodeId, NodeStore, Role,
    SemanticProjector, Sizing, StateStore, TextEdits, VirtualLists, paint_tree,
};
use viso::widgets::{ViewStyle, color_picker, view};

const STEP: Duration = Duration::from_millis(16);
const WIN: WindowId = WindowId(1);

thread_local! {
    static HEARD: RefCell<Vec<Rgba>> = const { RefCell::new(Vec::new()) };
}

fn heard() -> Vec<Rgba> {
    HEARD.with(|h| std::mem::take(&mut *h.borrow_mut()))
}

/// The starting colour: hue a third round from red, half saturated, at
/// four fifths brightness.
const START: Hsv = Hsv::new(1.0 / 3.0, 0.5, 0.8);

/// A square picker at 20, 20 and a wheel without opacity beside it.
struct PickerApp;

impl Application for PickerApp {
    fn new(cx: &mut AppCx) -> Self {
        cx.load_font(&include_bytes!("../../text/tests/fixtures/DejaVuSans-subset.ttf")[..]);
        PickerApp
    }

    fn window_config(&self) -> viso::ui::WindowConfig {
        viso::ui::WindowConfig {
            caption: false,
            ..Default::default()
        }
    }

    fn build(&mut self, cx: &mut BuildCx<'_>) {
        cx.flex(
            FlexStyle {
                size: Sizing::fill(),
                axis: Axis::Row,
                padding: Inset::all(20.0),
                gap: 20.0,
                ..FlexStyle::default()
            },
            |cx| {
                color_picker("Fill")
                    .value(START.to_rgba(1.0))
                    .on_change(|_, c| HEARD.with(|h| h.borrow_mut().push(c)))
                    .build(cx);
                color_picker("Stroke")
                    .wheel(true)
                    .alpha(false)
                    .value(START.to_rgba(1.0))
                    .on_change(|_, c| HEARD.with(|h| h.borrow_mut().push(c)))
                    .build(cx);
            },
        );
    }
}

fn redraw() -> RawEvent {
    RawEvent::RedrawRequested { window: WIN }
}

fn mouse(at: (f32, f32), buttons: RawButtons, phase: RawPhase) -> RawEvent {
    RawEvent::Pointer(RawPointer::mouse(
        WIN,
        f64::from(at.0),
        f64::from(at.1),
        buttons,
        RawModifiers::default(),
        phase,
    ))
}

fn press(at: (f32, f32)) -> Vec<RawEvent> {
    vec![
        mouse(at, RawButtons::NONE, RawPhase::Moved),
        mouse(at, RawButtons::PRIMARY, RawPhase::Down),
        redraw(),
    ]
}

fn drag(to: (f32, f32)) -> Vec<RawEvent> {
    vec![mouse(to, RawButtons::PRIMARY, RawPhase::Moved), redraw()]
}

fn release(at: (f32, f32)) -> Vec<RawEvent> {
    vec![
        mouse(at, RawButtons::PRIMARY, RawPhase::Up),
        redraw(),
        redraw(),
    ]
}

fn click(at: (f32, f32)) -> Vec<RawEvent> {
    let mut events = press(at);
    events.extend(release(at));
    events
}

fn key(code: Key) -> Vec<RawEvent> {
    let raw = |pressed| {
        RawEvent::Key(RawKey {
            window: WIN,
            code,
            pressed,
            repeat: false,
            modifiers: RawModifiers::default(),
        })
    };
    vec![raw(true), raw(false), redraw(), redraw()]
}

fn typed(text: &str) -> Vec<RawEvent> {
    vec![
        RawEvent::Text(RawText {
            window: WIN,
            text: text.to_owned(),
        }),
        redraw(),
        redraw(),
    ]
}

fn run(steps: &[Vec<RawEvent>]) -> DrivenApp<PickerApp> {
    heard();
    let mut script = vec![redraw(), redraw()];
    for step in steps {
        script.extend(step.iter().cloned());
    }
    script.push(redraw());
    drive_scripted::<PickerApp>(script, STEP)
}

/// A picker's parts.
struct Parts {
    plane: NodeId,
    stops: Vec<NodeId>,
    bars: Vec<NodeId>,
    hex: NodeId,
    swatch: NodeId,
}

fn parts(store: &NodeStore, root: NodeId, which: usize) -> Parts {
    let shown = |ids: Vec<NodeId>| -> Vec<NodeId> {
        ids.into_iter().filter(|&id| !store.hidden(id)).collect()
    };
    let picker = store.arena().children(root).nth(which).unwrap();
    let kids: Vec<NodeId> = store.arena().children(picker).collect();
    let (plane, row) = (kids[0], *kids.last().unwrap());
    let mut row = store.arena().children(row);
    let field = row.next().unwrap();
    Parts {
        plane,
        // The parts a picker does not use are hidden.
        stops: shown(store.arena().children(plane).collect()),
        bars: shown(kids[1..kids.len() - 1].to_vec()),
        hex: store.arena().first_child(field).unwrap(),
        swatch: row.next().unwrap(),
    }
}

/// The point at fractions `fx` across and `fy` down a node.
fn at(store: &NodeStore, id: NodeId, fx: f32, fy: f32) -> (f32, f32) {
    let r = store.world(id);
    (r.x + r.w * fx, r.y + r.h * fy)
}

/// The point on a square plane's tones at saturation `s` and brightness `v`:
/// its tones sit eight in from each side.
fn tone_at(store: &NodeStore, plane: NodeId, s: f32, v: f32) -> (f32, f32) {
    let r = store.world(plane);
    let t = Rect::new(r.x + 8.0, r.y + 8.0, r.w - 16.0, r.h - 16.0);
    (t.x + t.w * s, t.y + t.h * (1.0 - v))
}

fn hsv(store: &NodeStore, plane: NodeId) -> Hsv {
    store.color_plane(plane).unwrap().hsv
}

fn near(a: f32, b: f32) -> bool {
    (a - b).abs() < 2e-3
}

fn layout() -> (Parts, Parts, DrivenApp<PickerApp>) {
    let app = run(&[]);
    let root = app.root().unwrap();
    let square = parts(app.store(), root, 0);
    let wheel = parts(app.store(), root, 1);
    (square, wheel, app)
}

#[test]
fn a_press_on_the_square_picks_its_tone_and_a_drag_carries_on() {
    let (p, _, app) = layout();
    let store = app.store();
    assert_eq!(p.stops.len(), 1);
    assert_eq!(p.bars.len(), 2, "a hue bar and an opacity bar");
    let sem = store.semantics(p.stops[0]).unwrap();
    assert_eq!(sem.role, Role::Slider);
    assert_eq!(sem.value.as_deref(), Some("Saturation 50%, Brightness 80%"));
    let a = tone_at(store, p.plane, 0.75, 0.25);
    let b = tone_at(store, p.plane, 0.25, 0.9);
    let app = run(&[press(a)]);
    let got = hsv(app.store(), p.plane);
    assert!(near(got.s, 0.75) && near(got.v, 0.25), "{got:?}");
    assert!(near(got.h, 1.0 / 3.0), "the hue holds");
    assert_eq!(app.store().focused(), Some(p.stops[0]), "a press focuses");
    let colour = heard();
    let want = Hsv::new(1.0 / 3.0, got.s, got.v).to_rgba(1.0);
    assert_eq!(colour, [want]);
    let app = run(&[press(a), drag(b), drag((9000.0, -9000.0)), release(b)]);
    let got = hsv(app.store(), p.plane);
    assert_eq!(
        (got.s, got.v),
        (1.0, 1.0),
        "a drag beyond holds to the square"
    );
    assert_eq!(heard().len(), 3);
    assert_eq!(app.store().color_plane(p.plane).unwrap().drag, None);
    let sw = app.store().color_plane(p.swatch).unwrap().hsv;
    assert_eq!((sw.s, sw.v), (1.0, 1.0), "the swatch shows it");
}

#[test]
fn the_bars_pick_hue_and_opacity_and_the_arrows_step_the_focused_part() {
    let (p, _, app) = layout();
    let store = app.store();
    let hue = at(store, p.bars[0], 0.5, 0.5);
    let alpha = at(store, p.bars[1], 0.5, 0.5);
    let app = run(&[click(hue), click(alpha)]);
    let store = app.store();
    let got = hsv(store, p.plane);
    assert!(near(got.h, 0.5), "{got:?}");
    let plane = store.color_plane(p.swatch).unwrap();
    assert!(near(plane.alpha, 0.5));
    let last = *heard().last().unwrap();
    assert!(near(last.a, 0.5));
    let state = store.semantic_state(p.bars[0]).unwrap();
    assert!(near(state.value.unwrap(), 180.0));
    assert_eq!(state.range, Some((0.0, 360.0)));
    // The square's arrows step saturation across and brightness up.
    let tone = tone_at(store, p.plane, 0.5, 0.5);
    let app = run(&[click(tone), key(Key::Right), key(Key::Up)]);
    let got = hsv(app.store(), p.plane);
    assert!(near(got.s, 0.51) && near(got.v, 0.51), "{got:?}");
    assert_eq!(
        app.store().color_plane(p.plane).unwrap().focus,
        Some(PlanePart::Tone)
    );
}

#[test]
fn a_wheel_picks_its_hue_round_the_ring_and_steps_it_with_the_arrows() {
    let (_, w, app) = layout();
    let store = app.store();
    assert_eq!(
        w.stops.len(),
        2,
        "a stop for the ring and one for the square"
    );
    assert!(w.bars.is_empty(), "no hue bar and no opacity");
    let sem = store.semantics(w.stops[0]).unwrap();
    assert_eq!(
        (sem.role, sem.label.as_deref()),
        (Role::Slider, Some("Hue"))
    );
    let r = store.world(w.plane);
    let (cx, cy) = (r.x + r.w * 0.5, r.y + r.h * 0.5);
    let ring = r.w.min(r.h) * 0.43;
    // A quarter round, at the right.
    let app = run(&[click((cx + ring, cy)), key(Key::Right)]);
    let got = hsv(app.store(), w.plane);
    assert!(near(got.h, 0.26), "{got:?}");
    assert_eq!(app.store().focused(), Some(w.stops[0]));
    assert_eq!(
        app.store().color_plane(w.plane).unwrap().focus,
        Some(PlanePart::Hue)
    );
    // Its square sits within the ring.
    let app = run(&[click((cx, cy))]);
    let got = hsv(app.store(), w.plane);
    assert!(near(got.s, 0.5) && near(got.v, 0.5), "{got:?}");
    assert_eq!(app.store().focused(), Some(w.stops[1]));
}

#[test]
fn typed_hex_sets_the_colour_and_settles_to_its_full_form() {
    let (p, _, app) = layout();
    let hex = at(app.store(), p.hex, 0.9, 0.5);
    let mut steps = vec![click(hex)];
    steps.extend((0..9).map(|_| key(Key::Backspace)));
    steps.push(typed("#f00"));
    let app = run(&steps);
    assert_eq!(app.focused_text(), Some("#f00"));
    assert!(heard().is_empty(), "a short form waits until it settles");
    steps.push(key(Key::Enter));
    let app = run(&steps);
    assert_eq!(
        app.focused_text(),
        Some("#FF0000"),
        "Enter shows it in full"
    );
    let got = hsv(app.store(), p.plane);
    assert!(
        near(got.h, 0.0) && near(got.s, 1.0) && near(got.v, 1.0),
        "{got:?}"
    );
    assert_eq!(*heard().last().unwrap(), Rgba::new(1.0, 0.0, 0.0, 1.0));
    // Half clear, as eight digits.
    steps.extend((0..9).map(|_| key(Key::Backspace)));
    steps.push(typed("00000080"));
    let app = run(&steps);
    assert_eq!(
        app.focused_text(),
        Some("00000080"),
        "the field keeps what is typed"
    );
    let plane = app.store().color_plane(p.swatch).unwrap();
    assert!(near(plane.alpha, 128.0 / 255.0));
    assert_eq!(plane.hsv.h, 0.0, "black keeps the hue");
    assert_eq!(plane.hsv.s, 1.0, "and the saturation");
}

#[test]
fn dragging_to_black_and_back_keeps_the_hue() {
    let (p, _, app) = layout();
    let store = app.store();
    let black = tone_at(store, p.plane, 0.6, 0.0);
    let back = tone_at(store, p.plane, 0.6, 0.7);
    let app = run(&[press(black), drag(back), release(back)]);
    let got = hsv(app.store(), p.plane);
    assert!(near(got.h, 1.0 / 3.0), "{got:?}");
    let colours = heard();
    let first = colours[0];
    assert_eq!((first.r, first.g, first.b), (0.0, 0.0, 0.0));
}

const GW: u32 = 520;
const GH: u32 = 340;
const CLEAR: [f32; 4] = [0.0, 0.0, 0.0, 1.0];
const TOL: u8 = 2;

#[test]
fn a_square_and_a_wheel_draw_their_tones_hues_and_swatch_and_match_golden() {
    let mut gpu = CpuRaster::new();
    let surface = gpu.create_surface(RawWindowHandle::Headless, GW, GH);
    let format = gpu.surface_format(surface);
    let mut renderer = Renderer::new(&mut gpu, format);
    let mut store = NodeStore::new();
    let mut states = StateStore::new();
    let mut bindings = BindingTable::new();
    let mut lists = VirtualLists::new();
    let mut text_edits = TextEdits::new();
    let mut projectors = SemanticProjector::new();
    let scene = view(ViewStyle {
        axis: Axis::Row,
        padding: Inset::all(16.0),
        gap: 20.0,
        size: Sizing::fill(),
        background: BoxStyle::solid(Rgba::new(0.97, 0.97, 0.98, 1.0)),
        ..Default::default()
    })
    .children(|cx| {
        color_picker("Fill")
            .value(Hsv::new(0.58, 0.7, 0.75).to_rgba(0.6))
            .build(cx);
        color_picker("Stroke")
            .wheel(true)
            .value(Hsv::new(0.05, 0.85, 0.9).to_rgba(1.0))
            .build(cx);
    });
    let root = {
        let mut cx = viso::ui::BuildCx::with_reactive(
            &mut store,
            &mut states,
            &mut bindings,
            &mut lists,
            &mut text_edits,
            &mut projectors,
        );
        scene.build(&mut cx);
        cx.root().unwrap()
    };
    store.layout(
        root,
        Rect::new(0.0, 0.0, GW as f32, GH as f32),
        &mut Vec::new(),
    );
    let mut primitives = Vec::new();
    paint_tree(&store, root, &mut primitives);
    renderer.upload(&mut gpu, &primitives);
    renderer.submit(&mut gpu, surface, CLEAR, [GW as f32, GH as f32]);
    let actual = gpu.read_pixels_bgra8(surface);
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/color_picker.bgra8");
    if std::env::var("BLESS").is_ok() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &actual).unwrap();
        return;
    }
    let expected = std::fs::read(&path)
        .unwrap_or_else(|_| panic!("missing golden {}; run with BLESS=1", path.display()));
    assert_eq!(actual.len(), expected.len());
    let worst = actual
        .iter()
        .zip(&expected)
        .map(|(a, e)| a.abs_diff(*e))
        .max()
        .unwrap_or(0);
    assert!(
        worst <= TOL,
        "golden mismatch: max per-channel diff {worst}"
    );
}
