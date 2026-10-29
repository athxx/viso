//! A wrapped paragraph whose text changes to one that wraps never draws a
//! frame on one line past its box: the frames until the new lines land
//! draw the old text, wrapped to the box.
#![cfg(feature = "headless")]

use std::cell::Cell;
use std::time::Duration;

use viso::__test_support::{DrivenApp, drive_scripted_frames};
use viso::platform::{
    Modifiers as RawModifiers, PointerButtons as RawButtons, PointerPhase as RawPhase, RawEvent,
    RawPointer, WindowId,
};
use viso::prelude::*;
use viso::ui::{
    Axis, BoxStyle, Content, FlexStyle, FontSize, Inset, LeafStyle, Length, NodeId, Rgba, Sizing,
    TextRequest, TextStyle,
};

const STEP: Duration = Duration::from_millis(16);
const WIN: WindowId = WindowId(1);
const LONG: &str = "No routing: the server answered 404 for the road graph it asked for";

thread_local! {
    static WORDS: Cell<Option<NodeId>> = const { Cell::new(None) };
}

fn text(text: &str) -> TextRequest {
    TextRequest {
        text: text.to_owned(),
        font_size: FontSize::Fixed(14.0),
        color: Rgba::new(0.0, 0.0, 0.0, 1.0),
        soft_wrap: true,
        locale: None,
        style: TextStyle::default(),
        spans: Vec::new(),
    }
}

/// A 200-wide card with a paragraph filling its width, which a press on
/// the card makes long.
struct Card;

impl Application for Card {
    fn new(cx: &mut AppCx) -> Self {
        cx.load_font(&include_bytes!("../../text/tests/fixtures/DejaVuSans-subset.ttf")[..]);
        Card
    }

    fn window_config(&self) -> viso::ui::WindowConfig {
        viso::ui::WindowConfig {
            caption: false,
            ..Default::default()
        }
    }

    fn build(&mut self, cx: &mut BuildCx<'_>) {
        let said = cx.state_of(0_i32);
        let mut card = None;
        cx.flex(
            FlexStyle {
                size: Sizing::fill(),
                ..FlexStyle::default()
            },
            |cx| {
                card = Some(cx.flex(
                    FlexStyle {
                        axis: Axis::Column,
                        padding: Inset::all(10.0),
                        size: Sizing {
                            width: Length::Fixed(200.0),
                            height: Length::Fit,
                        },
                        ..FlexStyle::default()
                    },
                    |cx| {
                        let leaf = cx.leaf(LeafStyle {
                            size: Sizing {
                                width: Length::Fill { weight: 1.0 },
                                height: Length::Fit,
                            },
                            style: BoxStyle::NONE,
                        });
                        cx.text_request(leaf, text("Loading"));
                        WORDS.with(|w| w.set(Some(leaf.id())));
                    },
                ));
            },
        );
        cx.on_pointer(card.expect("built"), move |ev| {
            said.set(ev, 1);
        });
        let words = WORDS.with(Cell::get).expect("built");
        cx.structure(move |s| {
            s.store.add_structure_hook([said.id()], move |s, _| {
                s.store.set_text(words, text(LONG));
            });
        });
    }
}

fn press() -> Vec<RawEvent> {
    let at = |buttons, phase| {
        RawEvent::Pointer(RawPointer::mouse(
            WIN,
            20.0,
            15.0,
            buttons,
            RawModifiers::default(),
            phase,
        ))
    };
    vec![
        at(RawButtons::PRIMARY, RawPhase::Down),
        at(RawButtons::PRIMARY, RawPhase::Up),
    ]
}

/// The paragraph's drawn width and height and its box's width, `frames`
/// frames after the press.
fn after(frames: u32) -> (f32, f32, f32) {
    let redraw = RawEvent::RedrawRequested { window: WIN };
    let mut script = vec![redraw.clone(), redraw];
    script.extend(press());
    let app: DrivenApp<Card> = drive_scripted_frames(script, STEP, frames);
    let words = WORDS.with(Cell::get).expect("built");
    let store = app.store();
    let Some(Content::Text { natural, .. }) = store.content_payload(words) else {
        panic!("text content after {frames} frames");
    };
    (natural.x, natural.y, store.world(words).w)
}

#[test]
fn a_paragraph_that_comes_to_wrap_never_runs_past_its_box() {
    let (_, first, _) = after(0);
    let mut last = 0.0;
    for frames in 0..8 {
        let (width, height, room) = after(frames);
        assert!(
            width <= room + 0.5,
            "after {frames} frames the paragraph is {width} wide in {room}",
        );
        last = height;
    }
    assert!(last > first * 1.5, "it wraps onto lines of its own");
}
