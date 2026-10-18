//! BC1, BC3 and BC7, the desktop GPUs' formats: 4×4 texels per block,
//! sampled as stored (no sRGB decode).
//!
//! - BC1: two RGB565 endpoints and a 2-bit index per texel, 8 bytes.
//!   Endpoints in descending order pick four opaque colors; otherwise three
//!   colors and transparent black, the punch-through alpha.
//! - BC3: a BC4 alpha block before a BC1 color block that always decodes four
//!   colors, 16 bytes.
//! - BC7: 16 bytes in one of eight modes trading subsets (one to three, each
//!   its own endpoint pair, laid out by a partition table), endpoint
//!   precision, index precision and alpha.
//!
//! The encoders fit endpoints along the principal axis of each subset's
//! texels and refine them by least squares against the indices they select;
//! `Thorough` adds a cluster fit or more partitions and p-bits, and a search
//! of the neighboring endpoint codes. Each keeps the candidate whose decoded
//! texels lie closest to the source in squared error over RGBA. A
//! transparent texel is premultiplied black and always decodes as such: the
//! error charges a penalty for one that does not, and every encoder has a
//! candidate that pins an endpoint to black.

mod alpha;
mod bc7;
mod color;

use super::Effort;

/// The error charged for a transparent texel that decodes visible: more than
/// any block of visible error, so a candidate keeping it transparent wins.
const PENALTY: u32 = 1 << 22;

/// One 4×4 block of premultiplied texels, row by row, as BC1. A texel with
/// alpha under 128 is punched through to transparent black.
pub fn encode_bc1(texels: &[[u8; 4]; 16], effort: Effort) -> [u8; 8] {
    let holes = mask(texels, |t| t[3] < 128);
    color::encode(texels, color::Format::Bc1 { holes }, effort)
}

/// A BC1 block decoded, row by row.
pub fn decode_bc1(block: &[u8; 8]) -> [[u8; 4]; 16] {
    color::decode(block, false)
}

/// One 4×4 block of premultiplied texels, row by row, as BC3.
pub fn encode_bc3(texels: &[[u8; 4]; 16], effort: Effort) -> [u8; 16] {
    let alpha = alpha::encode(&texels.map(|t| t[3]), effort);
    let black = mask(texels, |t| *t == [0; 4]);
    let color = color::encode(texels, color::Format::Bc3 { black }, effort);
    let mut out = [0; 16];
    out[..8].copy_from_slice(&alpha);
    out[8..].copy_from_slice(&color);
    out
}

/// A BC3 block decoded, row by row.
pub fn decode_bc3(block: &[u8; 16]) -> [[u8; 4]; 16] {
    let (alpha, color) = block.split_at(8);
    let alpha = alpha::decode(alpha.try_into().expect("8 bytes"));
    let mut out = color::decode(color.try_into().expect("8 bytes"), true);
    for (t, a) in out.iter_mut().zip(alpha) {
        t[3] = a;
    }
    out
}

/// One 4×4 block of premultiplied texels, row by row, as BC7.
pub fn encode_bc7(texels: &[[u8; 4]; 16], effort: Effort) -> [u8; 16] {
    bc7::encode(texels, effort)
}

/// A BC7 block decoded, row by row; a reserved mode decodes transparent
/// black.
pub fn decode_bc7(block: &[u8; 16]) -> [[u8; 4]; 16] {
    bc7::decode(block)
}

/// The texels of a block `pick` holds for, a bit per texel.
fn mask(texels: &[[u8; 4]; 16], pick: impl Fn(&[u8; 4]) -> bool) -> u16 {
    texels
        .iter()
        .enumerate()
        .filter(|(_, t)| pick(t))
        .fold(0, |m, (i, _)| m | 1 << i)
}

/// The texels of `members`, by index.
fn members(members: u16) -> impl Iterator<Item = usize> {
    (0..16).filter(move |i| members >> i & 1 != 0)
}

/// The mean of `points`, the unit direction they spread along most and the
/// spread along it (the largest eigenvalue of their scatter); a zero axis
/// when they coincide.
fn principal_axis<const N: usize>(points: &[[f32; N]]) -> ([f32; N], [f32; N], f32) {
    let mut mean = [0.0f32; N];
    for p in points {
        for (m, v) in mean.iter_mut().zip(p) {
            *m += v;
        }
    }
    let n = points.len().max(1) as f32;
    mean.iter_mut().for_each(|m| *m /= n);
    let mut scatter = [[0.0f32; N]; N];
    for p in points {
        for i in 0..N {
            let di = p[i] - mean[i];
            for j in 0..N {
                scatter[i][j] += di * (p[j] - mean[j]);
            }
        }
    }
    let (axis, spread) = dominant(&scatter);
    (mean, axis, spread)
}

