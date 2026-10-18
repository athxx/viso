//! BC7: a 128-bit little-endian block whose lowest set bit names its mode
//! (none: reserved, transparent black). After the mode come the partition,
//! rotation and index-selection fields the mode has; the endpoints channel by
//! channel, every subset's pair of red, then of green, blue and alpha; the
//! p-bits, a low bit under every channel of an endpoint, or of a subset's
//! pair; then the indices texel by texel, modes 4 and 5 a second set after
//! the first. Each subset's anchor texel (texel 0 for the first, a table's
//! for the others) stores its index without the top bit, which is zero.
//! Endpoints widen by bit replication; a texel mixes its subset's pair by the
//! 2-, 3- or 4-bit weight table as `(e0·(64 - w) + e1·w + 32) >> 6`. Modes 4
//! and 5 mix color and alpha by separate index sets, then swap alpha with
//! the color channel their rotation names.
//!
//! The encoder tries mode 6 on every block. An opaque block also tries modes
//! 1 and 3 on the partitions whose subsets lie closest to a line, a block
//! with alpha mode 7 so; every block tries mode 5. `Thorough` keeps more
//! partitions, adds modes 0 and 2 for opaque blocks, every rotation of modes
//! 4 and 5, every p-bit combination and a search of neighboring endpoint
//! codes. A subset holding a transparent texel pins its first endpoint to
//! zero.

use std::array::from_fn;

use super::{Effort, PENALTY, dominant, mask, principal_axis};

/// How a mode stores p-bits.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PBits {
    None,
    /// One per subset, under both its endpoints.
    Shared,
    /// One per endpoint.
    Unique,
}

/// The layout of a BC7 mode.
#[derive(Debug)]
struct Mode {
    subsets: usize,
    partition_bits: u32,
    rotation_bits: u32,
    select_bits: u32,
    color_bits: u32,
    /// Alpha endpoint bits; none decodes opaque.
    alpha_bits: u32,
    pbits: PBits,
    index_bits: u32,
    /// The second index set's bits; none mixes alpha by the first set.
    index2_bits: u32,
}

/// A [`Mode`], its fields in order.
#[allow(clippy::too_many_arguments)]
const fn mode(
    subsets: usize,
    partition_bits: u32,
    rotation_bits: u32,
    select_bits: u32,
    color_bits: u32,
    alpha_bits: u32,
    pbits: PBits,
    index_bits: u32,
    index2_bits: u32,
) -> Mode {
    Mode {
        subsets,
        partition_bits,
        rotation_bits,
        select_bits,
        color_bits,
        alpha_bits,
        pbits,
        index_bits,
        index2_bits,
    }
}

const MODES: [Mode; 8] = [
    mode(3, 4, 0, 0, 4, 0, PBits::Unique, 3, 0),
    mode(2, 6, 0, 0, 6, 0, PBits::Shared, 3, 0),
    mode(3, 6, 0, 0, 5, 0, PBits::None, 2, 0),
    mode(2, 6, 0, 0, 7, 0, PBits::Unique, 2, 0),
    mode(1, 0, 2, 1, 5, 6, PBits::None, 2, 3),
    mode(1, 0, 2, 0, 7, 8, PBits::None, 2, 2),
    mode(1, 0, 0, 0, 7, 7, PBits::Unique, 4, 0),
    mode(2, 6, 0, 0, 5, 5, PBits::Unique, 2, 0),
];

/// The weight of index `k` of a `bits`-bit index, in 64ths.
fn weight(bits: u32, k: u8) -> u32 {
    const WEIGHTS2: [u32; 4] = [0, 21, 43, 64];
    const WEIGHTS3: [u32; 8] = [0, 9, 18, 27, 37, 46, 55, 64];
    const WEIGHTS4: [u32; 16] = [0, 4, 9, 13, 17, 21, 26, 30, 34, 38, 43, 47, 51, 55, 60, 64];
    match bits {
        2 => WEIGHTS2[k as usize],
        3 => WEIGHTS3[k as usize],
        _ => WEIGHTS4[k as usize],
    }
}

/// For 2-, 3- and 4-bit indices, the index whose weight lies nearest each
/// weight in `0..=64`.
const NEAREST: [[u8; 65]; 3] = {
    let tables: [&[u32]; 3] = [
        &[0, 21, 43, 64],
        &[0, 9, 18, 27, 37, 46, 55, 64],
        &[0, 4, 9, 13, 17, 21, 26, 30, 34, 38, 43, 47, 51, 55, 60, 64],
    ];
    let mut out = [[0u8; 65]; 3];
    let mut t = 0;
    while t < 3 {
        let mut w = 0;
        while w <= 64 {
            let mut k = 0;
            while k < tables[t].len() {
                let best = tables[t][out[t][w as usize] as usize];
                if tables[t][k].abs_diff(w) < best.abs_diff(w) {
                    out[t][w as usize] = k as u8;
                }
                k += 1;
            }
            w += 1;
        }
        t += 1;
    }
    out
};

/// `e0` and `e1` mixed at weight `w`.
fn interpolate(e0: u8, e1: u8, w: u32) -> u8 {
    ((u32::from(e0) * (64 - w) + u32::from(e1) * w + 32) >> 6) as u8
}

/// A `bits`-bit value widened to 8 bits by replicating its top bits.
fn widen(value: u32, bits: u32) -> u8 {
    let v = value << (8 - bits);
    (v | v >> bits) as u8
}

