//! Tooltips driven through the facade: a rest on the content shows one
//! after its delay, below and centred; leaving takes it away and the next
//! shows at once within the grace; a press and Escape take it away; the
//! keyboard's focus shows it; and the control it describes reads its text.
#![cfg(feature = "headless")]

use std::time::Duration;

use viso::__test_support::{DrivenApp, drive_scripted};
use viso::platform::{
    KeyCode as Key, Modifiers as RawModifiers, PointerButtons as RawButtons,
    PointerPhase as RawPhase, RawEvent, RawKey, RawPointer, WindowId,
};
use viso::prelude::*;
use viso::ui::{Axis, BoxStyle, Component, FlexStyle, Inset, LeafStyle, NodeId, Role, Sizing};
use viso::widgets::tooltip;

const STEP: Duration = Duration::from_millis(16);
const WIN: WindowId = WindowId(1);

/// Three 80 by 30 buttons at 100, 100, 200, 100 and 300, 100, each with a
/// tooltip, the last one long.
struct TipApp;

impl Application for TipApp {
    fn new(cx: &mut AppCx) -> Self {
        cx.load_font(&include_bytes!("../../text/tests/fixtures/DejaVuSans-subset.ttf")[..]);
        TipApp
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
                axis: Axis::Row,
                padding: Inset {
                    left: 100.0,
                    top: 100.0,
                    ..Inset::default()
                },
                gap: 20.0,
                ..FlexStyle::default()
            },
            |cx| {
                let long = "Opens the document picker, where you can choose any file to open \
                            from this computer or a connected drive";
                for text in ["Save", "Open", long] {
                    tooltip(text)
                        .content(|cx| {
                            let button = cx.leaf(LeafStyle {
                                size: Sizing::fixed(80.0, 30.0),
                                style: BoxStyle::NONE,
                            });
                            cx.focusable(button, true);
                            cx.semantics(
                                button,
                                viso::ui::Semantics::role(Role::Button).with_label("button"),
                            );
                            cx.on_pointer(button, |_| {});
                        })
                        .build(cx);
                }
            },
        );
    }
}

fn redraw() -> RawEvent {
    RawEvent::RedrawRequested { window: WIN }
}

fn mouse(x: f32, y: f32, buttons: RawButtons, phase: RawPhase) -> Vec<RawEvent> {
    vec![
        RawEvent::Pointer(RawPointer::mouse(
            WIN,
            f64::from(x),
            f64::from(y),
            buttons,
            RawModifiers::default(),
            phase,
        )),
        redraw(),
    ]
}

fn hover(x: f32, y: f32) -> Vec<RawEvent> {
    mouse(x, y, RawButtons::NONE, RawPhase::Moved)
}

