//! BMP, and the Windows icon whose images are PNGs or bare BMP bitmaps.
//!
//! An uncompressed 32-bit bitmap's fourth channel is its alpha, unless it is
//! zero throughout — then it is padding and the bitmap shows opaque. An icon shows its largest
//! image; a bitmap image of fewer than 32 bits, or one whose alpha is empty,
//! takes its transparency from the one-bit mask after its colours.

use zune_core::bytestream::ZCursor;

use super::options;
use crate::raster::{AssetError, RasterImage, check_size, premultiplied};

fn error(e: &zune_bmp::BmpDecoderErrors) -> AssetError {
    AssetError::Decode(format!("{e:?}"))
}

pub(crate) fn bmp(bytes: &[u8]) -> Result<RasterImage, AssetError> {
    let (pixels, w, h) = decode(bytes)?;
    premultiplied(&pixels, 4, w, h)
}

/// A BMP's texels as straight RGBA8, with its size.
fn decode(bytes: &[u8]) -> Result<(Vec<u8>, u32, u32), AssetError> {
    let mut decoder = zune_bmp::BmpDecoder::new_with_options(ZCursor::new(bytes), options());
    decoder.decode_headers().map_err(|e| error(&e))?;
    let (w, h) = decoder
        .dimensions()
        .ok_or_else(|| AssetError::Decode("the BMP has no header".to_owned()))?;
    let (w, h) = check_size(w, h)?;
    let pixels = decoder.decode().map_err(|e| error(&e))?;
    let count = w as usize * h as usize;
    let mut rgba = match pixels.len() / count {
        4 => pixels,
        3 => pixels
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|p| [p[0], p[1], p[2], 255])
            .collect(),
        1 => pixels.iter().flat_map(|&v| [v, v, v, 255]).collect(),
        n => return Err(AssetError::Decode(format!("{n} channels per pixel"))),
    };
    if let Some(alpha) = uncompressed_alpha(bytes, w as usize, h as usize) {
        for (px, a) in rgba.as_chunks_mut::<4>().0.iter_mut().zip(alpha) {
            px[3] = a;
        }
    }
    if rgba.as_chunks::<4>().0.iter().all(|p| p[3] == 0) {
        rgba.as_chunks_mut::<4>()
            .0
            .iter_mut()
            .for_each(|p| p[3] = 255);
    }
    Ok((rgba, w, h))
}

/// The fourth byte of each texel of an uncompressed 32-bit BMP, rows top
/// first — the decoder reads it as padding; `None` for any other layout.
fn uncompressed_alpha(bytes: &[u8], w: usize, h: usize) -> Option<impl Iterator<Item = u8> + '_> {
    let u32_at = |at: usize| Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?));
    let data = u32_at(10)? as usize;
    let bottom_up = (u32_at(22)? as i32) > 0;
    let bits = u16::from_le_bytes(bytes.get(28..30)?.try_into().ok()?);
    if bits != 32 || u32_at(30)? != 0 {
        return None;
    }
    let pixels = bytes.get(data..data.checked_add(w * h * 4)?)?;
    Some((0..h).flat_map(move |y| {
        let row = if bottom_up { h - 1 - y } else { y };
        pixels[row * w * 4..][..w * 4]
            .iter()
            .skip(3)
            .step_by(4)
            .copied()
    }))
}

/// An icon directory entry: where its image lies and its bit depth.
struct Entry {
    offset: usize,
    len: usize,
    width: u32,
    height: u32,
    bits: u16,
}

/// The entries of an icon's directory whose images lie within it.
fn entries(bytes: &[u8]) -> impl Iterator<Item = Entry> + '_ {
    let count = u16::from_le_bytes([bytes[4], bytes[5]]) as usize;
    (0..count).filter_map(move |i| {
        let e = bytes.get(6 + i * 16..6 + i * 16 + 16)?;
        let side = |b: u8| if b == 0 { 256 } else { u32::from(b) };
        let len = u32::from_le_bytes([e[8], e[9], e[10], e[11]]) as usize;
        let offset = u32::from_le_bytes([e[12], e[13], e[14], e[15]]) as usize;
        (len > 0 && offset.checked_add(len)? <= bytes.len()).then(|| Entry {
            offset,
            len,
            width: side(e[0]),
            height: side(e[1]),
            bits: u16::from_le_bytes([e[6], e[7]]),
        })
    })
}

