//! The LDR decoder, to the letter of the Khronos Data Format Specification
//! (1.3, chapter 23) for 2D blocks: block mode, partitions, color endpoint
//! modes, integer sequences, unquantization, weight infill, dual planes and
//! void-extent blocks. Every illegal encoding, an HDR void extent and the
//! texels of a partition with an HDR endpoint mode decode to the error color.

use super::ise;
use super::tables::{COLORS, MIN_COLOR_RANGE, WEIGHTS, sequence_bits};

/// What an illegal block, or a partition with an HDR endpoint mode, decodes
/// to: opaque magenta, in UNORM16.
pub(super) const ERROR_COLOR: [u16; 4] = [0xffff, 0, 0xffff, 0xffff];

/// The weight grid of a block mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct BlockMode {
    pub(super) width: u32,
    pub(super) height: u32,
    /// Two weight planes, the second for the channel the block selects.
    pub(super) dual: bool,
    /// The weight range, an index into the first `WEIGHT_RANGES` ranges.
    pub(super) range: usize,
}

/// The weight grid the 11-bit block mode field `mode` names; `None` for the
/// reserved encodings and the void-extent pattern.
pub(super) fn block_mode(mode: u32) -> Option<BlockMode> {
    let bit = |i: u32| (mode >> i) & 1;
    let a = (mode >> 5) & 3;
    let (width, height, rho, fixed_precision);
    if mode & 3 != 0 {
        rho = ((mode & 3) << 1) | bit(4);
        let b = (mode >> 7) & 3;
        (width, height) = match (mode >> 2) & 3 {
            0 => (b + 4, a + 2),
            1 => (b + 8, a + 2),
            2 => (a + 2, b + 8),
            _ if bit(8) == 0 => (a + 2, (b & 1) + 6),
            _ => ((b & 1) + 2, a + 2),
        };
        fixed_precision = false;
    } else {
        if (mode >> 2) & 3 == 0 {
            return None;
        }
        rho = ((mode >> 1) & 6) | bit(4);
        let b = (mode >> 9) & 3;
        (width, height) = match (mode >> 7) & 3 {
            0 => (12, a + 2),
            1 => (a + 2, 12),
            2 => (a + 6, b + 6),
            _ => match a {
                0 => (6, 10),
                1 => (10, 6),
                _ => return None,
            },
        };
        fixed_precision = (mode >> 7) & 3 == 2;
    }
    let (dual, high) = if fixed_precision {
        (false, 0)
    } else {
        (bit(10) == 1, bit(9))
    };
    Some(BlockMode {
        width,
        height,
        dual,
        range: (rho - 2 + 6 * high) as usize,
    })
}

/// The largest color range `values` color values fit in `remaining` bits
/// with; `None` when not even `0..6` fits, an illegal block.
pub(super) fn color_range(values: u32, remaining: i32) -> Option<usize> {
    (MIN_COLOR_RANGE..21)
        .rev()
        .find(|&r| sequence_bits(r, values) as i32 <= remaining)
}

/// The partition texel `(x, y)` of a block belongs to, as the spec's
/// `select_partition` hashes it.
pub(super) fn select_partition(seed: u32, x: u32, y: u32, count: u32, small: bool) -> usize {
    let (x, y, z) = if small {
        (x << 1, y << 1, 0)
    } else {
        (x, y, 0u32)
    };
    let seed = seed + (count - 1) * 1024;
    let rnum = hash52(seed);
    let mut s = [
        rnum & 0xf,
        (rnum >> 4) & 0xf,
        (rnum >> 8) & 0xf,
        (rnum >> 12) & 0xf,
        (rnum >> 16) & 0xf,
        (rnum >> 20) & 0xf,
        (rnum >> 24) & 0xf,
        (rnum >> 28) & 0xf,
        (rnum >> 18) & 0xf,
        (rnum >> 22) & 0xf,
        (rnum >> 26) & 0xf,
        rnum.rotate_left(2) & 0xf,
    ];
    for v in &mut s {
        *v *= *v;
    }
    let (sh1, sh2) = if seed & 1 != 0 {
        (
            if seed & 2 != 0 { 4 } else { 5 },
            if count == 3 { 6 } else { 5 },
        )
    } else {
        (
            if count == 3 { 6 } else { 5 },
            if seed & 2 != 0 { 4 } else { 5 },
        )
    };
    let sh3 = if seed & 0x10 != 0 { sh1 } else { sh2 };
    for (i, v) in s.iter_mut().enumerate() {
        *v >>= match i {
            0..8 if i % 2 == 0 => sh1,
            0..8 => sh2,
            _ => sh3,
        };
    }
    let a = (s[0] * x + s[1] * y + s[10] * z + (rnum >> 14)) & 0x3f;
    let b = (s[2] * x + s[3] * y + s[11] * z + (rnum >> 10)) & 0x3f;
    let c = if count < 3 {
        0
    } else {
        (s[4] * x + s[5] * y + s[8] * z + (rnum >> 6)) & 0x3f
    };
    let d = if count < 4 {
        0
    } else {
        (s[6] * x + s[7] * y + s[9] * z + (rnum >> 2)) & 0x3f
    };
    if a >= b && a >= c && a >= d {
        0
    } else if b >= c && b >= d {
        1
    } else if c >= d {
        2
    } else {
        3
    }
}

