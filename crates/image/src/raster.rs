//! A raster asset as the renderer uploads it: an encoded image — PNG, JPEG,
//! GIF, WebP, AVIF, BMP, ICO or QOI ([`crate::decode`]) — decoded to the
//! texture convention, or a converted image ([`crate::texture`]) read in
//! place. An animated image decodes to its first frame here; its frames play
//! through [`crate::animation`].
//!
//! Decoding is bounded: a raster larger than [`MAX_DIMENSION`] on a side or
//! [`MAX_PIXELS`] in all is refused before its pixels are allocated, so hostile
//! bytes cost at most their header. Errors are values; nothing here panics on
//! input.

use std::sync::Arc;

use viso_gpu_abi::{TextureFormat, srgb};
use viso_schema::resource::{AssetBytes, AssetFormat};

/// The longest side a raster may have.
pub const MAX_DIMENSION: u32 = 16_384;

/// The most pixels a raster may have: 64 megapixels, 256 MiB decoded.
pub const MAX_PIXELS: u64 = 1 << 26;

/// Why an asset did not decode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssetError {
    /// The bytes are in no format the asset system reads.
    Format,
    /// The bytes are in a format another kind of asset uses.
    Kind(AssetFormat),
    /// The decoder refused the bytes.
    Decode(String),
    /// The raster exceeds [`MAX_DIMENSION`] or [`MAX_PIXELS`].
    TooLarge { width: u32, height: u32 },
}

impl std::fmt::Display for AssetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AssetError::Format => {
                f.write_str("not an image file in a format Viso reads, nor one the build converted")
            }
            AssetError::Kind(format) => write!(f, "a {} file draws another way", format.name()),
            AssetError::Decode(why) => f.write_str(why),
            AssetError::TooLarge { width, height } => write!(
                f,
                "a {width}×{height} image is larger than {MAX_DIMENSION} pixels on a side or \
                 {MAX_PIXELS} in all"
            ),
        }
    }
}

impl std::error::Error for AssetError {}

/// An image's texels: bytes it owns, or a range of the asset bytes it was read
/// from — borrowed for good when those are embedded in the binary.
#[derive(Clone)]
pub struct Texels {
    bytes: AssetBytes,
    start: usize,
    end: usize,
}

impl Texels {
    /// `bytes`, owned.
    pub fn owned(bytes: Vec<u8>) -> Texels {
        let end = bytes.len();
        Texels {
            bytes: AssetBytes::Shared(Arc::from(bytes)),
            start: 0,
            end,
        }
    }

    /// `bytes[start..end]`, sharing them.
    pub(crate) fn slice(bytes: AssetBytes, start: usize, end: usize) -> Texels {
        debug_assert!(start <= end && end <= bytes.len());
        Texels { bytes, start, end }
    }

    /// The texels as bytes that outlive every device, when they are embedded:
    /// a backend that samples system memory reads them where they are.
    pub fn as_static(&self) -> Option<&'static [u8]> {
        match self.bytes {
            AssetBytes::Static(bytes) => Some(&bytes[self.start..self.end]),
            AssetBytes::Shared(_) => None,
        }
    }
}

impl std::ops::Deref for Texels {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        &self.bytes[self.start..self.end]
    }
}

impl PartialEq for Texels {
    fn eq(&self, other: &Texels) -> bool {
        **self == **other
    }
}

impl Eq for Texels {}

/// A raster as a texture takes it: `width × height` texels of `format`,
/// premultiplied, the sRGB-encoded channels as stored, rows top first (rows of
/// blocks for a block-compressed format).
#[derive(Clone, PartialEq)]
pub struct RasterImage {
    /// Texels across.
    pub width: u32,
    /// Texels down.
    pub height: u32,
    /// The size it lays out at, in pixels: its texel size, unless the build
    /// scaled it down to the display or rasterized it from a vector.
    pub natural: [f32; 2],
    pub format: TextureFormat,
    /// [`TextureFormat::data_size`] bytes.
    pub texels: Texels,
}

impl std::fmt::Debug for RasterImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "RasterImage({}×{} {:?})",
            self.width, self.height, self.format
        )
    }
}

impl RasterImage {
    /// `width × height` premultiplied RGBA8 pixels (`width × height × 4`
    /// bytes, rows top first), laid out at their texel size.
    pub fn rgba8(width: u32, height: u32, pixels: Vec<u8>) -> RasterImage {
        debug_assert_eq!(pixels.len(), width as usize * height as usize * 4);
        RasterImage {
            width,
            height,
            natural: [width as f32, height as f32],
            format: TextureFormat::Rgba8Unorm,
            texels: Texels::owned(pixels),
        }
    }

    /// `width × height` straight-alpha sRGB RGBA8 pixels, premultiplied in
    /// linear light as every decoded image is.
    ///
    /// # Errors
    ///
    /// When `pixels` are not `width × height × 4` bytes.
    pub fn from_straight_rgba8(
        width: u32,
        height: u32,
        pixels: &[u8],
    ) -> Result<RasterImage, AssetError> {
        premultiplied(pixels, 4, width, height)
    }

