//! A UI input tape recorded through the facade replays the same run: the
//! input reaches the same frames, a text field takes the same edits, and the
//! tape survives its file form.
#![cfg(any(feature = "headless", feature = "hot-reload"))]

use std::cell::RefCell;
use std::time::Duration;

use viso::__test_support::{UiTape, drive_recording, drive_tape};
use viso::platform::{
    Modifiers as RawModifiers, PointerButtons as RawButtons, PointerPhase as RawPhase, RawEvent,
    RawPointer, RawText, WindowId,
};
use viso::prelude::*;
use viso::ui::{Component, FlexStyle, Size};
use viso::widgets::text_input;

const STEP: Duration = Duration::from_millis(16);
const WIN: WindowId = WindowId(1);

thread_local! {
    /// Every value the field reported, in order.
    static CHANGES: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

struct FieldApp;

impl Application for FieldApp {
    fn new(_cx: &mut AppCx) -> Self {
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
                size: Size::fill(),
                ..FlexStyle::default()
            },
            |cx| {
                text_input("Name")
                    .value("")
                    .size(Size::fixed(160.0, 30.0))
                    .on_change(|_ev, text| CHANGES.with(|c| c.borrow_mut().push(text.to_owned())))
                    .build(cx);
            },
        );
    }
}

fn redraw() -> RawEvent {
    RawEvent::RedrawRequested { window: WIN }
}

fn mouse(x: f64, y: f64, phase: RawPhase) -> RawEvent {
    let buttons = if phase == RawPhase::Down {
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

fn text(text: &str) -> RawEvent {
    RawEvent::Text(RawText {
        window: WIN,
        text: text.to_owned(),
    })
}

fn changes() -> Vec<String> {
    CHANGES.with(|c| std::mem::take(&mut *c.borrow_mut()))
}

#[test]
fn a_recorded_tape_replays_the_same_edits_at_the_same_frames() {
    let script = vec![
        redraw(),
        mouse(20.0, 10.0, RawPhase::Down),
        mouse(20.0, 10.0, RawPhase::Up),
        redraw(),
        text("he"),
        redraw(),
        redraw(),
        text("llo"),
        redraw(),
    ];
    let (_recorded, tape) = drive_recording::<FieldApp>(script, STEP);
    let edits = changes();
    assert_eq!(edits.last().map(String::as_str), Some("hello"), "{edits:?}");
    let with_input: Vec<usize> = tape
        .frames
        .iter()
        .enumerate()
        .filter(|(_, f)| !f.inputs.is_empty())
        .map(|(at, _)| at)
        .collect();
    assert_eq!(with_input.len(), 3, "the click, then each text: {tape:?}");
    assert_eq!(tape.frames[0].delta_us, 0, "the first frame has no delta");

    let bytes = tape.to_bytes();
    let read = UiTape::from_bytes(&bytes).expect("decodes");
    assert_eq!(read, tape);

    let _replayed = drive_tape::<FieldApp>(read);
    assert_eq!(changes(), edits, "the replay makes the same edits");
}
