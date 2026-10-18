//! ETC2 RGB, RGB with punch-through alpha, and RGBA (EAC alpha).
//!
//! A color block is a big-endian 64-bit word in one of five modes. The
//! individual and differential modes split the block into two 2×4 or 4×2
//! sub-blocks, each a base color shifted along the gray axis by one of a
//! table's four modifiers; T and H hold two 4-bit colors and a distance, four
//! paint colors between them; planar holds three colors a plane through the
//! block interpolates. A differential word whose red, green or blue sum
//! overflows is a T, H or planar word. The punch-through format spends the
//! differential bit on "opaque": it has no individual mode, and a block
//! without the bit reserves texel index 2 for transparent black.
//!
//! The alpha half of an RGBA block is EAC: a base, a multiplier and one of 16
//! modifier tables give eight levels a 3-bit index per texel picks from.
//!
//! Inside a word the texels run down each column first (`x * 4 + y`); every
//! function here but the public ones works in that order. The encoders try
//! each mode their effort allows and keep the word whose decoded texels lie
//! closest to the source in squared error.

use std::array::from_fn;
use std::ops::RangeInclusive;

use super::Effort;

type Rgb = [i32; 3];

/// The small and large modifier of each table of the sub-block modes.
const MODIFIERS: [[i32; 2]; 8] = [
    [2, 8],
    [5, 17],
    [9, 29],
    [13, 42],
    [18, 60],
    [24, 80],
    [33, 106],
    [47, 183],
];

/// The distances of the T and H modes.
const DISTANCES: [i32; 8] = [3, 6, 11, 16, 23, 32, 41, 64];

/// The EAC modifier tables; entry 3 is each table's least, entry 7 its
/// greatest.
const ALPHA_MODIFIERS: [[i32; 8]; 16] = [
    [-3, -6, -9, -15, 2, 5, 8, 14],
    [-3, -7, -10, -13, 2, 6, 9, 12],
    [-2, -5, -8, -13, 1, 4, 7, 12],
    [-2, -4, -6, -13, 1, 3, 5, 12],
    [-3, -6, -8, -12, 2, 5, 7, 11],
    [-3, -7, -9, -11, 2, 6, 8, 10],
    [-4, -7, -8, -11, 3, 6, 7, 10],
    [-3, -5, -8, -11, 2, 4, 7, 10],
    [-2, -6, -8, -10, 1, 5, 7, 9],
    [-2, -5, -8, -10, 1, 4, 7, 9],
    [-2, -4, -8, -10, 1, 3, 7, 9],
    [-2, -5, -7, -10, 1, 4, 6, 9],
    [-3, -4, -7, -10, 2, 3, 6, 9],
    [-1, -2, -3, -10, 0, 1, 2, 9],
    [-4, -6, -8, -9, 3, 5, 7, 8],
    [-3, -5, -7, -9, 2, 4, 6, 8],
];

/// The EAC table and index whose modifier is 0: a flat block.
const ALPHA_FLAT: (u64, u64) = (13, 4);

/// The texels of the two sub-blocks: side by side (flip 0), then stacked
/// (flip 1).
const SUB_BLOCKS: [[[usize; 8]; 2]; 2] = [
    [[0, 1, 2, 3, 4, 5, 6, 7], [8, 9, 10, 11, 12, 13, 14, 15]],
    [[0, 1, 4, 5, 8, 9, 12, 13], [2, 3, 6, 7, 10, 11, 14, 15]],
];

/// A punch-through word every texel of which decodes transparent: a
/// differential block without its opaque bit, every index 2.
const ALL_CLEAR: u64 = 0x0000_0000_ffff_0000;

/// The error a texel decoding on the wrong side of transparent costs: more
/// than any block's color error.
const WRONG_ALPHA: u64 = 1 << 32;

/// One 4×4 block of premultiplied texels, row by row, as ETC2 RGB (alpha
/// ignored).
pub fn encode_etc2_rgb(texels: &[[u8; 4]; 16], effort: Effort) -> [u8; 8] {
    encode_color(&Target::new(texels, 0), effort, false).to_be_bytes()
}

/// An ETC2 RGB block decoded, row by row, opaque.
pub fn decode_etc2_rgb(block: &[u8; 8]) -> [[u8; 4]; 16] {
    transpose(&decode_color(u64::from_be_bytes(*block), false))
}

/// One 4×4 block of premultiplied texels, row by row, as ETC2 RGB with
/// punch-through alpha: a texel of alpha below 128 is transparent black.
pub fn encode_etc2_rgb8a1(texels: &[[u8; 4]; 16], effort: Effort) -> [u8; 8] {
    let clear = transpose(texels)
        .iter()
        .enumerate()
        .filter(|(_, t)| t[3] < 128)
        .fold(0u16, |mask, (i, _)| mask | 1 << i);
    encode_color(&Target::new(texels, clear), effort, true).to_be_bytes()
}

/// An ETC2 RGB8A1 block decoded, row by row.
pub fn decode_etc2_rgb8a1(block: &[u8; 8]) -> [[u8; 4]; 16] {
    transpose(&decode_color(u64::from_be_bytes(*block), true))
}

/// One 4×4 block of premultiplied texels, row by row, as ETC2 RGBA: the EAC
/// alpha block, then the ETC2 color block.
pub fn encode_etc2_rgba8(texels: &[[u8; 4]; 16], effort: Effort) -> [u8; 16] {
    let alpha = transpose(texels).map(|t| t[3]);
    let mut out = [0u8; 16];
    out[..8].copy_from_slice(&encode_alpha(&alpha, effort).to_be_bytes());
    out[8..].copy_from_slice(&encode_etc2_rgb(texels, effort));
    out
}

/// An ETC2 RGBA8 block decoded, row by row.
pub fn decode_etc2_rgba8(block: &[u8; 16]) -> [[u8; 4]; 16] {
    let words = block.as_chunks::<8>().0;
    let alpha = decode_alpha(u64::from_be_bytes(words[0]));
    let mut texels = decode_color(u64::from_be_bytes(words[1]), false);
    for (texel, a) in texels.iter_mut().zip(alpha) {
        texel[3] = a;
    }
    transpose(&texels)
}

/// Row-by-row texels in the column-by-column order ETC numbers them, and
/// back: the 4×4 transpose.
fn transpose<T: Copy>(texels: &[T; 16]) -> [T; 16] {
    from_fn(|i| texels[(i & 3) * 4 + (i >> 2)])
}

/// The `len`-bit field of `word` starting at bit `at`.
#[inline]
fn field(word: u64, at: u32, len: u32) -> i32 {
    ((word >> at) & ((1 << len) - 1)) as i32
}

/// A 3-bit two's-complement field.
#[inline]
fn signed3(v: i32) -> i32 {
    (v ^ 4) - 4
}

/// A `bits`-bit channel widened to 8 bits by replicating its high bits.
#[inline]
fn expand(v: i32, bits: u32) -> i32 {
    (v << (8 - bits)) | (v >> (2 * bits - 8))
}

/// `color` with `m` added to each channel, clamped.
#[inline]
fn shift(color: Rgb, m: i32) -> Rgb {
    color.map(|c| (c + m).clamp(0, 255))
}