    /// Its natural size in pixels.
    pub fn size(&self) -> [f32; 2] {
        self.natural
    }

    /// The bytes a texture of it holds.
    pub fn texture_bytes(&self) -> u64 {
        self.texels.len() as u64
    }

    /// Its texels decoded to premultiplied RGBA8 — what a device that does
    /// not sample its format is given instead; `None` when its texels are not
    /// its format's size.
    pub fn to_rgba8(&self) -> Option<RasterImage> {
        let texels = crate::codec::decode(self.format, self.width, self.height, &self.texels)?;
        Some(RasterImage {
            format: TextureFormat::Rgba8Unorm,
            texels: Texels::owned(texels.into_flattened()),
            ..self.clone()
        })
    }

    /// Its texels as premultiplied RGBA8 texels; `None` as [`to_rgba8`]
    /// says.
    ///
    /// [`to_rgba8`]: Self::to_rgba8
    pub fn rgba8_texels(&self) -> Option<Vec<[u8; 4]>> {
        crate::codec::decode(self.format, self.width, self.height, &self.texels)
    }
}

/// `width × height` as a raster's size, refused when it has no pixels or is
/// past [`MAX_DIMENSION`] or [`MAX_PIXELS`].
pub(crate) fn check_size(width: usize, height: usize) -> Result<(u32, u32), AssetError> {
    let (w, h) = (
        u32::try_from(width).unwrap_or(u32::MAX),
        u32::try_from(height).unwrap_or(u32::MAX),
    );
    if w == 0 || h == 0 {
        return Err(AssetError::Decode("the image has no pixels".to_owned()));
    }
    if w > MAX_DIMENSION || h > MAX_DIMENSION || u64::from(w) * u64::from(h) > MAX_PIXELS {
        return Err(AssetError::TooLarge {
            width: w,
            height: h,
        });
    }
    Ok((w, h))
}

/// `pixels` of `channels` channels each (gray, gray-alpha, RGB or RGBA) as
/// premultiplied RGBA8, premultiplied in linear light: a channel `c` of
/// alpha `a` stores `enc(dec(c) · a)`.
pub(crate) fn premultiplied(
    pixels: &[u8],
    channels: usize,
    w: u32,
    h: u32,
) -> Result<RasterImage, AssetError> {
    let count = w as usize * h as usize;
    if channels == 0 || pixels.len() != count * channels {
        return Err(AssetError::Decode(
            "the decoder gave pixels of an unexpected shape".to_owned(),
        ));
    }
    let mut rgba = Vec::with_capacity(count * 4);
    for p in pixels.chunks_exact(channels) {
        let (r, g, b, a) = match channels {
            4 => (p[0], p[1], p[2], p[3]),
            3 => (p[0], p[1], p[2], 255),
            2 => (p[0], p[0], p[0], p[1]),
            1 => (p[0], p[0], p[0], 255),
            _ => {
                return Err(AssetError::Decode(format!("{channels} channels per pixel")));
            }
        };
        rgba.extend_from_slice(&premultiply([r, g, b, a]));
    }
    Ok(RasterImage::rgba8(w, h, rgba))
}

/// A straight-alpha sRGB pixel premultiplied in linear light.
#[inline]
pub(crate) fn premultiply([r, g, b, a]: [u8; 4]) -> [u8; 4] {
    match a {
        255 => [r, g, b, 255],
        0 => [0; 4],
        a => {
            let a1 = f32::from(a) / 255.0;
            let premul = |v: u8| srgb::encode(srgb::decode(v) * a1);
            [premul(r), premul(g), premul(b), a]
        }
    }
}

/// Straight-alpha RGBA8 `src` premultiplied into `dst`, of the same length.
pub(crate) fn premultiply_into(src: &[u8], dst: &mut [u8]) {
    for (s, d) in src
        .as_chunks::<4>()
        .0
        .iter()
        .zip(dst.as_chunks_mut::<4>().0.iter_mut())
    {
        d.copy_from_slice(&premultiply([s[0], s[1], s[2], s[3]]));
    }
}

/// Decodes an encoded image, or reads a converted image, copying its texels.
///
/// # Errors
///
/// Bytes in no raster format, a decoder error, or a raster past the limits —
/// checked from the header, before any pixel is allocated.
pub fn decode_raster(bytes: &[u8]) -> Result<RasterImage, AssetError> {
    if crate::texture::is_texture(bytes) {
        return crate::texture::read(&AssetBytes::Shared(Arc::from(bytes)));
    }
    decode_encoded(bytes)
}

/// [`decode_raster`] of an asset's bytes: a converted image borrows its
/// texels from them, in place when they are embedded, instead of copying.
///
/// # Errors
///
/// As [`decode_raster`].
pub fn decode_raster_asset(bytes: &AssetBytes) -> Result<RasterImage, AssetError> {
    if crate::texture::is_texture(bytes) {
        return crate::texture::read(bytes);
    }
    decode_encoded(bytes)
}

