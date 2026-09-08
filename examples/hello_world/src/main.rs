//! The canonical minimal Viso application (AGENTS section 1): a centered,
//! multilingual greeting.
//!
//! One [`label`] renders English, Chinese, Thai, and a color emoji in a single
//! mixed run — proving the text subsystem shapes across scripts and draws color
//! glyphs, all resolved from *system* fonts with nothing embedded. The facade
//! resolves each script to a system face on demand and rasterizes the emoji
//! through CoreText; on a platform with no system-font provider the paragraph
//! still lays out but draws no glyphs (it never panics).
//!
//! Centering uses only cross-axis alignment, the one form the flex engine
//! offers: an outer Row fills the window and centers its child vertically, and
//! that child — a full-width Column — centers the label horizontally. The
//! label's own box is `Fit`, so it measures to the shaped run.

use viso::prelude::*;
use viso::render::Rgba;
use viso::ui::{Align, Axis, Component, Size};

struct Hello;

impl Application for Hello {
    fn new(_cx: &mut AppCx) -> Self {
        Hello
    }

    fn build(&mut self, cx: &mut BuildCx<'_>) {
        // Outer Row fills the window and centers its child on the cross (vertical)
        // axis; the child Column fills the width and centers the label on its
        // cross (horizontal) axis. Two cross-axis centers = centered on both.
        cx.flex(
            FlexStyle {
                axis: Axis::Row,
                align: Align::Center,
                size: Size::fill(),
                ..Default::default()
            },
            |cx| {
                cx.flex(
                    FlexStyle {
                        axis: Axis::Column,
                        align: Align::Center,
                        size: Size::fill(),
                        ..Default::default()
                    },
                    |cx| {
                        label("Hello 世界 สวัสดี 🎉")
                            .font_size(48.0)
                            .color(Rgba {
                                r: 0.93,
                                g: 0.94,
                                b: 0.97,
                                a: 1.0,
                            })
                            .build(cx);
                    },
                );
            },
        );
    }
}

fn main() {
    viso::run::<Hello>();
}
