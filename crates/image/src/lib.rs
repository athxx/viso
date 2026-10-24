//! `viso-image` — raster images on the CPU: an encoded asset — PNG, JPEG, GIF,
//! WebP, AVIF, BMP, ICO or QOI — decoded to the texture convention, or an
//! image the build converted read in place ([`raster`], [`texture`]); an
//! animated one's frames decoded as it plays ([`animation`]); the build-time conversion itself — the texel
//! codecs ([`codec`]), the scaling to the display ([`resample`]), the choice of
//! format per target ([`convert`]) and SVG rasterized into an image
//! ([`vector`]); and the straight-alpha image a headless snapshot captures, its
//! PNG file and its comparison with a golden ([`snapshot`]).
//!
//! The renderer decodes assets with it; the CLI converts assets, checks them,
//! writes icons and compares snapshots with it without linking a renderer.

pub mod animation;
pub mod codec;
pub mod convert;
mod decode;
pub mod raster;
pub mod resample;
pub mod snapshot;
pub mod texture;
pub mod vector;

pub use animation::{Animation, Frame, Loops, is_animated};
pub use raster::{
    AssetError, MAX_DIMENSION, MAX_PIXELS, RasterImage, Texels, decode_raster, decode_raster_asset,
};
pub use snapshot::{Comparison, Image, Tolerance, compare, diff};
