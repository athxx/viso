//! A pager driven through the facade: a tap on a page dot turns to its page,
//! the arrow keys step one page from the dots, focused by Tab, and the
//! pager's handler hears each page it turns to.
#![cfg(feature = "headless")]

use std::cell::RefCell;
use std::time::Duration;

use viso::__test_support::{DrivenApp, drive_scripted};
use viso::platform::{
    KeyCode, Modifiers, PointerButtons, PointerPhase, RawEvent, RawKey, RawPointer, WindowId,
};
use viso::prelude::*;
use viso::ui::{Component, LeafStyle, NodeId, Sizing};
use viso::widgets::{PagerStyle, pager};

const STEP: Duration = Duration::from_millis(16);
const WIN: WindowId = WindowId(1);

thread_local! {
    static PAGES: RefCell<Vec<usize>> = const { RefCell::new(Vec::new()) };
}

/// A window-filling pager of three pages.
struct PagerApp;

impl Application for PagerApp {
    fn new(_cx: &mut AppCx) -> Self {
        PagerApp
    }

    fn window_config(&self) -> viso::ui::WindowConfig {
        viso::ui::WindowConfig {
            caption: false,
            size: (400.0, 300.0),
            ..Default::default()
        }
    }

    fn build(&mut self, cx: &mut BuildCx<'_>) {
        pager(PagerStyle::default())
            .pages(3, |_, cx| {
                cx.leaf(LeafStyle {
                    size: Sizing::fill(),
                    ..Default::default()
                });
            })
            .on_page(|_, _, page| PAGES.with(|p| p.borrow_mut().push(page)))
            .build(cx);
    }
}

fn redraw() -> RawEvent {
    RawEvent::RedrawRequested { window: WIN }
}

fn click(x: f64, y: f64) -> [RawEvent; 2] {
    let at = |buttons, phase| {
        RawEvent::Pointer(RawPointer::mouse(
            WIN,
            x,
            y,
            buttons,
            Modifiers::default(),
            phase,
        ))
    };
    [
        at(PointerButtons::PRIMARY, PointerPhase::Down),
        at(PointerButtons::PRIMARY, PointerPhase::Up),
    ]
}

fn key(code: KeyCode) -> [RawEvent; 2] {
    let raw = |pressed| {
        RawEvent::Key(RawKey {
            window: WIN,
            code,
            pressed,
            repeat: false,
            modifiers: Modifiers::default(),
        })
    };
    [raw(true), raw(false)]
}

fn drive(script: impl IntoIterator<Item = RawEvent>) -> DrivenApp<PagerApp> {
    PAGES.with(|p| p.borrow_mut().clear());
    let mut events = vec![redraw(), redraw(), redraw()];
    events.extend(script);
    events.extend(std::iter::repeat_n(redraw(), 60));
    drive_scripted::<PagerApp>(events, STEP)
}

fn pages(app: &DrivenApp<PagerApp>) -> NodeId {
    app.root().unwrap()
}

// Three 16-wide dot cells centered in 400 across, 25 tall at the bottom:
// the last from 208 to 224.
const LAST_DOT: (f64, f64) = (216.0, 288.0);

#[test]
fn a_tap_on_a_dot_turns_to_its_page() {
    let app = drive(click(LAST_DOT.0, LAST_DOT.1));
    assert_eq!(app.store().scroll(pages(&app)).x, 800.0);
    assert_eq!(PAGES.with(|p| p.borrow().clone()), [2]);
}

#[test]
fn the_keys_step_a_page_from_the_focused_dots() {
    let mut script = Vec::from(key(KeyCode::Tab));
    for code in [KeyCode::Left, KeyCode::Right, KeyCode::End, KeyCode::Left] {
        script.extend(key(code));
        script.extend(std::iter::repeat_n(redraw(), 60));
    }
    let app = drive(script);
    assert_eq!(app.store().scroll(pages(&app)).x, 400.0);
    assert_eq!(
        PAGES.with(|p| p.borrow().clone()),
        [1, 2, 1],
        "none before the first"
    );
}