/// The dominant eigenvector of the symmetric `m` and its eigenvalue: the
/// largest column of `m` raised to the 16th power, by repeated squaring
/// rescaled to its trace; zero for a zero matrix.
fn dominant<const N: usize>(m: &[[f32; N]; N]) -> ([f32; N], f32) {
    let mut power = *m;
    for _ in 0..4 {
        let trace: f32 = (0..N).map(|i| power[i][i]).sum();
        if trace <= 1e-6 {
            return ([0.0; N], 0.0);
        }
        let scale = 1.0 / (trace * trace);
        let mut next = [[0.0f32; N]; N];
        for (out, row) in next.iter_mut().zip(&power) {
            for (a, other) in row.iter().zip(&power) {
                let a = a * scale;
                for (o, b) in out.iter_mut().zip(other) {
                    *o += a * b;
                }
            }
        }
        power = next;
    }
    let mut column = 0;
    for i in 1..N {
        if power[i][i] > power[column][column] {
            column = i;
        }
    }
    let v = power[column];
    let len = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if len < 1e-12 {
        return ([0.0; N], 0.0);
    }
    let v = v.map(|x| x / len);
    let mut value = 0.0;
    for i in 0..N {
        for j in 0..N {
            value += v[i] * m[i][j] * v[j];
        }
    }
    (v, value.max(0.0))
}

#[cfg(test)]
mod tests {
    use super::super::{Effort, decode, encode, fixtures, psnr};
    use super::*;
    use viso_gpu_abi::TextureFormat;

    const FORMATS: [TextureFormat; 3] = [
        TextureFormat::Bc1RgbaUnorm,
        TextureFormat::Bc3RgbaUnorm,
        TextureFormat::Bc7RgbaUnorm,
    ];

    /// `fields`, `(value, bits)` from the low bits up, as a 16-byte block.
    fn bits(fields: &[(u32, u32)]) -> [u8; 16] {
        let (mut out, mut at) = (0u128, 0);
        for &(v, n) in fields {
            assert!(v < 1 << n);
            out |= u128::from(v) << at;
            at += n;
        }
        assert_eq!(at, 128);
        out.to_le_bytes()
    }

    fn mix(a: u32, b: u32, w: u32) -> u8 {
        ((a * (64 - w) + b * w + 32) >> 6) as u8
    }

    const W2: [u32; 4] = [0, 21, 43, 64];
    const W3: [u32; 8] = [0, 9, 18, 27, 37, 46, 55, 64];
    const W4: [u32; 16] = [0, 4, 9, 13, 17, 21, 26, 30, 34, 38, 43, 47, 51, 55, 60, 64];

    fn round_trip(
        format: TextureFormat,
        w: u32,
        h: u32,
        src: &[[u8; 4]],
        effort: Effort,
    ) -> Vec<[u8; 4]> {
        let bytes = encode(format, w, h, src, effort);
        assert_eq!(bytes.len(), format.data_size(w, h));
        decode(format, w, h, &bytes).expect("decodes")
    }

    #[test]
    fn bc1_decodes_four_and_three_color_blocks() {
        // Red over blue in descending order: four colors.
        let mut block = [0x00, 0xf8, 0x1f, 0x00, 0, 0, 0, 0];
        assert_eq!(decode_bc1(&block), [[255, 0, 0, 255]; 16]);
        block[4..].copy_from_slice(&0xaaaa_aaaau32.to_le_bytes());
        assert_eq!(decode_bc1(&block), [[170, 0, 85, 255]; 16]);
        block[4..].copy_from_slice(&0xffff_ffffu32.to_le_bytes());
        assert_eq!(decode_bc1(&block), [[85, 0, 170, 255]; 16]);
        // Blue over red, ascending: three colors and transparent black.
        let mut block = [0x1f, 0x00, 0x00, 0xf8, 0, 0, 0, 0];
        block[4..].copy_from_slice(&0xffff_ffffu32.to_le_bytes());
        assert_eq!(decode_bc1(&block), [[0; 4]; 16]);
        block[4..].copy_from_slice(&0xaaaa_aaaau32.to_le_bytes());
        assert_eq!(decode_bc1(&block), [[128, 0, 128, 255]; 16]);
        // Equal endpoints are three-color.
        let block = [0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff];
        assert_eq!(decode_bc1(&block), [[0; 4]; 16]);
    }

