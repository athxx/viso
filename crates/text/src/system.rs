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
use crate::atlas::{ATLAS_SIZE, Atlas, DirtyRect, GlyphKind};
use crate::color_raster::ColorGlyphRasterizer;
use crate::layout::layout;
use crate::provider::{FontRole, SystemFallback, SystemFontProvider, SystemFontQuery};
use crate::shape::shape;

/// A single positioned glyph ready for the GPU: where it lands on screen, where
/// it lives in its atlas, how to decode it, and which atlas it lives in.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GlyphQuad {
    /// Screen rectangle `[x, y, w, h]` in top-left pixel coordinates.
    pub rect_px: [f32; 4],
    /// Atlas UV sub-rectangle `[u_min, v_min, u_max, v_max]`.
    pub uv: [f32; 4],
    /// SDF coverage-ramp width for the shader (see [`crate::raster`]); `0.0` for
    /// color glyphs, which sample RGBA directly with no coverage ramp.
    pub px_range: f32,
    /// Which atlas this quad samples: [`GlyphKind::Sdf`] (R8 outline, decoded via
    /// the coverage ramp) or [`GlyphKind::Color`] (RGBA bitmap, sampled direct).
    /// The caller routes each quad to the matching texture.
    pub kind: GlyphKind,
}

/// Loading + shaping + layout + atlases, behind one `prepare` entry point. Holds
/// two atlases: an R8 SDF atlas for outline glyphs and an RGBA8 color atlas for
/// bitmap-emoji strikes. Each prepared glyph names which it lives in.
pub struct TextSystem {
    store: FontStore,
    atlas: Atlas,
    color_atlas: Atlas,
}

impl Default for TextSystem {
    fn default() -> Self {
        Self::new()
    }
}

impl TextSystem {
    /// A text system with an empty font store and default-size SDF + color atlases.
    pub fn new() -> Self {
        Self {
            store: FontStore::new(),
            atlas: Atlas::new(ATLAS_SIZE),
            color_atlas: Atlas::new_color(ATLAS_SIZE),
        }
    }

    /// Load a face from raw sfnt bytes. Returns `None` if unparseable.
    pub fn load_font(&mut self, bytes: impl Into<Box<[u8]>>, index: u32) -> Option<FontId> {
        self.store.load(bytes, index)
    }

    /// The primary (chain-head) face id, or `None` if no face has been loaded or
    /// resolved yet. This is the face a request shapes against; a `None` primary
    /// means an empty chain (no bundled default, no system face resolved) and a
    /// run shapes to no glyphs.
    pub fn primary(&self) -> Option<FontId> {
        self.store.primary()
    }

