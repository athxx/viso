//! How a text node sets its text beyond its content and color: the size it
//! draws at, the face it draws with, its line height, how many lines it shows
//! and how text it cuts off ends, and how its lines align in its box.

use std::num::NonZeroU32;
use std::sync::Arc;

pub use viso_text::FontWeight;

use crate::component::NodeStore;
use crate::content::TextRequest;
use crate::length::{LengthEnv, LengthTerms};
use crate::node::NodeId;

/// The size a text draws at.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FontSize {
    /// This size, in dp.
    Fixed(f32),
    /// Its node's font size: its own `font_size`, else its nearest
    /// ancestor's, else the environment's root font size. It follows each of
    /// them, and the environment, as they change.
    Inherited,
}

/// A text's size as its node and the environment resolve it, in dp.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextSizing {
    pub font_size: f32,
    /// The height of each line's box; `None` for the face's own spacing.
    pub line_height: Option<f32>,
}

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
    /// The family it draws with; `None` for the store's
    /// [`text_family`](NodeStore::text_family), the theme's.
    pub family: Option<FontFamily>,
    /// The height of each line's box, the glyphs centered in it, its `em`
    /// and `%` of the text's font size; `None` for the face's own line
    /// spacing.
    pub line_height: Option<LengthTerms>,
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

impl NodeStore {
    /// The family a text that names none draws with: the theme's
    /// `typography.family`.
    pub fn text_family(&self) -> &FontFamily {
        &self.lengths().text_family
    }

    /// Sets the theme's typography: the root's font size, `base_size` sp,
    /// and the family a text that names none draws with. A text whose size
    /// or face moves shapes anew.
    pub fn set_typography(&mut self, base_size: f32, family: FontFamily) {
        let env = self.length_env();
        if env.base_font_size != base_size {
            self.set_length_env(LengthEnv {
                base_font_size: base_size,
                ..env
            });
        }
        if self.lengths().text_family != family {
            self.lengths_mut().text_family = family;
            self.resize_text();
        }
    }

    /// The size `request`, declared on node `id`, draws at now. A line height
    /// that does not fold to a finite size is the face's own.
    pub fn text_sizing(&self, id: NodeId, request: &TextRequest) -> TextSizing {
        let env = self.length_env();
        let font_size = match request.font_size {
            FontSize::Fixed(size) => size,
            FontSize::Inherited => self
                .resolved_font_size(id)
                .unwrap_or_else(|| env.root_font_size()),
        };
        TextSizing {
            font_size,
            line_height: request
                .style
                .line_height
                .and_then(|terms| terms.fold_font(&env, font_size)),
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
