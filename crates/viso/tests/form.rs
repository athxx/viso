//! A form driven through the facade: a field shows what is wrong with its
//! value only once the focus leaves it, then follows each change; a required
//! control is announced so, and a refused one as invalid naming its error
//! line; submitting shows every field's error, focuses the first refused and
//! runs the submit handler only when all pass, Enter in a text field
//! submitting too; a fieldset is a group named by its legend.
#![cfg(feature = "headless")]

use std::cell::Cell;
use std::time::Duration;

use viso::__test_support::{DrivenApp, drive_scripted};
use viso::platform::{
    KeyCode as Key, Modifiers as RawModifiers, PointerButtons as RawButtons,
    PointerPhase as RawPhase, RawEvent, RawKey, RawPointer, RawText, WindowId,
};
use viso::prelude::*;
use viso::ui::{Axis, Component, FlexStyle, NodeId, NodeStore, Role, Size, Validity};
use viso::widgets::{Form, button, checkbox, field, fieldset, text_input};

const STEP: Duration = Duration::from_millis(16);
const WIN: WindowId = WindowId(1);

thread_local! {
    static SUBMITTED: Cell<u32> = const { Cell::new(0) };
}

fn submitted() -> u32 {
    SUBMITTED.with(|s| s.replace(0))
}

struct FormApp;

impl Application for FormApp {
    fn new(cx: &mut AppCx) -> Self {
        cx.load_font(&include_bytes!("../../text/tests/fixtures/DejaVuSans-subset.ttf")[..]);
        FormApp
    }

    fn window_config(&self) -> viso::ui::WindowConfig {
        viso::ui::WindowConfig {
            caption: false,
            ..Default::default()
        }
    }

    fn build(&mut self, cx: &mut BuildCx<'_>) {
        let form = Form::new();
        form.on_submit(|_| SUBMITTED.with(|s| s.set(s.get() + 1)));
        let fields = form.clone();
        cx.flex(
            FlexStyle {
                axis: Axis::Column,
                gap: 16.0,
                size: Size::fixed(360.0, 400.0),
                ..FlexStyle::default()
            },
            move |cx| {
                let gathered = fields.clone();
                fieldset("Account", move |cx| {
                    field("&Email", text_input("Email"))
                        .required("Enter an email")
                        .rule(|value| match value.text() {
                            Some(text) if !text.contains('@') => Err("Needs an @".into()),
                            _ => Ok(()),
                        })
                        .help("We never share it")
                        .form(&gathered)
                        .build(cx);
                    field("Terms", checkbox("I accept"))
                        .required("Accept the terms")
                        .form(&gathered)
                        .build(cx);
                })
                .build(cx);
                let submit = fields.clone();
                button("Send")
                    .on_click(move |ev| {
                        submit.submit(ev);
                    })
                    .build(cx);
            },
        );
    }
}

fn redraw() -> RawEvent {
    RawEvent::RedrawRequested { window: WIN }
}

