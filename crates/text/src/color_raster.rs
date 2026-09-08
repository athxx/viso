//! Color-bitmap glyph rasterization: an embedded PNG (or premultiplied BGRA)
//! emoji strike → a premultiplied-RGBA bitmap ready for the color atlas.
//!
//! Modern color emoji fonts store each glyph as a small raster image in an
//! `sbix`/`CBDT` strike rather than an outline. `ttf-parser` hands that image
//! back as raw bytes plus a format tag; this module decodes the two formats
//! that cover bitmap emoji — PNG (the common case, via `zune-png`) and the
//! `CBDT` premultiplied-BGRA32 blob — into a single-orientation, premultiplied
//! **RGBA** buffer. That matches the color atlas (`Rgba8Unorm`) and the Image
//! pipeline's `texel * tint` blend: with a white opaque tint the premultiplied
//! texel passes through unchanged.
//!
//! Outline color formats (COLR layers, SVG) are intentionally out of scope —
//! bitmap strikes cover the emoji the text stack needs, and an outline color
//! path would duplicate the SDF machinery for no coverage gain here.

use crate::font::FontFace;
use zune_core::bytestream::ZCursor;
use zune_core::colorspace::ColorSpace;
use zune_png::PngDecoder;

/// A decoded color glyph: a premultiplied-RGBA bitmap plus the strike metadata
/// layout needs to place and scale it.
pub struct ColorGlyph {
    /// Bitmap width in texels (the decoded image's own width).
    pub width: u32,
    /// Bitmap height in texels.
    pub height: u32,
    /// Premultiplied RGBA bytes, row-major top-left origin, `width * height * 4`
    /// long. Premultiplied so the Image pipeline's `texel * tint` (white tint)
    /// is an identity blend.
    pub rgba: Vec<u8>,
    /// Pixels-per-em of the selected strike: the density `rgba` was authored at,
    /// so the caller can scale the strike to the requested font size.
    pub pixels_per_em: u16,
    /// Strike origin offset from the pen point, in strike pixels (`sbix`/`CBDT`
    /// place the image relative to the glyph origin, not the top-left).
    pub origin_px: [f32; 2],
}

/// Decode the color-bitmap image for `glyph_id` in `face` at (or above)
/// `dpx_per_em` density, if the face has one in a supported format. Returns
/// `None` for glyphs with no strike or an unsupported strike format (the caller
/// falls back to outline rasterization).
pub fn rasterize_color_glyph(
    face: &FontFace,
    glyph_id: u16,
    dpx_per_em: f32,
) -> Option<ColorGlyph> {
    let ttf = face.ttf();
    // Ask for a strike at least as dense as the request so upscaling never
    // happens — the atlas box-downsamples a larger strike to display size.
    let ppem = dpx_per_em.ceil().max(1.0) as u16;
    let image = ttf.glyph_raster_image(ttf_parser::GlyphId(glyph_id), ppem)?;

    let (width, height, rgba) = match image.format {
        ttf_parser::RasterImageFormat::PNG => decode_png_premul_rgba(image.data)?,
        ttf_parser::RasterImageFormat::BitmapPremulBgra32 => {
            // Already premultiplied; swizzle BGRA → RGBA in place.
            let (w, h) = (image.width as usize, image.height as usize);
            let expected = w * h * 4;
            if image.data.len() < expected || w == 0 || h == 0 {
                return None;
            }
            let mut rgba = Vec::with_capacity(expected);
            for px in image.data[..expected].as_chunks::<4>().0 {
                // src is B, G, R, A → RGBA
                rgba.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
            }
            (w as u32, h as u32, rgba)
        }
        // Monochrome / grayscale bitmap strikes are not color emoji; leave them
        // to the outline path (they usually also carry an outline).
        _ => return None,
    };

    Some(ColorGlyph {
        width,
        height,
        rgba,
        pixels_per_em: image.pixels_per_em.max(1),
        origin_px: [image.x as f32, image.y as f32],
    })
}