/// Squared distance of two colors.
#[inline]
fn error(a: Rgb, b: Rgb) -> u32 {
    (0..3).map(|c| (a[c] - b[c]).pow(2) as u32).sum()
}

/// An opaque texel of `color`, or transparent black for `None`.
#[inline]
fn texel(color: Option<Rgb>) -> [u8; 4] {
    color.map_or([0; 4], |[r, g, b]| [r as u8, g as u8, b as u8, 255])
}

/// The 2-bit index of texel `i`: its high bit in the upper half of the
/// index plane, its low bit in the lower.
#[inline]
fn index(word: u64, i: usize) -> usize {
    ((word >> (16 + i) & 1) << 1 | (word >> i & 1)) as usize
}

/// The index planes of a word whose texel `i` has index `indices[i]`.
fn pack_indices(indices: &[u8; 16]) -> u64 {
    indices.iter().enumerate().fold(0, |word, (i, &k)| {
        word | u64::from(k >> 1) << (16 + i) | u64::from(k & 1) << i
    })
}

/// The modifier index `k` of `table` selects; `None` where it is transparent
/// (index 2 of a punch-through block without its opaque bit, whose 0 and 1
/// entries are 0 and the large modifier).
#[inline]
fn modifier(table: usize, k: usize, opaque: bool) -> Option<i32> {
    let [small, large] = MODIFIERS[table];
    match (k, opaque) {
        (0, true) => Some(small),
        (0, false) => Some(0),
        (1, _) => Some(large),
        (2, true) => Some(-small),
        (2, false) => None,
        _ => Some(-large),
    }
}

/// The mode of a color word.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Individual,
    Differential,
    T,
    H,
    Planar,
}

/// The mode `word` decodes in; `a1` is the punch-through format, whose bit 33
/// is the opaque bit and which has no individual mode.
fn mode(word: u64, a1: bool) -> Mode {
    if !a1 && word >> 33 & 1 == 0 {
        return Mode::Individual;
    }
    let sum = |at: u32| field(word, at, 5) + signed3(field(word, at - 3, 3));
    if !(0..32).contains(&sum(59)) {
        Mode::T
    } else if !(0..32).contains(&sum(51)) {
        Mode::H
    } else if !(0..32).contains(&sum(43)) {
        Mode::Planar
    } else {
        Mode::Differential
    }
}

/// A color word decoded, column by column; `a1` is the punch-through format.
fn decode_color(word: u64, a1: bool) -> [[u8; 4]; 16] {
    let opaque = !a1 || word >> 33 & 1 == 1;
    match mode(word, a1) {
        Mode::Individual => {
            let one = [60, 52, 44].map(|at| expand(field(word, at, 4), 4));
            let two = [56, 48, 40].map(|at| expand(field(word, at, 4), 4));
            decode_sub_blocks(word, [one, two], true)
        }
        Mode::Differential => {
            let one = [59, 51, 43].map(|at| field(word, at, 5));
            let two = [56, 48, 40].map(|at| signed3(field(word, at, 3)));
            let two = from_fn(|c| expand(one[c] + two[c], 5));
            decode_sub_blocks(word, [one.map(|v| expand(v, 5)), two], opaque)
        }
        Mode::T => {
            let r1 = field(word, 59, 2) << 2 | field(word, 56, 2);
            let c1 = [r1, field(word, 52, 4), field(word, 48, 4)].map(|v| expand(v, 4));
            let c2 = [44, 40, 36].map(|at| expand(field(word, at, 4), 4));
            let d = DISTANCES[(field(word, 34, 2) << 1 | field(word, 32, 1)) as usize];
            decode_paints(word, [c1, shift(c2, d), c2, shift(c2, -d)], opaque)
        }
        Mode::H => {
            let c1 = [
                field(word, 59, 4),
                field(word, 56, 3) << 1 | field(word, 52, 1),
                field(word, 51, 1) << 3 | field(word, 47, 3),
            ]
            .map(|v| expand(v, 4));
            let c2 = [43, 39, 35].map(|at| expand(field(word, at, 4), 4));
            let order = i32::from(value(c1) >= value(c2));
            let k = field(word, 34, 1) << 2 | field(word, 32, 1) << 1 | order;
            let d = DISTANCES[k as usize];
            let paints = [shift(c1, d), shift(c1, -d), shift(c2, d), shift(c2, -d)];
            decode_paints(word, paints, opaque)
        }
        Mode::Planar => {
            let (o, h, v) = planar_colors(word);
            from_fn(|i| {
                let (x, y) = ((i >> 2) as i32, (i & 3) as i32);
                texel(Some(from_fn(|c| planar_value(o[c], h[c], v[c], x, y))))
            })
        }
    }
}

/// The two sub-blocks of an individual or differential word around `bases`.
fn decode_sub_blocks(word: u64, bases: [Rgb; 2], opaque: bool) -> [[u8; 4]; 16] {
    let tables = [field(word, 37, 3) as usize, field(word, 34, 3) as usize];
    let flip = word >> 32 & 1 == 1;
    from_fn(|i| {
        let s = sub_block(i, flip);
        texel(modifier(tables[s], index(word, i), opaque).map(|m| shift(bases[s], m)))
    })
}

/// The sub-block texel `i` lies in.
#[inline]
fn sub_block(i: usize, flip: bool) -> usize {
    if flip { (i >> 1) & 1 } else { i >> 3 }
}

/// A T or H word's texels, each its index's paint color; index 2 is
/// transparent unless `opaque`.
fn decode_paints(word: u64, paints: [Rgb; 4], opaque: bool) -> [[u8; 4]; 16] {
    from_fn(|i| {
        let k = index(word, i);
        texel((opaque || k != 2).then_some(paints[k]))
    })
}

/// The order of an H word's two colors.
#[inline]
fn value(c: Rgb) -> i32 {
    c[0] << 16 | c[1] << 8 | c[2]
}

/// A planar word's origin, horizontal and vertical colors, widened to 8 bits.
fn planar_colors(word: u64) -> (Rgb, Rgb, Rgb) {
    let o = [
        expand(field(word, 57, 6), 6),
        expand(field(word, 56, 1) << 6 | field(word, 49, 6), 7),
        expand(
            field(word, 48, 1) << 5 | field(word, 43, 2) << 3 | field(word, 39, 3),
            6,
        ),
    ];
    let h = [
        expand(field(word, 34, 5) << 1 | field(word, 32, 1), 6),
        expand(field(word, 25, 7), 7),
        expand(field(word, 19, 6), 6),
    ];
    let v = [
        expand(field(word, 13, 6), 6),
        expand(field(word, 6, 7), 7),
        expand(field(word, 0, 6), 6),
    ];
    (o, h, v)
}

/// One channel of the plane through origin `o`, `h` four texels right and
/// `v` four texels down, at texel `(x, y)`.
#[inline]
fn planar_value(o: i32, h: i32, v: i32, x: i32, y: i32) -> i32 {
    ((x * (h - o) + y * (v - o) + 4 * o + 2) >> 2).clamp(0, 255)
}

