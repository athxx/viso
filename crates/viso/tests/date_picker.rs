//! Date pickers driven through the facade: the field shows the date or a
//! placeholder; a press or Space opens the calendar below it with the
//! picked day focused; a pick closes it, reports and focuses the field
//! again; Escape and a press outside close it unchanged; Tab goes round the
//! calendar's buttons and its days while it is open.
#![cfg(feature = "headless")]

use std::cell::RefCell;
use std::time::Duration;

use viso::__test_support::{DrivenApp, drive_scripted};
use viso::platform::{
    KeyCode as Key, Modifiers as RawModifiers, PointerButtons as RawButtons,
    PointerPhase as RawPhase, RawEvent, RawKey, RawPointer, WindowId,
};
use viso::prelude::*;
use viso::ui::calendar::{Date, Weekday};
use viso::ui::{Axis, Component, FlexStyle, Inset, NodeId, NodeStore, Role, Size};
use viso::widgets::date_picker;

const STEP: Duration = Duration::from_millis(16);
const WIN: WindowId = WindowId(1);

thread_local! {
    static HEARD: RefCell<Vec<Date>> = const { RefCell::new(Vec::new()) };
}

fn heard() -> Vec<Date> {
    HEARD.with(|h| std::mem::take(&mut *h.borrow_mut()))
}

fn date(y: i32, m: u32, d: u32) -> Date {
    Date::new(y, m, d).unwrap()
}

