use viso_gpu_abi::TextureFormat;

use super::decode::{block_mode, decode_block};
use super::tables::{COLORS, RANGES, WEIGHT_RANGES, WEIGHTS, levels, sequence_bits};
use super::*;
use crate::codec::{decode, encode, fixtures, psnr};

/// The error color, as 8-bit texels.
const ERROR_COLOR: [u8; 4] = [255, 0, 255, 255];

/// A deterministic stream of pseudo-random words.
fn xorshift(mut seed: u64) -> impl FnMut() -> u64 {
    move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    }
}

/// `bits` of `v` set in `block` from bit `at`.
fn put(block: &mut u128, at: u32, bits: u32, v: u32) {
    *block |= u128::from(v & ((1 << bits) - 1)) << at;
}

const FORMATS: [(TextureFormat, (u32, u32)); 3] = [
    (TextureFormat::Astc4x4Unorm, (4, 4)),
    (TextureFormat::Astc6x6Unorm, (6, 6)),
    (TextureFormat::Astc8x8Unorm, (8, 8)),
];

#[test]
fn integer_sequences_round_trip_in_every_range() {
    let mut rand = xorshift(0x9e37_79b9_7f4a_7c15);
    for range in 0..RANGES.len() {
        let n = levels(range) as u64;
        for count in 1..=40u32 {
            if sequence_bits(range, count) > 128 {
                break;
            }
            for _ in 0..8 {
                let values: Vec<u8> = (0..count).map(|_| (rand() % n) as u8).collect();
                let stream = ise::encode(&values, range);
                let len = sequence_bits(range, count);
                assert_eq!(
                    ise::truncate(stream, len),
                    stream,
                    "range {range} count {count}"
                );
                let mut back = vec![0u8; count as usize];
                ise::decode(stream, range, &mut back);
                assert_eq!(back, values, "range {range} count {count}");
            }
        }
    }
}

#[test]
fn every_packed_digit_block_unpacks_to_digits_in_range() {
    for t in &tables::TRITS {
        assert!(t.iter().all(|&d| d < 3), "{t:?}");
    }
    for q in &tables::QUINTS {
        assert!(q.iter().all(|&d| d < 5), "{q:?}");
    }
}

#[test]
fn unquantization_matches_the_spec_tables() {
    // Ranges 0..6 and 0..12 in stored order, from the spec's procedure.
    assert_eq!(&WEIGHTS[4][..6], &[0, 64, 12, 52, 25, 39]);
    assert_eq!(
        &WEIGHTS[7][..12],
        &[0, 64, 17, 47, 5, 59, 23, 41, 11, 53, 28, 36]
    );
    assert_eq!(&WEIGHTS[1][..3], &[0, 32, 64]);
    assert_eq!(&WEIGHTS[3][..5], &[0, 16, 32, 48, 64]);
    assert_eq!(&COLORS[4][..6], &[0, 255, 51, 204, 102, 153]);
    assert_eq!(
        &COLORS[7][..12],
        &[0, 255, 69, 186, 23, 232, 92, 163, 46, 209, 116, 139]
    );
    // Every range spans its extremes, has distinct levels, and its weights
    // mirror about 32, so inverting a weight is exact.
    for r in 0..WEIGHT_RANGES {
        let mut v: Vec<u8> = WEIGHTS[r][..levels(r) as usize].to_vec();
        v.sort_unstable();
        v.dedup();
        assert_eq!(v.len(), levels(r) as usize, "weight range {r}");
        assert_eq!((v[0], v[v.len() - 1]), (0, 64), "weight range {r}");
        for (a, b) in v.iter().zip(v.iter().rev()) {
            assert_eq!(a + b, 64, "weight range {r} is symmetric");
        }
    }
    for r in tables::MIN_COLOR_RANGE..21 {
        let mut v: Vec<u8> = COLORS[r][..levels(r) as usize].to_vec();
        v.sort_unstable();
        v.dedup();
        assert_eq!(v.len(), levels(r) as usize, "color range {r}");
        assert_eq!((v[0], v[v.len() - 1]), (0, 255), "color range {r}");
    }
}