/// An EAC word decoded, column by column.
fn decode_alpha(word: u64) -> [u8; 16] {
    let base = field(word, 56, 8);
    let multiplier = field(word, 52, 4);
    let table = &ALPHA_MODIFIERS[field(word, 48, 4) as usize];
    from_fn(|i| {
        let k = field(word, 45 - 3 * i as u32, 3) as usize;
        (base + table[k] * multiplier).clamp(0, 255) as u8
    })
}

/// The texels a color word reproduces, column by column.
struct Target {
    /// Each texel's color.
    px: [Rgb; 16],
    /// The texels that decode transparent, a bit each: any makes the word a
    /// punch-through one without its opaque bit.
    clear: u16,
}

impl Target {
    /// The target of row-by-row `texels`, `clear` (column by column) the
    /// transparent ones.
    fn new(texels: &[[u8; 4]; 16], clear: u16) -> Self {
        let px = transpose(texels).map(|[r, g, b, _]| [r, g, b].map(i32::from));
        Self { px, clear }
    }

    /// Whether the word is punch-through without its opaque bit.
    #[inline]
    fn punch(&self) -> bool {
        self.clear != 0
    }

    /// Whether texel `i` decodes transparent.
    #[inline]
    fn is_clear(&self, i: usize) -> bool {
        self.clear >> i & 1 == 1
    }

    /// The texels that count toward a color fit.
    #[inline]
    fn opaque(&self) -> u16 {
        !self.clear
    }
}

/// Squared error of `word` decoded against `t`.
fn word_error(t: &Target, word: u64, a1: bool) -> u64 {
    let decoded = decode_color(word, a1);
    let mut sum = 0u64;
    for (i, d) in decoded.iter().enumerate() {
        let clear = t.is_clear(i);
        if clear != (d[3] == 0) {
            sum += WRONG_ALPHA;
        } else if !clear {
            sum += u64::from(error([d[0], d[1], d[2]].map(i32::from), t.px[i]));
        }
    }
    sum
}

/// The color word for `t`: of the candidates of each mode `effort` searches,
/// the one that decodes closest. `a1` is the punch-through format.
fn encode_color(t: &Target, effort: Effort, a1: bool) -> u64 {
    if t.clear == u16::MAX {
        return ALL_CLEAR;
    }
    let thorough = effort == Effort::Thorough;
    let mut best = (u64::MAX, 0u64);
    let mut consider = |word: u64| {
        let e = word_error(t, word, a1);
        if e < best.0 {
            best = (e, word);
        }
    };
    for flip in [false, true] {
        if !a1 {
            consider(individual(t, flip, thorough));
        }
        consider(differential(t, flip, thorough));
    }
    if !t.punch() {
        consider(planar(t, thorough));
        if let Some(word) = h_mode(t, thorough) {
            consider(word);
        }
    }
    consider(t_mode(t, thorough));
    best.1
}

/// The index of the paint color nearest texel `i` (`None` paints are
/// transparent); 2, the transparent index, for a texel that decodes
/// transparent.
fn pick(t: &Target, i: usize, paints: &[Option<Rgb>; 4]) -> u8 {
    if t.is_clear(i) {
        return 2;
    }
    (0..4)
        .filter_map(|k| paints[k].map(|c| (error(c, t.px[i]), k)))
        .min()
        .map_or(0, |(_, k)| k as u8)
}

/// Mean color of the texels in `mask`; black when it is empty.
fn mean(t: &Target, mask: u16) -> [f32; 3] {
    let mut sum = [0i32; 3];
    let mut n = 0;
    for i in (0..16).filter(|i| mask >> i & 1 == 1) {
        for (s, v) in sum.iter_mut().zip(t.px[i]) {
            *s += v;
        }
        n += 1;
    }
    sum.map(|s| s as f32 / n.max(1) as f32)
}

/// The mask of a list of texels.
fn mask_of(ids: &[usize]) -> u16 {
    ids.iter().fold(0, |mask, &i| mask | 1 << i)
}

/// A sub-block's base color candidate: its levels, the table that fits the
/// sub-block best around it, and that fit's error.
#[derive(Debug, Clone, Copy)]
struct Base {
    q: Rgb,
    table: usize,
    err: u32,
}

/// Squared error of the counted texels of sub-block `ids` against `base` (8
/// bits a channel) under `table`, each at its best modifier.
fn sub_error(t: &Target, ids: &[usize; 8], base: Rgb, table: usize) -> u32 {
    let [small, large] = MODIFIERS[table];
    let opaque = [small, large, -small, -large];
    let punch = [0, large, -large];
    let mods: &[i32] = if t.punch() { &punch } else { &opaque };
    let mut total = 0;
    for &i in ids {
        if t.is_clear(i) {
            continue;
        }
        let p = t.px[i];
        let mut least = u32::MAX;
        for &m in mods {
            let mut e = 0;
            for c in 0..3 {
                let d = (base[c] + m).clamp(0, 255) - p[c];
                e += d * d;
            }
            least = least.min(e as u32);
        }
        total += least;
    }
    total
}

/// The table that fits sub-block `ids` best around the `bits`-bit color `q`.
fn fit_base(t: &Target, ids: &[usize; 8], q: Rgb, bits: u32) -> Base {
    let base = q.map(|v| expand(v, bits));
    (0..8)
        .map(|table| Base {
            q,
            table,
            err: sub_error(t, ids, base, table),
        })
        .min_by_key(|b| b.err)
        .expect("eight tables")
}

/// The `bits`-bit levels a channel of mean `mean` tries: the two around it,
/// widened by one each way when `thorough`.
fn levels(mean: f32, bits: u32, thorough: bool) -> RangeInclusive<i32> {
    let max = (1 << bits) - 1;
    let mut q = ((mean * max as f32 / 255.0) as i32).clamp(0, max);
    while q < max && expand(q + 1, bits) as f32 <= mean {
        q += 1;
    }
    while q > 0 && expand(q, bits) as f32 > mean {
        q -= 1;
    }
    let w = i32::from(thorough);
    (q - w).max(0)..=(q + 1 + w).min(max)
}

/// Base color candidates of sub-block `ids` at `bits` bits a channel, each
/// with its best table.
fn sub_candidates(t: &Target, ids: &[usize; 8], bits: u32, thorough: bool) -> Vec<Base> {
    let mean = mean(t, mask_of(ids) & t.opaque());
    let [rs, gs, bs] = mean.map(|m| levels(m, bits, thorough));
    let mut out = Vec::with_capacity(64);
    for r in rs {
        for g in gs.clone() {
            for b in bs.clone() {
                out.push(fit_base(t, ids, [r, g, b], bits));
            }
        }
    }
    out
}

/// The least-error candidate.
fn least(candidates: &[Base]) -> Base {
    *candidates
        .iter()
        .min_by_key(|b| b.err)
        .expect("a candidate")
}

/// The individual-mode word of orientation `flip`: each sub-block's best
/// 4-bit base.
fn individual(t: &Target, flip: bool, thorough: bool) -> u64 {
    let bases =
        SUB_BLOCKS[usize::from(flip)].map(|ids| least(&sub_candidates(t, &ids, 4, thorough)));
    pack_sub_blocks(t, bases, flip, false)
}