/// The subset of each texel in the 2-subset partitions, 2 bits a texel from
/// texel 0 in the low bits.
const PARTITIONS2: [u32; 64] = [
    0x50505050, 0x40404040, 0x54545454, 0x54505040, 0x50404000, 0x55545450, 0x55545040, 0x54504000,
    0x50400000, 0x55555450, 0x55544000, 0x54400000, 0x55555440, 0x55550000, 0x55555500, 0x55000000,
    0x55150100, 0x00004054, 0x15010000, 0x00405054, 0x00004050, 0x15050100, 0x05010000, 0x40505054,
    0x00404050, 0x05010100, 0x14141414, 0x05141450, 0x01155440, 0x00555500, 0x15014054, 0x05414150,
    0x44444444, 0x55005500, 0x11441144, 0x05055050, 0x05500550, 0x11114444, 0x41144114, 0x44111144,
    0x15055054, 0x01055040, 0x05041050, 0x05455150, 0x14414114, 0x50050550, 0x41411414, 0x00141400,
    0x00041504, 0x00105410, 0x10541000, 0x04150400, 0x50410514, 0x41051450, 0x05415014, 0x14054150,
    0x41050514, 0x41505014, 0x40011554, 0x54150140, 0x50505500, 0x00555050, 0x15151010, 0x54540404,
];

/// The subset of each texel in the 3-subset partitions; mode 0 has the first 16.
const PARTITIONS3: [u32; 64] = [
    0xaa685050, 0x6a5a5040, 0x5a5a4200, 0x5450a0a8, 0xa5a50000, 0xa0a05050, 0x5555a0a0, 0x5a5a5050,
    0xaa550000, 0xaa555500, 0xaaaa5500, 0x90909090, 0x94949494, 0xa4a4a4a4, 0xa9a59450, 0x2a0a4250,
    0xa5945040, 0x0a425054, 0xa5a5a500, 0x55a0a0a0, 0xa8a85454, 0x6a6a4040, 0xa4a45000, 0x1a1a0500,
    0x0050a4a4, 0xaaa59090, 0x14696914, 0x69691400, 0xa08585a0, 0xaa821414, 0x50a4a450, 0x6a5a0200,
    0xa9a58000, 0x5090a0a8, 0xa8a09050, 0x24242424, 0x00aa5500, 0x24924924, 0x24499224, 0x50a50a50,
    0x500aa550, 0xaaaa4444, 0x66660000, 0xa5a0a5a0, 0x50a050a0, 0x69286928, 0x44aaaa44, 0x66666600,
    0xaa444444, 0x54a854a8, 0x95809580, 0x96969600, 0xa85454a8, 0x80959580, 0xaa141414, 0x96960000,
    0xaaaa1414, 0xa05050a0, 0xa0a5a5a0, 0x96000000, 0x40804080, 0xa9a8a9a8, 0xaaaaaa44, 0x2a4a5254,
];

/// The anchor texel of the second subset of each 2-subset partition.
const ANCHOR2: [u8; 64] = [
    15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 2, 8, 2, 2, 8, 8, 15, 2, 8,
    2, 2, 8, 8, 2, 2, 15, 15, 6, 8, 2, 8, 15, 15, 2, 8, 2, 2, 2, 15, 15, 6, 6, 2, 6, 8, 15, 15, 2,
    2, 15, 15, 15, 15, 15, 2, 2, 15,
];

/// The anchor texel of the second subset of each 3-subset partition.
const ANCHOR3_SECOND: [u8; 64] = [
    3, 3, 15, 15, 8, 3, 15, 15, 8, 8, 6, 6, 6, 5, 3, 3, 3, 3, 8, 15, 3, 3, 6, 10, 5, 8, 8, 6, 8, 5,
    15, 15, 8, 15, 3, 5, 6, 10, 8, 15, 15, 3, 15, 5, 15, 15, 15, 15, 3, 15, 5, 5, 5, 8, 5, 10, 5,
    10, 8, 13, 15, 12, 3, 3,
];

/// The anchor texel of the third subset of each 3-subset partition.
const ANCHOR3_THIRD: [u8; 64] = [
    15, 8, 8, 3, 15, 15, 3, 8, 15, 15, 15, 15, 15, 15, 15, 8, 15, 8, 15, 3, 15, 8, 15, 8, 3, 15, 6,
    10, 15, 15, 10, 8, 15, 3, 15, 10, 10, 8, 9, 10, 6, 15, 8, 15, 3, 6, 6, 8, 15, 3, 15, 15, 15,
    15, 15, 15, 15, 15, 15, 15, 3, 15, 15, 8,
];

/// The subset texel `i` lies in, under `partition` of a mode with `subsets`.
fn subset_of(subsets: usize, partition: usize, i: usize) -> usize {
    match subsets {
        1 => 0,
        2 => (PARTITIONS2[partition] >> (2 * i) & 3) as usize,
        _ => (PARTITIONS3[partition] >> (2 * i) & 3) as usize,
    }
}

/// The texels of `subset`, a bit per texel.
fn subset_mask(subsets: usize, partition: usize, subset: usize) -> u16 {
    if subsets == 1 {
        return 0xffff;
    }
    let [second, third] = SUBSET_MASKS[subsets - 2][partition];
    match subset {
        0 => !(second | third),
        1 => second,
        _ => third,
    }
}

/// The texels of the second and third subsets of each 2- and 3-subset
/// partition, a bit per texel.
const SUBSET_MASKS: [[[u16; 2]; 64]; 2] = {
    let mut out = [[[0u16; 2]; 64]; 2];
    let mut p = 0;
    while p < 64 {
        let mut i = 0;
        while i < 16 {
            if PARTITIONS2[p] >> (2 * i) & 3 == 1 {
                out[0][p][0] |= 1 << i;
            }
            let s = PARTITIONS3[p] >> (2 * i) & 3;
            if s > 0 {
                out[1][p][s as usize - 1] |= 1 << i;
            }
            i += 1;
        }
        p += 1;
    }
    out
};

