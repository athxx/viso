//! [`TextSystem`] — the crate façade tying loading, shaping, layout, and the
//! glyph atlas together into a single `prepare` call.
//!
//! The render layer calls [`TextSystem::prepare`] with a font, string, pixel
//! size, and DPI factor and gets back one [`GlyphQuad`] per visible glyph:
//! screen rectangle, atlas UV sub-rectangle, and SDF coverage-ramp width. The
//! caller creates/uploads the atlas texture from [`TextSystem::atlas_pixels`]
//! (whole buffer) and [`TextSystem::take_atlas_dirty`] (incremental region).

use crate::FontId;
use crate::FontStore;
use crate::atlas::{ATLAS_SIZE, Atlas, DirtyRect};
use crate::layout::layout;

/// A single positioned glyph ready for the GPU: where it lands on screen, where
/// it lives in the atlas, and how to decode its SDF.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GlyphQuad {
    /// Screen rectangle `[x, y, w, h]` in top-left pixel coordinates.
    pub rect_px: [f32; 4],
    /// Atlas UV sub-rectangle `[u_min, v_min, u_max, v_max]`.
    pub uv: [f32; 4],
    /// SDF coverage-ramp width for the shader (see [`crate::raster`]).
    pub px_range: f32,
}

/// Loading + shaping + layout + atlas, behind one `prepare` entry point.
pub struct TextSystem {
    store: FontStore,
    atlas: Atlas,
}

impl Default for TextSystem {
    fn default() -> Self {
        Self::new()
    }
}

impl TextSystem {
    /// A text system with an empty font store and a default-size atlas.
    pub fn new() -> Self {
        Self {
            store: FontStore::new(),
            atlas: Atlas::new(ATLAS_SIZE),
        }
    }

    /// Load a face from raw sfnt bytes. Returns `None` if unparseable.
    pub fn load_font(&mut self, bytes: impl Into<Box<[u8]>>, index: u32) -> Option<FontId> {
        self.store.load(bytes, index)
    }

    /// Atlas edge length in texels.
    pub fn atlas_size(&self) -> u32 {
        self.atlas.size()
    }

    /// The full R8 atlas pixel buffer (`atlas_size²` bytes).
    pub fn atlas_pixels(&self) -> &[u8] {
        self.atlas.pixels()
    }

    /// Take the atlas region written since the last call, if any.
    pub fn take_atlas_dirty(&mut self) -> Option<DirtyRect> {
        self.atlas.take_dirty()
    }

    /// Shape and lay out `text` with `font` at `font_size_px`, rasterizing at
    /// `dpi_factor` density, and return one [`GlyphQuad`] per visible glyph.
    ///
    /// Glyphs with no outline (whitespace) contribute layout advance but no
    /// quad. Handles multi-line text (hard `\n` breaks) via [`layout`].
    pub fn prepare(
        &mut self,
        font: FontId,
        text: &str,
        font_size_px: f32,
        dpi_factor: f32,
    ) -> Vec<GlyphQuad> {
        let dpx_per_em = font_size_px * dpi_factor;
        let positioned = layout(&self.store, font, text, font_size_px);
        // Split borrows: `store` (shared) feeds the face while `atlas` (unique)
        // packs — taking them as separate fields keeps the borrow checker happy.
        let store = &self.store;
        let atlas = &mut self.atlas;
        let face = store.face(font);

        let mut quads = Vec::with_capacity(positioned.len());
        for g in positioned {
            let Some(entry) = atlas.glyph(face, font, g.id, dpx_per_em) else {
                continue;
            };
            // The SDF bitmap was rasterized at `dpi_factor` density; convert its
            // texel extents and bearing back to logical pixels for placement.
            let inv = 1.0 / dpi_factor;
            let w = entry.width as f32 * inv;
            let h = entry.height as f32 * inv;
            let x = g.origin_px[0] + entry.bearing_px[0] * inv;
            let y = g.origin_px[1] + entry.bearing_px[1] * inv;
            quads.push(GlyphQuad {
                rect_px: [x, y, w, h],
                uv: entry.uv(atlas.size()),
                px_range: entry.px_range,
            });
        }
        quads
    }
}