/// The differential-mode word of orientation `flip`: the pair of 5-bit
/// bases within reach of each other that fits best.
fn differential(t: &Target, flip: bool, thorough: bool) -> u64 {
    let [ids1, ids2] = &SUB_BLOCKS[usize::from(flip)];
    let one = sub_candidates(t, ids1, 5, thorough);
    let two = sub_candidates(t, ids2, 5, thorough);
    let near = |a: Rgb, b: Rgb| (0..3).all(|c| (-4..=3).contains(&(b[c] - a[c])));
    let (a, b) = (least(&one), least(&two));
    // Each sub-block's best base, its partner pulled within reach.
    let b_near = fit_base(
        t,
        ids2,
        from_fn(|c| b.q[c].clamp(a.q[c] - 4, a.q[c] + 3).clamp(0, 31)),
        5,
    );
    let a_near = fit_base(
        t,
        ids1,
        from_fn(|c| a.q[c].clamp(b.q[c] - 3, b.q[c] + 4).clamp(0, 31)),
        5,
    );
    let mut best = if a.err + b_near.err <= a_near.err + b.err {
        [a, b_near]
    } else {
        [a_near, b]
    };
    let mut cost = best[0].err + best[1].err;
    for x in &one {
        for y in &two {
            if x.err + y.err < cost && near(x.q, y.q) {
                best = [*x, *y];
                cost = x.err + y.err;
            }
        }
    }
    pack_sub_blocks(t, best, flip, true)
}

/// An individual (4-bit `bases`) or differential (5-bit) word, each texel at
/// its best index.
fn pack_sub_blocks(t: &Target, bases: [Base; 2], flip: bool, differential: bool) -> u64 {
    let bits = if differential { 5 } else { 4 };
    let opaque = !t.punch();
    let mut indices = [0u8; 16];
    for (s, ids) in SUB_BLOCKS[usize::from(flip)].iter().enumerate() {
        let color = bases[s].q.map(|v| expand(v, bits));
        let paints = from_fn(|k| modifier(bases[s].table, k, opaque).map(|m| shift(color, m)));
        for &i in ids {
            indices[i] = pick(t, i, &paints);
        }
    }
    let [a, b] = bases.map(|b| b.q.map(|v| v as u64));
    let word = if differential {
        let d: [u64; 3] = from_fn(|c| (b[c].wrapping_sub(a[c])) & 7);
        a[0] << 59 | d[0] << 56 | a[1] << 51 | d[1] << 48 | a[2] << 43 | d[2] << 40
    } else {
        a[0] << 60 | b[0] << 56 | a[1] << 52 | b[1] << 48 | a[2] << 44 | b[2] << 40
    };
    word | (bases[0].table as u64) << 37
        | (bases[1].table as u64) << 34
        | u64::from(differential && opaque) << 33
        | u64::from(flip) << 32
        | pack_indices(&indices)
}

/// The planar word: per channel the least-squares plane through the block,
/// its three corners quantized and searched within one level (two when
/// `thorough`) for the least error.
fn planar(t: &Target, thorough: bool) -> u64 {
    let radius = if thorough { 2 } else { 1 };
    let (mut o, mut h, mut v) = ([0; 3], [0; 3], [0; 3]);
    for c in 0..3 {
        let bits = if c == 1 { 7 } else { 6 };
        let max = (1 << bits) - 1;
        let (mut sx, mut sy, mut sum) = (0.0f32, 0.0f32, 0.0f32);
        for (i, p) in t.px.iter().enumerate() {
            let p = p[c] as f32;
            sx += ((i >> 2) as f32 - 1.5) * p;
            sy += ((i & 3) as f32 - 1.5) * p;
            sum += p;
        }
        let (dx, dy) = (sx / 20.0, sy / 20.0);
        let origin = sum / 16.0 - 1.5 * (dx + dy);
        let ideal = [origin, origin + 4.0 * dx, origin + 4.0 * dy]
            .map(|e| ((e * max as f32 / 255.0).round() as i32).clamp(0, max));
        let span = |q: i32| (q - radius).max(0)..=(q + radius).min(max);
        let mut best = (u32::MAX, ideal);
        for qo in span(ideal[0]) {
            for qh in span(ideal[1]) {
                for qv in span(ideal[2]) {
                    let (eo, eh, ev) = (expand(qo, bits), expand(qh, bits), expand(qv, bits));
                    let mut e = 0u32;
                    for (i, p) in t.px.iter().enumerate() {
                        let (x, y) = ((i >> 2) as i32, (i & 3) as i32);
                        e += (planar_value(eo, eh, ev, x, y) - p[c]).pow(2) as u32;
                    }
                    if e < best.0 {
                        best = (e, [qo, qh, qv]);
                    }
                }
            }
        }
        [o[c], h[c], v[c]] = best.1;
    }
    pack_planar(o, h, v)
}

/// A planar word of the 6-7-6-bit origin `o`, horizontal `h` and vertical
/// `v` colors; the bits around them chosen so red and green do not overflow
/// and blue does.
fn pack_planar(o: Rgb, h: Rgb, v: Rgb) -> u64 {
    let u = |v: i32| v as u64;
    let mut word = u(o[0]) << 57
        | u(o[1] >> 6) << 56
        | u(o[1] & 63) << 49
        | u(o[2] >> 5) << 48
        | u(o[2] >> 3 & 3) << 43
        | u(o[2] & 7) << 39
        | u(h[0] >> 1) << 34
        | 1 << 33
        | u(h[0] & 1) << 32
        | u(h[1]) << 25
        | u(h[2]) << 19
        | u(v[0]) << 13
        | u(v[1]) << 6
        | u(v[2]);
    if (o[0] >> 2) + signed3((o[0] & 3) << 1 | o[1] >> 6) < 0 {
        word |= 1 << 63;
    }
    if (o[1] >> 2 & 15) + signed3((o[1] & 3) << 1 | o[2] >> 5) < 0 {
        word |= 1 << 55;
    }
    if (o[2] >> 3 & 3) + (o[2] >> 1 & 3) >= 4 {
        word |= 0b111 << 45;
    } else {
        word |= 1 << 42;
    }
    word
}

/// The counted texels in two clusters, the set bits of the result and the
/// rest: two-means from the farthest pair.
fn split(t: &Target) -> u16 {
    let counted: Vec<usize> = (0..16).filter(|&i| !t.is_clear(i)).collect();
    let mut far = (0, 0, 0);
    for (n, &i) in counted.iter().enumerate() {
        for &j in &counted[n + 1..] {
            let d = error(t.px[i], t.px[j]);
            if d > far.2 {
                far = (i, j, d);
            }
        }
    }
    let as_f = |p: Rgb| p.map(|c| c as f32);
    let (mut a, mut b) = (as_f(t.px[far.0]), as_f(t.px[far.1]));
    let dist = |p: Rgb, c: [f32; 3]| (0..3).map(|k| (p[k] as f32 - c[k]).powi(2)).sum::<f32>();
    let mut mask = 0u16;
    for _ in 0..4 {
        let next = counted
            .iter()
            .filter(|&&i| dist(t.px[i], b) < dist(t.px[i], a))
            .fold(0u16, |m, &i| m | 1 << i);
        if next == mask && next != 0 {
            break;
        }
        mask = next;
        a = mean(t, t.opaque() & !mask);
        b = mean(t, mask);
    }
    mask
}