/// The anchor texel of each subset.
fn anchors(subsets: usize, partition: usize) -> [usize; 3] {
    match subsets {
        1 => [0; 3],
        2 => [0, usize::from(ANCHOR2[partition]), 0],
        _ => [
            0,
            usize::from(ANCHOR3_SECOND[partition]),
            usize::from(ANCHOR3_THIRD[partition]),
        ],
    }
}

/// A BC7 block's fields.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
struct Block {
    mode: usize,
    partition: usize,
    rotation: u32,
    select: u32,
    /// Stored endpoint codes by subset, endpoint and channel, without p-bits.
    endpoints: [[[u8; 4]; 2]; 3],
    /// P-bits by subset and endpoint; a shared one is in both.
    pbits: [[u8; 2]; 3],
    /// The index sets by texel: the first, and the second of modes 4 and 5.
    indices: [[u8; 16]; 2],
}

/// Reads fields from the low bits of a block up.
struct Reader {
    bits: u128,
    at: u32,
}

impl Reader {
    fn read(&mut self, n: u32) -> u8 {
        let v = (self.bits >> self.at) as u32 & ((1 << n) - 1);
        self.at += n;
        v as u8
    }
}

/// Writes fields into a block from the low bits up.
#[derive(Default)]
struct Writer {
    bits: u128,
    at: u32,
}

impl Writer {
    fn write(&mut self, value: u32, n: u32) {
        debug_assert!(value < 1 << n, "{value} in {n} bits");
        self.bits |= u128::from(value) << self.at;
        self.at += n;
    }
}

/// A block decoded, by texel.
pub(super) fn decode(bytes: &[u8; 16]) -> [[u8; 4]; 16] {
    parse(bytes).map_or([[0; 4]; 16], |block| texels(&block))
}

/// A block's fields; `None` for a reserved mode.
fn parse(bytes: &[u8; 16]) -> Option<Block> {
    let mode = bytes[0].trailing_zeros() as usize;
    let m = MODES.get(mode)?;
    let mut r = Reader {
        bits: u128::from_le_bytes(*bytes),
        at: mode as u32 + 1,
    };
    let mut block = Block {
        mode,
        partition: usize::from(r.read(m.partition_bits)),
        rotation: u32::from(r.read(m.rotation_bits)),
        select: u32::from(r.read(m.select_bits)),
        ..Block::default()
    };
    let channels = if m.alpha_bits > 0 { 4 } else { 3 };
    for c in 0..channels {
        let bits = if c < 3 { m.color_bits } else { m.alpha_bits };
        for s in 0..m.subsets {
            for e in 0..2 {
                block.endpoints[s][e][c] = r.read(bits);
            }
        }
    }
    for s in 0..m.subsets {
        match m.pbits {
            PBits::None => {}
            PBits::Shared => block.pbits[s] = [r.read(1); 2],
            PBits::Unique => block.pbits[s] = [r.read(1), r.read(1)],
        }
    }
    let anchors = anchors(m.subsets, block.partition);
    for i in 0..16 {
        let anchor = anchors[..m.subsets].contains(&i);
        block.indices[0][i] = r.read(m.index_bits - u32::from(anchor));
    }
    if m.index2_bits > 0 {
        for i in 0..16 {
            block.indices[1][i] = r.read(m.index2_bits - u32::from(i == 0));
        }
    }
    debug_assert_eq!(r.at, 128);
    Some(block)
}

/// A block's fields as bytes; every anchor index must have its top bit
/// clear.
fn pack(block: &Block) -> [u8; 16] {
    let m = &MODES[block.mode];
    let mut w = Writer::default();
    w.write(1 << block.mode, block.mode as u32 + 1);
    w.write(block.partition as u32, m.partition_bits);
    w.write(block.rotation, m.rotation_bits);
    w.write(block.select, m.select_bits);
    let channels = if m.alpha_bits > 0 { 4 } else { 3 };
    for c in 0..channels {
        let bits = if c < 3 { m.color_bits } else { m.alpha_bits };
        for s in 0..m.subsets {
            for e in 0..2 {
                w.write(u32::from(block.endpoints[s][e][c]), bits);
            }
        }
    }
    for s in 0..m.subsets {
        match m.pbits {
            PBits::None => {}
            PBits::Shared => w.write(u32::from(block.pbits[s][0]), 1),
            PBits::Unique => {
                w.write(u32::from(block.pbits[s][0]), 1);
                w.write(u32::from(block.pbits[s][1]), 1);
            }
        }
    }
    let anchors = anchors(m.subsets, block.partition);
    for i in 0..16 {
        let anchor = anchors[..m.subsets].contains(&i);
        w.write(
            u32::from(block.indices[0][i]),
            m.index_bits - u32::from(anchor),
        );
    }
    if m.index2_bits > 0 {
        for i in 0..16 {
            w.write(
                u32::from(block.indices[1][i]),
                m.index2_bits - u32::from(i == 0),
            );
        }
    }
    debug_assert_eq!(w.at, 128);
    w.bits.to_le_bytes()
}

/// The endpoint channel `code` of a mode with `bits` a channel, under p-bit
/// `pbit` when the mode has p-bits, widened.
fn endpoint_value(code: u8, pbit: u8, bits: u32, pbits: PBits) -> u8 {
    if pbits == PBits::None {
        widen(u32::from(code), bits)
    } else {
        widen(u32::from(code) << 1 | u32::from(pbit), bits + 1)
    }
}

