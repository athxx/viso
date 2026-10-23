//! Tabs driven through the facade: a press or the arrows select a tab and
//! show its panel, the tabs one Tab stop; a strip wider than the control
//! scrolls under a wheel and to the tab selected; a close button, a middle
//! press or Delete close a tab and free it, the one before it selected; a
//! permanent tab never closes; a tab dragged along the strip, or moved by
//! Ctrl+Shift+Page Up/Page Down, takes another place.
#![cfg(feature = "headless")]

use std::cell::RefCell;
use std::time::Duration;

use viso::__test_support::{DrivenApp, drive_scripted};
use viso::platform::{
    KeyCode as Key, Modifiers as RawModifiers, PointerButtons as RawButtons,
    PointerPhase as RawPhase, RawEvent, RawKey, RawPointer, RawScroll, ScrollPhase, WindowId,
};
use viso::prelude::*;
use viso::ui::{Axis, BoxStyle, Component, FlexStyle, LeafStyle, NodeId, NodeStore, Role, Size};
use viso::widgets::tabs;

const STEP: Duration = Duration::from_millis(16);
const WIN: WindowId = WindowId(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Heard {
    Change(usize),
    Close(usize),
    Move(usize, usize),
}

thread_local! {
    static HEARD: RefCell<Vec<Heard>> = const { RefCell::new(Vec::new()) };
}

fn heard() -> Vec<Heard> {
    HEARD.with(|h| std::mem::take(&mut *h.borrow_mut()))
}

fn hear(h: Heard) {
    HEARD.with(|v| v.borrow_mut().push(h));
}

const CAPTIONS: [&str; 6] = ["Home", "One", "Two", "Three", "Four", "Five"];

/// Six tabs in a 300-wide control, the first permanent, the rest closable,
/// all reorderable.
struct TabsApp;

impl Application for TabsApp {
    fn new(cx: &mut AppCx) -> Self {
        cx.load_font(&include_bytes!("../../text/tests/fixtures/DejaVuSans-subset.ttf")[..]);
        TabsApp
    }

    fn window_config(&self) -> viso::ui::WindowConfig {
        viso::ui::WindowConfig {
            caption: false,
            ..Default::default()
        }
    }

    fn build(&mut self, cx: &mut BuildCx<'_>) {
        cx.flex(FlexStyle::default(), |cx| {
            cx.flex(
                FlexStyle {
                    axis: Axis::Column,
                    size: Size::fixed(300.0, 200.0),
                    ..FlexStyle::default()
                },
                control,
            );
        });
    }
}

fn control(cx: &mut BuildCx<'_>) {
    let panel = |cx: &mut BuildCx<'_>| {
        cx.leaf(LeafStyle {
            size: Size::fill(),
            style: BoxStyle::NONE,
        });
    };
    let mut control = tabs().permanent_tab(CAPTIONS[0], panel);
    for caption in &CAPTIONS[1..] {
        control = control.tab(*caption, panel);
    }
    control
        .closable(true)
        .reorderable(true)
        .on_change(|_, t| hear(Heard::Change(t)))
        .on_close(|_, t| hear(Heard::Close(t)))
        .on_reorder(|_, from, to| hear(Heard::Move(from, to)))
        .build(cx);
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

fn press(at: (f32, f32), buttons: RawButtons) -> Vec<RawEvent> {
    vec![
        mouse(at, RawButtons::NONE, RawPhase::Moved),
        mouse(at, buttons, RawPhase::Down),
        mouse(at, buttons, RawPhase::Up),
        redraw(),
        redraw(),
    ]
}

fn click(at: (f32, f32)) -> Vec<RawEvent> {
    press(at, RawButtons::PRIMARY)
}

/// A press at `from` dragged across to `to` x in steps, then let go.
fn drag(from: (f32, f32), to: f32) -> Vec<RawEvent> {
    let mut out = vec![
        mouse(from, RawButtons::NONE, RawPhase::Moved),
        mouse(from, RawButtons::PRIMARY, RawPhase::Down),
    ];
    for k in 1..=10 {
        let x = from.0 + (to - from.0) * k as f32 / 10.0;
        out.push(mouse((x, from.1), RawButtons::PRIMARY, RawPhase::Moved));
        out.push(redraw());
    }
    out.push(mouse((to, from.1), RawButtons::PRIMARY, RawPhase::Up));
    out.extend(std::iter::repeat_n(redraw(), 60));
    out
}

fn key_with(code: Key, modifiers: RawModifiers) -> Vec<RawEvent> {
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
    key_with(code, RawModifiers::default())
}

fn wheel(at: (f32, f32), dy: f32) -> Vec<RawEvent> {
    vec![
        RawEvent::Scroll(RawScroll {
            window: WIN,
            x: f64::from(at.0),
            y: f64::from(at.1),
            delta_x: 0.0,
            delta_y: f64::from(dy),
            modifiers: RawModifiers::default(),
            phase: ScrollPhase::None,
        }),
        redraw(),
        redraw(),
    ]
}

fn run(steps: &[Vec<RawEvent>]) -> DrivenApp<TabsApp> {
    heard();
    let mut script = vec![redraw(), redraw()];
    for step in steps {
        script.extend(step.iter().cloned());
    }
    script.push(redraw());
    drive_scripted::<TabsApp>(script, STEP)
}

/// The control's strip viewport, the tab buttons in the strip's order, and
/// the panels as declared.
struct Parts {
    viewport: NodeId,
    row: NodeId,
    buttons: Vec<NodeId>,
    panels: Vec<NodeId>,
}

fn parts(app: &DrivenApp<TabsApp>) -> Parts {
    let store = app.store();
    let frame = store.arena().first_child(app.root().unwrap()).unwrap();
    let control = store.arena().first_child(frame).unwrap();
    let mut kids = store.arena().children(control);
    let (viewport, area) = (kids.next().unwrap(), kids.next().unwrap());
    let row = store.arena().first_child(viewport).unwrap();
    Parts {
        viewport,
        row,
        buttons: store.arena().children(row).collect(),
        panels: store.arena().children(area).collect(),
    }
}

fn middle(store: &NodeStore, id: NodeId) -> (f32, f32) {
    let r = store.world(id);
    (r.x + r.w / 2.0, r.y + r.h / 2.0)
}

fn label(store: &NodeStore, id: NodeId) -> String {
    store.semantics(id).and_then(|s| s.label.clone()).unwrap()
}

/// The captions in the strip's order.
fn strip(app: &DrivenApp<TabsApp>) -> Vec<String> {
    let p = parts(app);
    p.buttons.iter().map(|&b| label(app.store(), b)).collect()
}

/// The caption of the tab selected, as the semantics say.
fn selected(app: &DrivenApp<TabsApp>) -> Vec<String> {
    let store = app.store();
    parts(app)
        .buttons
        .iter()
        .filter(|&&b| store.semantic_state(b).and_then(|s| s.selected) == Some(true))
        .map(|&b| label(store, b))
        .collect()
}

/// A tab's close button: the button's last child.
fn close_of(store: &NodeStore, button: NodeId) -> NodeId {
    store.arena().children(button).last().unwrap()
}

fn layout() -> (Parts, DrivenApp<TabsApp>) {
    let app = run(&[]);
    (parts(&app), app)
}

#[test]
fn the_strip_is_a_tablist_of_tabs_over_tabpanels() {
    let (p, app) = layout();
    let store = app.store();
    assert_eq!(store.semantics(p.row).unwrap().role, Role::TabList);
    assert!(store.is_roving(p.row), "the tabs are one Tab stop");
    assert_eq!(strip(&app), CAPTIONS);
    let one = store.semantic_state(p.buttons[1]).unwrap();
    assert_eq!((one.selected, one.position), (Some(false), Some((2, 6))));
    assert_eq!(selected(&app), ["Home"]);
    let panel = store.semantics(p.panels[2]).unwrap();
    assert_eq!(
        (panel.role, panel.label.as_deref()),
        (Role::TabPanel, Some("Two"))
    );
    assert!(!store.hidden(p.panels[0]) && store.hidden(p.panels[1]));
    // The permanent tab has no close button; the rest each have one.
    assert_eq!(store.arena().children(p.buttons[0]).count(), 1);
    let close = close_of(store, p.buttons[2]);
    assert_eq!(
        store.semantics(close).map(|s| (s.role, s.label.clone())),
        Some((Role::Button, Some("Close Two".to_owned())))
    );
}

#[test]
fn a_press_and_the_arrows_select_and_show_a_tab() {
    let (p, app) = layout();
    let app = run(&[click(middle(app.store(), p.buttons[1]))]);
    assert_eq!(heard(), [Heard::Change(1)]);
    assert_eq!(selected(&app), ["One"]);
    let store = app.store();
    assert!(store.hidden(p.panels[0]) && !store.hidden(p.panels[1]));
    assert_eq!(store.focused(), Some(p.buttons[1]));
    let first = click(middle(app.store(), p.buttons[1]));
    let app = run(&[first.clone(), key(Key::Right)]);
    assert_eq!(selected(&app), ["Two"]);
    assert_eq!(
        app.store().focused(),
        Some(p.buttons[2]),
        "the focus follows"
    );
    let app = run(&[first.clone(), key(Key::End)]);
    assert_eq!(selected(&app), ["Five"]);
    let app = run(&[first.clone(), key(Key::Home)]);
    assert_eq!(selected(&app), ["Home"]);
    let ctrl = RawModifiers {
        control: true,
        ..RawModifiers::default()
    };
    let app = run(&[first, key_with(Key::PageDown, ctrl)]);
    assert_eq!(selected(&app), ["Two"]);
}

#[test]
fn a_strip_too_wide_scrolls_under_the_wheel_and_to_the_tab_selected() {
    let (p, app) = layout();
    let store = app.store();
    let row = store.world(p.row);
    assert!(
        row.w > store.world(p.viewport).w,
        "the tabs overflow: {row:?}"
    );
    let app = run(&[wheel(middle(store, p.buttons[0]), 40.0)]);
    assert!(
        app.store().scroll(p.viewport).x > 0.0,
        "the wheel moves it across"
    );
    let first = click(middle(store, p.buttons[0]));
    let app = run(&[first, key(Key::End)]);
    let store = app.store();
    let last = store.world(p.buttons[5]);
    let viewport = store.world(p.viewport);
    assert!(
        last.x + last.w <= viewport.x + viewport.w + 0.5,
        "the last tab in sight: {last:?} in {viewport:?}"
    );
}

#[test]
fn closing_frees_the_tab_and_selects_the_one_before() {
    let (p, start) = layout();
    let at = start.store();
    let two = click(middle(at, p.buttons[2]));
    let close_two = click(middle(at, close_of(at, p.buttons[2])));
    let app = run(&[two.clone(), close_two]);
    assert_eq!(
        heard(),
        [Heard::Change(2), Heard::Change(1), Heard::Close(2)]
    );
    assert_eq!(strip(&app), ["Home", "One", "Three", "Four", "Five"]);
    assert_eq!(selected(&app), ["One"]);
    let store = app.store();
    assert!(!store.arena().is_live(p.buttons[2]), "its button is freed");
    assert!(!store.arena().is_live(p.panels[2]), "and its panel");
    assert!(!store.hidden(p.panels[1]));
    assert_eq!(
        store.semantic_state(p.buttons[3]).unwrap().position,
        Some((3, 5))
    );
    // Closing a tab not selected leaves the selection be.
    let close_one = click(middle(at, close_of(at, p.buttons[1])));
    let app = run(&[close_one]);
    assert_eq!(heard(), [Heard::Close(1)]);
    assert_eq!(selected(&app), ["Home"]);
    // A middle press, and Delete.
    let _ = run(&[press(middle(at, p.buttons[3]), RawButtons::MIDDLE)]);
    assert_eq!(heard(), [Heard::Close(3)]);
    let app = run(&[two, key(Key::Delete)]);
    assert_eq!(
        heard(),
        [Heard::Change(2), Heard::Change(1), Heard::Close(2)]
    );
    assert_eq!(
        app.store().focused(),
        Some(p.buttons[1]),
        "the focus moves on"
    );
    // The permanent tab stays.
    let home = click(middle(at, p.buttons[0]));
    let app = run(&[home, key(Key::Delete)]);
    assert!(heard().is_empty());
    assert_eq!(strip(&app).len(), 6);
}

#[test]
fn a_tab_dragged_along_the_strip_takes_another_place() {
    let (p, start) = layout();
    let at = start.store();
    let from = middle(at, p.buttons[1]);
    let past = at.world(p.buttons[2]);
    let app = run(&[drag(from, past.x + past.w - 4.0)]);
    let moves: Vec<Heard> = heard()
        .into_iter()
        .filter(|h| matches!(h, Heard::Move(..)))
        .collect();
    assert_eq!(moves, [Heard::Move(1, 2)]);
    assert_eq!(strip(&app), ["Home", "Two", "One", "Three", "Four", "Five"]);
    let store = app.store();
    assert_eq!(
        store.semantic_state(p.buttons[1]).unwrap().position,
        Some((3, 6))
    );
    // The keys move the focused tab a place.
    let ctrl_shift = RawModifiers {
        control: true,
        shift: true,
        ..RawModifiers::default()
    };
    let one = click(middle(at, p.buttons[1]));
    let app = run(&[one, key_with(Key::PageDown, ctrl_shift)]);
    assert!(heard().contains(&Heard::Move(1, 2)));
    assert_eq!(strip(&app)[..3], ["Home", "Two", "One"]);
    assert_eq!(app.store().focused(), Some(p.buttons[1]));
}
