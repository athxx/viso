//! Text selection: an anchored range over logical positions and the visual
//! rectangles that cover it across lines and direction runs.
//!
//! A selection is two [`crate::text_position::TextPosition`]s (anchor and
//! focus); its visual coverage can be several rectangles when it spans wrapped
//! lines or mixed-direction runs. It reads line layout; it does not reshape.

use crate::text_position::TextPosition;

/// An anchored logical selection range.
#[derive(Debug, Clone, Copy)]
pub struct Selection {
    pub anchor: TextPosition,
    pub focus: TextPosition,
}

impl Selection {
    /// Whether the selection is empty (a bare caret).
    pub fn is_caret(&self) -> bool {
        self.anchor == self.focus
    }
}
