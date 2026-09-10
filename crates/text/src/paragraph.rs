//! The paragraph pipeline: the world-ready join of segmentation, BiDi, shaping,
//! line breaking, and width fit into laid-out lines, with a last-good result and
//! caching.
//!
//! A paragraph owns the full logical-to-visual layout. It reshapes only when
//! text, font, features, or width change (per the text caching contract), keeps
//! the last successfully laid-out result while a reflow is pending, and runs its
//! heavy work on a worker. It is the surface caret, hit testing, and selection
//! read.

/// A laid-out paragraph: the cached, world-ready layout result.
#[derive(Debug, Default)]
pub struct Paragraph {
    // TODO(TF-P2): lines, runs, shaped glyphs, and the layout version key that
    // gates reshape (text/font/features/width).
}

impl Paragraph {
    /// Lay out the paragraph to a target width, reusing cached results when the
    /// layout version key is unchanged.
    pub fn layout(&mut self, _width: f32) {
        todo!("TF-P2: world-ready paragraph layout with cache gate")
    }
}
