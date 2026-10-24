//! A splitter driven through the facade: dragging its bar re-lays the panes
//! out live under the pointer, the grab point held and the minimums kept; the
//! keys step it, send it to the ends and fold the leading pane; a pinned pane
//! keeps its extent as the window resizes while a share grows with it; the bar
//! is a focusable separator reporting where it stands.
#![cfg(feature = "headless")]

use std::cell::{Cell, RefCell};
use std::time::Duration;

use viso::__test_support::{DrivenApp, drive_scripted};
use viso::platform::{
    KeyCode as Key, Modifiers as RawModifiers, PointerButtons as RawButtons,
    PointerPhase as RawPhase, RawEvent, RawKey, RawPointer, WindowId,
};
use viso::prelude::*;
use viso::ui::{Axis, BoxStyle, Component, LeafStyle, NodeId, Role, Size};
use viso::widgets::{SplitAt, splitter};

const STEP: Duration = Duration::from_millis(16);
const WIN: WindowId = WindowId(1);
const BAR: f32 = 6.0;

thread_local! {
    static AT: Cell<SplitAt> = const { Cell::new(SplitAt::Fraction(0.5)) };
    static AXIS: Cell<Axis> = const { Cell::new(Axis::Row) };
    static HEARD: RefCell<Vec<SplitAt>> = const { RefCell::new(Vec::new()) };
}

fn heard() -> Vec<SplitAt> {
    HEARD.with(|h| std::mem::take(&mut *h.borrow_mut()))
}

/// A splitter filling the window, its leading pane at least 50 and its
/// trailing one at least 80 across.
struct SplitApp;

impl Application for SplitApp {
    fn new(_cx: &mut AppCx) -> Self {
        SplitApp
    }

    fn window_config(&self) -> viso::ui::WindowConfig {
        viso::ui::WindowConfig {
            caption: false,
            ..Default::default()
        }
    }

