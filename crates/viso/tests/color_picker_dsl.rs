//! A view's `ColorPicker`: the framework builds its parts inside it — a
//! square over hue and opacity bars, a hex field and a swatch — at its
//! value; a pick writes the bound state back and runs `changed`, a second
//! picker showing that state follows, `wheel` and `alpha` show and hide the
//! parts they need, and typed hex picks too.
#![cfg(feature = "headless")]

use std::time::Duration;

use viso::__test_support::{DrivenApp, drive_scripted};
use viso::platform::{
    KeyCode as Key, Modifiers as RawModifiers, PointerButtons as RawButtons,
    PointerPhase as RawPhase, RawEvent, RawKey, RawPointer, RawText, WindowId,
};
use viso::prelude::*;
use viso::render::{Rect, Rgba};
use viso::ui::color_plane::{Hsv, rgba32};
use viso::ui::{NodeId, NodeStore, Role, WindowConfig};

const STEP: Duration = Duration::from_millis(16);
const WIN: WindowId = WindowId(1);

viso::component! {
    Palette {
        state fill: Color = #3366cc;
        state picks = 0;
        state ring = false;
        view {
            Row {
                width: 900dp;
                height: 500dp;
                padding: 20dp;
                gap: 20dp;
                ColorPicker {
                    label: "Fill";
                    bind value <=> fill;
                    wheel: ring;
                    on changed(e) { picks += 1; }
                }
                ColorPicker { label: "Shown"; value: fill; alpha: false; }
                Column {
                    Text { width: 20dp; height: 20dp; on click { ring = !ring; } }
                    Text { width: 20dp; height: 20dp; visible: picks > 0; }
                }
            }
        }
    }
}

struct PaletteApp;

impl Application for PaletteApp {
    fn new(cx: &mut AppCx) -> Self {
        cx.load_font(&include_bytes!("../../text/tests/fixtures/DejaVuSans-subset.ttf")[..]);
        PaletteApp
    }

    fn window_config(&self) -> WindowConfig {
        WindowConfig {
            caption: false,
            ..Default::default()
        }
    }