/// The spec's 32-bit seed hash for partition selection.
fn hash52(mut p: u32) -> u32 {
    p ^= p >> 15;
    p = p.wrapping_sub(p << 17);
    p = p.wrapping_add(p << 7);
    p = p.wrapping_add(p << 4);
    p ^= p >> 5;
    p = p.wrapping_add(p << 16);
    p ^= p >> 7;
    p ^= p >> 3;
    p ^= p << 6;
    p ^= p >> 17;
    p
}

/// The four weight-grid entries texel `(x, y)` of a `footprint` block
/// interpolates over a `grid`, and their weights in sixteenths, by the
/// spec's fixed-point infill. An entry with weight zero may lie past the
/// grid.
pub(super) fn infill(
    footprint: (u32, u32),
    grid: (u32, u32),
    x: u32,
    y: u32,
) -> ([usize; 4], [u32; 4]) {
    let scale = |b: u32| (1024 + b / 2) / (b - 1);
    let gs = (scale(footprint.0) * x * (grid.0 - 1) + 32) >> 6;
    let gt = (scale(footprint.1) * y * (grid.1 - 1) + 32) >> 6;
    let (js, fs) = (gs >> 4, gs & 0xf);
    let (jt, ft) = (gt >> 4, gt & 0xf);
    let v0 = (js + jt * grid.0) as usize;
    let gw = grid.0 as usize;
    let w11 = (fs * ft + 8) >> 4;
    (
        [v0, v0 + 1, v0 + gw, v0 + gw + 1],
        [16 + w11 - fs - ft, fs - w11, ft - w11, w11],
    )
}

/// Channel `c0` to `c1` at weight `w` of 64, interpolated in UNORM16 as the
/// spec does for linear LDR: each endpoint widened as `(v << 8) | v`.
#[inline]
pub(super) fn interpolate16(c0: u8, c1: u8, w: u32) -> u16 {
    let (c0, c1) = (u32::from(c0) * 257, u32::from(c1) * 257);
    ((c0 * (64 - w) + c1 * w + 32) / 64) as u16
}

/// [`interpolate16`] narrowed to 8 bits by [`unorm8`].
#[inline]
pub(super) fn interpolate(c0: u8, c1: u8, w: u32) -> u8 {
    unorm8(interpolate16(c0, c1, w))
}

/// A UNORM16 texel value narrowed to the nearest UNORM8 one,
/// `(C · 255 + 32767) / 65535`. The spec's `decode_unorm8` mode takes the
/// top 8 bits instead; a GPU sampling the format as floats returns
/// `C / 65535`, and the rounded value is the 8-bit texel nearest to that.
#[inline]
pub(super) fn unorm8(c: u16) -> u8 {
    ((u32::from(c) * 255 + 32767) / 65535) as u8
}

/// The spec's `bit_transfer_signed`: the top bit of `a` moves into `b`, and
/// `a` becomes a signed 6-bit offset.
fn bit_transfer_signed(a: i32, b: i32) -> (i32, i32) {
    let b = (b >> 1) | (a & 0x80);
    let a = (a >> 1) & 0x3f;
    (if a & 0x20 != 0 { a - 0x40 } else { a }, b)
}