/// The 4-bit color nearest `m`.
fn round4(m: [f32; 3]) -> Rgb {
    m.map(|v| ((v / 17.0).round() as i32).clamp(0, 15))
}

/// Squared error of the texels in `mask` against `color`.
fn single_error(t: &Target, mask: u16, color: Rgb) -> u32 {
    (0..16)
        .filter(|i| mask >> i & 1 == 1)
        .map(|i| error(t.px[i], color))
        .sum()
}

/// The 4-bit color `c` within `radius` levels of the mean of the texels in
/// `mask` whose paints `c ± d` (and `c` itself when `middle`) fit them best,
/// and that error.
fn fit_spread(t: &Target, mask: u16, d: i32, middle: bool, radius: i32) -> (Rgb, u32) {
    let center = round4(mean(t, mask));
    let span = |q: i32| (q - radius).max(0)..=(q + radius).min(15);
    let ids: Vec<usize> = (0..16).filter(|i| mask >> i & 1 == 1).collect();
    let mut best = (center, u32::MAX);
    for r in span(center[0]) {
        for g in span(center[1]) {
            for b in span(center[2]) {
                let c = [r, g, b];
                let base = c.map(|v| expand(v, 4));
                let (up, down) = (shift(base, d), shift(base, -d));
                let e = ids
                    .iter()
                    .map(|&i| {
                        let p = t.px[i];
                        let e = error(up, p).min(error(down, p));
                        if middle { e.min(error(base, p)) } else { e }
                    })
                    .sum();
                if e < best.1 {
                    best = (c, e);
                }
            }
        }
    }
    best
}

/// The T-mode word: one cluster of the two-means [`split`] at the first
/// color, the other around the second; refined once more from the clusters
/// that fit settles on.
fn t_mode(t: &Target, thorough: bool) -> u64 {
    let radius = i32::from(thorough);
    let middle = !t.punch();
    let mut best = (u32::MAX, [0; 3], [0; 3], 0);
    let fit = |mask: u16, best: &mut (u32, Rgb, Rgb, usize)| {
        let rest = t.opaque() & !mask;
        for (single, spread) in [(mask, rest), (rest, mask)] {
            let c1 = round4(mean(t, single));
            let e1 = single_error(t, single, c1.map(|v| expand(v, 4)));
            for (k, &d) in DISTANCES.iter().enumerate() {
                let (c2, e2) = fit_spread(t, spread, d, middle, radius);
                if e1 + e2 < best.0 {
                    *best = (e1 + e2, c1, c2, k);
                }
            }
        }
    };
    fit(split(t), &mut best);
    let paints = t_paints(t, best.1, best.2, best.3);
    let settled = (0..16)
        .filter(|&i| !t.is_clear(i) && pick(t, i, &paints) == 0)
        .fold(0, |m, i| m | 1 << i);
    fit(settled, &mut best);
    pack_t(t, best.1, best.2, best.3)
}

/// The paint colors of a T word; index 2 transparent in a punch-through one.
fn t_paints(t: &Target, c1: Rgb, c2: Rgb, k: usize) -> [Option<Rgb>; 4] {
    let (e1, e2) = (c1.map(|v| expand(v, 4)), c2.map(|v| expand(v, 4)));
    let d = DISTANCES[k];
    [
        Some(e1),
        Some(shift(e2, d)),
        (!t.punch()).then_some(e2),
        Some(shift(e2, -d)),
    ]
}

/// A T word of 4-bit colors `c1`, `c2` and distance `k`, each texel at its
/// best paint; the bits around red chosen so it overflows.
fn pack_t(t: &Target, c1: Rgb, c2: Rgb, k: usize) -> u64 {
    let paints = t_paints(t, c1, c2, k);
    let indices = from_fn(|i| pick(t, i, &paints));
    let u = |v: i32| v as u64;
    let (hi, lo) = (c1[0] >> 2, c1[0] & 3);
    let mut word = u(hi) << 59
        | u(lo) << 56
        | u(c1[1]) << 52
        | u(c1[2]) << 48
        | u(c2[0]) << 44
        | u(c2[1]) << 40
        | u(c2[2]) << 36
        | (k as u64 >> 1) << 34
        | u64::from(!t.punch()) << 33
        | (k as u64 & 1) << 32;
    if hi + lo >= 4 {
        word |= 0b111 << 61;
    } else {
        word |= 1 << 58;
    }
    word | pack_indices(&indices)
}

/// The H-mode word of an opaque block: each cluster of the two-means
/// [`split`] around one color, at the distance that fits both best; refined
/// once more from the clusters that fit settles on. `None` when no
/// distance's order is representable.
fn h_mode(t: &Target, thorough: bool) -> Option<u64> {
    let radius = i32::from(thorough);
    let mut best: Option<(u32, Rgb, Rgb, usize)> = None;
    let fit = |mask: u16, best: &mut Option<(u32, Rgb, Rgb, usize)>| {
        let rest = t.opaque() & !mask;
        for (k, &d) in DISTANCES.iter().enumerate() {
            let (a, ea) = fit_spread(t, rest, d, false, radius);
            let (b, eb) = fit_spread(t, mask, d, false, radius);
            // The low bit of the distance is the order of the two colors.
            let odd = k & 1 == 1;
            let (c1, c2) = if (value(a) >= value(b)) == odd {
                (a, b)
            } else {
                (b, a)
            };
            if (value(c1) >= value(c2)) == odd && best.is_none_or(|(e, ..)| ea + eb < e) {
                *best = Some((ea + eb, c1, c2, k));
            }
        }
    };
    fit(split(t), &mut best);
    let (_, c1, c2, k) = best?;
    let paints = h_paints(c1, c2, k).map(Some);
    let settled = (0..16)
        .filter(|&i| pick(t, i, &paints) >= 2)
        .fold(0, |m, i| m | 1 << i);
    fit(settled, &mut best);
    best.map(|(_, c1, c2, k)| pack_h(t, c1, c2, k))
}

/// The paint colors of an H word.
fn h_paints(c1: Rgb, c2: Rgb, k: usize) -> [Rgb; 4] {
    let (e1, e2) = (c1.map(|v| expand(v, 4)), c2.map(|v| expand(v, 4)));
    let d = DISTANCES[k];
    [shift(e1, d), shift(e1, -d), shift(e2, d), shift(e2, -d)]
}

/// An opaque H word of 4-bit colors `c1`, `c2` (ordered as distance `k`'s low
/// bit says), each texel at its best paint; the bits around red and green
/// chosen so red does not overflow and green does.
fn pack_h(t: &Target, c1: Rgb, c2: Rgb, k: usize) -> u64 {
    let paints = h_paints(c1, c2, k).map(Some);
    let indices = from_fn(|i| pick(t, i, &paints));
    let u = |v: i32| v as u64;
    let mut word = u(c1[0]) << 59
        | u(c1[1] >> 1) << 56
        | u(c1[1] & 1) << 52
        | u(c1[2] >> 3) << 51
        | u(c1[2] & 7) << 47
        | u(c2[0]) << 43
        | u(c2[1]) << 39
        | u(c2[2]) << 35
        | (k as u64 >> 2) << 34
        | 1 << 33
        | (k as u64 >> 1 & 1) << 32;
    if c1[0] + signed3(c1[1] >> 1) < 0 {
        word |= 1 << 63;
    }
    if ((c1[1] & 1) << 1 | c1[2] >> 3) + ((c1[2] & 7) >> 1) >= 4 {
        word |= 0b111 << 53;
    } else {
        word |= 1 << 50;
    }
    word | pack_indices(&indices)
}