#[test]
fn block_modes_cover_the_spec_layouts() {
    let m = block_mode(0x042).expect("legal");
    assert_eq!(
        (m.width, m.height, m.dual, levels(m.range)),
        (4, 4, false, 4)
    );
    // The fixed 6×10 grid, and the 6–9 by 6–9 layout fixed to one plane at
    // low precision.
    let m = block_mode(0x198).expect("legal");
    assert_eq!((m.width, m.height), (6, 10));
    let m = block_mode(0b111_0110_0100).expect("legal");
    assert_eq!((m.width, m.height, m.dual), (9, 9, false));
    assert!(block_mode(0).is_none(), "reserved");
    assert!(block_mode(0x1fc).is_none(), "the void-extent pattern");
    assert!(block_mode(0b001_1100_0100).is_none(), "reserved");
    let mut count = 0;
    for mode in 0..2048 {
        if let Some(m) = block_mode(mode) {
            assert!(
                (2..=12).contains(&m.width) && (2..=12).contains(&m.height),
                "{mode:#x}"
            );
            count += 1;
        }
    }
    assert!(count > 1500, "{count} legal block modes");
}

#[test]
fn a_void_extent_block_decodes_to_its_color() {
    let mut block = 0u128;
    put(&mut block, 0, 12, 0xdfc);
    for at in [12, 25, 38, 51] {
        put(&mut block, at, 13, 0x1fff);
    }
    for (i, v) in [0x1234u32, 0x8000, 0xffff, 0x0080].into_iter().enumerate() {
        put(&mut block, 64 + 16 * i as u32, 16, v);
    }
    let mut out = [[0u8; 4]; 36];
    decode_astc(&block.to_le_bytes(), (6, 6), &mut out);
    assert!(out.iter().all(|&t| t == [18, 128, 255, 0]), "{:?}", out[0]);
    // An HDR void extent, and one whose reserved bits are clear, are errors.
    let mut out = [[0u8; 4]; 16];
    decode_astc(&(block | 1 << 9).to_le_bytes(), (4, 4), &mut out);
    assert!(out.iter().all(|&t| t == ERROR_COLOR));
    decode_astc(&(block & !(1 << 10)).to_le_bytes(), (4, 4), &mut out);
    assert!(out.iter().all(|&t| t == ERROR_COLOR));
}

#[test]
fn illegal_blocks_decode_to_the_error_color() {
    let mut out = [[0u8; 4]; 16];
    // Block mode 0 is reserved.
    decode_astc(&[0; 16], (4, 4), &mut out);
    assert!(out.iter().all(|&t| t == ERROR_COLOR));
    // A 4×4 weight grid in a 4×4 block with an HDR endpoint mode (2).
    let mut block = 0u128;
    put(&mut block, 0, 11, 0x042);
    put(&mut block, 13, 4, 2);
    decode_astc(&block.to_le_bytes(), (4, 4), &mut out);
    assert!(out.iter().all(|&t| t == ERROR_COLOR));
    // A 12-wide grid is larger than a 4×4 footprint.
    let mut block = 0u128;
    put(&mut block, 0, 11, 0b000_0001_1100);
    put(&mut block, 13, 4, 8);
    assert_eq!(block_mode(block as u32 & 0x7ff).map(|m| m.width), Some(12));
    decode_astc(&block.to_le_bytes(), (4, 4), &mut out);
    assert!(out.iter().all(|&t| t == ERROR_COLOR));
    // Dual plane with four partitions.
    let mut block = 0u128;
    put(&mut block, 0, 11, 0x042 | 1 << 10);
    put(&mut block, 11, 2, 3);
    decode_astc(&block.to_le_bytes(), (4, 4), &mut out);
    assert!(out.iter().all(|&t| t == ERROR_COLOR));
}