/// The spec's `blue_contract`: red and green drawn halfway to blue.
fn blue_contract([r, g, b, a]: [i32; 4]) -> [i32; 4] {
    [(r + b) >> 1, (g + b) >> 1, b, a]
}

/// The two RGBA endpoints color endpoint mode `cem` decodes its unquantized
/// values `v` to; `None` for an HDR mode.
pub(super) fn endpoints(cem: u32, v: &[u8]) -> Option<[[u8; 4]; 2]> {
    let v: [i32; 8] = std::array::from_fn(|i| v.get(i).map_or(0, |&x| i32::from(x)));
    let clamp = |e: [i32; 4]| e.map(|c| c.clamp(0, 255) as u8);
    let (e0, e1) = match cem {
        0 => ([v[0], v[0], v[0], 255], [v[1], v[1], v[1], 255]),
        1 => {
            let l0 = (v[0] >> 2) | (v[1] & 0xc0);
            let l1 = (l0 + (v[1] & 0x3f)).min(255);
            ([l0, l0, l0, 255], [l1, l1, l1, 255])
        }
        4 => ([v[0], v[0], v[0], v[2]], [v[1], v[1], v[1], v[3]]),
        5 => {
            let (d0, l0) = bit_transfer_signed(v[1], v[0]);
            let (d1, a0) = bit_transfer_signed(v[3], v[2]);
            let l1 = l0 + d0;
            ([l0, l0, l0, a0], [l1, l1, l1, a0 + d1])
        }
        6 | 10 => {
            let s = |c: i32| (c * v[3]) >> 8;
            let (a0, a1) = if cem == 6 { (255, 255) } else { (v[4], v[5]) };
            ([s(v[0]), s(v[1]), s(v[2]), a0], [v[0], v[1], v[2], a1])
        }
        8 | 12 => {
            let (a0, a1) = if cem == 8 { (255, 255) } else { (v[6], v[7]) };
            let e0 = [v[0], v[2], v[4], a0];
            let e1 = [v[1], v[3], v[5], a1];
            if v[1] + v[3] + v[5] >= v[0] + v[2] + v[4] {
                (e0, e1)
            } else {
                (blue_contract(e1), blue_contract(e0))
            }
        }
        9 | 13 => {
            let (dr, r) = bit_transfer_signed(v[1], v[0]);
            let (dg, g) = bit_transfer_signed(v[3], v[2]);
            let (db, b) = bit_transfer_signed(v[5], v[4]);
            let (a, da) = if cem == 9 {
                (255, 0)
            } else {
                let (da, a) = bit_transfer_signed(v[7], v[6]);
                (a, da)
            };
            let base = [r, g, b, a];
            let offset = [r + dr, g + dg, b + db, a + da];
            if dr + dg + db >= 0 {
                (base, offset)
            } else {
                (blue_contract(offset), blue_contract(base))
            }
        }
        _ => return None,
    };
    Some([clamp(e0), clamp(e1)])
}

/// The number of color values endpoint mode `cem` stores.
pub(super) const fn endpoint_values(cem: u32) -> u32 {
    ((cem >> 2) + 1) * 2
}

