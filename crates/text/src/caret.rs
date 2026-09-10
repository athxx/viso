//! Caret placement and movement: grapheme-aware, BiDi-aware cursor motion over
//! a laid-out paragraph.
//!
//! Caret motion moves by grapheme cluster (never by byte or `char`) and
//! resolves visual placement through [`crate::text_position::CaretAffinity`] at
//! direction boundaries and soft wraps. It reads segmentation and BiDi results;
//! it does not reshape.

use crate::text_position::TextPosition;

/// A caret over a laid-out paragraph.
#[derive(Debug, Default)]
pub struct Caret {
    // TODO(TF-P2): current TextPosition + preferred visual x for vertical moves.
}

impl Caret {
    /// Move one grapheme cluster to the visual right, honoring direction.
    pub fn move_right(&mut self) -> TextPosition {
        todo!("TF-P2: grapheme + BiDi-aware caret motion")
    }
}