#[test]
fn a_hand_assembled_block_decodes_as_computed() {
    // 4×4 weights of range 0..4 (2 bits each, unquantized 0, 21, 43, 64),
    // one partition, endpoint mode 8 with 8-bit color values: endpoints
    // (0, 0, 255) and (255, 128, 0); each texel's weight is its column.
    let mut block = 0u128;
    put(&mut block, 0, 11, 0x042);
    put(&mut block, 13, 4, 8);
    for (i, v) in [0u32, 255, 0, 128, 255, 0].into_iter().enumerate() {
        put(&mut block, 17 + 8 * i as u32, 8, v);
    }
    for i in 0..16u32 {
        let w = i % 4;
        put(&mut block, 127 - 2 * i, 1, w & 1);
        put(&mut block, 126 - 2 * i, 1, w >> 1);
    }
    let mut out = [[0u8; 4]; 16];
    decode_astc(&block.to_le_bytes(), (4, 4), &mut out);
    let columns = [
        [0, 0, 255, 255],
        [84, 42, 171, 255],
        [171, 86, 84, 255],
        [255, 128, 0, 255],
    ];
    for (i, t) in out.iter().enumerate() {
        assert_eq!(*t, columns[i % 4], "texel {i}");
    }
    // Endpoints whose second sum is smaller are blue-contracted and swapped.
    let mut swapped = block & !(((1u128 << 48) - 1) << 17);
    for (i, v) in [255u32, 0, 128, 0, 0, 255].into_iter().enumerate() {
        put(&mut swapped, 17 + 8 * i as u32, 8, v);
    }
    decode_astc(&swapped.to_le_bytes(), (4, 4), &mut out);
    assert_eq!(
        out[0],
        [127, 127, 255, 255],
        "e0 = blue_contract(0, 0, 255)"
    );
    assert_eq!(out[3], [127, 64, 0, 255], "e1 = blue_contract(255, 128, 0)");
}