/// A block's texels from its fields.
fn texels(block: &Block) -> [[u8; 4]; 16] {
    let m = &MODES[block.mode];
    let endpoints: [[[u8; 4]; 2]; 3] = from_fn(|s| {
        from_fn(|e| {
            from_fn(|c| {
                let bits = if c < 3 { m.color_bits } else { m.alpha_bits };
                if bits == 0 {
                    255
                } else {
                    endpoint_value(block.endpoints[s][e][c], block.pbits[s][e], bits, m.pbits)
                }
            })
        })
    });
    from_fn(|i| {
        let [e0, e1] = endpoints[subset_of(m.subsets, block.partition, i)];
        let (k1, k2) = (block.indices[0][i], block.indices[1][i]);
        let (color, alpha) = match (m.index2_bits, block.select) {
            (0, _) => (weight(m.index_bits, k1), weight(m.index_bits, k1)),
            (_, 0) => (weight(m.index_bits, k1), weight(m.index2_bits, k2)),
            _ => (weight(m.index2_bits, k2), weight(m.index_bits, k1)),
        };
        let mut t: [u8; 4] =
            from_fn(|c| interpolate(e0[c], e1[c], if c < 3 { color } else { alpha }));
        if block.rotation > 0 {
            t.swap(block.rotation as usize - 1, 3);
        }
        t
    })
}

/// Swaps the endpoints of every subset whose anchor index has its top bit
/// set, inverting its indices, so the anchors store without it.
fn settle_anchors(block: &mut Block) {
    let m = &MODES[block.mode];
    if m.index2_bits == 0 {
        let top = 1 << (m.index_bits - 1);
        let max = (1 << m.index_bits) - 1;
        for (s, &anchor) in anchors(m.subsets, block.partition)[..m.subsets]
            .iter()
            .enumerate()
        {
            if block.indices[0][anchor] & top == 0 {
                continue;
            }
            block.endpoints[s].swap(0, 1);
            block.pbits[s].swap(0, 1);
            for i in 0..16 {
                if subset_of(m.subsets, block.partition, i) == s {
                    block.indices[0][i] = max - block.indices[0][i];
                }
            }
        }
        return;
    }
    // The color set and the channels it mixes, then the alpha set.
    let color_set = block.select as usize;
    for (set, channels) in [(color_set, 0..3), (1 - color_set, 3..4)] {
        let bits = if set == 0 {
            m.index_bits
        } else {
            m.index2_bits
        };
        if block.indices[set][0] >> (bits - 1) == 0 {
            continue;
        }
        for c in channels {
            let [e0, e1] = &mut block.endpoints[0];
            std::mem::swap(&mut e0[c], &mut e1[c]);
        }
        let max = (1 << bits) - 1;
        block.indices[set] = block.indices[set].map(|k| max - k);
    }
}

/// The squared error of `decoded` against `texels` over RGBA, charging
/// [`PENALTY`] for a transparent texel decoded visible.
fn block_error(texels: &[[u8; 4]; 16], decoded: &[[u8; 4]; 16]) -> u32 {
    texels
        .iter()
        .zip(decoded)
        .map(|(t, d)| {
            let e = (0..4)
                .map(|c| (i32::from(t[c]) - i32::from(d[c])).pow(2) as u32)
                .sum();
            if *t == [0; 4] && e > 0 { PENALTY } else { e }
        })
        .sum()
}

/// One block's texels as a BC7 block.
pub(super) fn encode(texels: &[[u8; 4]; 16], effort: Effort) -> [u8; 16] {
    let opaque = texels.iter().all(|t| t[3] == 255);
    let mut search = Search {
        texels,
        zero: mask(texels, |t| *t == [0; 4]),
        effort,
        error: u32::MAX,
        best: Block::default(),
    };
    search.partitioned(6, 0);
    if search.error > 0 {
        search.others(opaque);
    }
    let mut block = search.best;
    settle_anchors(&mut block);
    let bytes = pack(&block);
    debug_assert_eq!(
        block_error(texels, &decode(&bytes)),
        search.error,
        "{block:?}"
    );
    bytes
}

/// How closely the subsets of each partition lie to a line: their scatter
/// off the line and along it, summed over the subsets.
struct Lines {
    off: [f32; 64],
    along: [f32; 64],
}

impl Lines {
    /// The lines through the subsets of each partition of a mode with
    /// `subsets`, over the first `channels` channels.
    fn new(texels: &[[u8; 4]; 16], subsets: usize, channels: usize) -> Lines {
        // Per texel: a count, the channels, then their products.
        let moments: [[f32; 16]; 16] = from_fn(|i| {
            let x: [f32; 4] = from_fn(|c| {
                if c < channels {
                    f32::from(texels[i][c])
                } else {
                    0.0
                }
            });
            let mut m = [0.0; 16];
            m[0] = 1.0;
            m[1..5].copy_from_slice(&x);
            let mut at = 5;
            for a in 0..4 {
                for b in a..4 {
                    m[at] = x[a] * x[b];
                    at += 1;
                }
            }
            m
        });
        let mut total = [0.0f32; 16];
        for m in &moments {
            add(&mut total, m);
        }
        let mut lines = Lines {
            off: [0.0; 64],
            along: [0.0; 64],
        };
        for (p, masks) in SUBSET_MASKS[subsets - 2].iter().enumerate() {
            let mut sums = [[0.0f32; 16]; 3];
            for (sum, &mask) in sums[1..subsets].iter_mut().zip(masks) {
                let mut bits = mask;
                while bits != 0 {
                    add(sum, &moments[bits.trailing_zeros() as usize]);
                    bits &= bits - 1;
                }
            }
            // The first subset is what the others leave of the total.
            sums[0] = from_fn(|k| total[k] - sums[1][k] - sums[2][k]);
            for sum in &sums[..subsets] {
                let (off, along) = line_fit(sum);
                lines.off[p] += off;
                lines.along[p] += along;
            }
        }
        lines
    }

