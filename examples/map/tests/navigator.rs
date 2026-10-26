//! The navigator driven headless over real data: a search planned to a
//! route, a long press planned to another, and a simulated drive with the
//! turn banner following it. Runs when `VISO_MAP_DATA` names the prefix of
//! a `viso-map-build` output; pictures go to `VISO_MAP_OUT`.

#[path = "../../../extras/map/tests/support/mod.rs"]
mod support;

use std::time::Duration;

use viso::__test_support::{DrivenApp, drive_scripted, drive_scripted_frames};
use viso::platform::{
    KeyCode as Key, Modifiers, PointerButtons, PointerPhase, RawEvent, RawKey, RawPointer, RawText,
    WindowId,
};
use viso::prelude::*;
use viso::ui::{NodeStore, WindowConfig};
use viso_example_map::Navigator;

const WIN: WindowId = WindowId(1);
const STEP: Duration = Duration::from_millis(16);

/// The navigator in a smaller window, with fonts for its text.
struct Test(Navigator);

impl Application for Test {
    fn new(cx: &mut AppCx) -> Self {
        for path in [
            "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
            "/Library/Fonts/Arial Unicode.ttf",
            "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        ] {
            if let Ok(face) = std::fs::read(path) {
                cx.load_font(&face[..]);
            }
        }
        cx.load_font(
            &include_bytes!("../../../crates/text/tests/fixtures/DejaVuSans-subset.ttf")[..],
        );
        Test(Navigator::new(cx))
    }

    fn window_config(&self) -> WindowConfig {
        WindowConfig {
            caption: false,
            size: (900.0, 640.0),
            ..Default::default()
        }
    }

    fn build(&mut self, cx: &mut BuildCx<'_>) {
        self.0.build(cx);
    }
}

fn redraw() -> RawEvent {
    RawEvent::RedrawRequested { window: WIN }
}

/// `n` frames passing, a wakeup before each.
fn frames(n: usize) -> Vec<RawEvent> {
    (0..n).flat_map(|_| [RawEvent::Wakeup, redraw()]).collect()
}

fn mouse(x: f64, y: f64, buttons: PointerButtons, phase: PointerPhase) -> RawEvent {
    RawEvent::Pointer(RawPointer::mouse(
        WIN,
        x,
        y,
        buttons,
        Modifiers::default(),
        phase,
    ))
}

fn click(at: (f64, f64)) -> Vec<RawEvent> {
    vec![
        mouse(at.0, at.1, PointerButtons::NONE, PointerPhase::Moved),
        mouse(at.0, at.1, PointerButtons::PRIMARY, PointerPhase::Down),
        mouse(at.0, at.1, PointerButtons::PRIMARY, PointerPhase::Up),
        redraw(),
        redraw(),
    ]
}

/// A press held 0.8 s, by the frames' clock.
fn long_press(at: (f64, f64)) -> Vec<RawEvent> {
    let mut e = vec![
        mouse(at.0, at.1, PointerButtons::NONE, PointerPhase::Moved),
        mouse(at.0, at.1, PointerButtons::PRIMARY, PointerPhase::Down),
    ];
    e.extend(frames(50));
    e.push(mouse(at.0, at.1, PointerButtons::NONE, PointerPhase::Up));
    e.push(redraw());
    e
}

fn key(code: Key) -> Vec<RawEvent> {
    let raw = |pressed| {
        RawEvent::Key(RawKey {
            window: WIN,
            code,
            pressed,
            repeat: false,
            modifiers: Modifiers::default(),
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

/// Every node under `id`, depth first.
fn all(store: &NodeStore, id: NodeId, out: &mut Vec<NodeId>) {
    out.push(id);
    for c in store.arena().children(id) {
        all(store, c, out);
    }
}

fn nodes(app: &DrivenApp<Test>) -> Vec<NodeId> {
    let mut out = Vec::new();
    all(app.store(), app.root().expect("a root"), &mut out);
    out
}

/// The middle of the button named `name`.
fn button_at(app: &DrivenApp<Test>, name: &str) -> (f64, f64) {
    let store = app.store();
    let id = nodes(app)
        .into_iter()
        .find(|&n| {
            store
                .semantics(n)
                .is_some_and(|s| s.role == Role::Button && s.label.as_deref() == Some(name))
        })
        .unwrap_or_else(|| panic!("a {name} button"));
    let r = store.world(id);
    (f64::from(r.x + r.w / 2.0), f64::from(r.y + r.h / 2.0))
}

/// What every label in the window reads out.
fn texts(app: &DrivenApp<Test>) -> Vec<String> {
    let store = app.store();
    nodes(app)
        .into_iter()
        .filter_map(|n| store.semantics(n).and_then(|s| s.label.clone()))
        .collect()
}

fn picture(app: &DrivenApp<Test>, name: &str) {
    if let Some((w, h, px)) = app.pixels() {
        support::save(name, w, h, &px);
    }
}

#[test]
fn search_route_and_drive_over_real_data() {
    if std::env::var_os("VISO_MAP_DATA").is_none() {
        eprintln!("VISO_MAP_DATA is not set: skipped");
        return;
    }
    // Where the parts are, from a first run.
    let layout = drive_scripted::<Test>(frames(4), STEP);
    let drive = button_at(&layout, "Drive");
    let field = (200.0, 32.0);
    drop(layout);

    let mut script = frames(60);
    script.extend(click(field));
    script.extend(typed("Place Guillaume"));
    script.extend(frames(4));
    script.extend(key(Key::Enter));
    script.extend(frames(90));
    let app = drive_scripted::<Test>(script.clone(), STEP);
    picture(&app, "navigator-search");
    let shown = texts(&app);
    assert!(
        shown
            .iter()
            .any(|t| t.contains(" km") || t.contains(" m ·")),
        "a route summary: {shown:?}"
    );

    // A long press on the map plans a route there; Drive follows it.
    script.extend(long_press((860.0, 150.0)));
    script.extend(frames(90));
    script.extend(click(drive));
    script.extend(frames(150));
    // A drive asks for frames until it arrives: stop where the script does.
    let redraws = script
        .iter()
        .filter(|e| matches!(e, RawEvent::RedrawRequested { .. }))
        .count();
    let app = drive_scripted_frames::<Test>(script, STEP, redraws as u32);
    picture(&app, "navigator-drive");
    let shown = texts(&app);
    assert!(
        shown.iter().any(|t| t.contains("to go")),
        "the drive's progress: {shown:?}"
    );
    assert!(
        shown.iter().any(|t| ["↑", "↖", "↗", "←", "→", "↶", "↻"]
            .iter()
            .any(|a| t.starts_with(a) && t.ends_with(" m"))),
        "a turn banner: {shown:?}"
    );
}
