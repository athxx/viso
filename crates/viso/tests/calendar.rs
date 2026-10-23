//! Calendars driven through the facade: a month shows as six weeks from the
//! weeks' first day under column heads, the selected day on a disc and today
//! ringed, matching a blessed golden; a click picks a day, one of the next
//! month shows that month; the arrows, Home/End and Page Up/Down move the
//! focus through days, weeks, months and years, turning the month they
//! reach; Enter picks; bounds and refused days cannot be picked and the keys
//! pass over them; the buttons turn the month; the weeks start where the
//! locale's do.
#![cfg(feature = "headless")]

use std::cell::RefCell;
use std::path::PathBuf;
use std::time::Duration;

use viso::__test_support::{DrivenApp, drive_scripted};
use viso::gpu::{CpuRaster, GpuBackend, RawWindowHandle};
use viso::platform::{
    KeyCode as Key, Modifiers as RawModifiers, PointerButtons as RawButtons,
    PointerPhase as RawPhase, RawEvent, RawKey, RawPointer, WindowId,
};
use viso::prelude::*;
use viso::render::{Rect, Renderer, Rgba};
use viso::ui::calendar::{Date, Weekday};
use viso::ui::{
    Axis, BindingTable, BoxStyle, Component, FlexStyle, Inset, NodeId, NodeStore, Role,
    SemanticProjector, Size, StateStore, TextEdits, VirtualLists, paint_tree,
};
use viso::widgets::{ViewStyle, calendar, view};

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

/// A Monday-first calendar at 2026-10-24, picking only from October 5 to
/// November 20 and never a Sunday, then one keeping the locale's week.
struct CalendarApp;

