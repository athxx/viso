//! Text inside a box that clips to its rounded corners draws: the layer the
//! clip opens is sized to all of its content, the glyphs included.
#![cfg(feature = "headless")]

use std::time::Duration;

use viso::__test_support::drive_scripted;
use viso::platform::{RawEvent, WindowId};
use viso::prelude::*;
use viso::ui::{Axis, BoxStyle, FlexStyle, Inset, Rgba, Sizing};
use viso::widgets::label;

const STEP: Duration = Duration::from_millis(16);
const WIN: WindowId = WindowId(1);

/// A rounded card clipping a line of text and nothing else.
struct Card;

impl Application for Card {
    fn new(cx: &mut AppCx) -> Self {
        cx.use_packaged_fonts(viso::packaged_fonts!("../text/tests/fixtures"));
        Card
    }

    fn window_config(&self) -> viso::ui::WindowConfig {
        viso::ui::WindowConfig {
            caption: false,
            size: (200.0, 100.0),
            ..Default::default()
        }
    }

    fn build(&mut self, cx: &mut BuildCx<'_>) {
        let card = cx.flex(
            FlexStyle {
                axis: Axis::Column,
                padding: Inset::all(40.0),
                size: Sizing::fill(),
                style: BoxStyle {
                    radius: 14.0,
                    ..BoxStyle::solid(Rgba::new(0.9, 0.9, 0.9, 1.0))
                },
                ..FlexStyle::default()
            },
            |cx| {
                label("MMMMMMMM").build(cx);
            },
        );
        cx.clip(card, true);
    }
}

#[test]
fn text_in_a_rounded_clip_draws() {
    let script = std::iter::repeat_n(RawEvent::RedrawRequested { window: WIN }, 12).collect();
    let app = drive_scripted::<Card>(script, STEP);
    let (_, _, bgra) = app.pixels().expect("a surface");
    let ink = bgra
        .chunks(4)
        .filter(|p| p[0] < 100 && p[1] < 100 && p[2] < 100)
        .count();
    assert!(
        ink > 20,
        "the text draws inside the clip: {ink} dark pixels"
    );
}