    #[test]
    fn bc3_decodes_both_alpha_modes_and_always_four_colors() {
        // Eight alphas from 255 to 0; color ascending, still four colors.
        let mut block = [255, 0, 0, 0, 0, 0, 0, 0, 0x1f, 0x00, 0x00, 0xf8, 0, 0, 0, 0];
        block[2..8].copy_from_slice(&0o2222_2222_2222_2222u64.to_le_bytes()[..6]);
        block[12..].copy_from_slice(&0xffff_ffffu32.to_le_bytes());
        assert_eq!(decode_bc3(&block), [[170, 0, 85, 219]; 16]);
        // Six alphas from 0 to 255, then 0 and 255.
        let mut block = [10, 60, 0, 0, 0, 0, 0, 0, 0x00, 0xf8, 0x00, 0xf8, 0, 0, 0, 0];
        let indices: u64 = (0..16).map(|i| (i as u64 % 8) << (3 * i)).sum();
        block[2..8].copy_from_slice(&indices.to_le_bytes()[..6]);
        let alphas: Vec<u8> = decode_bc3(&block).iter().map(|t| t[3]).collect();
        let levels = [10, 60, 20, 30, 40, 50, 0, 255];
        assert_eq!(alphas, (0..16).map(|i| levels[i % 8]).collect::<Vec<_>>());
    }

    #[test]
    fn bc7_decodes_mode_6_with_p_bits() {
        let mut fields = vec![(1 << 6, 7)];
        for (e0, e1) in [(10, 100), (50, 20), (127, 0), (127, 127)] {
            fields.extend([(e0, 7), (e1, 7)]);
        }
        fields.extend([(1, 1), (0, 1), (0, 3)]);
        fields.extend((1..16).map(|i| (i, 4)));
        let texels = decode_bc7(&bits(&fields));
        // Endpoints (code << 1 | p) at 8 bits.
        let (e0, e1) = ([21, 101, 255, 255], [200, 40, 0, 254]);
        for (i, t) in texels.iter().enumerate() {
            let expect: [u8; 4] = std::array::from_fn(|c| mix(e0[c], e1[c], W4[i]));
            assert_eq!(*t, expect, "texel {i}");
        }
    }

    #[test]
    fn bc7_decodes_mode_1_with_shared_p_bits_and_anchors() {
        // Partition 13: the top two rows subset 0, the bottom two subset 1,
        // anchored at texel 15.
        let mut fields = vec![(0b10, 2), (13, 6)];
        for [a, b, c, d] in [[63, 0, 0, 0], [0, 0, 0, 0], [0, 0, 32, 63]] {
            fields.extend([(a, 6), (b, 6), (c, 6), (d, 6)]);
        }
        fields.extend([(1, 1), (0, 1)]);
        for i in 0..16u32 {
            let (k, n) = match i {
                0 => (0, 2),
                1..8 => (i, 3),
                15 => (0, 2),
                _ => (15 - i, 3),
            };
            fields.push((k, n));
        }
        let texels = decode_bc7(&bits(&fields));
        for (i, t) in texels.iter().enumerate() {
            let expect = if i < 8 {
                [mix(255, 2, W3[i]), 2, 2, 255]
            } else {
                [0, 0, mix(129, 253, W3[15 - i]), 255]
            };
            assert_eq!(*t, expect, "texel {i}");
        }
    }

    #[test]
    fn bc7_decodes_mode_5_with_rotation() {
        let mut fields = vec![(1 << 5, 6), (1, 2)];
        fields.extend([
            (127, 7),
            (0, 7),
            (0, 7),
            (0, 7),
            (0, 7),
            (0, 7),
            (200, 8),
            (100, 8),
        ]);
        fields.push((0, 1));
        fields.extend((1..16).map(|_| (1, 2)));
        fields.push((1, 1));
        fields.extend((1..16).map(|_| (3, 2)));
        let texels = decode_bc7(&bits(&fields));
        // Red and alpha swapped back.
        assert_eq!(texels[0], [mix(200, 100, W2[1]), 0, 0, 255]);
        for t in &texels[1..] {
            assert_eq!(*t, [100, 0, 0, mix(255, 0, W2[1])]);
        }
    }

    #[test]
    fn bc7_reserved_mode_decodes_transparent() {
        assert_eq!(decode_bc7(&[0; 16]), [[0; 4]; 16]);
        let mut block = [0xff; 16];
        block[0] = 0;
        assert_eq!(decode_bc7(&block), [[0; 4]; 16]);
    }

    #[test]
    fn decoders_take_any_bytes() {
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for i in 0..4096 {
            let bytes: [u8; 16] = std::array::from_fn(|_| next() as u8);
            decode_bc1(bytes[..8].try_into().expect("8 bytes"));
            decode_bc3(&bytes);
            let mut bc7 = bytes;
            // Every mode in turn, then whatever the bytes name.
            let mode = i % 9;
            let high = u32::from(bc7[0]) >> (mode + 1) << (mode + 1);
            bc7[0] = if mode == 8 {
                0
            } else {
                (high | 1 << mode) as u8
            };
            decode_bc7(&bc7);
            decode_bc7(&bytes);
        }
    }