fn click(at: (f32, f32)) -> Vec<RawEvent> {
    let mouse = |buttons, phase| {
        RawEvent::Pointer(RawPointer::mouse(
            WIN,
            f64::from(at.0),
            f64::from(at.1),
            buttons,
            RawModifiers::default(),
            phase,
        ))
    };
    vec![
        mouse(RawButtons::NONE, RawPhase::Moved),
        mouse(RawButtons::PRIMARY, RawPhase::Down),
        mouse(RawButtons::PRIMARY, RawPhase::Up),
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

fn run(steps: &[Vec<RawEvent>]) -> DrivenApp<FormApp> {
    submitted();
    let mut script = vec![redraw(), redraw()];
    for step in steps {
        script.extend(step.iter().cloned());
    }
    script.push(redraw());
    drive_scripted::<FormApp>(script, STEP)
}

/// The first node under `id`, depth first, with `role` and `label`.
fn find(store: &NodeStore, id: NodeId, role: Role, label: &str) -> Option<NodeId> {
    if store
        .semantics(id)
        .is_some_and(|s| s.role == role && s.label.as_deref() == Some(label))
    {
        return Some(id);
    }
    store
        .arena()
        .children(id)
        .find_map(|c| find(store, c, role, label))
}

fn node(app: &DrivenApp<FormApp>, role: Role, label: &str) -> NodeId {
    find(app.store(), app.root().unwrap(), role, label)
        .unwrap_or_else(|| panic!("no {role:?} named {label}"))
}

fn middle(app: &DrivenApp<FormApp>, id: NodeId) -> (f32, f32) {
    let r = app.store().world(id);
    (r.x + r.w / 2.0, r.y + r.h / 2.0)
}

/// What the field around `control` shows is wrong, if it shows anything.
fn shown_error(app: &DrivenApp<FormApp>, control: NodeId) -> Option<String> {
    let store = app.store();
    let validity = store.validity(control);
    let error = validity.error?;
    assert!(validity.invalid);
    assert!(!store.hidden(error), "the error line shows");
    store.semantics(error).and_then(|s| s.label.clone())
}

fn controls(app: &DrivenApp<FormApp>) -> (NodeId, NodeId, NodeId) {
    (
        node(app, Role::TextField, "Email"),
        node(app, Role::CheckBox, "Terms"),
        node(app, Role::Button, "Send"),
    )
}

#[test]
fn a_fresh_form_announces_what_is_required_and_shows_no_error() {
    let app = run(&[]);
    let (email, terms, _) = controls(&app);
    let store = app.store();
    for control in [email, terms] {
        assert_eq!(
            store.validity(control),
            Validity {
                required: true,
                invalid: false,
                error: None
            }
        );
    }
    let group = node(&app, Role::Group, "Account");
    assert!(
        store.arena().children(group).count() >= 3,
        "legend and fields"
    );
}

#[test]
fn an_error_shows_once_the_focus_leaves_and_then_follows_each_change() {
    let start = run(&[]);
    let (email, _, _) = controls(&start);
    let at = middle(&start, email);
    // Still filling it in: nothing shows.
    let app = run(&[click(at), typed("ab")]);
    assert_eq!(shown_error(&app, email), None);
    // The focus leaves it: what is wrong shows.
    let app = run(&[click(at), typed("ab"), key(Key::Tab)]);
    assert_eq!(shown_error(&app, email).as_deref(), Some("Needs an @"));
    // Back in it, a fix hides the error at once.
    let app = run(&[
        click(at),
        typed("ab"),
        key(Key::Tab),
        click(at),
        typed("@x"),
    ]);
    assert_eq!(shown_error(&app, email), None);
    let validity = app.store().validity(email);
    assert!(validity.required && !validity.invalid);
}

#[test]
fn submitting_shows_every_error_focuses_the_first_and_runs_only_when_all_pass() {
    let start = run(&[]);
    let (email, terms, send) = controls(&start);
    let (email_at, terms_at, send_at) = (
        middle(&start, email),
        middle(&start, terms),
        middle(&start, send),
    );
    let app = run(&[click(send_at)]);
    assert_eq!(submitted(), 0);
    assert_eq!(shown_error(&app, email).as_deref(), Some("Enter an email"));
    assert_eq!(
        shown_error(&app, terms).as_deref(),
        Some("Accept the terms")
    );
    assert_eq!(app.store().focused(), Some(email), "the first refused");

    let app = run(&[
        click(email_at),
        typed("a@b"),
        click(terms_at),
        click(send_at),
    ]);
    assert_eq!(submitted(), 1);
    assert_eq!(shown_error(&app, email), None);
    assert_eq!(shown_error(&app, terms), None);
}

#[test]
fn enter_in_a_text_field_submits_the_form() {
    let start = run(&[]);
    let (email, terms, _) = controls(&start);
    let (email_at, terms_at) = (middle(&start, email), middle(&start, terms));
    let _ = run(&[click(email_at), typed("a@b"), key(Key::Enter)]);
    assert_eq!(submitted(), 0, "the terms are not accepted yet");
    let _ = run(&[
        click(terms_at),
        click(email_at),
        typed("a@b"),
        key(Key::Enter),
    ]);
    assert_eq!(submitted(), 1);
}
