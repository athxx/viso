//! Codepoint coverage: does a face render a given codepoint / run, tested
//! without shaping.
//!
//! Coverage is the predicate [`crate::fallback`] uses to walk the fallback
//! chain. It reads the face's character map; it does not shape or rasterize.

use crate::FontFaceId;

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
