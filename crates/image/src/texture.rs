//! A converted image: the texels a target samples, behind a fixed header, laid
//! out to be read where it lies — in the binary, or memory-mapped from flash.
//!
//! The build converts every raster (and, when asked, every SVG) of a release
//! into the format its target draws fastest and stores smallest; the runtime
//! reads the header, borrows the texels and hands them to the backend, with
//! no decode on the device ([`crate::raster::decode_raster_asset`]).
//!
//! | offset | bytes | field |
//! |---|---|---|
//! | 0 | 4 | magic `VTX1` |
//! | 4 | 1 | format ([`format_code`]) |
//! | 5 | 3 | zero |
//! | 8 | 4 | width in texels, `u32` LE |
//! | 12 | 4 | height in texels, `u32` LE |
//! | 16 | 4 | natural width in pixels, `f32` LE |
//! | 20 | 4 | natural height in pixels, `f32` LE |
//! | 24 | 4 | texel bytes, `u32` LE |
//! | 28 | 4 | zero |
//! | 32 | … | texels: rows of texels, or of blocks, top first |
//!
//! The texels start [`TEXEL_OFFSET`] bytes in, a multiple of
//! [`TEXEL_ALIGN`]: a blob embedded at an 8-byte-aligned address (as a view
//! package lays its assets out) has them aligned for any texel size. A
//! block-compressed image is whole blocks — its size a multiple of the block
//! on both axes — so every backend can create it as is. The natural size is
//! what the image lays out at: its texel size, unless the build scaled it to
//! the display or rasterized it from a vector at a scale.

use viso_gpu_abi::TextureFormat;
use viso_schema::resource::AssetBytes;

use crate::raster::{AssetError, MAX_DIMENSION, RasterImage, Texels};

/// The first four bytes of a converted image.
pub const MAGIC: [u8; 4] = *b"VTX1";

/// Where the texels start.
pub const TEXEL_OFFSET: usize = 32;

/// The alignment the texels keep relative to the blob's start.
pub const TEXEL_ALIGN: usize = 8;

const _: () = assert!(TEXEL_OFFSET.is_multiple_of(TEXEL_ALIGN));

/// Whether `bytes` are a converted image (by their magic alone).
pub fn is_texture(bytes: &[u8]) -> bool {
    bytes.starts_with(&MAGIC)
}

/// The formats a converted image may hold, by their stable code.
const CODES: [(u8, TextureFormat); 14] = [
    (1, TextureFormat::Rgba8Unorm),
    (2, TextureFormat::Bgra8Unorm),
    (3, TextureFormat::Rgb565Unorm),
    (4, TextureFormat::Rgba4444Unorm),
    (5, TextureFormat::Rgba5551Unorm),
    (16, TextureFormat::Bc1RgbaUnorm),
    (17, TextureFormat::Bc3RgbaUnorm),
    (18, TextureFormat::Bc7RgbaUnorm),
    (32, TextureFormat::Etc2Rgb8Unorm),
    (33, TextureFormat::Etc2Rgb8A1Unorm),
    (34, TextureFormat::Etc2Rgba8Unorm),
    (48, TextureFormat::Astc4x4Unorm),
    (49, TextureFormat::Astc6x6Unorm),
    (50, TextureFormat::Astc8x8Unorm),
];

/// The code a converted image stores `format` under; `None` for a format no
/// image is converted to.
pub fn format_code(format: TextureFormat) -> Option<u8> {
    CODES.iter().find(|(_, f)| *f == format).map(|(c, _)| *c)
}

/// The format stored under `code`.
pub fn format_of(code: u8) -> Option<TextureFormat> {
    CODES.iter().find(|(c, _)| *c == code).map(|(_, f)| *f)
}

/// `image` as a converted image's bytes.
///
/// # Panics
///
/// When its format has no [`format_code`], its texels are not
/// [`TextureFormat::data_size`] bytes, or a block format's size is not whole
/// blocks — what the converter guarantees.
pub fn write(image: &RasterImage) -> Vec<u8> {
    let code = format_code(image.format).expect("a format a converted image holds");
    let block = image.format.block();
    assert!(
        image.width.is_multiple_of(block.width) && image.height.is_multiple_of(block.height),
        "a {:?} image is whole blocks",
        image.format
    );
    let len = image.format.data_size(image.width, image.height);
    assert_eq!(image.texels.len(), len, "texel bytes");
    let mut out = Vec::with_capacity(TEXEL_OFFSET + len);
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&[code, 0, 0, 0]);
    out.extend_from_slice(&image.width.to_le_bytes());
    out.extend_from_slice(&image.height.to_le_bytes());
    out.extend_from_slice(&image.natural[0].to_le_bytes());
    out.extend_from_slice(&image.natural[1].to_le_bytes());
    out.extend_from_slice(&(len as u32).to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&image.texels);
    out
}

