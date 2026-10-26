//! Encoded images decoded to the texture convention: premultiplied RGBA8,
//! sRGB-encoded, rows top first, turned upright as the file says.
//!
//! Each format's header is read first and its size checked against
//! [`MAX_DIMENSION`](crate::raster::MAX_DIMENSION) and
//! [`MAX_PIXELS`](crate::raster::MAX_PIXELS) before any pixel is allocated.
//! An animated image decodes to its first frame; [`crate::animation`] plays
//! the rest.

mod bmp;
mod exif;
mod gif;
mod png;
mod webp;

#[cfg(all(feature = "avif", not(target_arch = "wasm32")))]
pub(crate) mod avif;

pub(crate) use gif::GifFrames;
pub(crate) use png::{PngFrames, apng_frames};
pub(crate) use webp::WebPFrames;

use viso_schema::resource::AssetFormat;
use zune_core::bytestream::ZCursor;
use zune_core::options::DecoderOptions;

use crate::raster::{AssetError, MAX_DIMENSION, RasterImage, check_size, premultiplied};

/// The decoder options every zune decoder reads with: no side past the
/// limit.
fn options() -> DecoderOptions {
    DecoderOptions::default()
        .set_max_width(MAX_DIMENSION as usize)
        .set_max_height(MAX_DIMENSION as usize)
}

/// A decoder's error as an asset error.
fn error(e: &dyn std::fmt::Display) -> AssetError {
    AssetError::Decode(e.to_string().trim_end().to_owned())
}

/// Decodes an encoded image, the first frame of an animated one.
pub(crate) fn still(bytes: &[u8]) -> Result<RasterImage, AssetError> {
    match AssetFormat::sniff(bytes) {
        Some(AssetFormat::Png) => png::still(bytes),
        Some(AssetFormat::Jpeg) => jpeg(bytes),
        Some(AssetFormat::Gif) => gif::still(bytes),
        Some(AssetFormat::WebP) => webp::still(bytes),
        Some(AssetFormat::Avif) => avif_still(bytes),
        Some(AssetFormat::Bmp) => bmp::bmp(bytes),
        Some(AssetFormat::Ico) => bmp::ico(bytes),
        Some(AssetFormat::Qoi) => qoi(bytes),
        Some(format) => Err(AssetError::Kind(format)),
        None => Err(AssetError::Format),
    }
}

#[cfg(all(feature = "avif", not(target_arch = "wasm32")))]
fn avif_still(bytes: &[u8]) -> Result<RasterImage, AssetError> {
    avif::still(bytes)
}

/// Without the AV1 decoder an AVIF is a format the build does not read.
#[cfg(not(all(feature = "avif", not(target_arch = "wasm32"))))]
fn avif_still(_: &[u8]) -> Result<RasterImage, AssetError> {
    Err(AssetError::Decode(
        "AVIF decoding is not in this build (the `avif` feature)".to_owned(),
    ))
}

fn jpeg(bytes: &[u8]) -> Result<RasterImage, AssetError> {
    let (raw, orientation) = jpeg_raw(bytes)?;
    Ok(Orientation::of(orientation).apply(premultiplied(&raw.pixels, 3, raw.width, raw.height)?))
}

/// A decoder's pixels as it gave them: `channels` (gray, gray-alpha, RGB or
/// RGBA) eight-bit channels a pixel, rows top first, as stored.
pub(crate) struct Raw {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) channels: usize,
    pub(crate) pixels: Vec<u8>,
}

fn jpeg_raw(bytes: &[u8]) -> Result<(Raw, Option<u16>), AssetError> {
    let options = options().jpeg_set_out_colorspace(zune_core::colorspace::ColorSpace::RGB);
    let mut decoder = zune_jpeg::JpegDecoder::new_with_options(ZCursor::new(bytes), options);
    decoder.decode_headers().map_err(|e| error(&e))?;
    let (w, h) = decoder
        .dimensions()
        .ok_or_else(|| AssetError::Decode("the JPEG has no header".to_owned()))?;
    let (width, height) = check_size(w, h)?;
    let pixels = decoder.decode().map_err(|e| error(&e))?;
    let orientation = decoder.exif().and_then(|e| exif::orientation(e));
    let raw = Raw {
        width,
        height,
        channels: 3,
        pixels,
    };
    Ok((raw, orientation))
}

/// Decodes a PNG, JPEG or WebP to straight-alpha RGBA8 as stored: no
/// premultiplication and no turn upright, the texels a material's data
/// texture (a normal map, roughness) is sampled from unchanged.
pub(crate) fn straight(bytes: &[u8]) -> Result<Raw, AssetError> {
    let raw = match AssetFormat::sniff(bytes) {
        Some(AssetFormat::Png) => png::raw(bytes)?,
        Some(AssetFormat::Jpeg) => jpeg_raw(bytes)?.0,
        Some(AssetFormat::WebP) => webp::raw(bytes)?,
        Some(format) => return Err(AssetError::Kind(format)),
        None => return Err(AssetError::Format),
    };
    let count = raw.width as usize * raw.height as usize;
    if raw.channels == 0 || raw.pixels.len() != count * raw.channels {
        return Err(AssetError::Decode(
            "the decoder gave pixels of an unexpected shape".to_owned(),
        ));
    }
    if raw.channels == 4 {
        return Ok(raw);
    }
    let mut rgba = Vec::with_capacity(count * 4);
    for p in raw.pixels.chunks_exact(raw.channels) {
        rgba.extend_from_slice(&match raw.channels {
            3 => [p[0], p[1], p[2], 255],
            2 => [p[0], p[0], p[0], p[1]],
            _ => [p[0], p[0], p[0], 255],
        });
    }
    Ok(Raw {
        channels: 4,
        pixels: rgba,
        ..raw
    })
}

