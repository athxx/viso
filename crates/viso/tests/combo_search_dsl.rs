//! A view's `ComboBox` and `SearchField`: the framework builds their parts
//! inside them at their values; a chosen option or free text taken writes
//! the bound state back and runs `changed`, a value from outside shows, new
//! options build the parts again, a search's query is reported once the
//! typing rests, its `suggestions` show below it, and Enter submits.
#![cfg(feature = "headless")]

use std::time::Duration;

use viso::__test_support::{DrivenApp, drive_scripted};
use viso::platform::{
    KeyCode as Key, Modifiers as RawModifiers, PointerButtons as RawButtons,
    PointerPhase as RawPhase, RawEvent, RawKey, RawPointer, RawText, WindowId,
};
use viso::prelude::*;
use viso::ui::{NodeId, NodeStore, Role, WindowConfig};

const STEP: Duration = Duration::from_millis(16);
const WIN: WindowId = WindowId(1);

viso::component! {
    Finder {
        state fruit = "Cherry";
        state picks = 0;
        state city = "";
        state more = false;
        state query = "";
        state hints: List<String> = [];
        state searches = 0;
        view {
            Row {
                width: 900dp;
                height: 500dp;
                padding: 20dp;
                gap: 20dp;
                Column {
                    gap: 20dp;
                    ComboBox {
                        label: "Fruit";
                        options: ["Apple", "Banana", "Cherry", "Date", "Elderberry"];
                        bind value <=> fruit;
                        on changed(e) { picks += 1; }
                    }
                    ComboBox {
                        label: "City";
                        placeholder: "Where";
                        free_text: true;
                        options: if more { ["Bern", "Berlin", "Oslo"] } else { ["Bern", "Berlin"] };
                        bind value <=> city;
                    }
                    SearchField {
                        label: "Find";
                        width: 300dp;
                        debounce: 200ms;
                        suggestions: hints;
                        bind value <=> query;
                        on changed(e) { hints = if e.value == "" { [] } else { [e.value + "a", e.value + "b"] }; }
                        on submitted { searches += 1; }
                    }
                }
                Column {
                    Text { width: 20dp; height: 20dp; on click { fruit = "Date"; } }
                    Text { width: 20dp; height: 20dp; on click { more = !more; } }
                    Text { width: 20dp; height: 20dp; visible: picks > 0; }
                    Text { width: 20dp; height: 20dp; visible: searches > 0; }
                    Text { width: 20dp; height: 20dp; visible: city == "Paris"; }
                    Text { width: 20dp; height: 20dp; visible: query == "be"; }
                }
            }
        }
    }
}

struct FinderApp;

impl Application for FinderApp {
    fn new(cx: &mut AppCx) -> Self {
        cx.load_font(&include_bytes!("../../text/tests/fixtures/DejaVuSans-subset.ttf")[..]);
        FinderApp
    }

    fn window_config(&self) -> WindowConfig {
        WindowConfig {
            caption: false,
            ..Default::default()
        }
    }