pub(crate) fn ico(bytes: &[u8]) -> Result<RasterImage, AssetError> {
    // The largest image; of two as large, the deeper.
    let entry = entries(bytes)
        .max_by_key(|e| (u64::from(e.width) * u64::from(e.height), e.bits))
        .ok_or_else(|| AssetError::Decode("the icon holds no image".to_owned()))?;
    let image = &bytes[entry.offset..entry.offset + entry.len];
    if image.starts_with(b"\x89PNG\r\n\x1a\n") {
        return super::png::still(image);
    }
    let (pixels, w, h) = dib(image)?;
    premultiplied(&pixels, 4, w, h)
}

/// An icon's bare bitmap — an info header, colours, then the mask — as
/// straight RGBA8 with its size.
fn dib(dib: &[u8]) -> Result<(Vec<u8>, u32, u32), AssetError> {
    let bad = |why: &str| AssetError::Decode(format!("the icon's bitmap {why}"));
    let u32_at = |at: usize| -> Result<u32, AssetError> {
        let b = dib.get(at..at + 4).ok_or_else(|| bad("is cut short"))?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    };
    let header = u32_at(0)? as usize;
    if header < 40 || header > dib.len() {
        return Err(bad("has no info header"));
    }
    let width = u32_at(4)? as i32;
    // The height counts the colours and the mask.
    let height = (u32_at(8)? as i32).unsigned_abs() / 2;
    let bits = u16::from_le_bytes([dib[14], dib[15]]);
    let compression = u32_at(16)?;
    let used = u32_at(32)? as usize;
    if width <= 0 || height == 0 {
        return Err(bad("has no pixels"));
    }
    let (w, h) = check_size(width as usize, height as usize)?;
    let colours = match bits {
        1 | 4 | 8 if used == 0 => 1 << bits,
        1 | 4 | 8 => used.min(1 << bits),
        _ => 0,
    };
    // A plain info header keeps its channel masks after it.
    let masks = match (header, compression) {
        (40, 3) => 12,
        (40, 6) => 16,
        _ => 0,
    };
    let data = header + masks + colours * 4;
    if data > dib.len() {
        return Err(bad("is cut short"));
    }
    // Wrapped in a file header, the bitmap decodes as a BMP of its height.
    let mut file = Vec::with_capacity(14 + dib.len());
    file.extend_from_slice(b"BM");
    file.extend_from_slice(&((14 + dib.len()) as u32).to_le_bytes());
    file.extend_from_slice(&[0; 4]);
    file.extend_from_slice(&((14 + data) as u32).to_le_bytes());
    file.extend_from_slice(dib);
    file[14 + 8..14 + 12].copy_from_slice(&(h as i32).to_le_bytes());
    let (mut pixels, _, _) = decode(&file)?;
    let alpha = bits == 32 && pixels.as_chunks::<4>().0.iter().any(|p| p[3] != 255);
    if !alpha {
        // The mask follows the colours, rows bottom first, a bit a pixel,
        // each row padded to four bytes; a set bit is transparent.
        let stride = (w as usize * usize::from(bits)).div_ceil(32) * 4;
        let mask_stride = (w as usize).div_ceil(32) * 4;
        let at = data + stride * h as usize;
        if let Some(mask) = dib.get(at..at + mask_stride * h as usize) {
            for y in 0..h as usize {
                let row = &mask[(h as usize - 1 - y) * mask_stride..][..mask_stride];
                for x in 0..w as usize {
                    if row[x / 8] & (0x80 >> (x % 8)) != 0 {
                        pixels[(y * w as usize + x) * 4 + 3] = 0;
                    }
                }
            }
        }
    }
    Ok((pixels, w, h))
}
