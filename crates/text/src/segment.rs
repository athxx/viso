//! Text segmentation (UAX#29): grapheme clusters, word boundaries, and the
//! run splitting shaping needs (by script and direction).
//!
//! Segmentation feeds both shaping (single-script single-direction runs) and
//! caret/selection (grapheme-aware movement). It uses `unicode-segmentation`
//! and `unicode-script`.

use crate::text_position::TextOffset;

/// The segmenter over a paragraph's source text.
#[derive(Debug, Default)]
pub struct Segmenter {
    // TODO(TF-P2): grapheme/word iterators + script run splitting state.
}

impl Segmenter {
    /// The next grapheme-cluster boundary at or after an offset.
    pub fn next_grapheme(&self, _from: TextOffset) -> TextOffset {
        todo!("TF-P2: UAX#29 grapheme boundary")
    }
}
