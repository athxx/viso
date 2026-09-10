//! Retained vector outlines for glyphs promoted to [`OutlineVector`] at extreme
//! scale.
//!
//! At extreme sustained zoom a distance field can no longer represent a glyph
//! crisply, so the glyph is promoted to a retained tessellated outline that the
//! steady state reuses rather than re-tessellating each frame. This holds the
//! outline geometry keyed by face and glyph; `viso-render` owns the vertex
//! buffers.
//!
//! [`OutlineVector`]: crate::glyph_representation::GlyphImageKind::OutlineVector

use crate::FontFaceId;

/// A retained tessellated outline for one glyph.
#[derive(Debug)]
pub struct OutlineGlyph {
    // TODO(TF-P3): tessellated contours / fill geometry + extent.
}

/// The retained outline cache.
#[derive(Debug, Default)]
pub struct OutlineCache {
    // TODO(TF-P3): bounded map (FontFaceId, glyph) -> retained outline.
}

impl OutlineCache {
    /// Get or build the retained outline for a glyph.
    pub fn get_or_build(&mut self, _face: FontFaceId, _glyph: u16) -> &OutlineGlyph {
        todo!("TF-P3: retained outline tessellation")
    }
}
