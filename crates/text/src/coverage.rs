//! Codepoint coverage: does a face render a given codepoint / run, tested
//! without shaping.
//!
//! Coverage is the predicate [`crate::fallback`] uses to walk the fallback
//! chain. It reads the face's character map; it does not shape or rasterize.

use crate::FontFaceId;

/// Whether an sfnt face covers every scalar in `text`.
///
/// This is a cold-path coverage probe used before fallback planning. It checks
/// the cmap only and performs no shaping or rasterization.
pub fn face_covers(sfnt: &[u8], index: u32, text: &str) -> bool {
    ttf_parser::Face::parse(sfnt, index)
        .ok()
        .is_some_and(|face| text.chars().all(|ch| face.glyph_index(ch).is_some()))
}

/// A coverage index over resolved faces.
#[derive(Debug, Default)]
pub struct Coverage {
    // TODO(TF-P1): per-face cmap-derived coverage sets.
}

impl Coverage {
    /// Whether the face renders the codepoint.
    pub fn covers(&self, _face: FontFaceId, _codepoint: char) -> bool {
        todo!("TF-P1: cmap coverage test")
    }
}
