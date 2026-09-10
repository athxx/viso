//! Glyph identity and residency metadata: the [`GlyphKey`] every glyph resolves
//! to, and the per-representation residency pools with page-age + CLOCK
//! eviction.
//!
//! This crate owns only identity and residency *metadata* — which glyph, in
//! which representation, at which resolution bucket, and whether it is resident.
//! The actual atlas texture, page memory, and upload live in `viso-render`.
//! There are four independent pools (A8 coverage, MTSDF, RGBA color, vector);
//! eviction is by page age with a CLOCK second-chance sweep, not per-glyph LRU,
//! and exact coverage is always a correct fallback under pressure.

use crate::FontFaceId;
use crate::glyph_representation::GlyphImageKind;

/// The full identity of a resolved glyph image: the face, the glyph, the chosen
/// representation, and the resolution / size bucket it was resolved into. Color
/// glyphs quantize to a size bucket; scalable representations carry their
/// resolution bucket.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GlyphKey {
    pub face: FontFaceId,
    pub glyph: u16,
    pub kind: GlyphImageKind,
    /// Resolution / size bucket the representation was resolved into.
    pub bucket: u16,
}

/// The residency metadata across the four representation pools. Owns no GPU
/// memory; tracks which [`GlyphKey`]s are resident and their page ages.
#[derive(Debug, Default)]
pub struct GlyphResidency {
    // TODO(TF-P0): four pools, page-age + CLOCK sweep state, resident set.
}

impl GlyphResidency {
    /// Note that a glyph was touched this frame, for the CLOCK sweep.
    pub fn touch(&mut self, _key: GlyphKey) {
        todo!("TF-P0: mark resident glyph used this frame")
    }
}