    fn build(&mut self, cx: &mut BuildCx<'_>) {
        Finder::build(cx);
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

/// Frames for `ms` milliseconds, the loop woken for each.
fn wait(ms: u64) -> Vec<RawEvent> {
    (0..ms.div_ceil(16))
        .flat_map(|_| [RawEvent::Wakeup, redraw()])
        .collect()
}

fn run(steps: &[Vec<RawEvent>]) -> DrivenApp<FinderApp> {
    let mut script = vec![redraw(), redraw()];
    for step in steps {
        script.extend(step.iter().cloned());
    }
    script.push(redraw());
    drive_scripted::<FinderApp>(script, STEP)
}

/// The fields' column, and the column of toggles and marks.
fn columns(app: &DrivenApp<FinderApp>) -> (NodeId, NodeId) {
    let store = app.store();
    let mut kids = store.arena().children(app.root().unwrap());
    (kids.next().unwrap(), kids.next().unwrap())
}

/// The `which`th node of the fields' column.
fn node(app: &DrivenApp<FinderApp>, which: usize) -> NodeId {
    let store = app.store();
    store.arena().children(columns(app).0).nth(which).unwrap()
}

/// The `which`th of the toggles and marks.
fn control(app: &DrivenApp<FinderApp>, which: usize) -> NodeId {
    let store = app.store();
    store.arena().children(columns(app).1).nth(which).unwrap()
}

/// The text field inside node `node`'s parts.
fn input(store: &NodeStore, node: NodeId) -> NodeId {
    let mut stack = vec![node];
    while let Some(id) = stack.pop() {
        if store
            .semantics(id)
            .is_some_and(|s| matches!(s.role, Role::EditableComboBox | Role::SearchField))
        {
            return id;
        }
        stack.extend(store.arena().children(id));
    }
    panic!("no field inside {node:?}")
}

/// The labels of the options or suggestions showing in node `node`'s list.
fn listed(store: &NodeStore, node: NodeId) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![node];
    while let Some(id) = stack.pop() {
        if store.hidden(id) {
            continue;
        }
        if let Some(s) = store.semantics(id)
            && s.role == Role::ListOption
        {
            out.push(s.label.clone().unwrap_or_default());
        }
        stack.extend(
            store
                .arena()
                .children(id)
                .collect::<Vec<_>>()
                .into_iter()
                .rev(),
        );
    }
    out
}

fn middle(store: &NodeStore, id: NodeId) -> (f32, f32) {
    let r = store.world(id);
    (r.x + r.w * 0.5, r.y + r.h * 0.5)
}

fn text(app: &DrivenApp<FinderApp>, id: NodeId) -> String {
    app.field_text(id).unwrap_or_default().to_owned()
}

fn shows(app: &DrivenApp<FinderApp>, which: usize) -> bool {
    !app.store().hidden(control(app, which))
}

#[test]
fn their_parts_stand_at_their_values_named_by_their_labels() {
    let app = run(&[]);
    let store = app.store();
    let fruit = input(store, node(&app, 0));
    let city = input(store, node(&app, 1));
    let find = input(store, node(&app, 2));
    let said = |id| {
        let s = store.semantics(id).unwrap();
        (s.role, s.label.clone())
    };
    assert_eq!(said(fruit), (Role::EditableComboBox, Some("Fruit".into())));
    assert_eq!(said(city), (Role::EditableComboBox, Some("City".into())));
    assert_eq!(said(find), (Role::SearchField, Some("Find".into())));
    assert_eq!(store.semantics(node(&app, 0)), None, "the field speaks");
    assert_eq!(text(&app, fruit), "Cherry");
    assert_eq!(text(&app, city), "");
    let r = store.world(node(&app, 2));
    assert!((r.w - 300.0).abs() < 0.5, "a sized field fills it: {r:?}");
    assert!((store.world(find).w - 300.0).abs() < 40.0);
    assert!(!shows(&app, 2) && !shows(&app, 3));
}

#[test]
fn a_chosen_option_writes_the_state_back_and_a_value_from_outside_shows() {
    let app = run(&[]);
    let field = middle(app.store(), input(app.store(), node(&app, 0)));
    let app = run(&[click(field), typed("ban"), key(Key::Enter)]);
    let fruit = input(app.store(), node(&app, 0));
    assert_eq!(text(&app, fruit), "Banana");
    assert!(shows(&app, 2), "changed ran");
    let set = middle(app.store(), control(&app, 0));
    let app = run(&[click(field), typed("ban"), key(Key::Enter), click(set)]);
    assert_eq!(text(&app, input(app.store(), node(&app, 0))), "Date");
}

#[test]
fn free_text_is_taken_and_new_options_build_the_parts_again() {
    let app = run(&[]);
    let field = middle(app.store(), input(app.store(), node(&app, 1)));
    let app = run(&[click(field), key(Key::Down)]);
    assert_eq!(listed(app.store(), node(&app, 1)), ["Bern", "Berlin"]);
    let app = run(&[click(field), typed("Paris"), key(Key::Enter)]);
    assert!(shows(&app, 4), "the text is taken");
    let more = middle(app.store(), control(&app, 1));
    let steps = [click(field), typed("Paris"), key(Key::Enter), click(more)];
    let app = run(&steps);
    let city = input(app.store(), node(&app, 1));
    assert_eq!(text(&app, city), "Paris", "the value holds through");
    let mut steps = steps.to_vec();
    steps.extend([click(field), key(Key::Down)]);
    let app = run(&steps);
    assert_eq!(
        listed(app.store(), node(&app, 1)),
        ["Bern", "Berlin", "Oslo"]
    );
}

#[test]
fn a_query_is_reported_once_the_typing_rests_and_enter_submits() {
    let app = run(&[]);
    let field = middle(app.store(), input(app.store(), node(&app, 2)));
    let app = run(&[click(field), typed("b"), typed("e")]);
    assert!(!shows(&app, 5), "not before the typing rests");
    let app = run(&[click(field), typed("b"), typed("e"), wait(300)]);
    assert!(shows(&app, 5), "the query reported");
    assert_eq!(listed(app.store(), node(&app, 2)), ["bea", "beb"]);
    assert!(!shows(&app, 3));
    let app = run(&[click(field), typed("be"), key(Key::Enter)]);
    assert!(shows(&app, 5), "Enter reports what waits");
    assert!(shows(&app, 3), "and submits");
}
