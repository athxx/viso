//! Multi-channel signed distance field generation for promoted glyphs.
//!
//! When a glyph is promoted under sustained transform (see
//! [`crate::glyph_representation`]), it is rendered once into a multi-channel
//! signed distance field so it stays sharp across a range of scales and
//! rotations without re-rasterizing per frame. Corners are preserved by the
//! multi-channel encoding; a true distance channel drives anti-aliasing. Uses
//! `sdfer`.

use crate::FontFaceId;

/// An MTSDF raster for one glyph at one resolution bucket: the metadata the
/// residency pool keys on. The pixel bytes are handed to `viso-render` for
/// upload; this crate holds identity and placement, not GPU memory.
#[derive(Debug)]
pub struct MtsdfGlyph {
    // TODO(TF-P3): source-to-distance range, resolution bucket, bearing/extent.
}

/// The MTSDF generator.
#[derive(Debug, Default)]
pub struct MtsdfGenerator {
    // TODO(TF-P3): sdfer configuration + scratch reuse.
}

impl MtsdfGenerator {
    /// Generate the multi-channel distance field for one glyph.
    pub fn generate(&mut self, _face: FontFaceId, _glyph: u16) -> MtsdfGlyph {
        todo!("TF-P3: sdfer multi-channel distance field")
    }
}