/// `block` of `footprint` decoded into `out` as UNORM16 texels; `None`,
/// with `out` unwritten, when the block is illegal.
pub(super) fn decode_block(block: u128, footprint: (u32, u32), out: &mut [[u16; 4]]) -> Option<()> {
    let bits = |pos: u32, n: u32| ((block >> pos) as u32) & ((1 << n) - 1);
    if bits(0, 9) == 0x1fc {
        return void_extent(block, out);
    }
    let mode = block_mode(bits(0, 11))?;
    if mode.width > footprint.0 || mode.height > footprint.1 {
        return None;
    }
    let planes = 1 + usize::from(mode.dual);
    let grid_count = (mode.width * mode.height) as usize;
    let weight_count = grid_count * planes;
    if weight_count > 64 {
        return None;
    }
    let weight_bits = sequence_bits(mode.range, weight_count as u32);
    if !(24..=96).contains(&weight_bits) {
        return None;
    }
    let partitions = bits(11, 2) + 1;
    if mode.dual && partitions == 4 {
        return None;
    }

    let mut cems = [0u32; 4];
    let (seed, extra_bits) = if partitions == 1 {
        cems[0] = bits(13, 4);
        (0, 0)
    } else if bits(23, 2) == 0 {
        cems[..partitions as usize].fill(bits(25, 4));
        (bits(13, 10), 0)
    } else {
        let extra = 3 * partitions - 4;
        let high = bits(128 - weight_bits - extra, extra);
        let encoded = bits(23, 6) | (high << 6);
        let base = (encoded & 3) - 1;
        for (i, cem) in cems[..partitions as usize].iter_mut().enumerate() {
            let i = i as u32;
            let class = base + ((encoded >> (2 + i)) & 1);
            let low = (encoded >> (2 + partitions + 2 * i)) & 3;
            *cem = (class << 2) | low;
        }
        (bits(13, 10), extra)
    };
    let color_start = if partitions == 1 { 17 } else { 29 };
    let config_bits = color_start + extra_bits + 2 * u32::from(mode.dual);
    let values: u32 = cems[..partitions as usize]
        .iter()
        .map(|&c| endpoint_values(c))
        .sum();
    if values > 18 {
        return None;
    }
    let remaining = 128 - config_bits as i32 - weight_bits as i32;
    let color = color_range(values, remaining)?;

    let mut raw = [0u8; 18];
    let stream = ise::truncate(block >> color_start, sequence_bits(color, values));
    ise::decode(stream, color, &mut raw[..values as usize]);
    let unquantized = raw.map(|v| COLORS[color][usize::from(v)]);
    let mut ends: [Option<[[u8; 4]; 2]>; 4] = [None; 4];
    let mut at = 0;
    for (p, &cem) in cems[..partitions as usize].iter().enumerate() {
        let n = endpoint_values(cem) as usize;
        ends[p] = endpoints(cem, &unquantized[at..at + n]);
        at += n;
    }
    let ccs = if mode.dual {
        bits(128 - weight_bits - extra_bits - 2, 2) as usize
    } else {
        4
    };

    let mut raw = [0u8; 64];
    let stream = ise::truncate(block.reverse_bits(), weight_bits);
    ise::decode(stream, mode.range, &mut raw[..weight_count]);
    let mut grid = [[0u8; 80]; 2];
    for (i, &v) in raw[..weight_count].iter().enumerate() {
        grid[i % planes][i / planes] = WEIGHTS[mode.range][usize::from(v)];
    }

    let texels = footprint.0 * footprint.1;
    for y in 0..footprint.1 {
        for x in 0..footprint.0 {
            let p = if partitions == 1 {
                0
            } else {
                select_partition(seed, x, y, partitions, texels < 31)
            };
            let texel = &mut out[(y * footprint.0 + x) as usize];
            let Some([e0, e1]) = ends[p] else {
                *texel = ERROR_COLOR;
                continue;
            };
            let (taps, w) = infill(footprint, (mode.width, mode.height), x, y);
            let effective = |plane: &[u8; 80]| {
                let sum: u32 = (0..4).map(|k| u32::from(plane[taps[k]]) * w[k]).sum();
                (sum + 8) >> 4
            };
            let w0 = effective(&grid[0]);
            let w1 = if mode.dual { effective(&grid[1]) } else { w0 };
            *texel = std::array::from_fn(|c| {
                interpolate16(e0[c], e1[c], if c == ccs { w1 } else { w0 })
            });
        }
    }
    Some(())
}

/// A void-extent block: one color for the whole block, `None` for an HDR
/// one, reserved bits not set, or a malformed extent.
fn void_extent(block: u128, out: &mut [[u16; 4]]) -> Option<()> {
    let bits = |pos: u32, n: u32| ((block >> pos) as u32) & ((1 << n) - 1);
    if bits(9, 1) == 1 || bits(10, 2) != 3 {
        return None;
    }
    let [min_s, max_s, min_t, max_t] = [12, 25, 38, 51].map(|at| bits(at, 13));
    let unbounded = [min_s, max_s, min_t, max_t].iter().all(|&c| c == 0x1fff);
    if !unbounded && (min_s >= max_s || min_t >= max_t) {
        return None;
    }
    let color = [64, 80, 96, 112].map(|at| bits(at, 16) as u16);
    out.fill(color);
    Some(())
}
