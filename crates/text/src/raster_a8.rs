//! Exact-coverage A8 glyphs, the MaskA8 representation's raster product.
//!
//! This is the default text lane. Stable UI, CJK, and editor text render as
//! exact coverage — the sharpest edges and smallest footprint, with the
//! simplest fragment shader — rather than being fed through an SDF. A glyph's
//! coverage comes from one of two sources that agree pixel for pixel, both in
//! `viso-glyph-a8`: [`rasterize_coverage`], which walks the `ttf-parser`
//! outline through `ab_glyph_rasterizer`, and a [`BakedFace`], the pages a
//! build rasterized ahead of time with that same function for the sizes it
//! declared. Producing pixels and owning where they live are separate
//! lifecycles; the latter belongs to the caller and to `viso-render`.

pub use viso_glyph_a8::{
    BakedFace, BakedGlyph, BakedStrike, CoverageBitmap, Lookup, rasterize_coverage,
    rasterize_coverage_at,
};
