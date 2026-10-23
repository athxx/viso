//! Access keys driven through the facade: an `&` in a caption underlines its
//! letter while Alt is held, and Alt with the letter clicks the button, flips
//! the check, selects the option or focuses the field the caption names.
#![cfg(feature = "headless")]

use std::cell::RefCell;
use std::time::Duration;

use viso::__test_support::{DrivenApp, drive_scripted, set_alt_access};
use viso::platform::{KeyCode as Key, Modifiers as RawModifiers, RawEvent, RawKey, WindowId};
use viso::prelude::*;
use viso::render::Primitive;
use viso::ui::{
    Axis, Component, Content, FlexStyle, Inset, NodeId, NodeStore, Role, Size, paint_tree,
};
use viso::widgets::{button, checkbox, labelled, radio_group, text_input};

const STEP: Duration = Duration::from_millis(16);
const WIN: WindowId = WindowId(1);

thread_local! {
    static HEARD: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

fn heard(what: String) {
    HEARD.with(|h| h.borrow_mut().push(what));
}

fn take_heard() -> Vec<String> {
    HEARD.with(|h| std::mem::take(&mut *h.borrow_mut()))
}

/// A button, a check, a radio group and a labelled field, each with an
/// access key.
struct Form;

impl Application for Form {
    fn new(cx: &mut AppCx) -> Self {
        cx.load_font(&include_bytes!("../../text/tests/fixtures/DejaVuSans-subset.ttf")[..]);
        Form
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
                axis: Axis::Column,
                gap: 10.0,
                padding: Inset::all(10.0),
                size: Size::fill(),
                ..FlexStyle::default()
            },
            |cx| {
                button("&Save").on_click(|_| heard("save".into())).build(cx);
                checkbox("&Wrap")
                    .on_change(|_, on| heard(format!("wrap {on}")))
                    .build(cx);
                radio_group(["&Low", "&High"])
                    .on_change(|_, at| heard(format!("level {at}")))
                    .build(cx);
                labelled("&Name:", text_input("Name")).build(cx);
            },
        );
    }
}

fn redraw() -> RawEvent {
    RawEvent::RedrawRequested { window: WIN }
}

fn raw_key(code: Key, pressed: bool, alt: bool) -> RawEvent {
    RawEvent::Key(RawKey {
        window: WIN,
        code,
        pressed,
        repeat: false,
        modifiers: RawModifiers {
            alt,
            ..RawModifiers::default()
        },
    })
}

/// `code` pressed with Alt held.
fn alt_key(code: Key) -> Vec<RawEvent> {
    vec![
        raw_key(Key::AltLeft, true, true),
        raw_key(code, true, true),
        raw_key(code, false, true),
        raw_key(Key::AltLeft, false, false),
        redraw(),
        redraw(),
    ]
}

fn run(steps: &[Vec<RawEvent>]) -> DrivenApp<Form> {
    set_alt_access(Some(true));
    take_heard();
    let mut script = vec![redraw(), redraw()];
    for step in steps {
        script.extend(step.iter().cloned());
    }
    script.push(redraw());
    drive_scripted::<Form>(script, STEP)
}

/// The nodes of `role` under `node`, in tree order.
fn of_role(store: &NodeStore, node: NodeId, role: Role, out: &mut Vec<NodeId>) {
    if store.semantics(node).is_some_and(|s| s.role == role) {
        out.push(node);
    }
    for child in store.arena().children(node).collect::<Vec<_>>() {
        of_role(store, child, role, out);
    }
}

fn find(app: &DrivenApp<Form>, role: Role) -> Vec<NodeId> {
    let mut out = Vec::new();
    of_role(app.store(), app.root().unwrap(), role, &mut out);
    out
}

/// The cue underlines the texts under `node` carry.
fn cues(store: &NodeStore, node: NodeId) -> usize {
    let own = match store.content_payload(node) {
        Some(Content::Text { ink, .. }) => ink.as_ref().map_or(0, |i| i.cues.len()),
        _ => 0,
    };
    own + store
        .arena()
        .children(node)
        .map(|c| cues(store, c))
        .sum::<usize>()
}

/// The solid quads the scene draws.
fn quads(app: &DrivenApp<Form>) -> usize {
    let mut out = Vec::new();
    paint_tree(app.store(), app.root().unwrap(), &mut out);
    out.iter()
        .filter(|p| matches!(p, Primitive::Quad(q) if q.border.width == 0.0 && q.color.a > 0.0))
        .count()
}

#[test]
fn captions_lose_their_marks_and_underline_their_keys_while_alt_is_held() {
    let app = run(&[]);
    let store = app.store();
    let save = find(&app, Role::Button)[0];
    assert_eq!(
        store.semantics(save).and_then(|s| s.label.as_deref()),
        Some("Save")
    );
    assert_eq!(store.access_key(save), Some('s'));
    let root = app.root().unwrap();
    assert_eq!(cues(store, root), 5, "one per caption");
    let rest = quads(&app);

    let held = run(&[vec![raw_key(Key::AltLeft, true, true), redraw()]]);
    assert!(held.store().access_cues());
    assert_eq!(quads(&held), rest + 5, "the cues show while Alt is held");

    let let_go = run(&[vec![
        raw_key(Key::AltLeft, true, true),
        raw_key(Key::AltLeft, false, false),
        redraw(),
    ]]);
    assert_eq!(quads(&let_go), rest);
}

#[test]
fn alt_and_a_key_activates_the_control_it_names() {
    run(&[alt_key(Key::S)]);
    assert_eq!(take_heard(), ["save"]);

    let app = run(&[alt_key(Key::W)]);
    assert_eq!(take_heard(), ["wrap true"]);
    assert_eq!(app.store().focused(), Some(find(&app, Role::CheckBox)[0]));

    let app = run(&[alt_key(Key::H)]);
    assert_eq!(take_heard(), ["level 1"]);
    assert_eq!(app.store().focused(), Some(find(&app, Role::Radio)[1]));
}

#[test]
fn a_caption_s_key_focuses_the_field_it_names() {
    let app = run(&[alt_key(Key::N)]);
    let field = find(&app, Role::TextField)[0];
    assert_eq!(app.store().focused(), Some(field));
    assert_eq!(
        app.store()
            .semantics(field)
            .and_then(|s| s.label.as_deref()),
        Some("Name:"),
        "the caption names it"
    );
}
