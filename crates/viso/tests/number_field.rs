//! A `NumberField` driven through the facade: a drag scrubs it before it has
//! the focus, typed text reports its number once in range, and the focus
//! leaving settles its text within the range.

use std::cell::RefCell;
use std::time::Duration;

use viso::__test_support::drive_scripted;
use viso::platform::{
    Modifiers as RawModifiers, PointerButtons as RawButtons, PointerPhase as RawPhase, RawEvent,
    RawPointer, RawText, WindowId,
};
use viso::prelude::*;
use viso::ui::{Axis, Component, FlexStyle, Size};
use viso::widgets::text_input;

const STEP: Duration = Duration::from_millis(16);
const WIN: WindowId = WindowId(1);

thread_local! {
    /// Every number the field reported, in order.
    static CHANGES: RefCell<Vec<f64>> = const { RefCell::new(Vec::new()) };
}

/// A number field from 0 to 10 in halves holding 2, over a plain field.
struct VolumeApp;

impl Application for VolumeApp {
    fn new(_cx: &mut AppCx) -> Self {
        VolumeApp
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
                number_field("Volume")
                    .range(0.0, 10.0)
                    .step(0.5)
                    .value(2.0)
                    .size(Size::fixed(160.0, 30.0))
                    .on_change(|_ev, v| CHANGES.with(|c| c.borrow_mut().push(v)))
                    .build(cx);
                text_input("Name").size(Size::fixed(160.0, 30.0)).build(cx);
            },
        );
    }
}

fn redraw() -> RawEvent {
    RawEvent::RedrawRequested { window: WIN }
}

fn mouse(x: f64, y: f64, phase: RawPhase) -> RawEvent {
    let buttons = if matches!(phase, RawPhase::Down | RawPhase::Moved) {
        RawButtons::PRIMARY
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

fn typed(text: &str) -> RawEvent {
    RawEvent::Text(RawText {
        window: WIN,
        text: text.to_owned(),
    })
}

fn changes() -> Vec<f64> {
    CHANGES.with(|c| std::mem::take(&mut *c.borrow_mut()))
}

#[test]
fn a_drag_scrubs_the_number_before_it_has_the_focus() {
    changes();
    let app = drive_scripted::<VolumeApp>(
        vec![
            redraw(),
            mouse(20.0, 10.0, RawPhase::Down),
            mouse(30.0, 10.0, RawPhase::Moved),
            mouse(40.0, 10.0, RawPhase::Moved),
            mouse(40.0, 10.0, RawPhase::Up),
            redraw(),
        ],
        STEP,
    );
    let seen = changes();
    assert_eq!(
        seen.last(),
        Some(&7.0),
        "twenty pixels are ten steps: {seen:?}"
    );
    assert_eq!(app.focused_text(), None, "a scrub takes no focus");
}

#[test]
fn the_focus_leaving_settles_the_typed_text_within_the_range() {
    changes();
    let app = drive_scripted::<VolumeApp>(
        vec![
            redraw(),
            mouse(20.0, 10.0, RawPhase::Down),
            mouse(20.0, 10.0, RawPhase::Up),
            redraw(),
            typed("1"),
            redraw(),
            typed("2x"),
            redraw(),
            mouse(20.0, 45.0, RawPhase::Down),
            mouse(20.0, 45.0, RawPhase::Up),
            redraw(),
            redraw(),
        ],
        STEP,
    );
    assert_eq!(
        changes(),
        [1.0, 10.0],
        "\"1\" is in range, \"12\" is not until it settles"
    );
    assert_eq!(
        app.focused_text(),
        Some(""),
        "the plain field has the focus"
    );
}
