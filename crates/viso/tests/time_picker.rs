//! Time pickers driven through the facade: the field shows the time as
//! spin-button segments, AM or PM on a twelve-hour clock and none on a
//! twenty-four-hour one, placeholders before a time is set; Up and Down step
//! the focused segment round its range, the minute by its step, held to the
//! bounds; Left and Right move between segments; typed digits set a segment
//! and move on once no digit could follow; `a` and `p` set AM or PM; the
//! clock runs as the locale's does unless it is told.
#![cfg(feature = "headless")]

use std::cell::RefCell;
use std::time::Duration;

use viso::__test_support::{DrivenApp, drive_scripted};
use viso::platform::{
    KeyCode as Key, Modifiers as RawModifiers, PointerButtons as RawButtons,
    PointerPhase as RawPhase, RawEvent, RawKey, RawPointer, RawText, WindowId,
};
use viso::prelude::*;
use viso::ui::calendar::{HourCycle, Time};
use viso::ui::{Axis, Component, FlexStyle, Inset, NodeId, NodeStore, Role, Size};
use viso::widgets::time_picker;

const STEP: Duration = Duration::from_millis(16);
const WIN: WindowId = WindowId(1);

thread_local! {
    static HEARD: RefCell<Vec<Time>> = const { RefCell::new(Vec::new()) };
}

fn heard() -> Vec<Time> {
    HEARD.with(|h| std::mem::take(&mut *h.borrow_mut()))
}

fn time(h: u32, m: u32) -> Time {
    Time::new(h, m, 0).unwrap()
}

