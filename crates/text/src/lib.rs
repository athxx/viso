//! `viso-text` — font & text layout system (Part XIII).
//!
//! Responsibilities: font loading & metrics, shaping (rustybuzz), multi-line
//! paragraph layout, glyph SDF rasterization, and a single-channel glyph atlas.
//! The [`TextSystem`] façade ties these together: [`TextSystem::prepare`] turns
//! a font + string into per-glyph screen quads with atlas UVs, and exposes the
//! R8 atlas pixels for the caller to upload.
//!
//! Scope (Phase 2): single face per run, left-to-right, hard `\n` line breaks,
//! SDF coverage via `sdfer` ESDT. BiDi, font fallback, automatic word wrapping,
//! and complex-script shaping are deferred.

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