fn press(x: f32, y: f32) -> Vec<RawEvent> {
    let mut out = mouse(x, y, RawButtons::PRIMARY, RawPhase::Down);
    out.extend(mouse(x, y, RawButtons::PRIMARY, RawPhase::Up));
    out
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

/// Frames for `ms` milliseconds, a wakeup before each.
fn wait(ms: u64) -> Vec<RawEvent> {
    (0..ms.div_ceil(16) + 1)
        .flat_map(|_| [RawEvent::Wakeup, redraw()])
        .collect()
}

fn run(steps: &[Vec<RawEvent>]) -> DrivenApp<TipApp> {
    let mut script = vec![redraw(), redraw()];
    for step in steps {
        script.extend(step.iter().cloned());
    }
    script.push(redraw());
    drive_scripted::<TipApp>(script, STEP)
}

/// Each tooltip's content and its bubble.
fn tips(app: &DrivenApp<TipApp>) -> Vec<(NodeId, NodeId)> {
    let store = app.store();
    store
        .arena()
        .children(app.root().unwrap())
        .map(|body| {
            let mut kids = store.arena().children(body);
            (kids.next().unwrap(), kids.next().unwrap())
        })
        .collect()
}

fn showing(app: &DrivenApp<TipApp>) -> Vec<bool> {
    tips(app)
        .iter()
        .map(|&(_, tip)| !app.store().hidden(tip))
        .collect()
}

#[test]
fn a_rest_shows_it_after_its_delay_below_and_centred() {
    let app = run(&[hover(140.0, 115.0), wait(300)]);
    assert_eq!(showing(&app), [false, false, false], "not yet");
    let app = run(&[hover(140.0, 115.0), wait(600)]);
    assert_eq!(showing(&app), [true, false, false]);
    let store = app.store();
    let (button, tip) = tips(&app)[0];
    let (b, t) = (store.world(button), store.world(tip));
    assert!((t.y - (b.y + b.h + 6.0)).abs() < 0.5, "below: {t:?} {b:?}");
    let (bm, tm) = (b.x + b.w / 2.0, t.x + t.w / 2.0);
    assert!((bm - tm).abs() < 0.5, "centred: {t:?} {b:?}");
    assert_eq!(store.semantics(tip).map(|s| s.role), Some(Role::Tooltip));
    assert_eq!(
        store.semantics(button).and_then(|s| s.hint.clone()),
        Some("Save".to_owned()),
        "the control reads it as its description"
    );
}

#[test]
fn leaving_takes_it_away_and_the_next_shows_at_once() {
    let app = run(&[
        hover(140.0, 115.0),
        wait(600),
        hover(500.0, 400.0),
        wait(200),
    ]);
    assert_eq!(
        showing(&app),
        [false, false, false],
        "gone after the pointer left"
    );
    let app = run(&[
        hover(140.0, 115.0),
        wait(600),
        hover(240.0, 115.0),
        wait(50),
    ]);
    assert_eq!(showing(&app), [false, true, false], "the next at once");
    let app = run(&[
        hover(140.0, 115.0),
        wait(600),
        hover(500.0, 400.0),
        wait(1000),
        hover(240.0, 115.0),
        wait(100),
    ]);
    assert_eq!(
        showing(&app),
        [false, false, false],
        "after the grace it waits again"
    );
    // The pointer crossing onto the tooltip keeps it.
    let app = run(&[hover(140.0, 115.0), wait(600)]);
    let tip = app.store().world(tips(&app)[0].1);
    let on = (tip.x + tip.w / 2.0, tip.y + tip.h / 2.0);
    let app = run(&[
        hover(140.0, 115.0),
        wait(600),
        hover(140.0, 133.0),
        hover(on.0, on.1),
        wait(300),
    ]);
    assert_eq!(showing(&app), [true, false, false], "kept on the tooltip");
}

#[test]
fn a_press_and_escape_take_it_away() {
    let app = run(&[
        hover(140.0, 115.0),
        wait(600),
        press(140.0, 115.0),
        wait(700),
    ]);
    assert_eq!(
        showing(&app),
        [false, false, false],
        "a press keeps it away"
    );
    let app = run(&[hover(140.0, 115.0), wait(600), key(Key::Escape)]);
    assert_eq!(showing(&app), [false, false, false]);
}

#[test]
fn the_keyboard_focus_shows_it_and_leaving_takes_it_away() {
    let app = run(&[key(Key::Tab)]);
    assert_eq!(app.store().focused(), Some(tips(&app)[0].0));
    assert_eq!(showing(&app), [true, false, false], "at once");
    let app = run(&[key(Key::Tab), key(Key::Tab)]);
    assert_eq!(showing(&app), [false, true, false], "it follows the focus");
}

#[test]
fn a_long_tooltip_wraps_at_its_widest() {
    let app = run(&[hover(340.0, 115.0), wait(600)]);
    assert_eq!(showing(&app), [false, false, true]);
    let store = app.store();
    let tip = store.world(tips(&app)[2].1);
    assert!(tip.w <= 280.0 + 0.5, "no wider than its widest: {tip:?}");
    assert!(tip.h > 30.0, "on more than one line: {tip:?}");
}
