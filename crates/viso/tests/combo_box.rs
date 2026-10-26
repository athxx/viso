//! Combo boxes driven through the facade: typing filters the list and
//! highlights a match, Enter and a click choose, Escape and leaving put the
//! value back over a closed set, free text is taken as typed, and the field
//! names the highlighted option as its active descendant.
#![cfg(feature = "headless")]

use std::cell::RefCell;
use std::time::Duration;

use viso::__test_support::{DrivenApp, drive_scripted};
use viso::platform::{
    KeyCode as Key, Modifiers as RawModifiers, PointerButtons as RawButtons,
    PointerPhase as RawPhase, RawEvent, RawKey, RawPointer, RawText, WindowId,
};
use viso::prelude::*;
use viso::ui::{Axis, FlexStyle, Inset, NodeId, NodeStore, Role, Sizing, StateValue};
use viso::widgets::combo_box;

const STEP: Duration = Duration::from_millis(16);
const WIN: WindowId = WindowId(1);

thread_local! {
    static HEARD: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

fn heard() -> Vec<String> {
    HEARD.with(|h| std::mem::take(&mut *h.borrow_mut()))
}

fn hear(what: String) {
    HEARD.with(|h| h.borrow_mut().push(what));
}

/// A closed set of fruits at 100, 100, Cherry chosen; a free-text city box
/// below it.
struct ComboApp;

impl Application for ComboApp {
    fn new(cx: &mut AppCx) -> Self {
        cx.load_font(&include_bytes!("../../text/tests/fixtures/DejaVuSans-subset.ttf")[..]);
        ComboApp
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
                axis: Axis::Column,
                padding: Inset {
                    left: 100.0,
                    top: 100.0,
                    ..Inset::default()
                },
                gap: 200.0,
                ..FlexStyle::default()
            },
            |cx| {
                combo_box(["Apple", "Banana", "Cherry", "Date", "Elderberry"])
                    .selected(2)
                    .label("Fruit")
                    .on_pick(|_, i| hear(format!("pick {i}")))
                    .build(cx);
                combo_box(["Berlin", "Bern", "Boston"])
                    .free_text(true)
                    .label("City")
                    .on_pick(|_, i| hear(format!("city {i}")))
                    .on_text(|_, t| hear(format!("text {t}")))
                    .build(cx);
            },
        );
    }
}

fn redraw() -> RawEvent {
    RawEvent::RedrawRequested { window: WIN }
}

fn mouse(x: f32, y: f32, buttons: RawButtons, phase: RawPhase) -> RawEvent {
    RawEvent::Pointer(RawPointer::mouse(
        WIN,
        f64::from(x),
        f64::from(y),
        buttons,
        RawModifiers::default(),
        phase,
    ))
}