    /// The `keep` best of the first `count` partitions, best first, for
    /// indices of `levels` steps along each line.
    fn best(&self, count: usize, levels: u32, keep: usize) -> impl Iterator<Item = usize> {
        // Steps spread evenly over the line leave a quantization error of
        // about the scatter along it over the steps squared.
        let quantization = 1.0 / ((levels - 1) * (levels - 1)) as f32;
        let score = |p: usize| self.off[p] + self.along[p] * quantization;
        let mut order: [usize; 64] = from_fn(|i| i);
        let order_of = |a: &usize, b: &usize| score(*a).total_cmp(&score(*b));
        let keep = keep.min(count);
        if keep < count {
            order[..count].select_nth_unstable_by(keep, order_of);
        }
        order[..keep].sort_unstable_by(order_of);
        order.into_iter().take(keep)
    }
}

/// `b` added into `a`.
fn add(a: &mut [f32; 16], b: &[f32; 16]) {
    for (a, b) in a.iter_mut().zip(b) {
        *a += b;
    }
}

/// The scatter off and along the principal axis of a subset given its
/// moments: count, channel sums, then the sums of channel products.
fn line_fit(m: &[f32; 16]) -> (f32, f32) {
    let n = m[0];
    if n == 0.0 {
        return (0.0, 0.0);
    }
    let mut scatter = [[0.0f32; 4]; 4];
    let mut at = 5;
    for a in 0..4 {
        for b in a..4 {
            let s = m[at] - m[1 + a] * m[1 + b] / n;
            scatter[a][b] = s;
            scatter[b][a] = s;
            at += 1;
        }
    }
    let trace: f32 = (0..4).map(|c| scatter[c][c]).sum();
    let along = if scatter[3][3] == 0.0 {
        largest_eigenvalue(&from_fn(|a| from_fn(|b| scatter[a][b])))
    } else {
        dominant(&scatter).1
    };
    ((trace - along).max(0.0), along)
}

/// The largest eigenvalue of the symmetric `m`, in closed form.
fn largest_eigenvalue(m: &[[f32; 3]; 3]) -> f32 {
    let off = m[0][1] * m[0][1] + m[0][2] * m[0][2] + m[1][2] * m[1][2];
    let mean = (m[0][0] + m[1][1] + m[2][2]) / 3.0;
    let spread = (0..3).map(|i| (m[i][i] - mean).powi(2)).sum::<f32>() + 2.0 * off;
    if spread <= 1e-9 {
        return mean.max(0.0);
    }
    let p = (spread / 6.0).sqrt();
    let b: [[f32; 3]; 3] =
        from_fn(|i| from_fn(|j| (m[i][j] - if i == j { mean } else { 0.0 }) / p));
    let det = b[0][0] * (b[1][1] * b[2][2] - b[1][2] * b[2][1])
        - b[0][1] * (b[1][0] * b[2][2] - b[1][2] * b[2][0])
        + b[0][2] * (b[1][0] * b[2][1] - b[1][1] * b[2][0]);
    let phi = (det / 2.0).clamp(-1.0, 1.0).acos() / 3.0;
    (mean + 2.0 * p * phi.cos()).max(0.0)
}

/// The candidates of one block, and the best so far.
struct Search<'a> {
    texels: &'a [[u8; 4]; 16],
    /// The transparent texels.
    zero: u16,
    effort: Effort,
    error: u32,
    best: Block,
}

impl Search<'_> {
    /// `mode` (one of a single index set) under `partition`.
    fn partitioned(&mut self, mode: usize, partition: usize) {
        let m = &MODES[mode];
        let coding = Coding {
            first: 0,
            count: if m.alpha_bits > 0 { 4 } else { 3 },
            bits: m.color_bits,
            pbits: m.pbits,
            index_bits: m.index_bits,
        };
        let mut block = Block {
            mode,
            partition,
            ..Block::default()
        };
        let mut error = 0;
        for s in 0..m.subsets {
            let members = subset_mask(m.subsets, partition, s);
            let pin = members & self.zero != 0;
            let fit = fit(self.texels, members, coding, pin, self.effort);
            error += fit.error;
            if error >= self.error {
                return;
            }
            block.endpoints[s] = fit.codes;
            block.pbits[s] = fit.pbits;
            for i in super::members(members) {
                block.indices[0][i] = fit.indices[i];
            }
        }
        self.offer(error, block);
    }

    /// `mode` 4 or 5, alpha swapped with channel `rotation - 1` and color
    /// mixed by index set `select`.
    fn separate(&mut self, mode: usize, rotation: u32, select: u32) {
        let m = &MODES[mode];
        let texels = self.texels.map(|mut t| {
            if rotation > 0 {
                t.swap(rotation as usize - 1, 3);
            }
            t
        });
        let (color_bits, alpha_bits) = if select == 0 {
            (m.index_bits, m.index2_bits)
        } else {
            (m.index2_bits, m.index_bits)
        };
        let pin = self.zero != 0;
        let coding = |first, count, bits, index_bits| Coding {
            first,
            count,
            bits,
            pbits: PBits::None,
            index_bits,
        };
        let color = fit(
            &texels,
            0xffff,
            coding(0, 3, m.color_bits, color_bits),
            pin,
            self.effort,
        );
        if color.error >= self.error {
            return;
        }
        let alpha = fit(
            &texels,
            0xffff,
            coding(3, 1, m.alpha_bits, alpha_bits),
            pin,
            self.effort,
        );
        let mut block = Block {
            mode,
            rotation,
            select,
            ..Block::default()
        };
        for e in 0..2 {
            let [r, g, b, _] = color.codes[e];
            block.endpoints[0][e] = [r, g, b, alpha.codes[e][0]];
        }
        let color_set = select as usize;
        block.indices[color_set] = color.indices;
        block.indices[1 - color_set] = alpha.indices;
        self.offer(color.error + alpha.error, block);
    }

    /// The modes past 6 the effort allows: 1 and 3 for an opaque block, 7
    /// for one with alpha, then 5, and with `Thorough` 0, 2 and every rotation
    /// of 4 and 5.
    fn others(&mut self, opaque: bool) {
        let thorough = self.effort == Effort::Thorough;
        let keep = if thorough { 6 } else { 1 };
        if opaque {
            let lines = Lines::new(self.texels, 2, 3);
            for p in lines.best(64, 8, keep) {
                self.partitioned(1, p);
            }
            for p in lines.best(64, 4, keep) {
                self.partitioned(3, p);
            }
            if thorough {
                let lines = Lines::new(self.texels, 3, 3);
                for p in lines.best(16, 8, 3) {
                    self.partitioned(0, p);
                }
                for p in lines.best(64, 4, 3) {
                    self.partitioned(2, p);
                }
            }
        } else {
            for p in Lines::new(self.texels, 2, 4).best(64, 4, keep) {
                self.partitioned(7, p);
            }
        }
        self.separate(5, 0, 0);
        if thorough {
            for rotation in 0..4 {
                if rotation > 0 {
                    self.separate(5, rotation, 0);
                }
                self.separate(4, rotation, 0);
                self.separate(4, rotation, 1);
            }
        }
    }

    /// `block` kept as the best if its `error` is the lowest yet.
    fn offer(&mut self, error: u32, block: Block) {
        if error < self.error {
            self.error = error;
            self.best = block;
        }
    }
}

