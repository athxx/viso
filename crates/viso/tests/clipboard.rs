//! The clipboard through the facade on the headless backend: a handler
//! copies HTML and an image, a paste target hears a paste gesture with the
//! richest kind it takes, a node asks for a paste and hears the answer, and
//! a text field keeps taking plain text.

use std::cell::RefCell;
use std::time::Duration;

use viso::__test_support::drive_scripted;
use viso::clipboard::{ClipboardData, ClipboardImage, ClipboardKinds};
use viso::platform::{
    Modifiers as RawModifiers, PasteReply, PointerButtons as RawButtons, PointerPhase as RawPhase,
    RawEvent, RawPointer, WindowId,
};
use viso::prelude::*;
use viso::ui::{Component, FlexStyle, PointerPhase, Size, StackStyle};
use viso::widgets::text_input;

const STEP: Duration = Duration::from_millis(16);
const WIN: WindowId = WindowId(1);

thread_local! {
    /// What the pad heard pasted, in order.
    static PASTED: RefCell<Vec<ClipboardData>> = const { RefCell::new(Vec::new()) };
}

fn image() -> ClipboardImage {
    ClipboardImage::rgba(2, 1, vec![255, 0, 0, 255, 0, 0, 255, 128]).unwrap()
}

/// A copy strip at the top, a pad taking every kind below it, then a field.
struct Board;

impl Application for Board {
    fn new(_cx: &mut AppCx) -> Self {
        Board
    }

    fn window_config(&self) -> viso::ui::WindowConfig {
        viso::ui::WindowConfig {
            caption: false,
            ..Default::default()
        }
    }

    fn build(&mut self, cx: &mut BuildCx<'_>) {
        let style = StackStyle {
            size: Size::fixed(200.0, 40.0),
            ..StackStyle::default()
        };
        cx.flex(
            FlexStyle {
                size: Size::fill(),
                axis: viso::ui::Axis::Column,
                ..FlexStyle::default()
            },
            |cx| {
                let copier = cx.stack(style, |_| {});
                cx.on_pointer(copier, |ev| {
                    if ev.pointer().is_some_and(|p| p.phase == PointerPhase::Down) {
                        ev.copy(ClipboardData {
                            image: Some(image()),
                            ..ClipboardData::html("<b>hi</b>", "hi")
                        });
                    }
                });
                let pad = cx.stack(style, |_| {});
                let pad_id = pad.id();
                cx.focusable(pad, true);
                cx.paste_target(pad, ClipboardKinds::ALL);
                cx.on_pointer(pad, move |ev| {
                    if ev.pointer().is_some_and(|p| p.phase == PointerPhase::Down) {
                        ev.request_focus(pad_id);
                    }
                });
                cx.on_key(pad, |ev| {
                    if let Some(data) = ev.pasted() {
                        PASTED.with(|p| p.borrow_mut().push(data.clone()));
                    }
                });
                // A press below the pad asks for its HTML, delivered to it.
                let asker = cx.stack(style, |_| {});
                cx.on_pointer(asker, move |ev| {
                    if ev.pointer().is_some_and(|p| p.phase == PointerPhase::Down) {
                        ev.request_paste(
                            pad_id,
                            ClipboardKinds {
                                image: false,
                                ..ClipboardKinds::ALL
                            },
                        );
                    }
                });
                text_input("Name")
                    .value("")
                    .size(Size::fixed(160.0, 30.0))
                    .build(cx);
            },
        );
    }
}

fn redraw() -> RawEvent {
    RawEvent::RedrawRequested { window: WIN }
}

fn click(y: f64) -> [RawEvent; 3] {
    let mouse = |phase, buttons| {
        RawEvent::Pointer(RawPointer::mouse(
            WIN,
            20.0,
            y,
            buttons,
            RawModifiers::default(),
            phase,
        ))
    };
    [
        mouse(RawPhase::Down, RawButtons::PRIMARY),
        mouse(RawPhase::Up, RawButtons::NONE),
        redraw(),
    ]
}

#[test]
fn copies_reach_the_paste_target_and_the_asker_and_a_field_takes_text() {
    PASTED.with(|p| p.borrow_mut().clear());
    let gesture = PasteReply::new();
    let field_gesture = PasteReply::new();
    let mut script = vec![redraw()];
    // Copy, focus the pad, paste into it by gesture.
    script.extend(click(20.0));
    script.extend(click(60.0));
    script.push(RawEvent::PasteRequested {
        window: WIN,
        reply: gesture.clone(),
    });
    script.push(redraw());
    // Ask for the HTML for the pad from below it.
    script.extend(click(100.0));
    script.push(redraw());
    // Focus the field and paste by gesture: plain text.
    script.extend(click(135.0));
    script.push(RawEvent::PasteRequested {
        window: WIN,
        reply: field_gesture.clone(),
    });
    script.push(redraw());
    let app = drive_scripted::<Board>(script, STEP);

    assert_eq!(
        gesture.get(),
        ClipboardKinds::ALL,
        "the pad takes every kind"
    );
    assert_eq!(field_gesture.get(), ClipboardKinds::TEXT);
    let pasted = PASTED.with(|p| p.borrow().clone());
    assert_eq!(pasted.len(), 2, "{pasted:?}");
    assert_eq!(pasted[0].text.as_deref(), Some("hi"));
    assert_eq!(pasted[0].html, None, "the image is the richer");
    assert_eq!(pasted[0].image, Some(image()));
    assert_eq!(pasted[1], ClipboardData::html("<b>hi</b>", "hi"));
    assert_eq!(app.focused_text(), Some("hi"));
}