fn click(at: (f32, f32)) -> Vec<RawEvent> {
    vec![
        mouse(at.0, at.1, RawButtons::NONE, RawPhase::Moved),
        mouse(at.0, at.1, RawButtons::PRIMARY, RawPhase::Down),
        mouse(at.0, at.1, RawButtons::PRIMARY, RawPhase::Up),
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

fn run(steps: &[Vec<RawEvent>]) -> DrivenApp<ComboApp> {
    heard();
    let mut script = vec![redraw(), redraw()];
    for step in steps {
        script.extend(step.iter().cloned());
    }
    script.push(redraw());
    drive_scripted::<ComboApp>(script, STEP)
}

/// A combo box's field box, its text field, its panel and its options.
struct Parts {
    field: NodeId,
    input: NodeId,
    panel: NodeId,
    options: Vec<NodeId>,
}

fn parts(app: &DrivenApp<ComboApp>, which: usize) -> Parts {
    let store = app.store();
    let wrapper = store
        .arena()
        .children(app.root().unwrap())
        .nth(which)
        .unwrap();
    let mut kids = store.arena().children(wrapper);
    let (field, panel) = (kids.next().unwrap(), kids.next().unwrap());
    let input = store.arena().children(field).next().unwrap();
    let viewport = store.arena().children(panel).next().unwrap();
    let list = store.arena().children(viewport).next().unwrap();
    let options = store
        .arena()
        .children(list)
        .filter(|&k| store.semantics(k).map(|s| s.role) == Some(Role::ListOption))
        .collect();
    Parts {
        field,
        input,
        panel,
        options,
    }
}

fn middle(store: &NodeStore, id: NodeId) -> (f32, f32) {
    let r = store.world(id);
    (r.x + r.w / 2.0, r.y + r.h / 2.0)
}

fn lit(app: &DrivenApp<ComboApp>, row: NodeId) -> bool {
    let look = app
        .store()
        .interaction_style(row)
        .expect("an option has a look");
    matches!(
        look.hover_cell.and_then(|c| app.states().get(c)),
        Some(StateValue::Bool(true))
    )
}

/// The options showing.
fn shown(app: &DrivenApp<ComboApp>, p: &Parts) -> Vec<usize> {
    (0..p.options.len())
        .filter(|&i| !app.store().hidden(p.options[i]))
        .collect()
}

fn fields() -> ((f32, f32), (f32, f32)) {
    let app = run(&[]);
    let store = app.store();
    (
        middle(store, parts(&app, 0).input),
        middle(store, parts(&app, 1).input),
    )
}

#[test]
fn a_first_press_opens_the_whole_list_on_the_value() {
    let (fruit, _) = fields();
    let app = run(&[click(fruit)]);
    let store = app.store();
    let p = parts(&app, 0);
    assert!(!store.hidden(p.panel), "open");
    assert_eq!(
        store.focused(),
        Some(p.input),
        "the focus stays in the field"
    );
    assert_eq!(app.focused_text(), Some("Cherry"));
    assert_eq!(shown(&app, &p), [0, 1, 2, 3, 4]);
    assert!(lit(&app, p.options[2]));
    assert_eq!(store.active_descendant(p.input), Some(p.options[2]));
    assert_eq!(
        store.semantics(p.input).map(|s| s.role),
        Some(Role::EditableComboBox)
    );
    assert_eq!(
        store.semantic_state(p.input).and_then(|s| s.expanded),
        Some(true)
    );
    let panel = store.world(p.panel);
    let field = store.world(p.field);
    assert!(panel.y >= field.y + field.h, "below the field");
    assert!(panel.w >= field.w - 0.5, "as wide as the field");
}

#[test]
fn typing_filters_and_enter_chooses_the_match() {
    let (fruit, _) = fields();
    let app = run(&[click(fruit), typed("e")]);
    let p = parts(&app, 0);
    assert_eq!(app.focused_text(), Some("e"), "the selected text replaced");
    assert_eq!(shown(&app, &p), [0, 2, 3, 4], "the labels holding an e");
    assert!(lit(&app, p.options[4]), "the first starting with it");
    let app = run(&[click(fruit), typed("e"), key(Key::Down), key(Key::Enter)]);
    let p = parts(&app, 0);
    assert_eq!(heard(), ["pick 0"], "Down wraps from Elderberry to Apple");
    assert_eq!(app.focused_text(), Some("Apple"));
    assert!(app.store().hidden(p.panel));
    assert_eq!(app.store().active_descendant(p.input), None);
    // The next open lists everything again.
    let app = run(&[click(fruit), typed("e"), key(Key::Enter), key(Key::Down)]);
    assert_eq!(shown(&app, &parts(&app, 0)).len(), 5);
}

#[test]
fn a_closed_set_takes_no_text_of_its_own() {
    let (fruit, city) = fields();
    let app = run(&[click(fruit), typed("zz")]);
    let p = parts(&app, 0);
    assert!(shown(&app, &p).is_empty(), "nothing matches");
    assert!(!app.store().hidden(p.panel), "the list says so");
    let app = run(&[click(fruit), typed("zz"), key(Key::Enter)]);
    assert!(heard().is_empty(), "Enter chooses nothing");
    assert_eq!(app.focused_text(), Some("zz"), "the text stays to edit");
    let app = run(&[click(fruit), typed("ban"), key(Key::Escape)]);
    assert_eq!(app.focused_text(), Some("Cherry"), "Escape puts it back");
    assert!(app.store().hidden(parts(&app, 0).panel));
    // The focus leaving puts it back too.
    let app = run(&[click(fruit), typed("ban"), click(city)]);
    let store = app.store();
    assert_eq!(store.focused(), Some(parts(&app, 1).input));
    assert!(store.hidden(parts(&app, 0).panel));
    let app = run(&[click(fruit), typed("ban"), click(city), click(fruit)]);
    assert_eq!(app.focused_text(), Some("Cherry"));
    assert!(heard().is_empty());
}

#[test]
fn tab_chooses_the_highlight_and_moves_on() {
    let (fruit, _) = fields();
    let app = run(&[click(fruit), typed("ban"), key(Key::Tab)]);
    assert_eq!(heard(), ["pick 1"]);
    assert_eq!(app.store().focused(), Some(parts(&app, 1).input));
    assert!(app.store().hidden(parts(&app, 0).panel));
}

#[test]
fn a_click_on_an_option_chooses_it() {
    let (fruit, _) = fields();
    let probe = run(&[click(fruit)]);
    let date = middle(probe.store(), parts(&probe, 0).options[3]);
    let app = run(&[click(fruit), click(date)]);
    assert_eq!(heard(), ["pick 3"]);
    assert_eq!(app.focused_text(), Some("Date"));
    assert_eq!(app.store().focused(), Some(parts(&app, 0).input));
}

#[test]
fn free_text_is_taken_as_typed_and_suggestions_wait_for_an_arrow() {
    let (_, city) = fields();
    let app = run(&[click(city), typed("Be")]);
    let p = parts(&app, 1);
    assert_eq!(shown(&app, &p), [0, 1]);
    assert!(
        p.options.iter().all(|&o| !lit(&app, o)),
        "nothing highlighted"
    );
    run(&[click(city), typed("Be"), key(Key::Enter)]);
    assert_eq!(heard(), ["text Be"]);
    run(&[
        click(city),
        typed("Be"),
        key(Key::Down),
        key(Key::Down),
        key(Key::Enter),
    ]);
    assert_eq!(heard(), ["city 1", "text Bern"]);
    let app = run(&[click(city), typed("Paris")]);
    assert!(app.store().hidden(parts(&app, 1).panel), "no match: closed");
    // Leaving takes the text.
    let (fruit, _) = fields();
    run(&[click(city), typed("Rome"), click(fruit)]);
    assert_eq!(heard(), ["text Rome"]);
}