fn decode_encoded(bytes: &[u8]) -> Result<RasterImage, AssetError> {
    crate::decode::still(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_format_is_read_from_the_bytes() {
        assert_eq!(
            AssetFormat::sniff(b"\x89PNG\r\n\x1a\n...."),
            Some(AssetFormat::Png)
        );
        assert_eq!(
            AssetFormat::sniff(&[0xff, 0xd8, 0xff, 0xe0]),
            Some(AssetFormat::Jpeg)
        );
        assert_eq!(
            AssetFormat::sniff(b"\xef\xbb\xbf<?xml version=\"1.0\"?>\n<svg width=\"1\"/>"),
            Some(AssetFormat::Svg)
        );
        assert_eq!(AssetFormat::sniff(b"<html><body/>"), None);
        assert_eq!(AssetFormat::sniff(b"GIF89a"), Some(AssetFormat::Gif));
        assert_eq!(decode_raster(b"GIF8"), Err(AssetError::Format));
        assert!(matches!(
            decode_raster(b"GIF89a"),
            Err(AssetError::Decode(_))
        ));
        assert_eq!(
            decode_raster(b"<svg/>"),
            Err(AssetError::Kind(AssetFormat::Svg))
        );
    }

    /// A PNG of `w × h` RGBA pixels, stored uncompressed.
    fn png(w: u32, h: u32, rgba: &[u8]) -> Vec<u8> {
        fn crc(bytes: &[u8]) -> u32 {
            let mut c = 0xffff_ffffu32;
            for &b in bytes {
                c ^= u32::from(b);
                for _ in 0..8 {
                    c = if c & 1 != 0 {
                        0xedb8_8320 ^ (c >> 1)
                    } else {
                        c >> 1
                    };
                }
            }
            !c
        }
        fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
            out.extend_from_slice(&(data.len() as u32).to_be_bytes());
            let start = out.len();
            out.extend_from_slice(kind);
            out.extend_from_slice(data);
            let sum = crc(&out[start..]);
            out.extend_from_slice(&sum.to_be_bytes());
        }
        let mut raw = Vec::new();
        for row in rgba.chunks_exact(w as usize * 4) {
            raw.push(0);
            raw.extend_from_slice(row);
        }
        // A zlib stream of stored deflate blocks.
        let mut z = vec![0x78, 0x01];
        let blocks: Vec<&[u8]> = raw.chunks(65_535).collect();
        for (i, block) in blocks.iter().enumerate() {
            z.push(u8::from(i + 1 == blocks.len()));
            let n = block.len() as u16;
            z.extend_from_slice(&n.to_le_bytes());
            z.extend_from_slice(&(!n).to_le_bytes());
            z.extend_from_slice(block);
        }
        let (mut a, mut b) = (1u32, 0u32);
        for &byte in &raw {
            a = (a + u32::from(byte)) % 65_521;
            b = (b + a) % 65_521;
        }
        z.extend_from_slice(&((b << 16) | a).to_be_bytes());
        let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&w.to_be_bytes());
        ihdr.extend_from_slice(&h.to_be_bytes());
        ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
        chunk(&mut out, b"IHDR", &ihdr);
        chunk(&mut out, b"IDAT", &z);
        chunk(&mut out, b"IEND", &[]);
        out
    }

    #[test]
    fn a_png_decodes_premultiplied_and_a_bad_one_is_an_error() {
        let pixels = [
            255, 0, 0, 255, 0, 255, 0, 128, 0, 0, 255, 0, 10, 20, 30, 255,
        ];
        let image = decode_raster(&png(2, 2, &pixels)).expect("decodes");
        assert_eq!((image.width, image.height), (2, 2));
        assert_eq!(image.format, TextureFormat::Rgba8Unorm);
        // Half-covered green keeps half its light: sRGB 188.
        assert_eq!(
            *image.texels,
            [255, 0, 0, 255, 0, 188, 0, 128, 0, 0, 0, 0, 10, 20, 30, 255]
        );
        let bytes = png(2, 2, &pixels);
        for end in [8, 20, 40, bytes.len() - 13] {
            assert!(decode_raster(&bytes[..end]).is_err(), "{end}");
        }
        let mut huge = png(1, 1, &[0; 4]);
        // IHDR width at byte 16: claim a raster past the limit.
        huge[16..20].copy_from_slice(&(MAX_DIMENSION + 1).to_be_bytes());
        assert!(decode_raster(&huge).is_err());
    }

    #[test]
    fn a_jpeg_decodes_opaque() {
        let image = decode_raster(include_bytes!("fixtures/two_tone.jpg")).expect("decodes");
        assert_eq!((image.width, image.height), (8, 4));
        let at = |x: usize, y: usize| {
            let i = (y * 8 + x) * 4;
            [
                image.texels[i],
                image.texels[i + 1],
                image.texels[i + 2],
                image.texels[i + 3],
            ]
        };
        let [r, _, b, a] = at(1, 1);
        assert!(r > 150 && b < 100 && a == 255, "{:?}", at(1, 1));
        let [r, _, b, a] = at(6, 2);
        assert!(b > 150 && r < 100 && a == 255, "{:?}", at(6, 2));
    }
}