/// How a fit codes its endpoints.
#[derive(Clone, Copy, Debug)]
struct Coding {
    /// The channels fitted, `first..first + count` of each texel.
    first: usize,
    count: usize,
    /// Stored bits per endpoint channel, without the p-bit.
    bits: u32,
    pbits: PBits,
    index_bits: u32,
}

impl Coding {
    /// The widened value of endpoint channel `code` under p-bit `pbit`.
    fn value(&self, code: u8, pbit: u8) -> u8 {
        endpoint_value(code, pbit, self.bits, self.pbits)
    }

    /// The code whose value under p-bit `pbit` lies nearest `v`.
    fn quantize(&self, v: f32, pbit: u8) -> u8 {
        let v = v.clamp(0.0, 255.0);
        let max = (1i32 << self.bits) - 1;
        let guess = if self.pbits == PBits::None {
            v * max as f32 / 255.0
        } else {
            (v * (2 * max + 1) as f32 / 255.0 - f32::from(pbit)) / 2.0
        };
        let guess = guess.round() as i32;
        (guess - 1..=guess + 1)
            .filter(|c| (0..=max).contains(c))
            .map(|c| c as u8)
            .min_by(|&a, &b| {
                let off = |c: u8| (f32::from(self.value(c, pbit)) - v).abs();
                off(a).total_cmp(&off(b))
            })
            .expect("a code")
    }

    /// The p-bit pairs to try: none, a shared one or one per endpoint, the
    /// first zero when `pin`.
    fn pbit_choices(&self, pin: bool) -> &'static [[u8; 2]] {
        match (self.pbits, pin) {
            (PBits::None, _) | (PBits::Shared, true) => &[[0, 0]],
            (PBits::Shared, false) => &[[0, 0], [1, 1]],
            (PBits::Unique, true) => &[[0, 0], [0, 1]],
            (PBits::Unique, false) => &[[0, 0], [0, 1], [1, 0], [1, 1]],
        }
    }
}

/// The endpoints of one subset (or of the color or alpha of a mode 4 or 5
/// block), in the fitted channels.
#[derive(Clone, Copy, Debug)]
struct Fit {
    codes: [[u8; 4]; 2],
    pbits: [u8; 2],
    /// Indices by texel of the block; meaningful for the subset's.
    indices: [u8; 16],
    error: u32,
}

/// The best endpoints for the `members` of a block under `coding`; the
/// first endpoint zero when `pin`.
fn fit(texels: &[[u8; 4]; 16], members: u16, coding: Coding, pin: bool, effort: Effort) -> Fit {
    let mut fitter = Fitter {
        coding,
        pin,
        effort,
        points: [[0; 4]; 16],
        texel: [0; 16],
        zero: 0,
        n: 0,
    };
    for i in super::members(members) {
        let n = fitter.n;
        fitter.points[n] = from_fn(|c| {
            if c < coding.count {
                i32::from(texels[i][coding.first + c])
            } else {
                0
            }
        });
        fitter.texel[n] = i as u8;
        fitter.zero |= u16::from(texels[i] == [0; 4]) << n;
        fitter.n += 1;
    }
    let points = fitter.points.map(|p| p.map(|v| v as f32));
    let points = &points[..fitter.n];
    let (mean, axis, _) = principal_axis(points);
    let (e0, e1) = if pin {
        // From zero out along the mean as far as the members reach.
        let norm = mean.iter().map(|v| v * v).sum::<f32>();
        let reach = if norm > 0.0 {
            let dot = |p: &[f32; 4]| (0..4).map(|c| p[c] * mean[c]).sum::<f32>() / norm;
            points.iter().map(dot).fold(0.0, f32::max)
        } else {
            0.0
        };
        ([0.0; 4], mean.map(|v| v * reach))
    } else {
        let along = |p: &[f32; 4]| (0..4).map(|c| (p[c] - mean[c]) * axis[c]).sum::<f32>();
        let lo = points.iter().map(along).fold(f32::INFINITY, f32::min);
        let hi = points.iter().map(along).fold(f32::NEG_INFINITY, f32::max);
        let at = |s: f32| -> [f32; 4] { from_fn(|c| mean[c] + axis[c] * s) };
        (at(lo), at(hi))
    };
    let choices = coding.pbit_choices(pin);
    let choices: &[[u8; 2]] = if effort == Effort::Thorough || choices.len() == 1 {
        choices
    } else {
        // The p-bits that quantize the endpoints closest.
        let off = |pbits: &[u8; 2]| -> f32 {
            (0..coding.count)
                .map(|c| {
                    let d = |e: f32, p: u8| f32::from(coding.value(coding.quantize(e, p), p)) - e;
                    d(e0[c], pbits[0]).powi(2) + d(e1[c], pbits[1]).powi(2)
                })
                .sum()
        };
        let best = choices
            .iter()
            .min_by(|a, b| off(a).total_cmp(&off(b)))
            .expect("a choice");
        std::slice::from_ref(best)
    };
    let mut best: Option<Fit> = None;
    for &pbits in choices {
        let fit = fitter.refine(e0, e1, pbits);
        if best.is_none_or(|b| fit.error < b.error) {
            best = Some(fit);
        }
    }
    let mut best = best.expect("a p-bit choice");
    let flat = fitter.points[..fitter.n]
        .iter()
        .all(|p| *p == fitter.points[0]);
    if !pin && flat {
        let flat = fitter.flat();
        if flat.error < best.error {
            best = flat;
        }
    }
    if effort == Effort::Thorough {
        fitter.polish(&mut best);
    }
    best
}

