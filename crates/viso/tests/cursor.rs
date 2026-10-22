//! The pointer cursor driven through the facade: the window shows the
//! cursor a node names while the mouse is over it, a field's text beam over
//! a field, and the default shape over nothing that names one.

use std::time::Duration;

use viso::__test_support::drive_scripted;
use viso::platform::{
    Modifiers as RawModifiers, PointerButtons as RawButtons, PointerPhase as RawPhase, RawEvent,
    RawPointer, WindowId,
};
use viso::prelude::*;
use viso::ui::{Axis, BoxStyle, Component, FlexStyle, LeafStyle, Size};
use viso::widgets::text_input;

const STEP: Duration = Duration::from_millis(16);
const WIN: WindowId = WindowId(1);

/// A leaf showing the hand over a text field.
struct PointedApp;

impl Application for PointedApp {
    fn new(_cx: &mut AppCx) -> Self {
        PointedApp
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
                axis: Axis::Column,
                ..FlexStyle::default()
            },
            |cx| {
                let leaf = cx.leaf(LeafStyle {
                    size: Size::fixed(160.0, 30.0),
                    style: BoxStyle::NONE,
                });
                cx.cursor(leaf, CursorIcon::Pointer);
                text_input("Name").size(Size::fixed(160.0, 30.0)).build(cx);
            },
        );
    }
}

fn redraw() -> RawEvent {
    RawEvent::RedrawRequested { window: WIN }
}

fn hover(x: f64, y: f64) -> RawEvent {
    RawEvent::Pointer(RawPointer::mouse(
        WIN,
        x,
        y,
        RawButtons::NONE,
        RawModifiers::default(),
        RawPhase::Moved,
    ))
}

fn cursor_after(x: f64, y: f64) -> CursorIcon {
    drive_scripted::<PointedApp>(vec![redraw(), hover(x, y), redraw()], STEP).cursor()
}

#[test]
fn the_window_shows_the_cursor_of_what_the_mouse_is_over() {
    assert_eq!(cursor_after(20.0, 10.0), CursorIcon::Pointer);
    assert_eq!(cursor_after(20.0, 45.0), CursorIcon::Text);
    assert_eq!(cursor_after(20.0, 200.0), CursorIcon::Default);
}