/// Decode a PNG strike into premultiplied RGBA. Expands palette/gray to RGBA and
/// premultiplies straight-alpha color by its alpha so the atlas stores the same
/// premultiplied convention as the `CBDT` path. Returns `None` on any decode
/// failure — a corrupt strike simply declines the color path.
fn decode_png_premul_rgba(data: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let mut decoder = PngDecoder::new(ZCursor::new(data));
    decoder.decode_headers().ok()?;
    let (width, height) = decoder.dimensions()?;
    if width == 0 || height == 0 {
        return None;
    }
    let colorspace = decoder.colorspace()?;
    let buffer = decoder.decode().ok()?.u8()?;
    let n = colorspace.num_components();
    if buffer.len() < width * height * n {
        return None;
    }

    let mut rgba = Vec::with_capacity(width * height * 4);
    match (colorspace, n) {
        (ColorSpace::RGBA, 4) => {
            // Straight-alpha RGBA → premultiply.
            for px in buffer.as_chunks::<4>().0 {
                let a = px[3] as u32;
                rgba.push(mul_alpha(px[0], a));
                rgba.push(mul_alpha(px[1], a));
                rgba.push(mul_alpha(px[2], a));
                rgba.push(px[3]);
            }
        }
        (ColorSpace::RGB, 3) => {
            for px in buffer.as_chunks::<3>().0 {
                rgba.extend_from_slice(&[px[0], px[1], px[2], 255]);
            }
        }
        (ColorSpace::LumaA, 2) => {
            for px in buffer.as_chunks::<2>().0 {
                let a = px[1] as u32;
                let g = mul_alpha(px[0], a);
                rgba.extend_from_slice(&[g, g, g, px[1]]);
            }
        }
        (ColorSpace::Luma, 1) => {
            for &g in buffer.iter() {
                rgba.extend_from_slice(&[g, g, g, 255]);
            }
        }
        _ => return None,
    }
    Some((width as u32, height as u32, rgba))
}

/// Premultiply an 8-bit channel by an 8-bit alpha, rounding to nearest.
#[inline]
fn mul_alpha(c: u8, a: u32) -> u8 {
    ((c as u32 * a + 127) / 255) as u8
}

#[cfg(test)]
mod tests {
    use super::*;
    use zune_core::bit_depth::BitDepth;
    use zune_core::options::EncoderOptions;
    use zune_png::PngEncoder;

    // Round-trip: encode raw pixels of `space` at 2x2, decode back to
    // premultiplied RGBA. `decode_png_premul_rgba` is what the sbix/CBDT PNG
    // path feeds every emoji strike through, so exercising it end to end here
    // is the real coverage — no font fixture needed.
    fn encode_png(pixels: &[u8], w: usize, h: usize, space: ColorSpace) -> Vec<u8> {
        let opts = EncoderOptions::new(w, h, space, BitDepth::Eight);
        let mut out = Vec::new();
        PngEncoder::new(pixels, opts).encode(&mut out).unwrap();
        out
    }

    #[test]
    fn mul_alpha_rounds_to_nearest() {
        assert_eq!(mul_alpha(255, 255), 255);
        assert_eq!(mul_alpha(0, 255), 0);
        assert_eq!(mul_alpha(255, 0), 0);
        // 200 * 128 / 255 = 100.39 → 100 after +127 rounding.
        assert_eq!(mul_alpha(200, 128), 100);
    }

    #[test]
    fn rgba_png_is_premultiplied() {
        // One opaque red, one half-alpha green, one transparent, one white.
        let pixels = [
            255, 0, 0, 255, // opaque red
            0, 255, 0, 128, // half green
            10, 20, 30, 0, // fully transparent
            255, 255, 255, 255, // opaque white
        ];
        let png = encode_png(&pixels, 2, 2, ColorSpace::RGBA);
        let (w, h, rgba) = decode_png_premul_rgba(&png).unwrap();
        assert_eq!((w, h), (2, 2));
        assert_eq!(&rgba[0..4], &[255, 0, 0, 255]);
        // green premultiplied by 128.
        assert_eq!(&rgba[4..8], &[0, mul_alpha(255, 128), 0, 128]);
        // transparent → all channels zero regardless of source color.
        assert_eq!(&rgba[8..12], &[0, 0, 0, 0]);
        assert_eq!(&rgba[12..16], &[255, 255, 255, 255]);
    }

    #[test]
    fn rgb_png_is_opaque() {
        let pixels = [255, 0, 0, 0, 255, 0]; // red, green, 2x1
        let png = encode_png(&pixels, 2, 1, ColorSpace::RGB);
        let (w, h, rgba) = decode_png_premul_rgba(&png).unwrap();
        assert_eq!((w, h), (2, 1));
        assert_eq!(&rgba[..], &[255, 0, 0, 255, 0, 255, 0, 255]);
    }

    #[test]
    fn corrupt_png_declines() {
        assert!(decode_png_premul_rgba(&[0u8; 8]).is_none());
        assert!(decode_png_premul_rgba(&[]).is_none());
    }
}
