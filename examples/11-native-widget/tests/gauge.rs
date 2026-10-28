//! The gauge, a Rust widget of a native package, mounts in a `view!`: it
//! shows the level the view binds, and a press on it writes the level back
//! and runs the view's handler of its own event.

use std::time::Duration;

use viso::__test_support::{DrivenApp, drive_scripted};
use viso::platform::{
    Modifiers as RawModifiers, PointerButtons as RawButtons, PointerPhase as RawPhase, RawEvent,
    RawPointer, WindowId,
};
use viso::ui::{NodeId, NodeStore, Role};
use viso_example_11_native_widget::Panel;

const STEP: Duration = Duration::from_millis(16);
const WIN: WindowId = WindowId(1);

fn settle() -> Vec<RawEvent> {
    (0..8)
        .flat_map(|_| [RawEvent::Wakeup, RawEvent::RedrawRequested { window: WIN }])
        .collect()
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

/// The shown node under `id` that `role` and `label` name.
fn find(store: &NodeStore, id: NodeId, role: Role, label: &str) -> Option<NodeId> {
    if store
        .semantics(id)
        .is_some_and(|s| s.role == role && s.label.as_deref() == Some(label))
    {
        return Some(id);
    }
    let kids: Vec<_> = store.arena().children(id).collect();
    kids.into_iter()
        .find_map(|kid| find(store, kid, role, label))
}

/// The gauge's track and the value it tells.
fn gauge(app: &DrivenApp<Panel>) -> (NodeId, String) {
    let store = app.store();
    let track = find(store, app.root().unwrap(), Role::Slider, "Volume").expect("the gauge shows");
    let value = store.semantics(track).and_then(|s| s.value.clone());
    (track, value.unwrap_or_default())
}

#[test]
fn the_gauge_shows_the_bound_level_and_a_press_writes_it_back() {
    let app = drive_scripted::<Panel>(settle(), STEP);
    let (track, value) = gauge(&app);
    assert_eq!(value, "25%", "it shows the state's level");
    let r = app.store().world(track);
    assert!(r.w > 0.0, "the box lays it out: {r:?}");

    let at = (r.x + r.w * 0.75, r.y + r.h * 0.5);
    let mut script = settle();
    script.extend([
        mouse(at, RawButtons::NONE, RawPhase::Moved),
        mouse(at, RawButtons::PRIMARY, RawPhase::Down),
        mouse(at, RawButtons::PRIMARY, RawPhase::Up),
    ]);
    script.extend(settle());
    let app = drive_scripted::<Panel>(script, STEP);
    let (_, value) = gauge(&app);
    assert_eq!(
        value, "75%",
        "the press wrote the level back, which it shows"
    );
    let pressed = find(app.store(), app.root().unwrap(), Role::Label, "1");
    assert!(pressed.is_some(), "the view's handler of `pressed` ran");
}