/// One subset's members and how their endpoints are coded.
struct Fitter {
    coding: Coding,
    pin: bool,
    effort: Effort,
    /// The members' fitted channels, from channel 0, zero past the coding's.
    points: [[i32; 4]; 16],
    /// The block texel of each point.
    texel: [u8; 16],
    /// The points that are transparent texels, a bit each.
    zero: u16,
    n: usize,
}

impl Fitter {
    /// Endpoints `e0` and `e1` quantized under `pbits`, then refit by least
    /// squares to the indices they select while that lowers the error.
    fn refine(&self, e0: [f32; 4], e1: [f32; 4], pbits: [u8; 2]) -> Fit {
        let iterations = if self.effort == Effort::Thorough {
            4
        } else {
            2
        };
        let quantize = |e0: [f32; 4], e1: [f32; 4]| -> [[u8; 4]; 2] {
            let q = |e: [f32; 4], p: u8| -> [u8; 4] {
                from_fn(|c| {
                    if c < self.coding.count {
                        self.coding.quantize(e[c], p)
                    } else {
                        0
                    }
                })
            };
            [
                if self.pin { [0; 4] } else { q(e0, pbits[0]) },
                q(e1, pbits[1]),
            ]
        };
        let mut current = self.evaluate(quantize(e0, e1), pbits);
        for _ in 0..iterations {
            let (e0, e1) = self.least_squares(&current.indices);
            let codes = quantize(e0, e1);
            if codes == current.codes {
                break;
            }
            let next = self.evaluate(codes, pbits);
            if next.error >= current.error {
                break;
            }
            current = next;
        }
        current
    }

    /// The endpoints that best fit the members to `indices` in least squares;
    /// the first zero when pinned.
    fn least_squares(&self, indices: &[u8; 16]) -> ([f32; 4], [f32; 4]) {
        let (mut aa, mut bb, mut ab) = (0.0f32, 0.0f32, 0.0f32);
        let (mut ax, mut bx, mut sum) = ([0.0f32; 4], [0.0f32; 4], [0.0f32; 4]);
        for (p, &i) in self.points[..self.n].iter().zip(&self.texel) {
            let t = weight(self.coding.index_bits, indices[usize::from(i)]) as f32 / 64.0;
            let s = 1.0 - t;
            aa += s * s;
            bb += t * t;
            ab += s * t;
            for c in 0..4 {
                let x = p[c] as f32;
                ax[c] += s * x;
                bx[c] += t * x;
                sum[c] += x;
            }
        }
        let mean = sum.map(|v| v / self.n as f32);
        if self.pin {
            return ([0.0; 4], if bb > 0.0 { bx.map(|v| v / bb) } else { mean });
        }
        let det = aa * bb - ab * ab;
        if det.abs() < 1e-3 {
            return (mean, mean);
        }
        (
            from_fn(|c| (bb * ax[c] - ab * bx[c]) / det),
            from_fn(|c| (aa * bx[c] - ab * ax[c]) / det),
        )
    }

    /// Each endpoint channel stepped a code up or down while that lowers the
    /// error; a pinned endpoint stays.
    fn polish(&self, best: &mut Fit) {
        let max = ((1u32 << self.coding.bits) - 1) as u8;
        let first = usize::from(self.pin);
        for _ in 0..3 {
            let mut improved = false;
            for e in first..2 {
                for c in 0..self.coding.count {
                    for up in [false, true] {
                        let code = best.codes[e][c];
                        if (up && code == max) || (!up && code == 0) {
                            continue;
                        }
                        let mut codes = best.codes;
                        codes[e][c] = if up { code + 1 } else { code - 1 };
                        let next = self.evaluate(codes, best.pbits);
                        if next.error < best.error {
                            *best = next;
                            improved = true;
                        }
                    }
                }
            }
            if !improved {
                break;
            }
        }
    }

