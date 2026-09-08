//! `viso-text` — font & text layout system (Part XIII).
//!
//! Responsibilities: font loading & metrics, shaping (rustybuzz), multi-line
//! paragraph layout, glyph SDF rasterization, and a single-channel glyph atlas.
//! The [`TextSystem`] façade ties these together: [`TextSystem::prepare`] turns
//! a font + string into per-glyph screen quads with atlas UVs, and exposes the
//! R8 atlas pixels for the caller to upload.
//!
//! Scope: the [`FontStore`] holds an ordered **fallback chain** of faces plus
//! per-face coverage metadata ([`FontFace::has_char`], `glyph_count`), so the
//! facade can resolve a face for a character before shaping. Shaping and layout
//! are still single-run, left-to-right, with hard `\n` line breaks and SDF
//! coverage via `sdfer` ESDT; itemized BiDi/script shaping over the chain,
//! automatic word wrapping, and system-font resolution are built on top of this
//! store in later sections.

#![forbid(unsafe_op_in_unsafe_fn)]

mod atlas;
mod font;
mod layout;
mod raster;
mod shape;
mod system;

pub use atlas::{ATLAS_SIZE, Atlas, AtlasEntry, DirtyRect};
pub use font::{Command, FontFace, FontStore};
pub use layout::{PositionedGlyph, layout};
pub use raster::{RasterGlyph, SDF_EDGE, SDF_RADIUS, rasterize_glyph};
pub use shape::{ShapedGlyph, shape};
pub use system::{GlyphQuad, TextSystem};

/// Opaque handle for a loaded font face.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FontId(pub u32);