/// Error of an alpha level against a texel's alpha: squared, plus more than
/// a whole block's squared error for a texel of alpha 0 not kept at 0.
#[inline]
fn alpha_error(level: i32, alpha: i32) -> u32 {
    let d = level - alpha;
    (d * d) as u32 + if alpha == 0 && level != 0 { 1 << 22 } else { 0 }
}

/// Error of `alpha` against the eight levels of table `mods` at multiplier
/// `m` around `base`, each texel at its nearest; stops once past `bound`.
fn alpha_fit(alpha: &[i32; 16], mods: &[i32; 8], m: i32, base: i32, bound: u32) -> u32 {
    let levels = mods.map(|x| (base + x * m).clamp(0, 255));
    let mut e = 0;
    for &v in alpha {
        e += levels.iter().map(|&l| alpha_error(l, v)).min().expect("8");
        if e >= bound {
            break;
        }
    }
    e
}

/// The EAC word for column-by-column `alpha`: every table, the multipliers
/// around the one that spans the block (all of them when `effort` is
/// thorough), and per multiplier the bases a coarse then a fine step from the
/// one that centers the block.
fn encode_alpha(alpha: &[u8; 16], effort: Effort) -> u64 {
    let thorough = effort == Effort::Thorough;
    let a = alpha.map(i32::from);
    let (lo, hi) = (*a.iter().min().expect("16"), *a.iter().max().expect("16"));
    if lo == hi {
        let (table, k) = ALPHA_FLAT;
        return (lo as u64) << 56
            | 1 << 52
            | table << 48
            | (0..16).fold(0, |w, i| w | k << (45 - 3 * i));
    }
    let coarse = if thorough { 4 } else { 2 };
    let mut best = (u32::MAX, 0, 1, 0);
    for (table, mods) in ALPHA_MODIFIERS.iter().enumerate() {
        let (low, high) = (mods[3], mods[7]);
        let ideal = ((hi - lo) as f32 / (high - low) as f32).round() as i32;
        let multipliers = if thorough {
            1..=15
        } else {
            (ideal - 1).max(1)..=(ideal + 2).min(15)
        };
        for m in multipliers {
            let center = ((lo + hi) as f32 / 2.0 - (low + high) as f32 * m as f32 / 2.0).round();
            let center = center as i32;
            let mut near = (u32::MAX, center);
            for j in -coarse..=coarse {
                let base = (center + j * m).clamp(0, 255);
                let e = alpha_fit(&a, mods, m, base, near.0);
                if e < near.0 {
                    near = (e, base);
                }
            }
            let r = if thorough { m / 2 } else { 1 };
            for base in (near.1 - r).max(0)..=(near.1 + r).min(255) {
                let e = alpha_fit(&a, mods, m, base, near.0);
                if e < near.0 {
                    near = (e, base);
                }
            }
            if near.0 < best.0 {
                best = (near.0, near.1, m, table);
            }
        }
        if best.0 == 0 {
            break;
        }
    }
    let (_, base, m, table) = best;
    let levels = ALPHA_MODIFIERS[table].map(|x| (base + x * m).clamp(0, 255));
    let mut word = (base as u64) << 56 | (m as u64) << 52 | (table as u64) << 48;
    for (i, &v) in a.iter().enumerate() {
        let k = (0..8)
            .min_by_key(|&k| alpha_error(levels[k], v))
            .expect("8") as u64;
        word |= k << (45 - 3 * i);
    }
    word
}

#[cfg(test)]
mod tests {
    use super::super::{Effort, decode as decode_image, encode as encode_image, fixtures, psnr};
    use super::*;
    use viso_gpu_abi::TextureFormat;

    /// Fields of a word, each `(value, bit)`.
    fn word(fields: &[(u64, u32)]) -> u64 {
        fields.iter().fold(0, |w, &(v, at)| w | v << at)
    }

    /// Index planes for `(texel, index)` pairs, texels column by column; the
    /// rest index 0.
    fn indices(set: &[(usize, u64)]) -> u64 {
        set.iter()
            .fold(0, |w, &(i, k)| w | (k >> 1) << (16 + i) | (k & 1) << i)
    }

    /// Row-by-row position of the texel ETC numbers `i`.
    fn at(i: usize) -> usize {
        (i & 3) * 4 + (i >> 2)
    }

    fn decode_rgb(word: u64) -> [[u8; 4]; 16] {
        decode_etc2_rgb(&word.to_be_bytes())
    }

    #[test]
    fn individual_block_decodes_per_sub_block() {
        // Bases (4, 11, 9) under table 4 and (14, 3, 8) under table 0.
        let fields = [
            (4, 60),
            (14, 56),
            (11, 52),
            (3, 48),
            (9, 44),
            (8, 40),
            (4, 37),
            (0, 34),
        ];
        let w = word(&fields) | indices(&[(0, 1), (4, 2), (8, 0), (15, 3), (2, 0)]);
        assert_eq!(mode(w, false), Mode::Individual);
        let out = decode_rgb(w);
        assert_eq!(out[at(0)], [128, 247, 213, 255]);
        assert_eq!(out[at(4)], [50, 169, 135, 255]);
        assert_eq!(out[at(8)], [240, 53, 138, 255]);
        assert_eq!(out[at(15)], [230, 43, 128, 255]);
        assert_eq!(out[at(2)], [86, 205, 171, 255]);
        // Stacked: texel 8 (x 2, y 0) moves to the top sub-block, 2 (x 0, y 2)
        // to the bottom.
        let out = decode_rgb(w | 1 << 32);
        assert_eq!(out[at(8)], [86, 205, 171, 255]);
        assert_eq!(out[at(2)], [240, 53, 138, 255]);
        assert_eq!(out[at(15)], [230, 43, 128, 255]);
    }

    #[test]
    fn differential_block_decodes_offset_base() {
        // (29, 26, 8) and (29, 26, 8) + (-4, -3, 3), tables 2 and 3.
        let fields = [
            (29, 59),
            (4, 56),
            (26, 51),
            (5, 48),
            (8, 43),
            (3, 40),
            (2, 37),
            (3, 34),
            (1, 33),
        ];
        let w = word(&fields) | indices(&[(0, 1), (15, 3), (4, 2), (12, 0)]);
        assert_eq!(mode(w, false), Mode::Differential);
        let out = decode_rgb(w);
        assert_eq!(out[at(0)], [255, 243, 95, 255]);
        assert_eq!(out[at(4)], [230, 205, 57, 255]);
        assert_eq!(out[at(12)], [219, 202, 103, 255]);
        assert_eq!(out[at(15)], [164, 147, 48, 255]);
    }

