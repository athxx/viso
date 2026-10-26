//! Selects driven through the facade: a pop-up opening over its field with
//! the chosen option level with it, a drop-down opening below, choosing by
//! click, by drag and by keys, type-ahead, a long list that scrolls to the
//! highlight, and the semantics of the field, the list and its options.
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
use viso::widgets::{SelectKind, SelectStyle, select, select_option};

const STEP: Duration = Duration::from_millis(16);
const WIN: WindowId = WindowId(1);

const FRUITS: [&str; 5] = ["Apple", "Banana", "Cherry", "Date", "Elderberry"];

thread_local! {
    static CHANGES: RefCell<Vec<(u8, usize)>> = const { RefCell::new(Vec::new()) };
}

fn changes() -> Vec<(u8, usize)> {
    CHANGES.with(|c| std::mem::take(&mut *c.borrow_mut()))
}

/// A pop-up of fruits at 100, 100, Cherry chosen; a drop-down of thirty
/// numbers below it with a short list, Disabled 2 among them.
struct SelectApp;

impl Application for SelectApp {
    fn new(cx: &mut AppCx) -> Self {
        cx.load_font(&include_bytes!("../../text/tests/fixtures/DejaVuSans-subset.ttf")[..]);
        SelectApp
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
                gap: 120.0,
                ..FlexStyle::default()
            },
            |cx| {
                select(FRUITS)
                    .selected(2)
                    .label("Fruit")
                    .style(SelectStyle {
                        kind: SelectKind::PopUp,
                        ..SelectStyle::default()
                    })
                    .on_change(|_, i| CHANGES.with(|c| c.borrow_mut().push((0, i))))
                    .build(cx);
                select((0..30).map(|i| select_option(format!("Number {i}")).enabled(i != 2)))
                    .placeholder("Pick")
                    .style(SelectStyle {
                        kind: SelectKind::DropDown,
                        max_height: 160.0,
                        ..SelectStyle::default()
                    })
                    .on_change(|_, i| CHANGES.with(|c| c.borrow_mut().push((1, i))))
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

fn press(code: Key, alt: bool) -> Vec<RawEvent> {
    let raw = |pressed| {
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
    };
    vec![raw(true), raw(false), redraw(), redraw()]
}

fn key(code: Key) -> Vec<RawEvent> {
    press(code, false)
}

fn typed(text: &str) -> Vec<RawEvent> {
    vec![
        RawEvent::Text(RawText {
            window: WIN,
            text: text.to_owned(),
        }),
        redraw(),
    ]
}

/// Frames for `ms` milliseconds, a wakeup before each.
fn wait(ms: u64) -> Vec<RawEvent> {
    (0..ms.div_ceil(16) + 1)
        .flat_map(|_| [RawEvent::Wakeup, redraw()])
        .collect()
}

fn run(steps: &[Vec<RawEvent>]) -> DrivenApp<SelectApp> {
    changes();
    let mut script = vec![redraw(), redraw()];
    for step in steps {
        script.extend(step.iter().cloned());
    }
    script.push(redraw());
    drive_scripted::<SelectApp>(script, STEP)
}

/// A select's field, its label, its panel, its viewport and its options.
struct Parts {
    field: NodeId,
    text: NodeId,
    panel: NodeId,
    viewport: NodeId,
    options: Vec<NodeId>,
}

fn parts(app: &DrivenApp<SelectApp>, which: usize) -> Parts {
    let store = app.store();
    let wrapper = store
        .arena()
        .children(app.root().unwrap())
        .nth(which)
        .unwrap();
    let mut kids = store.arena().children(wrapper);
    let (field, panel) = (kids.next().unwrap(), kids.next().unwrap());
    let column = store.arena().children(field).next().unwrap();
    let text = store.arena().children(column).nth(1).unwrap();
    let viewport = store.arena().children(panel).next().unwrap();
    let list = store.arena().children(viewport).next().unwrap();
    Parts {
        field,
        text,
        panel,
        viewport,
        options: store.arena().children(list).collect(),
    }
}

fn middle(store: &NodeStore, id: NodeId) -> (f32, f32) {
    let r = store.world(id);
    (r.x + r.w / 2.0, r.y + r.h / 2.0)
}

fn shown(store: &NodeStore, text: NodeId) -> String {
    store
        .semantics(text)
        .and_then(|s| s.label.clone())
        .unwrap_or_default()
}

fn lit(app: &DrivenApp<SelectApp>, row: NodeId) -> bool {
    let look = app
        .store()
        .interaction_style(row)
        .expect("an option has a look");
    matches!(
        look.hover_cell.and_then(|c| app.states().get(c)),
        Some(StateValue::Bool(true))
    )
}

/// The middle of the pop-up's field and of the drop-down's.
fn fields() -> ((f32, f32), (f32, f32)) {
    let app = run(&[]);
    let store = app.store();
    (
        middle(store, parts(&app, 0).field),
        middle(store, parts(&app, 1).field),
    )
}

#[test]
fn the_field_shows_the_choice_and_is_as_wide_as_the_widest_option() {
    let app = run(&[]);
    let store = app.store();
    let pop = parts(&app, 0);
    assert_eq!(shown(store, pop.text), "Cherry");
    assert!(store.hidden(pop.panel), "closed");
    let field = store.world(pop.field);
    let widest = store.world(pop.text).w;
    assert!(field.w >= 120.0, "at least the least width: {field:?}");
    let drop = parts(&app, 1);
    assert_eq!(shown(store, drop.text), "Pick", "the placeholder");
    // "Number 29" is wider than "Pick": the field holds room for it.
    assert!(
        store.world(drop.field).w > store.world(drop.text).w + 30.0,
        "room for the widest option"
    );
    assert!(widest > 0.0);
    assert_eq!(
        store.semantics(pop.field).map(|s| s.role),
        Some(Role::ComboBox)
    );
    assert_eq!(
        store.semantics(pop.field).and_then(|s| s.label.clone()),
        Some("Fruit".to_owned())
    );
    assert_eq!(
        store.semantic_state(pop.field).and_then(|s| s.expanded),
        Some(false)
    );
}

#[test]
fn a_pop_up_opens_over_its_field_with_the_choice_level_with_it() {
    let (pop, _) = fields();
    let app = run(&[click(pop)]);
    let store = app.store();
    let p = parts(&app, 0);
    assert!(!store.hidden(p.panel), "open");
    let (_, field_y) = middle(store, p.field);
    let (_, cherry_y) = middle(store, p.options[2]);
    assert!(
        (field_y - cherry_y).abs() < 0.5,
        "Cherry over the field: {field_y} {cherry_y}"
    );
    assert!(lit(&app, p.options[2]), "the choice highlighted");
    assert_eq!(store.focused(), Some(p.options[2]), "the focus on it");
    assert!(changes().is_empty(), "the opening click chose nothing");
    assert_eq!(
        store.semantic_state(p.field).and_then(|s| s.expanded),
        Some(true)
    );
    assert_eq!(
        store.semantics(p.panel).map(|s| s.role),
        Some(Role::ListBox)
    );
    let cherry = store.semantic_state(p.options[2]).unwrap();
    assert_eq!(cherry.selected, Some(true));
    assert_eq!(cherry.position, Some((3, 5)));
    assert_eq!(
        store.semantic_state(p.options[0]).and_then(|s| s.selected),
        Some(false)
    );
}

#[test]
fn a_click_or_a_drag_and_release_on_an_option_chooses_it() {
    let (pop, _) = fields();
    let probe = run(&[click(pop)]);
    let date = middle(probe.store(), parts(&probe, 0).options[3]);
    let app = run(&[click(pop), click(date)]);
    let p = parts(&app, 0);
    assert_eq!(changes(), [(0, 3)]);
    assert!(app.store().hidden(p.panel), "closed");
    assert_eq!(shown(app.store(), p.text), "Date");
    assert_eq!(app.store().focused(), Some(p.field), "the focus back");

    let elder = middle(probe.store(), parts(&probe, 0).options[4]);
    let app = run(&[vec![
        mouse(pop.0, pop.1, RawButtons::NONE, RawPhase::Moved),
        mouse(pop.0, pop.1, RawButtons::PRIMARY, RawPhase::Down),
        redraw(),
        redraw(),
        mouse(elder.0, elder.1, RawButtons::PRIMARY, RawPhase::Moved),
        mouse(elder.0, elder.1, RawButtons::PRIMARY, RawPhase::Up),
        redraw(),
    ]]);
    assert_eq!(changes(), [(0, 4)]);
    assert_eq!(shown(app.store(), parts(&app, 0).text), "Elderberry");
}

#[test]
fn escape_and_a_press_outside_close_it_unchanged() {
    let (pop, _) = fields();
    let app = run(&[click(pop), key(Key::Down), key(Key::Escape)]);
    let p = parts(&app, 0);
    assert!(app.store().hidden(p.panel));
    assert_eq!(shown(app.store(), p.text), "Cherry");
    assert_eq!(app.store().focused(), Some(p.field));
    let app = run(&[click(pop), key(Key::Tab)]);
    let p = parts(&app, 0);
    assert!(app.store().hidden(p.panel), "Tab closes it");
    assert_eq!(app.store().focused(), Some(p.field), "on the field");
    let app = run(&[click(pop), click((700.0, 500.0))]);
    assert!(app.store().hidden(parts(&app, 0).panel));
    assert!(changes().is_empty());
}

#[test]
fn the_keyboard_moves_the_highlight_and_chooses_in_the_open_list() {
    let (pop, _) = fields();
    let app = run(&[click(pop), key(Key::Down), key(Key::Down), key(Key::Down)]);
    let p = parts(&app, 0);
    assert!(lit(&app, p.options[4]), "stops at the last");
    let app = run(&[click(pop), key(Key::Home)]);
    assert!(lit(&app, parts(&app, 0).options[0]));
    let app = run(&[click(pop), key(Key::Up), key(Key::Enter)]);
    assert_eq!(changes(), [(0, 1)]);
    assert!(app.store().hidden(parts(&app, 0).panel));
    // A pop-up opens on Down from the field, with the choice highlighted.
    let app = run(&[click(pop), key(Key::Escape), key(Key::Down)]);
    let p = parts(&app, 0);
    assert!(!app.store().hidden(p.panel));
    assert!(lit(&app, p.options[2]), "Cherry, the choice");
    assert!(changes().is_empty());
}

#[test]
fn a_drop_down_opens_below_and_scrolls_to_the_highlight() {
    let (_, drop) = fields();
    let app = run(&[click(drop)]);
    let store = app.store();
    let d = parts(&app, 1);
    let field = store.world(d.field);
    let panel = store.world(d.panel);
    assert!(panel.y >= field.y + field.h, "below: {panel:?} {field:?}");
    assert!(panel.h <= 160.0 + 0.5, "no taller than its most: {panel:?}");
    assert!(lit(&app, d.options[0]), "none chosen: the first");
    let app = run(&[click(drop), key(Key::End)]);
    let store = app.store();
    let d = parts(&app, 1);
    assert!(lit(&app, d.options[29]));
    assert!(store.scroll(d.viewport).y > 0.0, "scrolled to the last");
    let last = store.world(d.options[29]);
    let view = store.world(d.viewport);
    assert!(
        last.y + last.h <= view.y + view.h + 0.5,
        "in view: {last:?} {view:?}"
    );
    let app = run(&[click(drop), key(Key::PageDown), key(Key::Enter)]);
    let chosen = changes();
    assert_eq!(chosen.len(), 1);
    assert!(chosen[0].1 > 2, "a page down: {chosen:?}");
    assert_eq!(
        shown(app.store(), parts(&app, 1).text),
        format!("Number {}", chosen[0].1)
    );
}

#[test]
fn a_closed_drop_down_chooses_with_the_arrows_past_disabled_options() {
    let (_, drop) = fields();
    let app = run(&[
        click(drop),
        key(Key::Escape),
        key(Key::Down),
        key(Key::Down),
        key(Key::Down),
    ]);
    let d = parts(&app, 1);
    assert!(app.store().hidden(d.panel), "still closed");
    assert_eq!(changes(), [(1, 0), (1, 1), (1, 3)], "past Number 2");
    assert_eq!(shown(app.store(), d.text), "Number 3");
    let app = run(&[click(drop), key(Key::Escape), key(Key::End)]);
    assert_eq!(changes(), [(1, 29)]);
    assert!(app.store().hidden(parts(&app, 1).panel));
    let app = run(&[click(drop), key(Key::Escape), press(Key::Down, true)]);
    assert!(
        !app.store().hidden(parts(&app, 1).panel),
        "Alt+Down opens it"
    );
}

#[test]
fn typing_goes_to_the_option_it_starts() {
    let (pop, drop) = fields();
    let app = run(&[click(pop), typed("e"), key(Key::Enter)]);
    assert_eq!(changes(), [(0, 4)]);
    assert_eq!(shown(app.store(), parts(&app, 0).text), "Elderberry");
    // Closed, typing chooses; a second later it starts again.
    run(&[
        click(drop),
        key(Key::Escape),
        typed("number 1"),
        typed("2"),
        wait(1100),
        typed("n"),
    ]);
    assert_eq!(changes(), [(1, 1), (1, 12), (1, 13)]);
}