    fn build(&mut self, cx: &mut BuildCx<'_>) {
        Palette::build(cx);
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

fn click(at: (f32, f32)) -> Vec<RawEvent> {
    vec![
        mouse(at, RawButtons::NONE, RawPhase::Moved),
        mouse(at, RawButtons::PRIMARY, RawPhase::Down),
        redraw(),
        mouse(at, RawButtons::PRIMARY, RawPhase::Up),
        redraw(),
        redraw(),
    ]
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

fn run(steps: &[Vec<RawEvent>]) -> DrivenApp<PaletteApp> {
    let mut script = vec![redraw(), redraw()];
    for step in steps {
        script.extend(step.iter().cloned());
    }
    script.push(redraw());
    drive_scripted::<PaletteApp>(script, STEP)
}

/// A picker's parts, those hidden left out.
struct Parts {
    node: NodeId,
    plane: NodeId,
    stops: Vec<NodeId>,
    bars: Vec<NodeId>,
    hex: NodeId,
    swatch: NodeId,
}

/// The parts of the `which`th picker in the row.
fn parts(app: &DrivenApp<PaletteApp>, which: usize) -> Parts {
    let store = app.store();
    let row = app.root().unwrap();
    let node = store.arena().children(row).nth(which).unwrap();
    let column = store.arena().first_child(node).expect("its parts");
    let shown = |ids: Vec<NodeId>| -> Vec<NodeId> {
        ids.into_iter().filter(|&id| !store.hidden(id)).collect()
    };
    let kids: Vec<NodeId> = store.arena().children(column).collect();
    let (plane, row) = (kids[0], *kids.last().unwrap());
    let mut row = store.arena().children(row);
    let field = row.next().unwrap();
    Parts {
        node,
        plane,
        stops: shown(store.arena().children(plane).collect()),
        bars: shown(kids[1..kids.len() - 1].to_vec()),
        hex: store.arena().first_child(field).unwrap(),
        swatch: row.next().unwrap(),
    }
}

/// The third child of the row: the toggle and the mark of a pick.
fn controls(app: &DrivenApp<PaletteApp>) -> (NodeId, NodeId) {
    let store = app.store();
    let column = store.arena().children(app.root().unwrap()).nth(2).unwrap();
    let mut kids = store.arena().children(column);
    (kids.next().unwrap(), kids.next().unwrap())
}

fn middle(store: &NodeStore, id: NodeId) -> (f32, f32) {
    let r = store.world(id);
    (r.x + r.w * 0.5, r.y + r.h * 0.5)
}

/// The point on a square plane's tones at saturation `s` and brightness `v`.
fn tone_at(store: &NodeStore, plane: NodeId, s: f32, v: f32) -> (f32, f32) {
    let r = store.world(plane);
    let t = Rect::new(r.x + 8.0, r.y + 8.0, r.w - 16.0, r.h - 16.0);
    (t.x + t.w * s, t.y + t.h * (1.0 - v))
}

fn hsv(store: &NodeStore, plane: NodeId) -> Hsv {
    store.color_plane(plane).unwrap().hsv
}

fn swatch(store: &NodeStore, p: &Parts) -> Rgba {
    let plane = store.color_plane(p.swatch).unwrap();
    plane.hsv.to_rgba(plane.alpha)
}

#[test]
fn its_parts_stand_at_its_value_named_as_a_group() {
    let app = run(&[]);
    let store = app.store();
    let fill = parts(&app, 0);
    let shown = parts(&app, 1);
    let sem = store.semantics(fill.node).unwrap();
    assert_eq!(
        (sem.role, sem.label.as_deref()),
        (Role::Group, Some("Fill"))
    );
    assert_eq!(fill.stops.len(), 1, "a square, no ring");
    assert_eq!(fill.bars.len(), 2, "a hue bar and an opacity bar");
    assert_eq!(shown.bars.len(), 1, "no opacity without alpha");
    let r = store.world(fill.plane);
    assert!(
        (r.w - 232.0).abs() < 0.5 && (r.h - 174.0).abs() < 0.5,
        "{r:?}"
    );
    assert_eq!(rgba32(swatch(store, &fill)), 0x3366ccff);
    assert_eq!(rgba32(swatch(store, &shown)), 0x3366ccff);
    assert_eq!(store.semantics(fill.hex).unwrap().role, Role::TextField);
    assert!(store.hidden(controls(&app).1), "nothing picked yet");
}

#[test]
fn a_pick_writes_the_state_back_and_runs_changed() {
    let app = run(&[]);
    let fill = parts(&app, 0);
    let shown = parts(&app, 1);
    let at = tone_at(app.store(), fill.plane, 0.75, 0.25);
    let app = run(&[click(at)]);
    let store = app.store();
    let got = hsv(store, fill.plane);
    assert!(
        (got.s - 0.75).abs() < 2e-3 && (got.v - 0.25).abs() < 2e-3,
        "{got:?}"
    );
    assert!(!store.hidden(controls(&app).1), "changed ran");
    let picked = rgba32(swatch(store, &fill));
    assert_eq!(
        rgba32(swatch(store, &shown)),
        picked,
        "the state follows, and the picker showing it"
    );
    // The hue holds through the state's round trip.
    let held = hsv(store, fill.plane);
    assert!((held.h - got.h).abs() < 1e-6);
    // The arrows step the square once it has the focus.
    let app = run(&[click(at), key(Key::Right)]);
    let stepped = hsv(app.store(), fill.plane);
    assert!((stepped.s - 0.76).abs() < 2e-3, "{stepped:?}");
    assert_ne!(rgba32(swatch(app.store(), &shown)), picked);
}

#[test]
fn wheel_and_alpha_show_the_parts_they_need() {
    let app = run(&[]);
    let toggle = middle(app.store(), controls(&app).0);
    let app = run(&[click(toggle)]);
    let store = app.store();
    let fill = parts(&app, 0);
    assert_eq!(fill.stops.len(), 2, "a ring and a square");
    assert_eq!(fill.bars.len(), 1, "the ring holds the hue");
    let r = store.world(fill.plane);
    assert!((r.h - r.w).abs() < 0.5, "a wheel as tall as wide: {r:?}");
    let app = run(&[click(toggle), click(toggle)]);
    let fill = parts(&app, 0);
    assert_eq!((fill.stops.len(), fill.bars.len()), (1, 2), "back again");
}

#[test]
fn typed_hex_picks_and_the_state_follows() {
    let app = run(&[]);
    let fill = parts(&app, 0);
    let shown = parts(&app, 1);
    let field = middle(app.store(), fill.hex);
    let mut steps = vec![click(field)];
    steps.extend((0..9).map(|_| key(Key::Backspace)));
    steps.push(typed("ff0000"));
    let app = run(&steps);
    let store = app.store();
    assert_eq!(rgba32(swatch(store, &fill)), 0xff0000ff);
    assert_eq!(rgba32(swatch(store, &shown)), 0xff0000ff);
    assert!(!store.hidden(controls(&app).1));
}