    /// The T block of colors (13, 1, 8) and (4, 12, 13), distance 32;
    /// `opaque` its bit 33.
    fn t_block(opaque: u64) -> u64 {
        let fields = [
            (0b111, 61),
            (3, 59),
            (1, 56),
            (1, 52),
            (8, 48),
            (4, 44),
            (12, 40),
            (13, 36),
            (0b10, 34),
            (opaque, 33),
            (1, 32),
        ];
        word(&fields) | indices(&[(0, 0), (1, 1), (2, 2), (3, 3)])
    }

    #[test]
    fn t_block_decodes_its_paints() {
        let w = t_block(1);
        assert_eq!(mode(w, false), Mode::T);
        let out = decode_rgb(w);
        assert_eq!(out[at(0)], [221, 17, 136, 255]);
        assert_eq!(out[at(1)], [100, 236, 253, 255]);
        assert_eq!(out[at(2)], [68, 204, 221, 255]);
        assert_eq!(out[at(3)], [36, 172, 189, 255]);
    }

    #[test]
    fn h_block_decodes_its_paints() {
        // Colors (13, 1, 8) and (4, 12, 13), distance index 0b101: its low bit
        // is the first color ordering above the second.
        let fields = [
            (13, 59),
            (0, 56),
            (1, 52),
            (1, 51),
            (0, 47),
            (1, 50),
            (4, 43),
            (12, 39),
            (13, 35),
            (1, 34),
            (1, 33),
            (0, 32),
        ];
        let w = word(&fields) | indices(&[(0, 0), (1, 1), (2, 2), (3, 3)]);
        assert_eq!(mode(w, false), Mode::H);
        let out = decode_rgb(w);
        assert_eq!(out[at(0)], [253, 49, 168, 255]);
        assert_eq!(out[at(1)], [189, 0, 104, 255]);
        assert_eq!(out[at(2)], [100, 236, 253, 255]);
        assert_eq!(out[at(3)], [36, 172, 189, 255]);
    }

    /// The planar block of origin (12, 64, 62), horizontal (50, 5, 37) and
    /// vertical (40, 112, 45); `opaque` its bit 33.
    fn planar_block(opaque: u64) -> u64 {
        word(&[
            (12, 57),
            (1, 56),
            (0, 49),
            (1, 48),
            (0b111, 45),
            (3, 43),
            (6, 39),
            (25, 34),
            (opaque, 33),
            (0, 32),
            (5, 25),
            (37, 19),
            (40, 13),
            (112, 6),
            (45, 0),
        ])
    }

    const PLANAR_TEXELS: [[u8; 3]; 16] = [
        [48, 129, 251],
        [87, 99, 226],
        [126, 70, 201],
        [164, 40, 175],
        [77, 153, 234],
        [115, 123, 209],
        [154, 94, 183],
        [193, 64, 158],
        [105, 177, 217],
        [144, 147, 191],
        [183, 118, 166],
        [221, 88, 141],
        [134, 201, 199],
        [172, 171, 174],
        [211, 142, 149],
        [250, 112, 124],
    ];

    #[test]
    fn planar_block_decodes_its_plane() {
        let w = planar_block(1);
        assert_eq!(mode(w, false), Mode::Planar);
        let out = decode_rgb(w);
        for (texel, &[r, g, b]) in out.iter().zip(&PLANAR_TEXELS) {
            assert_eq!(*texel, [r, g, b, 255]);
        }
    }

    #[test]
    fn eac_block_decodes_levels() {
        // Base 103, multiplier 2, table 13 [-1, -2, -3, -10, 0, 1, 2, 9].
        let alpha = word(&[(103, 56), (2, 52), (13, 48), (3, 45), (4, 42), (7, 0)]);
        let color = planar_block(1);
        let mut block = [0u8; 16];
        block[..8].copy_from_slice(&alpha.to_be_bytes());
        block[8..].copy_from_slice(&color.to_be_bytes());
        let out = decode_etc2_rgba8(&block);
        assert_eq!(out[at(0)][3], 83);
        assert_eq!(out[at(1)][3], 103);
        assert_eq!(out[at(15)][3], 121);
        assert_eq!(out[at(5)][3], 101);
        assert_eq!(out[at(0)][..3], PLANAR_TEXELS[at(0)]);
        // A zero multiplier leaves every texel at the base.
        assert!(decode_alpha(alpha & !(15 << 52)).iter().all(|&a| a == 103));
    }

    #[test]
    fn punch_through_blocks_reserve_index_two() {
        let fields = [
            (29, 59),
            (4, 56),
            (26, 51),
            (5, 48),
            (8, 43),
            (3, 40),
            (2, 37),
            (3, 34),
        ];
        let w = word(&fields) | indices(&[(0, 2), (4, 0), (8, 1), (15, 3)]);
        assert_eq!(mode(w, true), Mode::Differential);
        let out = decode_etc2_rgb8a1(&w.to_be_bytes());
        assert_eq!(out[at(0)], [0, 0, 0, 0]);
        assert_eq!(
            out[at(4)],
            [239, 214, 66, 255],
            "index 0 is the base itself"
        );
        assert_eq!(out[at(8)], [248, 231, 132, 255]);
        assert_eq!(out[at(15)], [164, 147, 48, 255]);
        // Opaque: the regular table, no transparency.
        let out = decode_etc2_rgb8a1(&(w | 1 << 33).to_be_bytes());
        assert_eq!(out[at(0)], [230, 205, 57, 255]);
        assert_eq!(out[at(4)], [248, 223, 75, 255]);
        // T: paint 2 is transparent without the opaque bit.
        let out = decode_etc2_rgb8a1(&t_block(0).to_be_bytes());
        assert_eq!(out[at(2)], [0, 0, 0, 0]);
        assert_eq!(out[at(1)], [100, 236, 253, 255]);
        let out = decode_etc2_rgb8a1(&t_block(1).to_be_bytes());
        assert_eq!(out[at(2)], [68, 204, 221, 255]);
        // Planar is opaque whatever the bit says.
        let out = decode_etc2_rgb8a1(&planar_block(0).to_be_bytes());
        assert!(out.iter().all(|t| t[3] == 255));
        assert_eq!(out[at(0)][..3], PLANAR_TEXELS[at(0)]);
    }

    /// The 4×4 blocks of a `w × h` image, row by row.
    fn blocks(texels: &[[u8; 4]], w: usize) -> Vec<[[u8; 4]; 16]> {
        let h = texels.len() / w;
        let mut out = Vec::new();
        for by in 0..h / 4 {
            for bx in 0..w / 4 {
                out.push(from_fn(|i| texels[(by * 4 + i / 4) * w + bx * 4 + i % 4]));
            }
        }
        out
    }

    #[test]
    fn every_packer_lands_in_its_mode() {
        let photo = fixtures::photo(32, 32);
        for texels in blocks(&photo, 32) {
            let t = Target::new(&texels, 0);
            for flip in [false, true] {
                assert_eq!(mode(individual(&t, flip, false), false), Mode::Individual);
                assert_eq!(
                    mode(differential(&t, flip, false), false),
                    Mode::Differential
                );
            }
            assert_eq!(mode(planar(&t, false), false), Mode::Planar);
            assert_eq!(mode(t_mode(&t, false), false), Mode::T);
            if let Some(w) = h_mode(&t, false) {
                assert_eq!(mode(w, false), Mode::H);
            }
        }
    }

