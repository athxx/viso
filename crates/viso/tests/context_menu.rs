//! A text field's context menu driven through the facade: a secondary click
//! focuses the field and pops the system menu up at the pointer, first
//! selecting the word under it on Apple systems or placing the caret
//! elsewhere; its Select All selects the whole text, and its closing takes
//! the request back. The headless build shapes text through its software
//! raster, which a pointer needs to find its place in the text.
#![cfg(feature = "headless")]

use std::time::Duration;

use viso::__test_support::drive_scripted;
use viso::platform::{
    EditMenuAction, Modifiers as RawModifiers, PointerButtons as RawButtons,
    PointerPhase as RawPhase, RawEvent, RawPointer, WindowId,
};
use viso::prelude::*;
use viso::ui::{Component, FlexStyle, Sizing};
use viso::widgets::text_input;

const STEP: Duration = Duration::from_millis(16);
const WIN: WindowId = WindowId(1);

/// A field holding "hello wide world" at the top of the window.
struct FieldApp;

impl Application for FieldApp {
    fn new(cx: &mut AppCx) -> Self {
        cx.load_font(&include_bytes!("../../text/tests/fixtures/DejaVuSans-subset.ttf")[..]);
        FieldApp
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
                ..FlexStyle::default()
            },
            |cx| {
                text_input("Note")
                    .value("hello wide world")
                    .size(Sizing::fixed(300.0, 30.0))
                    .build(cx);
            },
        );
    }
}

fn redraw() -> RawEvent {
    RawEvent::RedrawRequested { window: WIN }
}

fn secondary(x: f64, y: f64, phase: RawPhase) -> RawEvent {
    let buttons = if phase == RawPhase::Down {
        RawButtons::SECONDARY
    } else {
        RawButtons::NONE
    };
    RawEvent::Pointer(RawPointer::mouse(
        WIN,
        x,
        y,
        buttons,
        RawModifiers::default(),
        phase,
    ))
}

fn menu(action: EditMenuAction) -> RawEvent {
    RawEvent::EditMenu {
        window: WIN,
        action,
    }
}

/// A secondary click at `(x, y)` on the field.
fn right_click(x: f64, y: f64) -> Vec<RawEvent> {
    vec![
        secondary(x, y, RawPhase::Down),
        secondary(x, y, RawPhase::Up),
        redraw(),
    ]
}

#[test]
fn a_secondary_click_pops_the_menu_up_at_the_pointer() {
    let mut script = vec![redraw()];
    script.extend(right_click(12.0, 12.0));
    let app = drive_scripted::<FieldApp>(script, STEP);
    let shown = app.context_menu().expect("the menu pops up");
    assert!((shown.at.x - 12.0).abs() < 0.5 && (shown.at.y - 12.0).abs() < 0.5);
    assert_eq!(shown.at.width, 0.0, "a point");
    let (start, end) = app.focused_selection().expect("the field has the focus");
    if viso::platform::primary_is_logo() {
        assert_eq!((start, end), (0, 5), "the word under the pointer");
    } else {
        assert!(start == end && start > 0, "a caret at the pointer");
    }
    assert_eq!(shown.copy, start != end);
    assert!(shown.paste && shown.select_all);
}

#[test]
fn select_all_selects_the_whole_text_and_the_closing_takes_the_request_back() {
    let mut script = vec![redraw()];
    script.extend(right_click(12.0, 12.0));
    script.extend([menu(EditMenuAction::SelectAll), redraw()]);
    let app = drive_scripted::<FieldApp>(script.clone(), STEP);
    assert_eq!(app.focused_selection(), Some((0, 16)));

    script.extend([menu(EditMenuAction::Closed), redraw()]);
    let app = drive_scripted::<FieldApp>(script, STEP);
    assert_eq!(app.context_menu(), None);
    assert_eq!(app.focused_selection(), Some((0, 16)), "a plain selection");
}
