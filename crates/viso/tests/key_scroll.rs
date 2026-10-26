//! Keyboard scrolling driven through the facade: a page key, Space and End
//! scroll the window's list, a focused button's Space clicks it and a
//! focused slider's arrows move it without scrolling, and a focused text
//! field keeps every key it types or moves its caret with.
#![cfg(feature = "headless")]

use std::cell::Cell;
use std::time::Duration;

use viso::__test_support::{DrivenApp, drive_scripted};
use viso::platform::{KeyCode, Modifiers, RawEvent, RawKey, WindowId};
use viso::prelude::*;
use viso::ui::{Axis, Component, FlexStyle, LeafStyle, Sizing};
use viso::widgets::{ScrollViewStyle, button, scroll, slider, text_input};

const STEP: Duration = Duration::from_millis(16);
const WIN: WindowId = WindowId(1);

thread_local! {
    static CLICKS: Cell<u32> = const { Cell::new(0) };
}

/// A 400 by 300 window filled by a list: a button, a slider and a text
/// field over a tall block.
struct ListApp;

impl Application for ListApp {
    fn new(_cx: &mut AppCx) -> Self {
        ListApp
    }

    fn window_config(&self) -> viso::ui::WindowConfig {
        viso::ui::WindowConfig {
            caption: false,
            size: (400.0, 300.0),
            ..Default::default()
        }
    }

    fn build(&mut self, cx: &mut BuildCx<'_>) {
        scroll(ScrollViewStyle::default())
            .content(|cx| {
                cx.flex(
                    FlexStyle {
                        axis: Axis::Column,
                        ..FlexStyle::default()
                    },
                    |cx| {
                        button("Go")
                            .on_click(|_| CLICKS.with(|c| c.set(c.get() + 1)))
                            .build(cx);
                        slider("Volume").range(0.0, 100.0).value(50.0).build(cx);
                        text_input("Name").build(cx);
                        cx.leaf(LeafStyle {
                            size: Sizing::fixed(400.0, 3000.0),
                            ..Default::default()
                        });
                    },
                );
            })
            .build(cx);
    }
}

fn redraw() -> RawEvent {
    RawEvent::RedrawRequested { window: WIN }
}

fn key(code: KeyCode, shift: bool) -> Vec<RawEvent> {
    let raw = |pressed| {
        RawEvent::Key(RawKey {
            window: WIN,
            code,
            pressed,
            repeat: false,
            modifiers: Modifiers {
                shift,
                ..Modifiers::default()
            },
        })
    };
    let mut out = vec![raw(true), raw(false)];
    out.extend(std::iter::repeat_n(redraw(), 60));
    out
}

fn drive(keys: &[(KeyCode, bool)]) -> DrivenApp<ListApp> {
    CLICKS.with(|c| c.set(0));
    let mut script = vec![redraw(), redraw(), redraw()];
    for &(code, shift) in keys {
        script.extend(key(code, shift));
    }
    drive_scripted::<ListApp>(script, STEP)
}

fn offset(app: &DrivenApp<ListApp>) -> f32 {
    app.store().scroll(app.root().unwrap()).y
}

#[test]
fn page_keys_space_and_end_scroll_the_list() {
    let app = drive(&[(KeyCode::PageDown, false)]);
    // A page of 300 less its 40 of overlap.
    assert_eq!(offset(&app), 262.5);
    let app = drive(&[(KeyCode::Space, false), (KeyCode::Space, true)]);
    assert_eq!(offset(&app), 0.0, "Shift Space pages back");
    let app = drive(&[(KeyCode::End, false)]);
    let store = app.store();
    let list = app.root().unwrap();
    assert_eq!(offset(&app), store.scroll_range(list, Axis::Column));
}

#[test]
fn a_focused_control_takes_the_keys_it_acts_on() {
    let app = drive(&[(KeyCode::Tab, false), (KeyCode::Space, false)]);
    assert_eq!(CLICKS.with(Cell::get), 1);
    assert_eq!(offset(&app), 0.0, "the button's Space clicks it");

    let app = drive(&[
        (KeyCode::Tab, false),
        (KeyCode::Tab, false),
        (KeyCode::Down, false),
    ]);
    assert_eq!(offset(&app), 0.0, "the slider's Down moves it");

    let app = drive(&[(KeyCode::Tab, false), (KeyCode::Down, false)]);
    assert_eq!(offset(&app), 40.0, "a button leaves Down to the list");
}

#[test]
fn a_focused_text_field_keeps_its_keys() {
    let app = drive(&[
        (KeyCode::Tab, false),
        (KeyCode::Tab, false),
        (KeyCode::Tab, false),
        (KeyCode::Space, false),
        (KeyCode::PageDown, false),
        (KeyCode::End, false),
    ]);
    assert_eq!(offset(&app), 0.0);
}
