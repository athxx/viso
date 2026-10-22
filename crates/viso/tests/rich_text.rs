//! A rich text driven through the facade: its pieces read as one text to
//! assistive technology, a colored piece draws in its own color, and a
//! larger piece makes its line taller.
#![cfg(feature = "headless")]

use std::time::Duration;

use viso::__test_support::drive_scripted;
use viso::platform::{RawEvent, WindowId};
use viso::prelude::*;
use viso::ui::{Axis, Component, Content, FlexStyle, Rgba, Size};

const STEP: Duration = Duration::from_millis(16);
const WIN: WindowId = WindowId(1);
const RED: Rgba = Rgba {
    r: 1.0,
    g: 0.0,
    b: 0.0,
    a: 1.0,
};

/// A plain label over a rich text with a red bold word and a larger one.
struct NoteApp;

impl Application for NoteApp {
    fn new(cx: &mut AppCx) -> Self {
        cx.load_font(&include_bytes!("../../text/tests/fixtures/DejaVuSans-subset.ttf")[..]);
        NoteApp
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
                label("abc").font_size(20.0).build(cx);
                rich_text([
                    ("a", SpanStyle::default()),
                    ("b", SpanStyle::default().bold().color(RED)),
                    ("c", SpanStyle::default().scale(2.0)),
                ])
                .font_size(20.0)
                .build(cx);
            },
        );
    }
}

fn redraw() -> RawEvent {
    RawEvent::RedrawRequested { window: WIN }
}

#[test]
fn a_rich_text_reads_as_one_text_and_draws_each_piece_its_way() {
    let app = drive_scripted::<NoteApp>(vec![redraw(), redraw(), redraw()], STEP);
    let store = app.store();
    let column = app.root().expect("the column");

    let [plain, rich]: [_; 2] = store
        .arena()
        .children(column)
        .collect::<Vec<_>>()
        .try_into()
        .expect("two texts");
    assert_eq!(
        store.semantics(rich).and_then(|s| s.label.as_deref()),
        Some("abc")
    );
    let text = |node| match store.content_payload(node) {
        Some(Content::Text {
            natural,
            ink,
            glyphs,
            ..
        }) => (*natural, ink.clone(), glyphs.len()),
        other => panic!("a text: {other:?}"),
    };
    let (plain_size, plain_ink, _) = text(plain);
    let (size, ink, glyphs) = text(rich);
    assert!(plain_ink.is_none(), "a plain text draws all in its color");
    assert_eq!(glyphs, 3);
    let ink = ink.expect("the red piece inks");
    assert_eq!(
        ink.runs
            .iter()
            .map(|r| (r.end, r.color))
            .collect::<Vec<_>>(),
        [(1, None), (2, Some(RED))]
    );
    assert!(
        size.y > plain_size.y * 1.8,
        "the doubled `c` makes the line taller: {size:?} over {plain_size:?}"
    );
    assert!(size.x > plain_size.x, "and the text wider");
}
