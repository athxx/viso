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
//! Centering is a single fill container that centers its one `Fit`-sized child
//! on both axes: `justify: Center` places the packed child group along the main
//! axis, `align: Center` along the cross axis. Because the label's box is `Fit`
//! (it measures to the shaped run), both axes have slack to center against.

use viso::prelude::*;
use viso::render::Rgba;
use viso::ui::{Align, Component, Justify, Size};

struct Hello;

impl Application for Hello {
    fn new(_cx: &mut AppCx) -> Self {
        Hello
    }

    fn build(&mut self, cx: &mut BuildCx<'_>) {
        // A window-filling container centers its single Fit-sized label on both
        // axes: main axis via `justify`, cross axis via `align`.
        cx.flex(
            FlexStyle {
                align: Align::Center,
                justify: Justify::Center,
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
    }
}

fn main() {
    viso::run::<Hello>();
}