    /// `texels` (`w × h`) through `format` at `effort`.
    fn round_trip(
        format: TextureFormat,
        w: u32,
        h: u32,
        texels: &[[u8; 4]],
        effort: Effort,
    ) -> Vec<[u8; 4]> {
        let bytes = encode_image(format, w, h, texels, effort);
        assert_eq!(bytes.len(), format.data_size(w, h));
        decode_image(format, w, h, &bytes).expect("decodes")
    }

    #[test]
    fn a_photo_survives_rgb() {
        let photo = fixtures::photo(64, 48);
        let back = round_trip(TextureFormat::Etc2Rgb8Unorm, 64, 48, &photo, Effort::Fast);
        let db = psnr(&photo, &back);
        assert!(db > 39.0, "{db:.2} dB");
        assert!(back.iter().all(|t| t[3] == 255));
    }

    #[test]
    fn thorough_beats_fast() {
        let photo = fixtures::photo(16, 16);
        let fast = round_trip(TextureFormat::Etc2Rgb8Unorm, 16, 16, &photo, Effort::Fast);
        let thorough = round_trip(
            TextureFormat::Etc2Rgb8Unorm,
            16,
            16,
            &photo,
            Effort::Thorough,
        );
        let (fast, thorough) = (psnr(&photo, &fast), psnr(&photo, &thorough));
        assert!(thorough >= fast, "{thorough:.2} < {fast:.2} dB");
        assert!(thorough > 32.5, "{thorough:.2} dB");
        let icon = fixtures::icon(16, 16);
        let back = round_trip(
            TextureFormat::Etc2Rgba8Unorm,
            16,
            16,
            &icon,
            Effort::Thorough,
        );
        let db = psnr(&icon, &back);
        assert!(db > 32.0, "{db:.2} dB");
    }

    #[test]
    fn an_icon_survives_rgba8_and_transparency_holds() {
        let icon = fixtures::icon(64, 48);
        let back = round_trip(TextureFormat::Etc2Rgba8Unorm, 64, 48, &icon, Effort::Fast);
        let db = psnr(&icon, &back);
        assert!(db > 37.4, "{db:.2} dB");
        for (p, q) in icon.iter().zip(&back) {
            if p[3] == 0 {
                assert_eq!(q[3], 0, "{p:?} -> {q:?}");
            }
        }
    }

    #[test]
    fn a_binary_icon_survives_rgb8a1() {
        let icon = fixtures::binary(&fixtures::icon(64, 48));
        let back = round_trip(TextureFormat::Etc2Rgb8A1Unorm, 64, 48, &icon, Effort::Fast);
        let db = psnr(&icon, &back);
        assert!(db > 51.0, "{db:.2} dB");
        for (p, q) in icon.iter().zip(&back) {
            if p[3] < 128 {
                assert_eq!(*q, [0; 4]);
            } else {
                assert_eq!(q[3], 255);
            }
        }
    }

    #[test]
    fn mixed_punch_through_block_keeps_both_sides() {
        // Opaque texels in a checkerboard of two colors over transparency.
        let texels: [[u8; 4]; 16] = from_fn(|i| match (i / 4 + i % 4) % 3 {
            0 => [0, 0, 0, 0],
            1 => [200, 40, 30, 255],
            _ => [20, 90, 220, 255],
        });
        for effort in [Effort::Fast, Effort::Thorough] {
            let out = decode_etc2_rgb8a1(&encode_etc2_rgb8a1(&texels, effort));
            for (p, q) in texels.iter().zip(&out) {
                assert_eq!(p[3] == 0, *q == [0; 4], "{p:?} -> {q:?}");
            }
        }
        let clear = [[0u8; 4]; 16];
        assert_eq!(
            decode_etc2_rgb8a1(&encode_etc2_rgb8a1(&clear, Effort::Fast)),
            clear
        );
    }

    #[test]
    fn alpha_extremes_stay_exact() {
        let alpha =
            |a: [u8; 16]| -> [[u8; 4]; 16] { from_fn(|i| [a[i] / 2, a[i] / 3, a[i], a[i]]) };
        let checker = alpha(from_fn(|i| if (i / 4 + i % 4) % 2 == 0 { 0 } else { 255 }));
        let ramp = alpha(from_fn(|i| i as u8 * 17));
        let near = alpha(from_fn(|i| [0, 1, 2, 250, 255][i % 5]));
        for effort in [Effort::Fast, Effort::Thorough] {
            let out = decode_etc2_rgba8(&encode_etc2_rgba8(&checker, effort));
            assert!(checker.iter().zip(&out).all(|(p, q)| p[3] == q[3]));
            for (texels, tolerance) in [(ramp, 20), (near, 3)] {
                let out = decode_etc2_rgba8(&encode_etc2_rgba8(&texels, effort));
                for (p, q) in texels.iter().zip(&out) {
                    assert!(p[3] != 0 || q[3] == 0, "{effort:?}: {p:?} -> {q:?}");
                    assert!(
                        p[3].abs_diff(q[3]) <= tolerance,
                        "{effort:?}: {p:?} -> {q:?}"
                    );
                }
            }
        }
        let flat = [[10, 20, 30, 77]; 16];
        let out = decode_etc2_rgba8(&encode_etc2_rgba8(&flat, Effort::Fast));
        assert!(out.iter().all(|t| t[3] == 77));
    }

    #[test]
    fn partial_blocks_round_trip() {
        let (w, h) = (13, 7);
        let photo = fixtures::photo(w, h);
        let icon = fixtures::icon(w, h);
        let binary = fixtures::binary(&icon);
        let cases = [
            (TextureFormat::Etc2Rgb8Unorm, &photo, 38.5),
            (TextureFormat::Etc2Rgba8Unorm, &icon, 29.5),
            (TextureFormat::Etc2Rgb8A1Unorm, &binary, 44.0),
        ];
        for (format, texels, floor) in cases {
            let back = round_trip(format, w, h, texels, Effort::Fast);
            assert_eq!(back.len(), (w * h) as usize);
            let db = psnr(texels, &back);
            assert!(db > floor, "{format:?}: {db:.2} dB");
        }
    }

    #[test]
    #[ignore = "a release-mode measurement: run with --release --ignored --nocapture"]
    fn measure_speed_and_quality() {
        let (w, h) = (256, 256);
        let photo = fixtures::photo(w, h);
        let icon = fixtures::icon(w, h);
        let binary = fixtures::binary(&icon);
        let cases = [
            (TextureFormat::Etc2Rgb8Unorm, &photo, "photo"),
            (TextureFormat::Etc2Rgba8Unorm, &icon, "icon"),
            (TextureFormat::Etc2Rgb8A1Unorm, &binary, "binary icon"),
        ];
        for (format, texels, name) in cases {
            for effort in [Effort::Fast, Effort::Thorough] {
                let start = std::time::Instant::now();
                let bytes = encode_image(format, w, h, texels, effort);
                let time = start.elapsed();
                let back = decode_image(format, w, h, &bytes).expect("decodes");
                println!(
                    "{format:?} {name} {w}x{h} {effort:?}: {:.2} dB, {:.1} ms",
                    psnr(texels, &back),
                    time.as_secs_f64() * 1e3
                );
            }
        }
    }
}