    /// The endpoints whose mix at one index lands nearest the value every
    /// member has, searched near it channel by channel.
    fn flat(&self) -> Fit {
        let Coding {
            count, index_bits, ..
        } = self.coding;
        let t = self.points[0];
        let max = (1i32 << self.coding.bits) - 1;
        let mut best = (u32::MAX, [[0u8; 4]; 2], [0u8; 2]);
        for &pbits in self.coding.pbit_choices(false) {
            for k in 0..1u8 << index_bits {
                let w = weight(index_bits, k);
                let mut codes = [[0u8; 4]; 2];
                let mut error = 0;
                for (c, &v) in t[..count].iter().enumerate() {
                    let near = |p: u8| {
                        let q = i32::from(self.coding.quantize(v as f32, p));
                        (q - 3).max(0)..=(q + 3).min(max)
                    };
                    let mut channel = (u32::MAX, 0, 0);
                    for c0 in near(pbits[0]) {
                        for c1 in near(pbits[1]) {
                            let (c0, c1) = (c0 as u8, c1 as u8);
                            let e0 = self.coding.value(c0, pbits[0]);
                            let e1 = self.coding.value(c1, pbits[1]);
                            let e = (i32::from(interpolate(e0, e1, w)) - v).pow(2) as u32;
                            if e < channel.0 {
                                channel = (e, c0, c1);
                            }
                        }
                    }
                    error += channel.0;
                    codes[0][c] = channel.1;
                    codes[1][c] = channel.2;
                }
                if error < best.0 {
                    best = (error, codes, pbits);
                }
            }
        }
        self.evaluate(best.1, best.2)
    }

    /// Endpoint codes `codes` under `pbits` with the index nearest each
    /// member, and the error.
    fn evaluate(&self, codes: [[u8; 4]; 2], pbits: [u8; 2]) -> Fit {
        let Coding {
            count, index_bits, ..
        } = self.coding;
        let ends: [[u8; 4]; 2] = from_fn(|e| {
            from_fn(|c| {
                if c < count {
                    self.coding.value(codes[e][c], pbits[e])
                } else {
                    0
                }
            })
        });
        let levels = 1usize << index_bits;
        let mut palette = [[0i32; 4]; 16];
        for (k, entry) in palette[..levels].iter_mut().enumerate() {
            let w = weight(index_bits, k as u8);
            *entry = from_fn(|c| i32::from(interpolate(ends[0][c], ends[1][c], w)));
        }
        let mut indices = [0u8; 16];
        let mut error = 0;
        let distance = |p: &[i32; 4], k: usize| -> u32 {
            (0..4).map(|c| (p[c] - palette[k][c]).pow(2) as u32).sum()
        };
        // Past four levels, only the entries either side of the projection
        // onto the line between the endpoints.
        let line: [i32; 4] = from_fn(|c| palette[levels - 1][c] - palette[0][c]);
        let length = line.iter().map(|v| v * v).sum::<i32>();
        let nearest = &NEAREST[index_bits as usize - 2];
        for (j, p) in self.points[..self.n].iter().enumerate() {
            let range = if levels > 4 && length > 0 {
                let dot: i32 = (0..4).map(|c| (p[c] - palette[0][c]) * line[c]).sum();
                let at = (dot * 64 + length / 2).div_euclid(length).clamp(0, 64) as usize;
                let k = usize::from(nearest[at]);
                k.saturating_sub(1)..(k + 2).min(levels)
            } else {
                0..levels
            };
            let mut best = (u32::MAX, 0);
            for k in range {
                let e = distance(p, k);
                if e < best.0 {
                    best = (e, k);
                }
            }
            indices[usize::from(self.texel[j])] = best.1 as u8;
            error += if self.zero >> j & 1 != 0 && best.0 > 0 {
                PENALTY
            } else {
                best.0
            };
        }
        Fit {
            codes,
            pbits,
            indices,
            error,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::fixtures;
    use super::*;

    #[test]
    fn anchors_lie_in_their_subsets() {
        for p in 0..64 {
            assert_eq!(subset_of(2, p, 0), 0);
            assert_eq!(subset_of(2, p, anchors(2, p)[1]), 1, "partition {p}");
            assert_eq!(subset_of(3, p, 0), 0);
            assert_eq!(subset_of(3, p, anchors(3, p)[1]), 1, "partition {p}");
            assert_eq!(subset_of(3, p, anchors(3, p)[2]), 2, "partition {p}");
            for s in 0..3 {
                let mask = subset_mask(3, p, s);
                assert!((0..16).all(|i| (mask >> i & 1 != 0) == (subset_of(3, p, i) == s)));
            }
        }
    }

    #[test]
    fn every_mode_encodes_what_it_decodes() {
        let photo = fixtures::photo(16, 8);
        let icon = fixtures::icon(16, 8);
        let block = |image: &[[u8; 4]], b: usize| -> [[u8; 4]; 16] {
            from_fn(|i| image[(i / 4) * 16 + b * 4 + i % 4])
        };
        for b in 0..4 {
            for texels in [block(&photo, b), block(&icon, b)] {
                let opaque = texels.iter().all(|t| t[3] == 255);
                for effort in [Effort::Fast, Effort::Thorough] {
                    let mut candidates = vec![(6, 0, 0)];
                    for rotation in 0..4 {
                        candidates.extend([(4, rotation, 0), (4, rotation, 1), (5, rotation, 0)]);
                    }
                    for mode in [0, 1, 2, 3, 7] {
                        if opaque || mode == 7 {
                            candidates.extend([(mode, 0, 0), (mode, 5, 0), (mode, 13, 0)]);
                        }
                    }
                    for (mode, a, b) in candidates {
                        let mut search = Search {
                            texels: &texels,
                            zero: mask(&texels, |t| *t == [0; 4]),
                            effort,
                            error: u32::MAX,
                            best: Block::default(),
                        };
                        if mode == 4 || mode == 5 {
                            search.separate(mode, a as u32, b);
                        } else {
                            search.partitioned(mode, a);
                        }
                        let mut block = search.best;
                        settle_anchors(&mut block);
                        let bytes = pack(&block);
                        assert_eq!(parse(&bytes), Some(block), "mode {mode}");
                        let decoded = decode(&bytes);
                        assert_eq!(block_error(&texels, &decoded), search.error, "mode {mode}");
                    }
                }
            }
        }
    }
}