#[test]
fn every_infill_tap_with_weight_lies_in_the_grid() {
    for (fw, fh) in [(4, 4), (5, 5), (6, 6), (8, 8), (10, 6), (10, 10), (12, 12)] {
        for gw in 2..=fw {
            for gh in 2..=fh {
                for y in 0..fh {
                    for x in 0..fw {
                        let (idx, w) = decode::infill((fw, fh), (gw, gh), x, y);
                        assert_eq!(w.iter().sum::<u32>(), 16);
                        for k in 0..4 {
                            assert!(w[k] == 0 || idx[k] < (gw * gh) as usize);
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn random_blocks_decode_without_panicking() {
    let mut rand = xorshift(0x1234_5678_9abc_def1);
    let mut legal = 0;
    for i in 0..6000 {
        let block = (u128::from(rand()) << 64) | u128::from(rand());
        let footprint = [(4, 4), (5, 4), (6, 6), (8, 5), (10, 10), (12, 12)][i % 6];
        let mut out = vec![[0u16; 4]; (footprint.0 * footprint.1) as usize];
        legal += usize::from(decode_block(block, footprint, &mut out).is_some());
    }
    assert!(legal > 100, "{legal} legal random blocks");
}

/// Every legal 2D footprint.
const FOOTPRINTS: [(u32, u32); 14] = [
    (4, 4),
    (5, 4),
    (5, 5),
    (6, 5),
    (6, 6),
    (8, 5),
    (8, 6),
    (8, 8),
    (10, 5),
    (10, 6),
    (10, 8),
    (10, 10),
    (12, 10),
    (12, 12),
];

/// `count` deterministic blocks for `footprint`, weighted towards legal
/// ones: random bits under a legal block mode, with one partition and an
/// LDR endpoint mode, or several partitions, or anything; and void extents.
fn sample_blocks(footprint: (u32, u32), count: usize, seed: u64) -> Vec<u128> {
    let mut rand = xorshift(seed);
    let legal: Vec<u32> = (0..2048u32)
        .filter(|&m| {
            m & 0x1ff != 0x1fc
                && block_mode(m).is_some_and(|b| {
                    let weights = b.width * b.height * (1 + u32::from(b.dual));
                    let bits = sequence_bits(b.range, weights);
                    b.width <= footprint.0
                        && b.height <= footprint.1
                        && weights <= 64
                        && (24..=96).contains(&bits)
                })
        })
        .collect();
    const LDR: [u128; 10] = [0, 1, 4, 5, 6, 8, 9, 10, 12, 13];
    (0..count)
        .map(|i| {
            let mut block = (u128::from(rand()) << 64) | u128::from(rand());
            match i % 5 {
                0 => {}
                1 | 2 => {
                    let mode = legal[(rand() % legal.len() as u64) as usize];
                    let cem = LDR[(rand() % 10) as usize];
                    block = (block & !0x1_ffff) | u128::from(mode) | (cem << 13);
                }
                3 => {
                    let mode = legal[(rand() % legal.len() as u64) as usize];
                    let r = rand();
                    block = (block & !0x1fff) | u128::from(mode) | (u128::from(1 + r % 3) << 11);
                    if r & 8 != 0 {
                        let cem = LDR[((r >> 4) % 10) as usize];
                        block = (block & !(0x3f << 23)) | (cem << 25);
                    }
                }
                _ => {
                    block = (block & !((1 << 64) - 1)) | 0xffff_ffff_ffff_fdfc;
                    if rand().is_multiple_of(4) {
                        block ^= u128::from(rand() & 0xffff_ffff_ffff_f000);
                    }
                }
            }
            block
        })
        .collect()
}

/// FNV-1a over `bytes`, continuing from `hash`.
fn fnv1a(hash: u64, bytes: &[u8]) -> u64 {
    bytes.iter().fold(hash, |h, &b| {
        (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3)
    })
}

#[test]
fn decoding_matches_the_reference_decoder() {
    // The digest of these blocks decoded, every channel's top 8 bits (the
    // spec's `decode_unorm8`), as ARM's astcenc 5.3.0 decodes them.
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    let mut legal = 0;
    for (i, &footprint) in FOOTPRINTS.iter().enumerate() {
        let mut out = vec![[0u16; 4]; (footprint.0 * footprint.1) as usize];
        for block in sample_blocks(footprint, 300, 0x5eed + i as u64) {
            if decode_block(block, footprint, &mut out).is_some() {
                legal += 1;
            } else {
                out.fill([0xffff, 0, 0xffff, 0xffff]);
            }
            for t in &out {
                hash = fnv1a(hash, &t.map(|c| (c >> 8) as u8));
            }
        }
    }
    assert!(legal > 1500, "{legal} legal blocks");
    assert_eq!(hash, REFERENCE_DIGEST, "{hash:#018x}");
}

/// See [`decoding_matches_the_reference_decoder`].
const REFERENCE_DIGEST: u64 = 0x9703_05bc_7eb2_1c0e;

/// `texels` (`w × h`) through `format` at `effort`, with every block checked
/// to be legal and free of error texels.
fn round_trip(
    format: TextureFormat,
    w: u32,
    h: u32,
    texels: &[[u8; 4]],
    effort: Effort,
) -> Vec<[u8; 4]> {
    let bytes = encode(format, w, h, texels, effort);
    let block = format.block();
    let footprint = (block.width, block.height);
    let mut scratch = vec![[0u16; 4]; (footprint.0 * footprint.1) as usize];
    for b in bytes.as_chunks::<16>().0 {
        let bits = u128::from_le_bytes(*b);
        assert!(
            decode_block(bits, footprint, &mut scratch).is_some(),
            "an illegal block {bits:#x}"
        );
        assert!(
            !scratch.contains(&[0xffff, 0, 0xffff, 0xffff]),
            "an error texel in {bits:#x}"
        );
    }
    decode(format, w, h, &bytes).expect("decodes")
}

#[test]
fn a_photo_survives_every_footprint() {
    let photo = fixtures::photo(64, 48);
    for ((format, _), floor) in FORMATS.into_iter().zip([40.0, 36.5, 34.5]) {
        let back = round_trip(format, 64, 48, &photo, Effort::Fast);
        let db = psnr(&photo, &back);
        assert!(db > floor, "{format:?}: {db:.2} dB");
    }
}

#[test]
fn an_icon_survives_every_footprint_and_stays_transparent() {
    let icon = fixtures::icon(64, 48);
    for ((format, _), floor) in FORMATS.into_iter().zip([50.5, 42.5, 38.0]) {
        let back = round_trip(format, 64, 48, &icon, Effort::Fast);
        let db = psnr(&icon, &back);
        assert!(db > floor, "{format:?}: {db:.2} dB");
        for (s, d) in icon.iter().zip(&back) {
            if s[3] == 0 {
                assert_eq!(d[3], 0, "{format:?}: a transparent texel {d:?}");
            }
            assert!(
                d[0] <= d[3] && d[1] <= d[3] && d[2] <= d[3],
                "{format:?}: {d:?}"
            );
        }
    }
}

#[test]
fn thorough_does_at_least_as_well_as_fast() {
    let photo = fixtures::photo(24, 24);
    let icon = fixtures::icon(24, 24);
    for texels in [&photo, &icon] {
        let fast = round_trip(TextureFormat::Astc6x6Unorm, 24, 24, texels, Effort::Fast);
        let thorough = round_trip(
            TextureFormat::Astc6x6Unorm,
            24,
            24,
            texels,
            Effort::Thorough,
        );
        let (f, t) = (psnr(texels, &fast), psnr(texels, &thorough));
        assert!(t >= f, "thorough {t:.2} dB, fast {f:.2} dB");
    }
}

#[test]
fn sizes_that_are_not_a_block_multiple_round_trip() {
    let photo = fixtures::photo(13, 7);
    let back = round_trip(TextureFormat::Astc6x6Unorm, 13, 7, &photo, Effort::Fast);
    assert_eq!(back.len(), 13 * 7);
    let db = psnr(&photo, &back);
    assert!(db > 35.5, "{db:.2} dB");
}

#[test]
fn constant_blocks_are_exact() {
    for c in [[0u8; 4], [10, 20, 30, 40], [255; 4]] {
        let texels = vec![c; 64];
        let block = encode_astc(&texels, (8, 8), Effort::Fast);
        let mut out = vec![[1u8; 4]; 64];
        decode_astc(&block, (8, 8), &mut out);
        assert!(out.iter().all(|&t| t == c), "{c:?}");
    }
}

#[test]
#[ignore = "a release-mode measurement"]
fn measure_speed_and_quality() {
    let (w, h) = (256u32, 256u32);
    let photo = fixtures::photo(w, h);
    let icon = fixtures::icon(w, h);
    for (format, (bw, bh)) in FORMATS {
        for effort in [Effort::Fast, Effort::Thorough] {
            // One thread over every block, as the driver pads them.
            let start = std::time::Instant::now();
            let mut block = vec![[0u8; 4]; (bw * bh) as usize];
            for by in 0..h.div_ceil(bh) {
                for bx in 0..w.div_ceil(bw) {
                    for y in 0..bh {
                        for x in 0..bw {
                            let sx = (bx * bw + x).min(w - 1);
                            let sy = (by * bh + y).min(h - 1);
                            block[(y * bw + x) as usize] = photo[(sy * w + sx) as usize];
                        }
                    }
                    std::hint::black_box(encode_astc(&block, (bw, bh), effort));
                }
            }
            let single = start.elapsed().as_secs_f64();
            let start = std::time::Instant::now();
            let bytes = encode(format, w, h, &photo, effort);
            let parallel = start.elapsed().as_secs_f64();
            let back = decode(format, w, h, &bytes).expect("decodes");
            let icon_back =
                decode(format, w, h, &encode(format, w, h, &icon, effort)).expect("decodes");
            let texels = f64::from(w * h);
            println!(
                "{format:?} {effort:?}: photo {:.2} dB, icon {:.2} dB; {:.3} Mtexel/s on one \
                 thread, {:.3} Mtexel/s over all",
                psnr(&photo, &back),
                psnr(&icon, &icon_back),
                texels / single / 1e6,
                texels / parallel / 1e6,
            );
        }
    }
}