/// The image `bytes` hold, its texels borrowed from them.
///
/// # Errors
///
/// A header that is short, names an unknown format, holds non-zero reserved
/// bytes, a size of no texels or past [`MAX_DIMENSION`], a block image that is
/// not whole blocks, a natural size that is not finite and positive, or texel
/// bytes that are not exactly the format's size for the image.
pub fn read(bytes: &AssetBytes) -> Result<RasterImage, AssetError> {
    let fail = |why: &str| AssetError::Decode(format!("a converted image {why}"));
    let header = bytes
        .get(..TEXEL_OFFSET)
        .ok_or_else(|| fail("is shorter than its header"))?;
    if !is_texture(header) {
        return Err(AssetError::Format);
    }
    let u32_at = |at: usize| u32::from_le_bytes(header[at..at + 4].try_into().expect("4 bytes"));
    let f32_at = |at: usize| f32::from_le_bytes(header[at..at + 4].try_into().expect("4 bytes"));
    let format = format_of(header[4]).ok_or_else(|| fail("names an unknown format"))?;
    if header[5..8] != [0; 3] || header[28..32] != [0; 4] {
        return Err(fail("has non-zero reserved bytes"));
    }
    let (width, height) = (u32_at(8), u32_at(12));
    if width == 0 || height == 0 || width > MAX_DIMENSION || height > MAX_DIMENSION {
        return Err(AssetError::TooLarge { width, height });
    }
    let block = format.block();
    if !width.is_multiple_of(block.width) || !height.is_multiple_of(block.height) {
        return Err(fail("is not whole blocks"));
    }
    let natural = [f32_at(16), f32_at(20)];
    if natural.iter().any(|v| !v.is_finite() || *v <= 0.0) {
        return Err(fail("has no natural size"));
    }
    let len = format.data_size(width, height);
    if u32_at(24) as usize != len || bytes.len() != TEXEL_OFFSET + len {
        return Err(fail("does not hold its texels exactly"));
    }
    Ok(RasterImage {
        width,
        height,
        natural,
        format,
        texels: Texels::slice(bytes.clone(), TEXEL_OFFSET, TEXEL_OFFSET + len),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn sample() -> RasterImage {
        RasterImage {
            width: 4,
            height: 4,
            natural: [2.0, 2.0],
            format: TextureFormat::Bc1RgbaUnorm,
            texels: Texels::owned((0..8).collect()),
        }
    }

    #[test]
    fn a_written_image_reads_back_borrowing_its_texels() {
        let image = sample();
        let bytes = write(&image);
        assert_eq!(bytes.len(), TEXEL_OFFSET + 8);
        assert!(is_texture(&bytes));
        static EMBEDDED: std::sync::OnceLock<&'static [u8]> = std::sync::OnceLock::new();
        let embedded = *EMBEDDED.get_or_init(|| Box::leak(bytes.clone().into_boxed_slice()));
        let read_back = read(&AssetBytes::Static(embedded)).expect("reads");
        assert_eq!(read_back, image);
        let texels = read_back.texels.as_static().expect("borrowed in place");
        assert_eq!(texels.as_ptr(), embedded[TEXEL_OFFSET..].as_ptr());
        let shared = read(&AssetBytes::Shared(Arc::from(bytes))).expect("reads");
        assert_eq!(shared, image);
        assert!(shared.texels.as_static().is_none());
    }

    #[test]
    fn a_malformed_header_is_an_error() {
        let good = write(&sample());
        let read_with = |edit: &dyn Fn(&mut Vec<u8>)| {
            let mut bytes = good.clone();
            edit(&mut bytes);
            read(&AssetBytes::Shared(Arc::from(bytes)))
        };
        assert!(read_with(&|_| ()).is_ok());
        assert!(read_with(&|b| b.truncate(20)).is_err(), "short");
        assert!(read_with(&|b| b[4] = 99).is_err(), "unknown format");
        assert!(read_with(&|b| b[6] = 1).is_err(), "reserved");
        assert!(read_with(&|b| b[8] = 5).is_err(), "not whole blocks");
        assert!(read_with(&|b| b[16..20].copy_from_slice(&f32::NAN.to_le_bytes())).is_err());
        assert!(read_with(&|b| b.push(0)).is_err(), "trailing byte");
        assert!(read_with(&|b| b[8..12].copy_from_slice(&0u32.to_le_bytes())).is_err());
        assert_eq!(
            read(&AssetBytes::Static(
                b"\x89PNG\r\n\x1a\n........................"
            )),
            Err(AssetError::Format)
        );
    }

    #[test]
    fn every_code_names_one_format() {
        for (code, format) in CODES {
            assert_eq!(format_of(code), Some(format));
            assert_eq!(format_code(format), Some(code));
        }
        assert_eq!(format_code(TextureFormat::R8Unorm), None);
    }
}
