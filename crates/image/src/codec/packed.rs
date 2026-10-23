//! The 16-bit formats: `Rgb565Unorm`, `Rgba4444Unorm` and `Rgba5551Unorm`,
//! little-endian `u16`s with red in the high bits.
//!
//! Quantizing to 4–6 bits per channel bands a smooth ramp, so the encoder
//! rounds every channel against a 4×4 ordered dither — the same thresholds
//! the CPU raster dithers an RGB565 gradient with — and keeps the result
//! premultiplied: no color channel brighter in linear light than its alpha.

use viso_gpu_abi::{Packed16, TextureFormat, srgb};

/// The 4×4 Bayer matrix as rounding thresholds in `0..255`.
const BAYER: [[u32; 4]; 4] = {
    const M: [[u32; 4]; 4] = [[0, 8, 2, 10], [12, 4, 14, 6], [3, 11, 1, 9], [15, 7, 13, 5]];
    let mut out = [[0u32; 4]; 4];
    let mut y = 0;
    while y < 4 {
        let mut x = 0;
        while x < 4 {
            out[y][x] = M[y][x] * 16 + 8;
            x += 1;
        }
        y += 1;
    }
    out
};

/// `v` narrowed to `bits` bits, rounding up at threshold `t` of `0..255`.
#[inline]
fn narrow(v: u8, bits: u32, t: u32) -> u16 {
    let max = (1u32 << bits) - 1;
    ((u32::from(v) * max + t) / 255) as u16
}

/// `texels` of a `width`-wide image as `format`'s little-endian bytes.
pub(super) fn encode(format: TextureFormat, width: u32, texels: &[[u8; 4]]) -> Vec<u8> {
    let layout = Packed16::of(format).expect("a 16-bit format");
    let mut out = Vec::with_capacity(texels.len() * 2);
    for (i, &[r, g, b, a]) in texels.iter().enumerate() {
        let (x, y) = (i % width as usize, i / width as usize);
        let t = BAYER[y & 3][x & 3];
        let v = match layout {
            Packed16::Rgb565 => (narrow(r, 5, t) << 11) | (narrow(g, 6, t) << 5) | narrow(b, 5, t),
            Packed16::Rgba4444 => {
                let a4 = narrow(a, 4, t);
                // The brightest level whose light stays within the alpha.
                let cap = narrow(srgb::encode(f32::from(a4) / 15.0), 4, 0);
                let c = |v: u8| narrow(v, 4, t).min(cap);
                (c(r) << 12) | (c(g) << 8) | (c(b) << 4) | a4
            }
            Packed16::Rgba5551 => {
                if a < 128 {
                    0
                } else {
                    (narrow(r, 5, t) << 11) | (narrow(g, 5, t) << 6) | (narrow(b, 5, t) << 1) | 1
                }
            }
        };
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}

/// `data`, little-endian texels of `format`, widened to RGBA8 as a sampler
/// reads them.
pub(super) fn decode(format: TextureFormat, data: &[u8]) -> Vec<[u8; 4]> {
    let layout = Packed16::of(format).expect("a 16-bit format");
    data.as_chunks::<2>()
        .0
        .iter()
        .map(|&v| layout.rgba8(u16::from_le_bytes(v)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::super::{Effort, decode as decode_image, encode as encode_image, fixtures, psnr};
    use super::*;

    #[test]
    fn exact_levels_round_trip_and_premultiplication_holds() {
        let texels = [
            [0, 0, 0, 255],
            [255, 255, 255, 255],
            [0, 0, 0, 0],
            [136, 68, 0, 136],
        ];
        for format in [TextureFormat::Rgba4444Unorm, TextureFormat::Rgba5551Unorm] {
            let bytes = encode_image(format, 4, 1, &texels, Effort::Fast);
            let back = decode_image(format, 4, 1, &bytes).expect("decodes");
            assert_eq!(back[0], [0, 0, 0, 255], "{format:?}");
            assert_eq!(back[1], [255, 255, 255, 255], "{format:?}");
            assert_eq!(back[2], [0, 0, 0, 0], "{format:?}");
            for p in &back {
                let a = f32::from(p[3]) / 255.0;
                assert!(
                    p[..3].iter().all(|&c| srgb::decode(c) <= a + 0.005),
                    "{format:?} {p:?}"
                );
            }
        }
        let back = decode_image(
            TextureFormat::Rgba4444Unorm,
            4,
            1,
            &encode_image(TextureFormat::Rgba4444Unorm, 4, 1, &texels, Effort::Fast),
        )
        .expect("decodes");
        assert_eq!(back[3], [136, 68, 0, 136], "4-bit levels are exact");
    }

    #[test]
    fn a_dithered_ramp_keeps_its_mean() {
        // A flat level between two 5-bit steps: the dither mixes both, so the
        // mean over the 4×4 cell stays close to the source.
        let level = 100u8;
        let texels = vec![[level, level, level, 255]; 16];
        let bytes = encode_image(TextureFormat::Rgb565Unorm, 4, 4, &texels, Effort::Fast);
        let back = decode_image(TextureFormat::Rgb565Unorm, 4, 4, &bytes).expect("decodes");
        let mean = back.iter().map(|p| u32::from(p[0])).sum::<u32>() as f32 / 16.0;
        assert!((mean - f32::from(level)).abs() < 2.0, "{mean}");
        let distinct: std::collections::BTreeSet<u8> = back.iter().map(|p| p[0]).collect();
        assert!(distinct.len() > 1, "the dither mixes two levels");
    }

    #[test]
    fn a_photo_survives_rgb565() {
        let photo = fixtures::photo(64, 48);
        let bytes = encode_image(TextureFormat::Rgb565Unorm, 64, 48, &photo, Effort::Fast);
        assert_eq!(bytes.len(), 64 * 48 * 2);
        let back = decode_image(TextureFormat::Rgb565Unorm, 64, 48, &bytes).expect("decodes");
        let db = psnr(&photo, &back);
        assert!(db > 36.0, "{db:.1} dB");
    }
}
