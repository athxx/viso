//! `viso-text` — font & text layout system (Part XIII).
//!
//! Responsibilities: font loading & metrics, shaping (rustybuzz), multi-line
//! paragraph layout, glyph SDF rasterization, and a single-channel glyph atlas.
//! The [`TextSystem`] façade ties these together: [`TextSystem::prepare`] turns
//! a font + string into per-glyph screen quads with atlas UVs, and exposes the
//! R8 atlas pixels for the caller to upload.
//!
//! Scope: the [`FontStore`] holds an ordered **fallback chain** of faces plus
//! per-face coverage metadata ([`FontFace::has_char`], `glyph_count`). Shaping
//! ([`shape`]) itemizes a run over the chain with BiDi + script analysis and
//! reshapes `.notdef` spans against later faces; layout ([`layout`]) places the
//! itemized glyphs over lines, sizing each line over the faces it resolved to.
//! When the chain still cannot cover a character, [`SystemFallback`] turns the
//! uncovered scripts/emoji into [`SystemFontProvider`] queries and grows the
//! chain on demand — the provider (a platform binding) lives in the facade, so
//! this crate stays a pure algorithm layer. Automatic word wrapping and color
//! bitmap emoji are built on top of this store in later sections.

#![forbid(unsafe_op_in_unsafe_fn)]

mod atlas;
mod font;
mod layout;
mod provider;
mod raster;
mod shape;
mod system;

pub use atlas::{ATLAS_SIZE, Atlas, AtlasEntry, DirtyRect};
pub use font::{Command, FontFace, FontStore};
pub use layout::{PositionedGlyph, layout};
pub use provider::{
    FontRole, SystemFallback, SystemFontProvider, SystemFontQuery, SystemFontResult,
};
pub use raster::{RasterGlyph, SDF_EDGE, SDF_RADIUS, rasterize_glyph};
pub use shape::{ShapedGlyph, shape};
pub use system::{GlyphQuad, TextSystem};

/// Opaque handle for a loaded font face.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FontId(pub u32);