/// A twelve-hour alarm at 7:30 AM by fives between 6:00 and 20:00, a
/// twenty-four-hour one to the second with no time set, and one on the
/// locale's clock.
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
                gap: 20.0,
                ..FlexStyle::default()
            },
            |cx| {
                time_picker("Alarm")
                    .value(time(7, 30))
                    .hour_cycle(HourCycle::H12)
                    .minute_step(5)
                    .min(time(6, 0))
                    .max(time(20, 0))
                    .on_change(|_, t| HEARD.with(|h| h.borrow_mut().push(t)))
                    .build(cx);
                time_picker("Lap")
                    .hour_cycle(HourCycle::H24)
                    .seconds(true)
                    .build(cx);
                time_picker("Local").value(time(18, 5)).build(cx);
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

fn run(steps: &[Vec<RawEvent>]) -> DrivenApp<PickerApp> {
    heard();
    let mut script = vec![redraw(), redraw()];
    for step in steps {
        script.extend(step.iter().cloned());
    }
    script.push(redraw());
    drive_scripted::<PickerApp>(script, STEP)
}

/// A picker's field and its segments, the separators passed over.
fn parts(store: &NodeStore, root: NodeId, which: usize) -> (NodeId, Vec<NodeId>) {
    let field = store.arena().children(root).nth(which).unwrap();
    let segs = store.arena().children(field).step_by(2).collect();
    (field, segs)
}

fn middle(store: &NodeStore, id: NodeId) -> (f32, f32) {
    let r = store.world(id);
    (r.x + r.w / 2.0, r.y + r.h / 2.0)
}

/// What a segment shows, as it announces it: `--` before a time is set.
fn shown(store: &NodeStore, id: NodeId) -> String {
    store
        .semantics(id)
        .and_then(|s| s.value.clone())
        .unwrap_or_else(|| "--".to_owned())
}

/// The first picker's segments as they show.
fn alarm(app: &DrivenApp<PickerApp>) -> Vec<String> {
    let store = app.store();
    let (_, segs) = parts(store, app.root().unwrap(), 0);
    segs.iter().map(|&s| shown(store, s)).collect()
}

fn layout() -> (Vec<NodeId>, DrivenApp<PickerApp>) {
    let app = run(&[]);
    let (_, segs) = parts(app.store(), app.root().unwrap(), 0);
    (segs, app)
}

#[test]
fn the_field_shows_the_time_as_spin_buttons() {
    let (segs, app) = layout();
    let store = app.store();
    let (field, _) = parts(store, app.root().unwrap(), 0);
    let sem = store.semantics(field).unwrap();
    assert_eq!(
        (sem.role, sem.label.as_deref()),
        (Role::Group, Some("Alarm"))
    );
    assert_eq!(alarm(&app), ["7", "30", "AM"]);
    let hour = store.semantics(segs[0]).unwrap();
    assert_eq!(
        (hour.role, hour.label.as_deref()),
        (Role::SpinButton, Some("Hours"))
    );
    let state = store.semantic_state(segs[0]).unwrap();
    assert_eq!((state.value, state.range), (Some(7.0), Some((1.0, 12.0))));
    assert_eq!(
        store.semantics(segs[2]).unwrap().value.as_deref(),
        Some("AM")
    );
    // No time: placeholders, and no AM or PM on a twenty-four-hour clock.
    let (_, lap) = parts(store, app.root().unwrap(), 1);
    assert_eq!(lap.len(), 4);
    let lap_shown: Vec<String> = lap[..3].iter().map(|&s| shown(store, s)).collect();
    assert_eq!(lap_shown, ["--", "--", "--"]);
    assert!(store.hidden(lap[3]));
    assert_eq!(store.semantic_state(lap[0]).unwrap().value, None);
    assert!(store.focusable(segs[1]));
}

#[test]
fn up_and_down_step_round_the_range_held_to_the_bounds() {
    let (segs, app) = layout();
    let store = app.store();
    let hour = click(middle(store, segs[0]));
    let app = run(&[hour.clone(), key(Key::Up)]);
    assert_eq!(heard(), [time(8, 30)]);
    assert_eq!(app.store().focused(), Some(segs[0]));
    let app = run(&[hour.clone(), key(Key::Down), key(Key::Down)]);
    assert_eq!(alarm(&app), ["6", "00", "AM"], "held to 6:00");
    assert_eq!(heard(), [time(6, 30), time(6, 0)]);
    // The minute by fives, the period by Up.
    let minute = click(middle(store, segs[1]));
    let app = run(&[minute.clone(), key(Key::Up), key(Key::Up)]);
    assert_eq!(alarm(&app), ["7", "40", "AM"]);
    let app = run(&[minute.clone(), key(Key::End)]);
    assert_eq!(alarm(&app), ["7", "55", "AM"]);
    let period = click(middle(store, segs[2]));
    let app = run(&[period.clone(), key(Key::Up)]);
    assert_eq!(alarm(&app), ["7", "30", "PM"]);
    let app = run(&[period, key(Key::Up), key(Key::Up)]);
    assert_eq!(alarm(&app), ["7", "30", "AM"]);
    assert!(heard().ends_with(&[time(19, 30), time(7, 30)]));
}

#[test]
fn left_and_right_move_between_the_segments() {
    let (segs, app) = layout();
    let hour = click(middle(app.store(), segs[0]));
    let app = run(&[hour.clone(), key(Key::Right)]);
    assert_eq!(app.store().focused(), Some(segs[1]));
    let app = run(&[
        hour.clone(),
        key(Key::Right),
        key(Key::Right),
        key(Key::Right),
    ]);
    assert_eq!(app.store().focused(), Some(segs[2]), "the last holds");
    let app = run(&[hour.clone(), key(Key::Right), key(Key::Left)]);
    assert_eq!(app.store().focused(), Some(segs[0]));
    let app = run(&[hour, key(Key::Tab)]);
    assert_eq!(app.store().focused(), Some(segs[1]), "each is a Tab stop");
}

#[test]
fn typed_digits_set_a_segment_and_move_on() {
    let (segs, app) = layout();
    let hour = click(middle(app.store(), segs[0]));
    let app = run(&[hour.clone(), typed("9")]);
    assert_eq!(alarm(&app), ["9", "30", "AM"]);
    assert_eq!(app.store().focused(), Some(segs[1]), "no hour starts 9_");
    // `1` waits for a second digit; the minute holds through the bound.
    let app = run(&[hour.clone(), typed("1")]);
    assert_eq!(app.store().focused(), Some(segs[0]));
    let app = run(&[hour.clone(), typed("1"), typed("1")]);
    assert_eq!(alarm(&app), ["11", "30", "AM"]);
    assert_eq!(app.store().focused(), Some(segs[1]));
    let app = run(&[hour.clone(), typed("10"), typed("45")]);
    assert_eq!(alarm(&app), ["10", "45", "AM"]);
    assert_eq!(app.store().focused(), Some(segs[2]));
    let app = run(&[hour, typed("p")]);
    assert_eq!(alarm(&app), ["7", "30", "AM"], "letters are the period's");
    let period = click(middle(app.store(), segs[2]));
    let app = run(&[period.clone(), typed("p")]);
    assert_eq!(alarm(&app), ["7", "30", "PM"]);
    let app = run(&[period, typed("P"), typed("a")]);
    assert_eq!(alarm(&app), ["7", "30", "AM"]);
}

#[test]
fn a_time_set_from_none_starts_at_midnight() {
    let app = run(&[]);
    let (_, lap) = parts(app.store(), app.root().unwrap(), 1);
    let second = click(middle(app.store(), lap[2]));
    let app = run(&[second, key(Key::Up)]);
    let store = app.store();
    let lap_shown: Vec<String> = lap[..3].iter().map(|&s| shown(store, s)).collect();
    assert_eq!(lap_shown, ["00", "00", "01"]);
}

#[test]
fn the_clock_runs_as_the_locales_does() {
    let app = run(&[]);
    let store = app.store();
    let (_, local) = parts(store, app.root().unwrap(), 2);
    let cycle = viso::ui::calendar::hour_cycle(&app.states().env().environment().locale);
    let hour = shown(store, local[0]);
    match cycle {
        HourCycle::H12 => {
            assert_eq!(hour, "6");
            assert!(!store.hidden(local[2]));
        }
        HourCycle::H24 => {
            assert_eq!(hour, "18");
            assert!(store.hidden(local[2]));
        }
    }
}