    /// Seed the primary face from the system, if the chain is empty: query
    /// `provider` for the platform default UI face ([`FontRole::Ui`]) and load it
    /// as the chain head. Returns the primary face id (the freshly resolved one,
    /// or the existing one if the chain was already populated), or `None` if the
    /// chain is empty and the provider has no UI face (e.g. wasm) — in which case
    /// the run shapes to nothing.
    ///
    /// Cold path: runs on the first shape (and any later shape while the chain is
    /// still empty), not per frame in steady state.
    pub fn resolve_primary(&mut self, provider: &dyn SystemFontProvider) -> Option<FontId> {
        if let Some(id) = self.store.primary() {
            return Some(id);
        }
        let query = SystemFontQuery {
            role: FontRole::Ui,
            sample: String::new(),
            lang: String::new(),
        };
        let result = provider.load(&query)?;
        self.store.load(result.bytes, result.index)
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

    /// Color-atlas edge length in texels (currently the same as the SDF atlas).
    pub fn color_atlas_size(&self) -> u32 {
        self.color_atlas.size()
    }

    /// The full RGBA8 color-atlas pixel buffer (`color_atlas_size² * 4` bytes,
    /// premultiplied RGBA).
    pub fn color_atlas_pixels(&self) -> &[u8] {
        self.color_atlas.pixels()
    }

    /// Take the color-atlas region written since the last call, if any.
    pub fn take_color_atlas_dirty(&mut self) -> Option<DirtyRect> {
        self.color_atlas.take_dirty()
    }

    /// The first-line baseline of a run in `font` at `font_size_px`: the distance
    /// in logical pixels from the top of the layout box down to the baseline of
    /// the first line (one ascender below the top, matching [`layout`]). This is
    /// the vertical anchor cross-line/cross-cell baseline alignment aligns on.
    pub fn first_baseline(&self, font: FontId, font_size_px: f32) -> f32 {
        self.store.face(font).ascender_em * font_size_px
    }

    /// Extend the fallback chain with system faces covering any character in
    /// `text` the currently-loaded chain cannot render, so a subsequent
    /// [`prepare`](Self::prepare) resolves the run against the grown chain.
    ///
    /// Shapes `text` with the current chain, then hands the shaped run to
    /// `fallback` — which scans it for uncovered scripts / emoji, queries
    /// `provider` for a covering face per missing script not already attempted,
    /// and appends each resolved face to the store's chain. Returns `true` if the
    /// chain grew (a caller may reshape/relay out; `prepare` already reshapes from
    /// scratch, so calling it after this suffices).
    ///
    /// This is the seam the pure algorithm layer exposes for the facade's
    /// platform provider: the resolution *policy* and negative cache live in
    /// [`SystemFallback`], the platform binding behind the `provider` trait
    /// object, and this method only drives one shape → resolve step. Cold path —
    /// it runs when text is (re)declared, not per frame in steady state.
    pub fn resolve_missing(
        &mut self,
        font: FontId,
        text: &str,
        provider: &dyn SystemFontProvider,
        fallback: &mut SystemFallback,
    ) -> bool {
        // The store owns the chain; `font` is the request's primary face, which
        // is already the chain head, so shaping over the whole chain covers it.
        let _ = font;
        let shaped = shape(&self.store, text);
        fallback.resolve_missing(&mut self.store, provider, text, &shaped)
    }

    /// Shape and lay out `text` with `font` at `font_size_px`, rasterizing at
    /// `dpi_factor` density, and return one [`GlyphQuad`] per visible glyph.
    ///
    /// Glyphs with no outline (whitespace) contribute layout advance but no
    /// quad. Handles multi-line text (hard `\n` breaks) via [`layout`].
    ///
    /// `max_width_px` is the available content width for soft wrapping, forwarded
    /// to [`layout`]: `Some(w)` wraps each hard line into rows no wider than `w`;
    /// `None` disables soft wrapping (only hard `\n` breaks split the text). The
    /// layout engine passes `None` until measure-time constraint downflow lands.
    ///
    /// `color_raster` is the platform color-emoji rasterizer used for faces whose
    /// strikes were stripped at load ([`FontFace::is_color_emoji`]); pass `None`
    /// on platforms with no binding (wasm), where such faces yield no color glyph
    /// and fall through to the outline path.
    ///
    /// [`FontFace::is_color_emoji`]: crate::FontFace::is_color_emoji
    pub fn prepare(
        &mut self,
        font: FontId,
        text: &str,
        font_size_px: f32,
        max_width_px: Option<f32>,
        dpi_factor: f32,
        color_raster: Option<&dyn ColorGlyphRasterizer>,
    ) -> Vec<GlyphQuad> {
        let dpx_per_em = font_size_px * dpi_factor;
        let positioned = layout(&self.store, font, text, font_size_px, max_width_px);
        // Split borrows: `store` (shared) feeds each glyph's face while the two
        // atlases (unique) pack — taking them as separate fields keeps the
        // borrow checker happy.
        let store = &self.store;
        let atlas = &mut self.atlas;
        let color_atlas = &mut self.color_atlas;

        let inv = 1.0 / dpi_factor;
        let mut quads = Vec::with_capacity(positioned.len());
        for g in positioned {
            // A fallback glyph rasters from the face it resolved to, not the
            // requested primary — the atlas keys on that face.
            let face = store.face(g.font);

            // Color-emoji faces get a bitmap-strike probe first; a strike wins
            // and lands in the color atlas. A face qualifies either because it
            // carries readable strikes (`has_color_strikes`, the `ttf-parser`
            // path) or because the provider marked it a platform-rasterized
            // emoji face (`is_color_emoji`, strikes stripped at load). Any other
            // face skips the probe entirely (no per-glyph raster-image lookup)
            // and every glyph falls through to the SDF outline path.
            if (face.has_color_strikes() || face.is_color_emoji())
                && let Some(entry) =
                    color_atlas.color_glyph(face, g.font, g.id, dpx_per_em, color_raster)
            {
                let w = entry.width as f32 * inv;
                let h = entry.height as f32 * inv;
                let x = g.origin_px[0] + entry.bearing_px[0] * inv;
                let y = g.origin_px[1] + entry.bearing_px[1] * inv;
                quads.push(GlyphQuad {
                    rect_px: [x, y, w, h],
                    uv: entry.uv(color_atlas.size()),
                    px_range: 0.0,
                    kind: GlyphKind::Color,
                });
                continue;
            }

            let Some(entry) = atlas.glyph(face, g.font, g.id, dpx_per_em) else {
                continue;
            };
            // The SDF bitmap was rasterized at `dpi_factor` density; convert its
            // texel extents and bearing back to logical pixels for placement.
            let w = entry.width as f32 * inv;
            let h = entry.height as f32 * inv;
            let x = g.origin_px[0] + entry.bearing_px[0] * inv;
            let y = g.origin_px[1] + entry.bearing_px[1] * inv;
            quads.push(GlyphQuad {
                rect_px: [x, y, w, h],
                uv: entry.uv(atlas.size()),
                px_range: entry.px_range,
                kind: GlyphKind::Sdf,
            });
        }
        quads
    }
}