fn qoi(bytes: &[u8]) -> Result<RasterImage, AssetError> {
    let mut decoder = zune_qoi::QoiDecoder::new_with_options(ZCursor::new(bytes), options());
    decoder.decode_headers().map_err(|e| error(&e))?;
    let (w, h) = decoder
        .dimensions()
        .ok_or_else(|| AssetError::Decode("the QOI has no header".to_owned()))?;
    let (w, h) = check_size(w, h)?;
    let pixels = decoder.decode().map_err(|e| error(&e))?;
    let channels = pixels.len() / (w as usize * h as usize);
    premultiplied(&pixels, channels, w, h)
}

/// How a decoded image turns upright: one of the eight rotations and
/// mirrorings, numbered as EXIF numbers them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Orientation(u8);

// Without AVIF only EXIF's numbers name an orientation.
#[cfg_attr(
    not(all(feature = "avif", not(target_arch = "wasm32"))),
    allow(dead_code)
)]
impl Orientation {
    /// As stored.
    pub(crate) const UPRIGHT: Orientation = Orientation(1);
    /// Mirrored left to right.
    pub(crate) const MIRROR_ACROSS: Orientation = Orientation(2);
    /// A half turn.
    pub(crate) const HALF_TURN: Orientation = Orientation(3);
    /// Mirrored top to bottom.
    pub(crate) const MIRROR_DOWN: Orientation = Orientation(4);
    /// A quarter turn clockwise.
    pub(crate) const CLOCKWISE: Orientation = Orientation(6);
    /// A quarter turn anticlockwise.
    pub(crate) const ANTICLOCKWISE: Orientation = Orientation(8);

    /// EXIF orientation `tag`, upright when absent or out of range.
    pub(crate) fn of(tag: Option<u16>) -> Orientation {
        match tag {
            Some(t @ 1..=8) => Orientation(t as u8),
            _ => Orientation::UPRIGHT,
        }
    }

    /// Whether it swaps width and height.
    fn transposes(self) -> bool {
        self.0 >= 5
    }

    /// `image`'s premultiplied RGBA8 texels turned as it says.
    pub(crate) fn apply(self, image: RasterImage) -> RasterImage {
        if self == Orientation::UPRIGHT {
            return image;
        }
        let (w, h) = (image.width as usize, image.height as usize);
        let src = &image.texels;
        let (ow, oh) = if self.transposes() { (h, w) } else { (w, h) };
        let mut out = vec![0u8; src.len()];
        for y in 0..oh {
            for x in 0..ow {
                // The source texel the output one at `x, y` shows.
                let (sx, sy) = match self.0 {
                    2 => (w - 1 - x, y),
                    3 => (w - 1 - x, h - 1 - y),
                    4 => (x, h - 1 - y),
                    5 => (y, x),
                    6 => (y, h - 1 - x),
                    7 => (w - 1 - y, h - 1 - x),
                    8 => (w - 1 - y, x),
                    _ => (x, y),
                };
                let s = (sy * w + sx) * 4;
                let d = (y * ow + x) * 4;
                out[d..d + 4].copy_from_slice(&src[s..s + 4]);
            }
        }
        RasterImage::rgba8(ow as u32, oh as u32, out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 3 × 2 image whose texels are numbered in their red channel.
    fn numbered() -> RasterImage {
        let pixels = (0..6u8).flat_map(|i| [i, 0, 0, 255]).collect();
        RasterImage::rgba8(3, 2, pixels)
    }

    fn reds(image: &RasterImage) -> Vec<u8> {
        image
            .texels
            .as_chunks::<4>()
            .0
            .iter()
            .map(|t| t[0])
            .collect()
    }

    #[test]
    fn each_orientation_turns_the_texels_upright() {
        // 0 1 2
        // 3 4 5
        let turned = |tag| Orientation::of(Some(tag)).apply(numbered());
        assert_eq!(reds(&turned(1)), [0, 1, 2, 3, 4, 5]);
        assert_eq!(reds(&turned(2)), [2, 1, 0, 5, 4, 3]);
        assert_eq!(reds(&turned(3)), [5, 4, 3, 2, 1, 0]);
        assert_eq!(reds(&turned(4)), [3, 4, 5, 0, 1, 2]);
        let t = turned(6);
        assert_eq!((t.width, t.height), (2, 3));
        // A quarter turn clockwise: the left column becomes the top row.
        assert_eq!(reds(&t), [3, 0, 4, 1, 5, 2]);
        assert_eq!(reds(&turned(8)), [2, 5, 1, 4, 0, 3]);
        assert_eq!(reds(&turned(5)), [0, 3, 1, 4, 2, 5]);
        assert_eq!(reds(&turned(7)), [5, 2, 4, 1, 3, 0]);
        assert_eq!(Orientation::of(Some(9)), Orientation::UPRIGHT);
    }
}