    #[test]
    fn photo_round_trips() {
        let photo = fixtures::photo(64, 48);
        for (format, floor) in FORMATS.into_iter().zip([38.0, 38.0, 43.0]) {
            let back = round_trip(format, 64, 48, &photo, Effort::Fast);
            let db = psnr(&photo, &back);
            assert!(db > floor, "{format:?}: {db:.2} dB");
        }
    }

    #[test]
    fn icon_round_trips_with_transparency_kept() {
        let icon = fixtures::icon(64, 48);
        let binary = fixtures::binary(&icon);
        let cases = [
            (TextureFormat::Bc1RgbaUnorm, &binary, 53.0),
            (TextureFormat::Bc3RgbaUnorm, &icon, 37.5),
            (TextureFormat::Bc7RgbaUnorm, &icon, 48.5),
        ];
        for (format, src, floor) in cases {
            let back = round_trip(format, 64, 48, src, Effort::Fast);
            let db = psnr(src, &back);
            assert!(db > floor, "{format:?}: {db:.2} dB");
            for (s, d) in src.iter().zip(&back) {
                if *s == [0; 4] {
                    assert_eq!(*d, [0; 4], "{format:?}");
                }
            }
        }
    }

    #[test]
    fn thorough_beats_fast() {
        let photo = fixtures::photo(16, 16);
        let icon = fixtures::icon(16, 16);
        for format in FORMATS {
            let src = if format == TextureFormat::Bc1RgbaUnorm {
                &photo
            } else {
                &icon
            };
            let fast = psnr(src, &round_trip(format, 16, 16, src, Effort::Fast));
            let thorough = psnr(src, &round_trip(format, 16, 16, src, Effort::Thorough));
            assert!(
                thorough >= fast - 0.05,
                "{format:?}: {thorough:.2} < {fast:.2} dB"
            );
        }
    }

    #[test]
    fn partial_blocks_round_trip() {
        let photo = fixtures::photo(13, 7);
        for (format, floor) in FORMATS.into_iter().zip([35.5, 35.5, 41.0]) {
            let back = round_trip(format, 13, 7, &photo, Effort::Fast);
            assert_eq!(back.len(), 13 * 7);
            let db = psnr(&photo, &back);
            assert!(db > floor, "{format:?}: {db:.2} dB");
        }
    }

    #[test]
    fn flat_blocks_are_near_exact() {
        for color in [
            [0, 0, 0, 255],
            [255, 255, 255, 255],
            [37, 140, 201, 255],
            [3, 2, 1, 255],
        ] {
            let block = [color; 16];
            for effort in [Effort::Fast, Effort::Thorough] {
                let near = |t: &[u8; 4], tol: u8| (0..4).all(|c| t[c].abs_diff(color[c]) <= tol);
                assert!(
                    decode_bc1(&encode_bc1(&block, effort))
                        .iter()
                        .all(|t| near(t, 1))
                );
                assert!(
                    decode_bc3(&encode_bc3(&block, effort))
                        .iter()
                        .all(|t| near(t, 1))
                );
                assert!(
                    decode_bc7(&encode_bc7(&block, effort))
                        .iter()
                        .all(|t| near(t, 0))
                );
            }
        }
        assert_eq!(
            decode_bc1(&encode_bc1(&[[0; 4]; 16], Effort::Fast)),
            [[0; 4]; 16]
        );
        assert_eq!(
            decode_bc3(&encode_bc3(&[[0; 4]; 16], Effort::Fast)),
            [[0; 4]; 16]
        );
        assert_eq!(
            decode_bc7(&encode_bc7(&[[0; 4]; 16], Effort::Fast)),
            [[0; 4]; 16]
        );
    }

    /// Release-mode speed and quality at both efforts, for the report:
    /// `cargo test --release -p viso-image --lib codec::bc -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn measure() {
        for (w, h) in [(64, 48), (256, 256)] {
            let photo = fixtures::photo(w, h);
            let icon = fixtures::icon(w, h);
            let binary = fixtures::binary(&icon);
            for format in FORMATS {
                for effort in [Effort::Fast, Effort::Thorough] {
                    let alpha = if format == TextureFormat::Bc1RgbaUnorm {
                        &binary
                    } else {
                        &icon
                    };
                    for (name, src) in [("photo", &photo), ("icon", alpha)] {
                        let start = std::time::Instant::now();
                        let bytes = encode(format, w, h, src, effort);
                        let took = start.elapsed();
                        let back = decode(format, w, h, &bytes).expect("decodes");
                        println!(
                            "{w}x{h} {format:?} {effort:?} {name}: {:.2} dB, {:.1} ms",
                            psnr(src, &back),
                            took.as_secs_f64() * 1e3
                        );
                    }
                }
            }
        }
    }
}