    fn build(&mut self, cx: &mut BuildCx<'_>) {
        let pane = |cx: &mut BuildCx<'_>| {
            cx.leaf(LeafStyle {
                size: Size::fill(),
                style: BoxStyle::NONE,
            });
        };
        splitter("Sidebar")
            .at(AT.with(Cell::get))
            .axis(AXIS.with(Cell::get))
            .min(50.0, 80.0)
            .panes(pane, pane)
            .on_change(|_, at| HEARD.with(|h| h.borrow_mut().push(at)))
            .build(cx);
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

/// A press at `from` dragged to `to` in steps, let go unless `hold`.
fn drag(from: (f32, f32), to: (f32, f32), hold: bool) -> Vec<RawEvent> {
    let mut out = vec![
        mouse(from, RawButtons::NONE, RawPhase::Moved),
        mouse(from, RawButtons::PRIMARY, RawPhase::Down),
    ];
    for k in 1..=8 {
        let t = k as f32 / 8.0;
        let at = (from.0 + (to.0 - from.0) * t, from.1 + (to.1 - from.1) * t);
        out.push(mouse(at, RawButtons::PRIMARY, RawPhase::Moved));
        out.push(redraw());
    }
    if !hold {
        out.push(mouse(to, RawButtons::NONE, RawPhase::Up));
    }
    out.extend([redraw(), redraw()]);
    out
}

fn click(at: (f32, f32)) -> Vec<RawEvent> {
    vec![
        mouse(at, RawButtons::NONE, RawPhase::Moved),
        mouse(at, RawButtons::PRIMARY, RawPhase::Down),
        mouse(at, RawButtons::NONE, RawPhase::Up),
        redraw(),
    ]
}

fn key_with(code: Key, shift: bool) -> Vec<RawEvent> {
    let modifiers = RawModifiers {
        shift,
        ..RawModifiers::default()
    };
    let raw = |pressed| {
        RawEvent::Key(RawKey {
            window: WIN,
            code,
            pressed,
            repeat: false,
            modifiers,
        })
    };
    vec![raw(true), raw(false), redraw(), redraw()]
}

fn key(code: Key) -> Vec<RawEvent> {
    key_with(code, false)
}

fn resize(width: u32, height: u32) -> Vec<RawEvent> {
    vec![
        RawEvent::Resized {
            window: WIN,
            width,
            height,
        },
        redraw(),
        redraw(),
    ]
}

fn run(at: SplitAt, axis: Axis, steps: &[Vec<RawEvent>]) -> DrivenApp<SplitApp> {
    AT.with(|c| c.set(at));
    AXIS.with(|c| c.set(axis));
    heard();
    let mut script = vec![redraw(), redraw()];
    for step in steps {
        script.extend(step.iter().cloned());
    }
    script.push(redraw());
    drive_scripted::<SplitApp>(script, STEP)
}

/// The leading pane, the bar and the trailing pane.
fn parts(app: &DrivenApp<SplitApp>) -> [NodeId; 3] {
    let store = app.store();
    let root = app.root().unwrap();
    let kids: Vec<NodeId> = store.arena().children(root).collect();
    [kids[0], kids[1], kids[2]]
}

fn width(app: &DrivenApp<SplitApp>, id: NodeId) -> f32 {
    app.store().world(id).w
}

/// The bar's middle.
fn bar_at(app: &DrivenApp<SplitApp>) -> (f32, f32) {
    let r = app.store().world(parts(app)[1]);
    (r.x + r.w / 2.0, r.y + r.h / 2.0)
}

fn close(a: f32, b: f32) -> bool {
    (a - b).abs() < 0.01
}

#[test]
fn the_bar_is_a_focusable_separator_reporting_where_it_stands() {
    let app = run(SplitAt::Fraction(0.25), Axis::Row, &[]);
    let [lead, bar, trail] = parts(&app);
    let store = app.store();
    let total = store.world(store.arena().parent(bar).unwrap()).w;
    assert!(close(width(&app, lead), (total - BAR) * 0.25));
    assert!(close(width(&app, trail), (total - BAR) * 0.75));
    let semantics = store.semantics(bar).unwrap();
    assert_eq!(
        (semantics.role, semantics.label.as_deref()),
        (Role::Separator, Some("Sidebar"))
    );
    assert!(store.focusable(bar));
    let state = store.semantic_state(bar).unwrap();
    assert_eq!((state.value, state.range), (Some(25.0), Some((0.0, 100.0))));
    let app = run(SplitAt::Start(120.0), Axis::Row, &[]);
    let [lead, bar, _] = parts(&app);
    assert_eq!(width(&app, lead), 120.0);
    let state = app.store().semantic_state(bar).unwrap();
    assert_eq!((state.value, state.range), (Some(120.0), None));
}

#[test]
fn a_drag_relays_the_panes_out_live_under_the_pointer() {
    let start = run(SplitAt::Fraction(0.5), Axis::Row, &[]);
    let from = bar_at(&start);
    let [lead, bar, trail] = parts(&start);
    let total = start
        .store()
        .world(start.store().arena().parent(bar).unwrap())
        .w;
    let free = total - BAR;
    // Held mid-drag, the panes have already moved: the bar's middle stays
    // under the pointer.
    let to = (from.0 + 100.0, from.1);
    let held = run(SplitAt::Fraction(0.5), Axis::Row, &[drag(from, to, true)]);
    assert!(
        close(bar_at(&held).0, to.0),
        "{:?} under {to:?}",
        bar_at(&held)
    );
    let lead_now = width(&held, lead);
    assert!(close(lead_now, free * 0.5 + 100.0));
    assert!(close(width(&held, trail), free - lead_now));
    let reports = heard();
    assert!(reports.len() > 1, "each step reports: {reports:?}");
    let Some(&SplitAt::Fraction(f)) = reports.last() else {
        panic!("a share stays a share: {reports:?}");
    };
    assert!(close(f * free, lead_now));
    // Let go, it stays.
    let done = run(SplitAt::Fraction(0.5), Axis::Row, &[drag(from, to, false)]);
    assert!(close(width(&done, lead), lead_now));
}

#[test]
fn a_drag_keeps_both_minimums() {
    let start = run(SplitAt::Fraction(0.5), Axis::Row, &[]);
    let from = bar_at(&start);
    let [lead, bar, trail] = parts(&start);
    let total = start
        .store()
        .world(start.store().arena().parent(bar).unwrap())
        .w;
    let app = run(
        SplitAt::Fraction(0.5),
        Axis::Row,
        &[drag(from, (2.0, from.1), false)],
    );
    assert!(close(width(&app, lead), 50.0));
    let app = run(
        SplitAt::Fraction(0.5),
        Axis::Row,
        &[drag(from, (total - 2.0, from.1), false)],
    );
    assert!(close(width(&app, trail), 80.0));
}

#[test]
fn the_keys_step_the_bar_send_it_to_the_ends_and_fold_the_leading_pane() {
    let start = run(SplitAt::Start(200.0), Axis::Row, &[]);
    let focus = click(bar_at(&start));
    let [lead, bar, trail] = parts(&start);
    let app = run(
        SplitAt::Start(200.0),
        Axis::Row,
        std::slice::from_ref(&focus),
    );
    assert_eq!(app.store().focused(), Some(bar), "a press focuses the bar");
    let app = run(
        SplitAt::Start(200.0),
        Axis::Row,
        &[focus.clone(), key(Key::Right)],
    );
    assert_eq!(width(&app, lead), 210.0);
    assert_eq!(heard(), [SplitAt::Start(210.0)]);
    let app = run(
        SplitAt::Start(200.0),
        Axis::Row,
        &[focus.clone(), key_with(Key::Left, true)],
    );
    assert_eq!(width(&app, lead), 150.0);
    let app = run(
        SplitAt::Start(200.0),
        Axis::Row,
        &[focus.clone(), key(Key::Home)],
    );
    assert_eq!(width(&app, lead), 50.0);
    let app = run(
        SplitAt::Start(200.0),
        Axis::Row,
        &[focus.clone(), key(Key::End)],
    );
    assert!(close(width(&app, trail), 80.0));
    let app = run(
        SplitAt::Start(200.0),
        Axis::Row,
        &[focus.clone(), key(Key::Enter)],
    );
    assert_eq!(width(&app, lead), 50.0, "folded");
    let app = run(
        SplitAt::Start(200.0),
        Axis::Row,
        &[focus, key(Key::Enter), key(Key::Enter)],
    );
    assert_eq!(width(&app, lead), 200.0, "and back");
}

#[test]
fn a_pinned_pane_keeps_its_extent_as_the_window_resizes_a_share_grows() {
    let app = run(SplitAt::End(150.0), Axis::Row, &[resize(900, 400)]);
    let [lead, _, trail] = parts(&app);
    assert_eq!(width(&app, trail), 150.0);
    assert!(close(width(&app, lead), 900.0 - BAR - 150.0));
    let app = run(SplitAt::Fraction(0.5), Axis::Row, &[resize(900, 400)]);
    assert!(close(width(&app, lead), (900.0 - BAR) * 0.5));
}

#[test]
fn a_stacked_splitter_drags_and_steps_down_the_column() {
    let start = run(SplitAt::Start(100.0), Axis::Column, &[]);
    let from = bar_at(&start);
    let [lead, _, _] = parts(&start);
    let to = (from.0, from.1 + 60.0);
    let app = run(
        SplitAt::Start(100.0),
        Axis::Column,
        &[drag(from, to, false)],
    );
    assert!(close(app.store().world(lead).h, 160.0));
    let app = run(
        SplitAt::Start(100.0),
        Axis::Column,
        &[click(from), key(Key::Down)],
    );
    assert!(close(app.store().world(lead).h, 110.0));
}
