//! The `.vs` shell drives the Rust router: its buttons push and pop, a
//! system back action asks its `back_requested` first, a deep link resets
//! the stack, and the inbox keeps its state under every page.

use std::time::Duration;

use viso::__test_support::{DrivenApp, drive_scripted};
use viso::platform::{
    KeyCode as Key, Modifiers as RawModifiers, PointerButtons as RawButtons,
    PointerPhase as RawPhase, RawEvent, RawKey, RawPointer, WindowId,
};
use viso::ui::{NodeId, NodeStore, Role};
use viso_example_04_navigation::Mail;

const STEP: Duration = Duration::from_millis(16);
const WIN: WindowId = WindowId(1);

/// A step of the script.
enum Do {
    /// Clicks the shown control labelled so.
    Click(&'static str),
    Back,
    Open(&'static str),
}

fn redraw() -> RawEvent {
    RawEvent::RedrawRequested { window: WIN }
}

/// Frames for half a second, past any page transition.
fn settle() -> Vec<RawEvent> {
    (0..32).flat_map(|_| [RawEvent::Wakeup, redraw()]).collect()
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

fn key(code: Key, pressed: bool) -> RawEvent {
    RawEvent::Key(RawKey {
        window: WIN,
        code,
        pressed,
        repeat: false,
        modifiers: RawModifiers::default(),
    })
}

/// The shown node under `id` that says `label` as `role`.
fn find(store: &NodeStore, id: NodeId, role: Role, label: &str) -> Option<NodeId> {
    if store.hidden(id) {
        return None;
    }
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

/// Runs the app through `steps`; each click finds its control in a run of
/// the steps before it.
fn run(steps: &[Do]) -> DrivenApp<Mail> {
    let mut script = settle();
    for (at, step) in steps.iter().enumerate() {
        match step {
            Do::Click(label) => {
                let before = run(&steps[..at]);
                let store = before.store();
                let root = before.root().unwrap();
                let id = find(store, root, Role::Button, label)
                    .or_else(|| find(store, root, Role::CheckBox, label))
                    .unwrap_or_else(|| panic!("no `{label}` shows after {at} steps"));
                let r = store.world(id);
                let middle = (r.x + r.w * 0.5, r.y + r.h * 0.5);
                script.extend([
                    mouse(middle, RawButtons::NONE, RawPhase::Moved),
                    mouse(middle, RawButtons::PRIMARY, RawPhase::Down),
                    redraw(),
                    mouse(middle, RawButtons::PRIMARY, RawPhase::Up),
                ]);
            }
            Do::Back => script.extend([key(Key::Back, true), key(Key::Back, false)]),
            Do::Open(url) => script.push(RawEvent::OpenUrl {
                url: (*url).to_owned(),
            }),
        }
        script.extend(settle());
    }
    drive_scripted::<Mail>(script, STEP)
}

/// Whether a text saying `label` shows.
fn shows(app: &DrivenApp<Mail>, label: &str) -> bool {
    find(app.store(), app.root().unwrap(), Role::Label, label).is_some()
}

fn back_shows(app: &DrivenApp<Mail>) -> bool {
    find(app.store(), app.root().unwrap(), Role::Button, "Back").is_some()
}

#[test]
fn the_shell_pushes_and_pops_and_the_inbox_keeps_its_draft() {
    let app = run(&[]);
    assert!(shows(&app, "Inbox") && shows(&app, "0 drafts"));
    assert!(!back_shows(&app), "the root page has nothing to go back to");

    let drafted = [Do::Click("New draft"), Do::Click("Message 5")];
    let app = run(&drafted);
    assert!(
        shows(&app, "Message 5"),
        "the title follows the pushed route"
    );
    assert!(shows(&app, "Message #5"), "the message page shows");
    assert!(back_shows(&app));

    let app = run(&[
        Do::Click("New draft"),
        Do::Click("Message 5"),
        Do::Click("Back"),
    ]);
    assert!(shows(&app, "Inbox") && !back_shows(&app));
    assert!(shows(&app, "1 drafts"), "the inbox kept its draft");
}

#[test]
fn a_system_back_asks_the_shell_which_may_keep_the_page() {
    let kept = [
        Do::Click("New draft"),
        Do::Click("Message 5"),
        Do::Click("Editing"),
        Do::Back,
    ];
    let app = run(&kept);
    assert!(shows(&app, "Message #5"), "editing keeps the page");
    assert!(shows(&app, "1 kept"));

    let popped = [
        Do::Click("New draft"),
        Do::Click("Message 5"),
        Do::Click("Editing"),
        Do::Back,
        Do::Click("Editing"),
        Do::Back,
    ];
    let app = run(&popped);
    assert!(shows(&app, "Inbox") && shows(&app, "1 drafts"));
    assert!(shows(&app, "1 kept"), "the second back was not kept");
}

#[test]
fn a_deep_link_opens_over_the_inbox_and_the_shell_follows() {
    let linked = [
        Do::Click("New draft"),
        Do::Open("mail://settings"),
        Do::Open("mail://message/9"),
    ];
    let app = run(&linked);
    assert!(shows(&app, "Message 9") && shows(&app, "Message #9"));
    assert!(back_shows(&app));

    let back = [
        Do::Click("New draft"),
        Do::Open("mail://settings"),
        Do::Open("mail://message/9"),
        Do::Click("Back"),
    ];
    let app = run(&back);
    assert!(shows(&app, "Inbox") && shows(&app, "1 drafts"));
    assert!(
        !back_shows(&app),
        "the link left the settings page off the stack"
    );
}