impl Application for CalendarApp {
    fn new(cx: &mut AppCx) -> Self {
        cx.load_font(&include_bytes!("../../text/tests/fixtures/DejaVuSans-subset.ttf")[..]);
        CalendarApp
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
                gap: 20.0,
                ..FlexStyle::default()
            },
            |cx| {
                calendar("Due")
                    .value(date(2026, 10, 24))
                    .today(date(2026, 10, 21))
                    .first_weekday(Weekday::Monday)
                    .min(date(2026, 10, 5))
                    .max(date(2026, 11, 20))
                    .unavailable(|d| d.weekday() == Weekday::Sunday)
                    .on_change(|_, d| HEARD.with(|h| h.borrow_mut().push(d)))
                    .build(cx);
                calendar("Local").value(date(2026, 10, 24)).build(cx);
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

fn run(steps: &[Vec<RawEvent>]) -> DrivenApp<CalendarApp> {
    heard();
    let mut script = vec![redraw(), redraw()];
    for step in steps {
        script.extend(step.iter().cloned());
    }
    script.push(redraw());
    drive_scripted::<CalendarApp>(script, STEP)
}

/// A calendar's parts: its buttons, its table, column heads, weeks and
/// days.
struct Parts {
    previous: NodeId,
    next: NodeId,
    table: NodeId,
    heads: Vec<NodeId>,
    weeks: Vec<NodeId>,
    days: Vec<NodeId>,
}

fn parts(store: &NodeStore, root: NodeId, which: usize) -> Parts {
    let cal = store.arena().children(root).nth(which).unwrap();
    let mut kids = store.arena().children(cal);
    let (header, table) = (kids.next().unwrap(), kids.next().unwrap());
    let header: Vec<NodeId> = store.arena().children(header).collect();
    let rows: Vec<NodeId> = store.arena().children(table).collect();
    Parts {
        previous: header[0],
        next: header[2],
        table,
        heads: store.arena().children(rows[0]).collect(),
        weeks: rows[1..].to_vec(),
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

fn layout() -> (Parts, DrivenApp<CalendarApp>) {
    let app = run(&[]);
    let p = parts(app.store(), app.root().unwrap(), 0);
    (p, app)
}

/// The day focused, by its name.
fn focused(app: &DrivenApp<CalendarApp>) -> String {
    label(app.store(), app.store().focused().expect("a focus"))
}

#[test]
fn a_month_shows_six_weeks_from_their_first_day_under_its_heads() {
    let (p, app) = layout();
    let store = app.store();
    assert_eq!(p.weeks.len(), 6);
    assert_eq!(p.days.len(), 42);
    assert_eq!(label(store, p.table), "October 2026");
    assert_eq!(store.semantics(p.table).unwrap().role, Role::Table);
    let heads: Vec<String> = p.heads.iter().map(|&h| label(store, h)).collect();
    assert_eq!(heads[0], "Monday");
    assert_eq!(heads[6], "Sunday");
    assert_eq!(
        store.semantics(p.heads[0]).unwrap().role,
        Role::ColumnHeader
    );
    // October 2026 starts on a Thursday: the grid from Monday 28 September.
    assert_eq!(label(store, p.days[0]), "Monday, September 28, 2026");
    let picked = &p.days[3 * 7 + 5];
    assert_eq!(label(store, *picked), "Saturday, October 24, 2026");
    assert_eq!(store.semantic_state(*picked).unwrap().selected, Some(true));
    assert_eq!(
        store.semantic_state(p.days[0]).unwrap().selected,
        Some(false)
    );
    let today = p.days[3 * 7 + 2];
    assert_eq!(
        store.semantics(today).unwrap().hint.as_deref(),
        Some("Today")
    );
    let week = store.days(p.weeks[3]).unwrap();
    assert_eq!((week.selected, week.today), (Some(5), Some(2)));
    let sunday = p.days[6];
    assert_eq!(
        store.semantics(sunday).unwrap().hint.as_deref(),
        Some("Unavailable")
    );
    assert!(store.is_roving(p.table), "the days are one Tab stop");
}

#[test]
fn a_click_picks_a_day_and_one_of_the_next_month_shows_it() {
    let (p, app) = layout();
    let store = app.store();
    let tue = middle(store, p.days[2 * 7 + 1]);
    let app = run(&[click(tue)]);
    assert_eq!(heard(), [date(2026, 10, 13)]);
    assert_eq!(focused(&app), "Tuesday, October 13, 2026");
    // November 2, the first Monday after October.
    let nov = middle(store, p.days[5 * 7]);
    let app = run(&[click(nov)]);
    assert_eq!(heard(), [date(2026, 11, 2)]);
    let store = app.store();
    assert_eq!(label(store, p.table), "November 2026");
    assert_eq!(focused(&app), "Monday, November 2, 2026");
    // Neither a Sunday nor a day before the bound is picked.
    let app = run(&[
        click(middle(store, p.days[6])),
        click(middle(store, p.days[0])),
    ]);
    assert!(heard().is_empty());
    assert_eq!(label(app.store(), p.table), "October 2026");
}

#[test]
fn the_keys_move_through_days_weeks_and_months_and_enter_picks() {
    let (p, app) = layout();
    let picked = middle(app.store(), p.days[3 * 7 + 5]);
    let start = click(picked);
    let app = run(&[start.clone(), key(Key::Left)]);
    assert_eq!(focused(&app), "Friday, October 23, 2026");
    let app = run(&[start.clone(), key(Key::Right)]);
    assert_eq!(
        focused(&app),
        "Monday, October 26, 2026",
        "a Sunday is passed over"
    );
    let app = run(&[start.clone(), key(Key::Down), key(Key::Down)]);
    assert_eq!(focused(&app), "Saturday, November 7, 2026");
    assert_eq!(
        label(app.store(), p.table),
        "November 2026",
        "the month it reaches"
    );
    let app = run(&[start.clone(), key(Key::Home)]);
    assert_eq!(focused(&app), "Monday, October 19, 2026");
    let app = run(&[start.clone(), key(Key::End)]);
    assert_eq!(
        focused(&app),
        "Saturday, October 24, 2026",
        "Sunday refused, End holds"
    );
    let app = run(&[start.clone(), key(Key::PageDown)]);
    assert_eq!(
        focused(&app),
        "Friday, November 20, 2026",
        "held to the bound"
    );
    let app = run(&[start.clone(), key_with(Key::PageUp, true)]);
    assert_eq!(
        focused(&app),
        "Monday, October 5, 2026",
        "a year back holds to the bound"
    );
    heard();
    let app = run(&[start.clone(), key(Key::Left), key(Key::Enter)]);
    assert_eq!(heard(), [date(2026, 10, 23)]);
    let state = app.store().semantic_state(p.days[3 * 7 + 4]).unwrap();
    assert_eq!(state.selected, Some(true));
}

#[test]
fn the_buttons_turn_the_month_within_the_bounds() {
    let (p, app) = layout();
    let store = app.store();
    assert!(!store.enabled(p.previous), "nothing before October to pick");
    assert!(store.enabled(p.next));
    let app = run(&[click(middle(store, p.next))]);
    let store = app.store();
    assert_eq!(label(store, p.table), "November 2026");
    assert!(store.enabled(p.previous));
    assert!(!store.enabled(p.next), "nothing after November");
    assert_eq!(
        store.semantics(p.next).map(|s| (s.role, s.label.clone())),
        Some((Role::Button, Some("Next month".to_owned())))
    );
}

#[test]
fn the_weeks_start_where_the_locales_do() {
    let app = run(&[]);
    let store = app.store();
    let p = parts(store, app.root().unwrap(), 1);
    let locale = viso::ui::calendar::first_weekday(&app.states().env().environment().locale);
    let names = [
        "Monday",
        "Tuesday",
        "Wednesday",
        "Thursday",
        "Friday",
        "Saturday",
        "Sunday",
    ];
    assert_eq!(label(store, p.heads[0]), names[locale.index() as usize]);
}

const GW: u32 = 340;
const GH: u32 = 320;
const CLEAR: [f32; 4] = [0.0, 0.0, 0.0, 1.0];
const TOL: u8 = 2;

#[test]
fn a_calendar_marks_its_picked_day_and_today_and_matches_golden() {
    let mut gpu = CpuRaster::new();
    let surface = gpu.create_surface(RawWindowHandle::Headless, GW, GH);
    let format = gpu.surface_format(surface);
    let mut renderer = Renderer::new(&mut gpu, format);
    let mut store = NodeStore::new();
    let mut states = StateStore::new();
    let mut bindings = BindingTable::new();
    let mut lists = VirtualLists::new();
    let mut text_edits = TextEdits::new();
    let mut projectors = SemanticProjector::new();
    let scene = view(ViewStyle {
        padding: Inset::all(16.0),
        size: Size::fill(),
        background: BoxStyle::solid(Rgba::new(0.97, 0.97, 0.98, 1.0)),
        ..Default::default()
    })
    .children(|cx| {
        calendar("Due")
            .value(date(2026, 10, 24))
            .today(date(2026, 10, 21))
            .first_weekday(Weekday::Monday)
            .week_numbers(true)
            .build(cx);
    });
    let root = {
        let mut cx = viso::ui::BuildCx::with_reactive(
            &mut store,
            &mut states,
            &mut bindings,
            &mut lists,
            &mut text_edits,
            &mut projectors,
        );
        scene.build(&mut cx);
        cx.root().unwrap()
    };
    store.layout(
        root,
        Rect::new(0.0, 0.0, GW as f32, GH as f32),
        &mut Vec::new(),
    );
    let mut primitives = Vec::new();
    paint_tree(&store, root, &mut primitives);
    renderer.upload(&mut gpu, &primitives);
    renderer.submit(&mut gpu, surface, CLEAR, [GW as f32, GH as f32]);
    let actual = gpu.read_pixels_bgra8(surface);
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/calendar.bgra8");
    if std::env::var("BLESS").is_ok() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &actual).unwrap();
        return;
    }
    let expected = std::fs::read(&path)
        .unwrap_or_else(|_| panic!("missing golden {}; run with BLESS=1", path.display()));
    assert_eq!(actual.len(), expected.len());
    let worst = actual
        .iter()
        .zip(&expected)
        .map(|(a, e)| a.abs_diff(*e))
        .max()
        .unwrap_or(0);
    assert!(
        worst <= TOL,
        "golden mismatch: max per-channel diff {worst}"
    );
}
