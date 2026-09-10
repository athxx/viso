//! The paragraph pipeline: the world-ready join of segmentation, BiDi, shaping,
//! line breaking, and width fit into laid-out lines, with a last-good result and
//! caching.
//!
//! A paragraph owns the full logical-to-visual layout. It reshapes only when
//! text, font, features, or width change (per the text caching contract), keeps
//! the last successfully laid-out result while a reflow is pending, and runs its
//! heavy work on a worker. It is the surface caret, hit testing, and selection
//! read.
//!
//! # Logical versus visual order
//!
//! Logical order — the UTF-8 source and its [`crate::TextOffset`]s — is the
//! single source of truth; selection, copy, and undo always speak it. The
//! logical-to-visual reorder itself already exists one layer down: BiDi resolves
//! embedding levels and [`crate::bidi::BidiInfo::visual_order`] /
//! [`crate::bidi::BidiInfo::direction_runs`] give the reordered run sequence. What a
//! paragraph adds on top is per-line: the visual reorder is applied only after
//! line boundaries are known (never by reversing whole runs before breaking), so
//! the paragraph's own logical-to-visual caret map — visual runs and their
//! caret positions — is built during line formation, which is not yet
//! implemented here. Until then this stays a layout stub over the existing
//! lower-layer segmentation, BiDi, shaping, and line-break primitives.

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