/// A picker at 2026-10-24, Monday first, and an empty one beside it.
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
                size: Size::fill(),
                axis: Axis::Row,
                padding: Inset::all(20.0),
                gap: 300.0,
                ..FlexStyle::default()
            },
            |cx| {
                date_picker("Start")
                    .value(date(2026, 10, 24))
                    .first_weekday(Weekday::Monday)
                    .on_change(|_, d| HEARD.with(|h| h.borrow_mut().push(d)))
                    .build(cx);
                date_picker("End")
                    .placeholder("No date")
                    .today(date(2026, 10, 21))
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

fn click(at: (f32, f32)) -> Vec<RawEvent> {
    vec![
        mouse(at, RawButtons::NONE, RawPhase::Moved),
        mouse(at, RawButtons::PRIMARY, RawPhase::Down),
        mouse(at, RawButtons::PRIMARY, RawPhase::Up),
        redraw(),
        redraw(),
    ]
}

fn key_with(code: Key, shift: bool) -> Vec<RawEvent> {
    let raw = |pressed| {
        RawEvent::Key(RawKey {
            window: WIN,
            code,
            pressed,
            repeat: false,
            modifiers: RawModifiers {
                shift,
                ..RawModifiers::default()
            },
        })
    };
    vec![raw(true), raw(false), redraw(), redraw()]
}

fn key(code: Key) -> Vec<RawEvent> {
    key_with(code, false)
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

/// A picker's field, panel, calendar buttons and days.
struct Parts {
    field: NodeId,
    panel: NodeId,
    previous: NodeId,
    next: NodeId,
    days: Vec<NodeId>,
}

fn parts(store: &NodeStore, root: NodeId, which: usize) -> Parts {
    let wrapper = store.arena().children(root).nth(which).unwrap();
    let mut kids = store.arena().children(wrapper);
    let (field, panel) = (kids.next().unwrap(), kids.next().unwrap());
    let cal = store.arena().first_child(panel).unwrap();
    let mut cal_kids = store.arena().children(cal);
    let (header, table) = (cal_kids.next().unwrap(), cal_kids.next().unwrap());
    let header: Vec<NodeId> = store.arena().children(header).collect();
    let rows: Vec<NodeId> = store.arena().children(table).collect();
    Parts {
        field,
        panel,
        previous: header[0],
        next: header[2],
        days: rows[1..]
            .iter()
            .flat_map(|&w| store.arena().children(w))
            .collect(),
    }
}

fn middle(store: &NodeStore, id: NodeId) -> (f32, f32) {
    let r = store.world(id);
    (r.x + r.w / 2.0, r.y + r.h / 2.0)
}

fn label(store: &NodeStore, id: NodeId) -> String {
    store.semantics(id).and_then(|s| s.label.clone()).unwrap()
}

fn layout() -> (Parts, DrivenApp<PickerApp>) {
    let app = run(&[]);
    let p = parts(app.store(), app.root().unwrap(), 0);
    (p, app)
}

fn expanded(store: &NodeStore, field: NodeId) -> Option<bool> {
    store.semantic_state(field).and_then(|s| s.expanded)
}

#[test]
fn the_field_shows_the_date_and_a_press_opens_the_calendar_at_it() {
    let (p, app) = layout();
    let store = app.store();
    let sem = store.semantics(p.field).unwrap();
    assert_eq!(sem.role, Role::ComboBox);
    assert_eq!(sem.label.as_deref(), Some("Start"));
    assert_eq!(sem.value.as_deref(), Some("Saturday, October 24, 2026"));
    assert!(store.hidden(p.panel));
    assert_eq!(expanded(store, p.field), Some(false));
    let empty = parts(store, app.root().unwrap(), 1);
    assert_eq!(store.semantics(empty.field).unwrap().value, None);

    let app = run(&[click(middle(store, p.field))]);
    let store = app.store();
    assert!(!store.hidden(p.panel));
    assert_eq!(expanded(store, p.field), Some(true));
    assert_eq!(
        label(store, store.focused().unwrap()),
        "Saturday, October 24, 2026",
        "the picked day has the focus"
    );
    assert_eq!(store.semantics(p.panel).unwrap().role, Role::Dialog);
    let panel = store.world(p.panel);
    let field = store.world(p.field);
    assert!(panel.y >= field.y + field.h - 0.5, "below the field");
}

#[test]
fn a_pick_closes_reports_and_focuses_the_field() {
    let (p, app) = layout();
    let open = click(middle(app.store(), p.field));
    let app = run(std::slice::from_ref(&open));
    let day = middle(app.store(), p.days[2 * 7 + 1]);
    let app = run(&[open.clone(), click(day)]);
    let store = app.store();
    assert_eq!(heard(), [date(2026, 10, 13)]);
    assert!(store.hidden(p.panel));
    assert_eq!(store.focused(), Some(p.field));
    assert_eq!(
        store.semantics(p.field).unwrap().value.as_deref(),
        Some("Tuesday, October 13, 2026")
    );
    // The keys: Space opens, an arrow moves, Enter picks.
    let app = run(&[
        open.clone(),
        key(Key::Escape),
        key(Key::Space),
        key(Key::Right),
        key(Key::Enter),
    ]);
    assert_eq!(heard(), [date(2026, 10, 25)]);
    assert!(app.store().hidden(p.panel));
    assert_eq!(app.store().focused(), Some(p.field));
}

#[test]
fn escape_and_a_press_outside_close_it_unchanged() {
    let (p, app) = layout();
    let open = click(middle(app.store(), p.field));
    let app = run(&[open.clone(), key(Key::Down), key(Key::Escape)]);
    assert!(app.store().hidden(p.panel));
    assert_eq!(app.store().focused(), Some(p.field));
    assert!(heard().is_empty());
    let shown = run(std::slice::from_ref(&open));
    let r = shown.store().world(p.panel);
    let outside = (r.x + r.w + 40.0, r.y + r.h * 0.5);
    let app = run(&[open.clone(), click(outside)]);
    assert!(app.store().hidden(p.panel));
    assert!(heard().is_empty());
}

#[test]
fn tab_goes_round_the_buttons_and_the_days_while_open() {
    let (p, app) = layout();
    let open = click(middle(app.store(), p.field));
    let app = run(&[open.clone(), key(Key::Tab)]);
    assert_eq!(app.store().focused(), Some(p.previous));
    let app = run(&[open.clone(), key(Key::Tab), key(Key::Tab)]);
    assert_eq!(app.store().focused(), Some(p.next));
    let app = run(&[open.clone(), key(Key::Tab), key(Key::Tab), key(Key::Tab)]);
    let store = app.store();
    assert_eq!(
        label(store, store.focused().unwrap()),
        "Saturday, October 24, 2026"
    );
    let app = run(&[open.clone(), key_with(Key::Tab, true)]);
    assert_eq!(app.store().focused(), Some(p.next));
    assert!(!app.store().hidden(p.panel), "still open");
}
