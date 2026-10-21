//! `viso-glyph-a8` — the exact A8 coverage of a glyph, rasterized or baked.
//!
//! A glyph's MaskA8 image (Text/Font Runtime §13.1) has two sources that must
//! agree pixel for pixel:
//!
//! - [`rasterize_coverage`] (feature `raster`): the outline from `ttf-parser`,
//!   accumulated by `ab_glyph_rasterizer` at a device size;
//! - [`BakedFace`]: pages a build rasterized ahead of time with the same
//!   function and stored compressed ([`baked`]), read in place from the
//!   binary's read-only data and decoded with integer arithmetic.
//!
//! `packaged_fonts!` bakes and the text runtime reads, so both depend on this
//! crate rather than the proc macro on the runtime. Without `raster` the crate
//! is `no_std` + `alloc` and holds the reader alone.

#![cfg_attr(not(feature = "raster"), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod baked;
mod bitmap;
#[cfg(feature = "raster")]
mod raster;
mod resample;
mod rle;

#[cfg(feature = "raster")]
pub use baked::{BakeError, bake_face};
pub use baked::{BakedFace, BakedGlyph, BakedStrike, EncodeError, Lookup};
pub use bitmap::CoverageBitmap;
#[cfg(feature = "raster")]
pub use raster::{rasterize_coverage, rasterize_coverage_at};
