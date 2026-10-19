//! How a text node sets its text beyond its content, size and color: the
//! face it draws with, its line height, how many lines it shows and how text
//! it cuts off ends, and how its lines align in its box.

use std::num::NonZeroU32;
use std::sync::Arc;

pub use viso_text::FontWeight;

/// The family a text draws with: a role the platform or the app's fonts
/// fill, or a family by name.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub enum FontFamily {
    /// The platform's interface face, or the app's default family.
    #[default]
    SystemUi,
    /// A serif face.
    Serif,
    /// A monospace face.
    Monospace,
    /// The family of this name, from the app's fonts; an unknown one draws
    /// with the interface face.
    Named(Arc<str>),
}

/// How a line that does not fit ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum TextOverflow {
    /// It is cut at the box.
    #[default]
    Clip,
    /// It ends in an ellipsis within the box.
    Ellipsis,
}

/// Where a line sits within its box, by its paragraph's direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum TextAlign {
    /// At the paragraph's start: the left of a left-to-right one.
    #[default]
    Start,
    /// Centered.
    Center,
    /// At the paragraph's end.
    End,
}

/// A text node's style beyond its size and color.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TextStyle {
    pub weight: FontWeight,
    pub family: FontFamily,
    /// The height of each line's box in logical pixels, the glyphs centered
    /// in it; `None` for the face's own line spacing.
    pub line_height: Option<f32>,
    /// The most lines shown; `None` for every line.
    pub max_lines: Option<NonZeroU32>,
    pub overflow: TextOverflow,
    pub align: TextAlign,
}

impl TextStyle {
    /// How a run of this style, soft-wrapping or not, depends on the width
    /// of its box; `rtl` when a line of it runs right to left, which starts
    /// at the box's right edge.
    pub fn width_use(&self, soft_wrap: bool, rtl: bool) -> WidthUse {
        if self.align != TextAlign::Start || rtl {
            WidthUse::Any
        } else if soft_wrap || self.overflow == TextOverflow::Ellipsis {
            WidthUse::Narrower
        } else {
            WidthUse::Ignored
        }
    }
}

/// How a shaped run depends on the width its box gets, which decides
/// whether a new width lays it out again.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum WidthUse {
    /// Not at all: it draws the same in any box.
    #[default]
    Ignored,
    /// A box narrower than the run: it wraps or ends in an ellipsis to fit.
    Narrower,
    /// Any box: its lines align within it.
    Any,
}
