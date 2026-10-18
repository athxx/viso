//! The LDR encoder. A constant block becomes a void-extent block; any other
//! is searched over candidate encodings, each fitted, quantized exactly as
//! the decoder will read it, decoded, and measured against the texels.
//!
//! A candidate is a partitioning (one partition, or for `Thorough` two along
//! the hashed patterns closest to a two-cluster split), a color endpoint
//! mode (luminance or RGB, with alpha when the block is not opaque), one
//! weight plane or, for `Thorough`, two (the second for alpha, or for the
//! color channel least correlated with the others), and a block mode: a
//! weight grid no larger than the footprint and a weight range, which
//! together leave the bits that fix the color range. Block modes are ranked
//! by an estimate from the ideal fit (the error of the weights decimated to
//! the grid and quantized, plus the expected color quantization error), and
//! the best few are encoded:
//!
//! 1. endpoints along each partition's principal axis, oriented so the
//!    decoder never swaps them, and each texel's ideal weight on that axis;
//! 2. ideal weights decimated to the grid by least squares (the
//!    pseudo-inverse of the infill) and quantized;
//! 3. endpoints refitted by least squares to the weights as decoded, then
//!    quantized channel by channel to the level pair that decodes closest,
//!    color held to alpha so a decoded texel stays premultiplied;
//! 4. weights reprojected onto the quantized endpoints and requantized, and
//!    the endpoints refitted again.
//!
//! Each pass is packed, decoded and measured; the closest block wins.
//! `Thorough` then encodes its few closest candidates again with a local
//! search over the grid weights and a wider endpoint search. Weights that
//! reach a fully transparent texel are pinned to its endpoint, so
//! transparency survives exactly.

use std::collections::HashSet;
use std::sync::OnceLock;

use super::decode::{
    block_mode, color_range, decode_block, endpoint_values, infill, interpolate, select_partition,
    unorm8,
};
use super::ise;
use super::tables::{COLORS, MIN_COLOR_RANGE, WEIGHT_RANGES, WEIGHTS, levels, sequence_bits};
use crate::codec::Effort;

/// The most texels a footprint holds, 12×12.
const MAX_TEXELS: usize = 144;

/// A range's levels in increasing unquantized value: the stored value at
/// each rank and what it unquantizes to.
struct Ladder {
    code: Vec<u8>,
    value: Vec<u8>,
    /// The rank nearest each integer value.
    near: Vec<u8>,
}

impl Ladder {
    /// The ladder of a range whose stored values unquantize through `table`.
    fn new(table: &[u8], levels: usize) -> Self {
        let mut code: Vec<u8> = (0..levels as u16).map(|c| c as u8).collect();
        code.sort_by_key(|&c| table[usize::from(c)]);
        let value: Vec<u8> = code.iter().map(|&c| table[usize::from(c)]).collect();
        let near = (0..=255u8)
            .map(|v| {
                let f = value.partition_point(|&x| x <= v).saturating_sub(1);
                let up = f + 1 < value.len() && value[f + 1] - v < v.saturating_sub(value[f]);
                (f + usize::from(up)) as u8
            })
            .collect();
        Self { code, value, near }
    }

    fn len(&self) -> usize {
        self.value.len()
    }

    /// The highest rank whose value is at most `v`, or 0.
    fn floor(&self, v: f32) -> usize {
        self.value
            .partition_point(|&x| f32::from(x) <= v)
            .saturating_sub(1)
    }

    /// The rank whose value is nearest `v` rounded.
    fn nearest(&self, v: f32) -> usize {
        usize::from(self.near[v.round().clamp(0.0, 255.0) as usize])
    }
}

/// Every weight and color range's ladder.
struct Ladders {
    weights: Vec<Ladder>,
    colors: Vec<Ladder>,
}

fn ladders() -> &'static Ladders {
    static LADDERS: OnceLock<Ladders> = OnceLock::new();
    LADDERS.get_or_init(|| Ladders {
        weights: (0..WEIGHT_RANGES)
            .map(|r| Ladder::new(&WEIGHTS[r], levels(r) as usize))
            .collect(),
        colors: (0..21)
            .map(|r| {
                let n = if r < MIN_COLOR_RANGE {
                    0
                } else {
                    levels(r) as usize
                };
                Ladder::new(&COLORS[r], n)
            })
            .collect(),
    })
}

/// A legal block mode of a footprint.
struct Mode {
    /// The 11-bit block mode field.
    bits: u32,
    /// Its grid, an index into [`Shape::grids`].
    grid: usize,
    range: usize,
    dual: bool,
    /// The weights stored, over both planes.
    weights: usize,
    weight_bits: u32,
}

/// A weight grid over a footprint: the infill and its least-squares inverse.
struct Grid {
    count: usize,
    /// Per texel, the grid entries it interpolates and their weights in
    /// sixteenths (an entry of weight zero points at entry 0).
    taps: Vec<([u8; 4], [u8; 4])>,
    /// Per grid entry, the texels it reaches and with what weight.
    reach: Vec<Vec<(u8, u8)>>,
    /// The `count × texels` matrix taking ideal texel weights to the grid
    /// weights whose infill is closest to them.
    project: Vec<f32>,
}

/// A two-partition pattern: its seed and which texels lie in partition 1.
struct Pattern {
    seed: u32,
    mask: [u64; 3],
}

/// Everything the encoder derives from a footprint alone.
struct Shape {
    footprint: (u32, u32),
    modes: Vec<Mode>,
    grids: Vec<Grid>,
    /// The distinct two-partition patterns with both partitions occupied.
    pairs: Vec<Pattern>,
}

fn shape(footprint: (u32, u32)) -> &'static Shape {
    static SHAPES: [[OnceLock<Shape>; 13]; 13] = [const { [const { OnceLock::new() }; 13] }; 13];
    SHAPES[footprint.1 as usize][footprint.0 as usize].get_or_init(|| Shape::new(footprint))
}

impl Shape {
    fn new(footprint: (u32, u32)) -> Self {
        let texels = (footprint.0 * footprint.1) as usize;
        let mut modes: Vec<Mode> = Vec::new();
        let mut sizes: Vec<(u32, u32)> = Vec::new();
        let mut seen = HashSet::new();
        for bits in 0..2048u32 {
            if bits & 0x1ff == 0x1fc {
                continue;
            }
            let Some(m) = block_mode(bits) else { continue };
            if m.width > footprint.0 || m.height > footprint.1 {
                continue;
            }
            let count = m.width * m.height * (1 + u32::from(m.dual));
            let weight_bits = sequence_bits(m.range, count);
            if count > 64 || !(24..=96).contains(&weight_bits) || !seen.insert(m) {
                continue;
            }
            let grid = sizes
                .iter()
                .position(|&s| s == (m.width, m.height))
                .unwrap_or_else(|| {
                    sizes.push((m.width, m.height));
                    sizes.len() - 1
                });
            modes.push(Mode {
                bits,
                grid,
                range: m.range,
                dual: m.dual,
                weights: count as usize,
                weight_bits,
            });
        }
        let grids = sizes.iter().map(|&g| Grid::new(footprint, g)).collect();

        let mut pairs = Vec::new();
        let mut distinct = HashSet::new();
        for seed in 0..1024 {
            let mut mask = [0u64; 3];
            for t in 0..texels {
                let (x, y) = (t as u32 % footprint.0, t as u32 / footprint.0);
                if select_partition(seed, x, y, 2, texels < 31) == 1 {
                    mask[t / 64] |= 1 << (t % 64);
                }
            }
            let ones: u32 = mask.iter().map(|m| m.count_ones()).sum();
            if ones == 0 || ones as usize == texels {
                continue;
            }
            let canonical = if mask[0] & 1 == 1 {
                complement(mask, texels)
            } else {
                mask
            };
            if distinct.insert(canonical) {
                pairs.push(Pattern { seed, mask });
            }
        }
        Self {
            footprint,
            modes,
            grids,
            pairs,
        }
    }
}

/// `mask` with its first `n` bits inverted.
fn complement(mask: [u64; 3], n: usize) -> [u64; 3] {
    std::array::from_fn(|i| {
        let bits = n.saturating_sub(i * 64).min(64);
        let all = if bits == 64 {
            u64::MAX
        } else {
            (1u64 << bits) - 1
        };
        !mask[i] & all
    })
}

impl Grid {
    fn new(footprint: (u32, u32), size: (u32, u32)) -> Self {
        let texels = (footprint.0 * footprint.1) as usize;
        let count = (size.0 * size.1) as usize;
        let mut taps = Vec::with_capacity(texels);
        let mut reach = vec![Vec::new(); count];
        for t in 0..texels {
            let (x, y) = (t as u32 % footprint.0, t as u32 / footprint.0);
            let (idx, w) = infill(footprint, size, x, y);
            let mut tap = ([0u8; 4], [0u8; 4]);
            for k in 0..4 {
                if w[k] > 0 {
                    tap.0[k] = idx[k] as u8;
                    tap.1[k] = w[k] as u8;
                    reach[idx[k]].push((t as u8, w[k] as u8));
                }
            }
            taps.push(tap);
        }
        // The normal equations of the infill, `(MᵀM + λI) P = Mᵀ`, solved by
        // Gauss–Jordan elimination; λ keeps an entry no texel reaches at 0.
        let cols = count + texels;
        let mut a = vec![0f64; count * cols];
        for (t, (idx, w)) in taps.iter().enumerate() {
            for i in 0..4 {
                let (ri, wi) = (usize::from(idx[i]), f64::from(w[i]) / 16.0);
                if wi == 0.0 {
                    continue;
                }
                a[ri * cols + count + t] += wi;
                for j in 0..4 {
                    let wj = f64::from(w[j]) / 16.0;
                    a[ri * cols + usize::from(idx[j])] += wi * wj;
                }
            }
        }
        for i in 0..count {
            a[i * cols + i] += 1e-6;
        }
        for col in 0..count {
            let pivot = (col..count)
                .max_by(|&p, &q| a[p * cols + col].abs().total_cmp(&a[q * cols + col].abs()))
                .expect("a pivot row");
            if pivot != col {
                for k in 0..cols {
                    a.swap(pivot * cols + k, col * cols + k);
                }
            }
            let d = a[col * cols + col];
            for k in 0..cols {
                a[col * cols + k] /= d;
            }
            for row in 0..count {
                let f = a[row * cols + col];
                if row != col && f != 0.0 {
                    for k in 0..cols {
                        a[row * cols + k] -= f * a[col * cols + k];
                    }
                }
            }
        }
        let project = (0..count)
            .flat_map(|j| {
                let a = &a;
                (0..texels).map(move |t| a[j * cols + count + t] as f32)
            })
            .collect();
        Self {
            count,
            taps,
            reach,
            project,
        }
    }

    /// Ideal texel weights in `0..=1` taken to the grid.
    fn decimate(&self, ideal: &[f32]) -> [f32; 64] {
        let n = ideal.len();
        std::array::from_fn(|j| {
            if j < self.count {
                let row = &self.project[j * n..(j + 1) * n];
                row.iter()
                    .zip(ideal)
                    .map(|(p, y)| p * y)
                    .sum::<f32>()
                    .clamp(0.0, 1.0)
            } else {
                0.0
            }
        })
    }

    /// The sum the decoder rounds texel `t`'s weight from, `Σ value · tap`,
    /// for the grid `values` (each `0..=64`).
    fn accumulate(&self, t: usize, values: &[u8; 64]) -> u32 {
        let (idx, w) = &self.taps[t];
        (0..4)
            .map(|k| u32::from(values[usize::from(idx[k])]) * u32::from(w[k]))
            .sum()
    }
}

/// How a block is split and encoded, before the block mode is chosen.
struct Config {
    partitions: usize,
    seed: u32,
    /// The partition of each texel.
    part: [u8; MAX_TEXELS],
    /// The texels of each partition.
    members: [Vec<u8>; 2],
    /// The color endpoint mode every partition takes: 0, 4, 8 or 12.
    cem: u32,
    /// The channel the second weight plane drives, if any.
    ccs: Option<usize>,
}

impl Config {
    fn new(n: usize, cem: u32, ccs: Option<usize>, split: Option<&Pattern>) -> Self {
        let mut part = [0u8; MAX_TEXELS];
        let mut members = [Vec::new(), Vec::new()];
        for (t, part) in part[..n].iter_mut().enumerate() {
            let p = split.map_or(0, |s| ((s.mask[t / 64] >> (t % 64)) & 1) as usize);
            *part = p as u8;
            members[p].push(t as u8);
        }
        Self {
            partitions: if split.is_some() { 2 } else { 1 },
            seed: split.map_or(0, |s| s.seed),
            part,
            members,
            cem,
            ccs,
        }
    }

    /// The weight plane channel `c` is interpolated by.
    fn plane(&self, c: usize) -> usize {
        usize::from(self.ccs == Some(c))
    }

    fn planes(&self) -> usize {
        1 + usize::from(self.ccs.is_some())
    }

    /// Whether the endpoint mode stores alpha.
    fn alpha(&self) -> bool {
        self.cem == 4 || self.cem == 12
    }

    /// Whether the endpoint mode stores luminance alone for color.
    fn luminance(&self) -> bool {
        self.cem < 8
    }

    /// The channels fitted, alpha last when stored.
    fn channels(&self) -> &'static [usize] {
        match self.cem {
            0 => &[0, 1, 2],
            4 => &[0, 1, 2, 3],
            8 => &[0, 1, 2],
            _ => &[0, 1, 2, 3],
        }
    }

    /// The color values the block stores.
    fn values(&self) -> u32 {
        self.partitions as u32 * endpoint_values(self.cem)
    }

    /// The bits ahead of the color values and the plane selector.
    fn config_bits(&self) -> u32 {
        (if self.partitions == 1 { 17 } else { 29 }) + 2 * u32::from(self.ccs.is_some())
    }
}

/// The ideal, unquantized fit of a configuration.
struct Fit {
    /// Each texel's ideal weight in `0..=1`, per plane.
    ideal: [[f32; MAX_TEXELS]; 2],
    /// Each texel's squared endpoint distance over a plane's channels, what
    /// a unit of weight error costs there.
    span: [[f32; MAX_TEXELS]; 2],
    /// Per partition holding transparent texels, the endpoint of lower
    /// alpha, where they sit.
    clear_end: [Option<usize>; 2],
}

/// The texels of a block and what the search is tuned by.
struct Block<'a> {
    src: &'a [[u8; 4]],
    px: [[f32; 4]; MAX_TEXELS],
    /// Which texels are fully transparent.
    clear: [bool; MAX_TEXELS],
    n: usize,
    shape: &'static Shape,
}

/// Per partition, the ranks of both endpoints' channels in the color ladder
/// (luminance in channel 0).
type Ends = [[[u8; 4]; 2]; 2];

/// One encoding of a block, ready to pack.
#[derive(Clone)]
struct Encoding {
    ends: Ends,
    /// The rank of each grid weight in the weight ladder, per plane.
    ranks: [[u8; 64]; 2],
    /// Per partition, the endpoint its transparent texels sit on, if any.
    clear_end: [Option<usize>; 2],
}

/// One block of `texels` (`footprint` row by row) encoded as ASTC LDR.
pub(super) fn encode_block(texels: &[[u8; 4]], footprint: (u32, u32), effort: Effort) -> u128 {
    let first = texels[0];
    if texels.iter().all(|&t| t == first) {
        return void_extent(first);
    }
    let shape = shape(footprint);
    let n = texels.len();
    let mut px = [[0f32; 4]; MAX_TEXELS];
    let mut clear = [false; MAX_TEXELS];
    for (t, src) in texels.iter().enumerate() {
        px[t] = src.map(f32::from);
        clear[t] = src[3] == 0;
    }
    let block = Block {
        src: texels,
        px,
        clear,
        n,
        shape,
    };
    let thorough = effort == Effort::Thorough;

    let opaque = texels.iter().all(|t| t[3] == 255);
    let gray = texels.iter().all(|t| t[0] == t[1] && t[1] == t[2]);
    let alpha_varies = texels.iter().any(|t| t[3] != first[3]);
    let cem = match (gray, opaque) {
        (true, true) => 0,
        (true, false) => 4,
        (false, true) => 8,
        (false, false) => 12,
    };
    // Transparent texels among visible ones pin weights, which the mode
    // estimate ranks less reliably: such a block tries more modes.
    let edge = clear[..n].iter().any(|&c| c) && !clear[..n].iter().all(|&c| c);
    let keep = match (thorough, edge) {
        (false, false) => 3,
        (false, true) => 6,
        (true, false) => 8,
        (true, true) => 12,
    };
    let mut configs = vec![(Config::new(n, cem, None, None), keep)];
    if thorough {
        if alpha_varies {
            configs.push((Config::new(n, cem, Some(3), None), 4));
        }
        if !gray && let Some(c) = loosest_channel(&block, if opaque { 3 } else { 4 }) {
            configs.push((Config::new(n, cem, Some(c), None), 4));
        }
        for pattern in best_pairs(&block, 3) {
            configs.push((Config::new(n, cem, None, Some(pattern)), 3));
        }
    }

    // Every candidate encoded plainly; for `Thorough` the closest few are
    // encoded again with the weight search and the wider endpoint search.
    let fits: Vec<Fit> = configs
        .iter()
        .map(|(config, _)| fit(&block, config))
        .collect();
    let mut tried: Vec<(u64, u128, usize, &Mode)> = Vec::new();
    for (i, (config, keep)) in configs.iter().enumerate() {
        for mode in rank_modes(&block, config, &fits[i], *keep) {
            let (err, bits) = trial(&block, config, &fits[i], mode, false);
            if err == 0 {
                return bits;
            }
            tried.push((err, bits, i, mode));
        }
    }
    // One partition on one plane has a legal mode in every footprint.
    tried.sort_by_key(|t| t.0);
    let mut best = (tried[0].0, tried[0].1);
    if thorough {
        for &(_, _, i, mode) in tried.iter().take(REFINED) {
            let (err, bits) = trial(&block, &configs[i].0, &fits[i], mode, true);
            if err < best.0 {
                best = (err, bits);
            }
        }
    }
    best.1
}

/// The candidates `Thorough` refines.
const REFINED: usize = 3;

/// A void-extent block of one color, its extent left unbounded.
fn void_extent(c: [u8; 4]) -> u128 {
    let mut block = 0xffff_ffff_ffff_fdfcu128;
    for (i, &v) in c.iter().enumerate() {
        block |= u128::from(u16::from(v) * 257) << (64 + 16 * i);
    }
    block
}

/// The color channel (of the first `channels`) least correlated with the
/// others, the one a second weight plane frees the most; `None` when no
/// channel varies enough to matter.
fn loosest_channel(block: &Block, channels: usize) -> Option<usize> {
    let n = block.n as f32;
    let mut mean = [0f32; 4];
    for p in &block.px[..block.n] {
        for c in 0..4 {
            mean[c] += p[c] / n;
        }
    }
    let mut cov = [[0f32; 4]; 4];
    for p in &block.px[..block.n] {
        for i in 0..4 {
            for j in 0..4 {
                cov[i][j] += (p[i] - mean[i]) * (p[j] - mean[j]);
            }
        }
    }
    (0..3)
        .filter(|&c| cov[c][c] > n)
        .map(|c| {
            let score: f32 = (0..channels)
                .filter(|&d| d != c)
                .map(|d| {
                    let v = cov[c][c] * cov[d][d];
                    if v > 0.0 {
                        cov[c][d].abs() / v.sqrt()
                    } else {
                        0.0
                    }
                })
                .sum();
            (c, score)
        })
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(c, _)| c)
}

/// The `count` two-partition patterns closest to a two-cluster split of the
/// block's texels.
fn best_pairs<'s>(block: &Block<'s>, count: usize) -> Vec<&'static Pattern> {
    let n = block.n;
    let px = &block.px[..n];
    // Two clusters seeded by a split along the principal axis, then refined.
    let all: Vec<u8> = (0..n as u8).collect();
    let (mean, axis) = principal_axis(px, &all, [true; 4]);
    let mut label: Vec<bool> = px.iter().map(|p| dot(sub(*p, mean), axis) > 0.0).collect();
    for _ in 0..4 {
        let mut centre = [[0f32; 4]; 2];
        let mut size = [0f32; 2];
        for (p, &l) in px.iter().zip(&label) {
            let k = usize::from(l);
            size[k] += 1.0;
            for c in 0..4 {
                centre[k][c] += p[c];
            }
        }
        if size[0] == 0.0 || size[1] == 0.0 {
            break;
        }
        for k in 0..2 {
            centre[k] = centre[k].map(|v| v / size[k]);
        }
        for (p, l) in px.iter().zip(label.iter_mut()) {
            *l = distance(*p, centre[1]) < distance(*p, centre[0]);
        }
    }
    let mut mask = [0u64; 3];
    for (t, &l) in label.iter().enumerate() {
        if l {
            mask[t / 64] |= 1 << (t % 64);
        }
    }
    let mut scored: Vec<(u32, &'static Pattern)> = block
        .shape
        .pairs
        .iter()
        .map(|p| {
            let wrong: u32 = (0..3).map(|i| (p.mask[i] ^ mask[i]).count_ones()).sum();
            (wrong.min(n as u32 - wrong), p)
        })
        .collect();
    scored.sort_by_key(|s| s.0);
    scored.into_iter().take(count).map(|s| s.1).collect()
}

fn sub(a: [f32; 4], b: [f32; 4]) -> [f32; 4] {
    std::array::from_fn(|c| a[c] - b[c])
}

fn dot(a: [f32; 4], b: [f32; 4]) -> f32 {
    (0..4).map(|c| a[c] * b[c]).sum()
}

fn distance(a: [f32; 4], b: [f32; 4]) -> f32 {
    let d = sub(a, b);
    dot(d, d)
}

/// The mean of `members` of `px` and their principal axis (unit length, or
/// zero when they do not vary) over the channels in `mask`.
fn principal_axis(px: &[[f32; 4]], members: &[u8], mask: [bool; 4]) -> ([f32; 4], [f32; 4]) {
    let n = members.len().max(1) as f32;
    let mut mean = [0f32; 4];
    for &t in members {
        for c in 0..4 {
            if mask[c] {
                mean[c] += px[usize::from(t)][c] / n;
            }
        }
    }
    let mut cov = [[0f32; 4]; 4];
    for &t in members {
        let d = sub(px[usize::from(t)], mean);
        for i in 0..4 {
            for j in 0..4 {
                if mask[i] && mask[j] {
                    cov[i][j] += d[i] * d[j];
                }
            }
        }
    }
    let start = (0..4)
        .max_by(|&a, &b| cov[a][a].total_cmp(&cov[b][b]))
        .unwrap_or(0);
    if cov[start][start] <= 1e-3 {
        return (mean, [0.0; 4]);
    }
    let mut axis = cov[start];
    for _ in 0..12 {
        let next: [f32; 4] = std::array::from_fn(|i| dot(cov[i], axis));
        let len = dot(next, next).sqrt();
        if len <= 1e-12 {
            break;
        }
        axis = next.map(|v| v / len);
    }
    let len = dot(axis, axis).sqrt();
    (
        mean,
        if len > 0.0 {
            axis.map(|v| v / len)
        } else {
            [0.0; 4]
        },
    )
}

/// The ideal fit of `config`: per partition, the endpoints spanning its
/// texels along the principal axis (and along the second plane's channel),
/// oriented so the decoder keeps their order, and every texel's weight.
fn fit(block: &Block, config: &Config) -> Fit {
    let mut out = Fit {
        ideal: [[0.0; MAX_TEXELS]; 2],
        span: [[0.0; MAX_TEXELS]; 2],
        clear_end: [None; 2],
    };
    let px = &block.px[..block.n];
    let mut mask = [false; 4];
    for &c in config.channels() {
        mask[c] = true;
    }
    if let Some(c) = config.ccs {
        mask[c] = false;
    }
    for p in 0..config.partitions {
        let members = &config.members[p];
        let (mean, axis) = principal_axis(px, members, mask);
        let proj = |t: u8| dot(sub(px[usize::from(t)], mean), axis);
        let (lo, hi) = members.iter().fold((f32::MAX, f32::MIN), |(lo, hi), &t| {
            (lo.min(proj(t)), hi.max(proj(t)))
        });
        let mut e = [[255f32; 4]; 2];
        for c in 0..4 {
            if mask[c] {
                e[0][c] = (mean[c] + axis[c] * lo).clamp(0.0, 255.0);
                e[1][c] = (mean[c] + axis[c] * hi).clamp(0.0, 255.0);
            }
        }
        for &t in members {
            let t = usize::from(t);
            out.ideal[0][t] = if hi > lo {
                (proj(t as u8) - lo) / (hi - lo)
            } else {
                0.0
            };
        }
        if let Some(c) = config.ccs {
            let (lo, hi) = members.iter().fold((255f32, 0f32), |(lo, hi), &t| {
                let v = px[usize::from(t)][c];
                (lo.min(v), hi.max(v))
            });
            e[0][c] = lo;
            e[1][c] = hi;
            for &t in members {
                let t = usize::from(t);
                out.ideal[1][t] = if hi > lo {
                    (px[t][c] - lo) / (hi - lo)
                } else {
                    0.0
                };
            }
        }
        if !config.luminance() && e[1][..3].iter().sum::<f32>() < e[0][..3].iter().sum::<f32>() {
            e.swap(0, 1);
            for &t in members {
                let t = usize::from(t);
                out.ideal[0][t] = 1.0 - out.ideal[0][t];
                out.ideal[1][t] = 1.0 - out.ideal[1][t];
            }
        }
        let mut span = [0f32; 2];
        for c in 0..4 {
            let d = e[1][c] - e[0][c];
            span[config.plane(c)] += d * d;
        }
        for &t in members {
            out.span[0][usize::from(t)] = span[0];
            out.span[1][usize::from(t)] = span[1];
        }
        if config.alpha() && members.iter().any(|&t| block.clear[usize::from(t)]) {
            out.clear_end[p] = Some(usize::from(e[1][3] < e[0][3]));
        }
    }
    out
}

/// The `keep` block modes for `config` with the lowest estimated error.
fn rank_modes<'s>(block: &Block<'s>, config: &Config, fit: &Fit, keep: usize) -> Vec<&'s Mode> {
    let shape = block.shape;
    let ladders = ladders();
    let n = block.n;
    let planes = config.planes();
    let channels = config.channels().len() as f32;
    // Per grid: the ideal weights decimated to it (the entries that reach a
    // transparent texel pinned), the error of their infill, and what a unit
    // of error in each entry costs over the texels it reaches.
    let mut decimated: Vec<Option<Decimated>> = vec![None; shape.grids.len()];
    let colors: [Option<usize>; 97] = std::array::from_fn(|weight_bits| {
        color_range(
            config.values(),
            128 - config.config_bits() as i32 - weight_bits as i32,
        )
    });
    let mut scored: Vec<(f32, &Mode)> = Vec::new();
    for mode in &shape.modes {
        if mode.dual != config.ccs.is_some() {
            continue;
        }
        let Some(color) = colors[mode.weight_bits as usize] else {
            continue;
        };
        let grid = &shape.grids[mode.grid];
        let d =
            decimated[mode.grid].get_or_insert_with(|| Decimated::new(block, config, fit, grid));
        // The added error of quantizing the decimated weights, entry by entry
        // (the cross terms between neighbours average out).
        let wl = &ladders.weights[mode.range];
        let mut estimate = d.floor;
        for pl in 0..planes {
            for j in 0..grid.count {
                let g = d.weights[pl][j];
                let q = f32::from(wl.value[wl.nearest(g * 64.0)]) / 64.0;
                estimate += (q - g) * (q - g) * d.cost[pl][j];
            }
        }
        let step_c = 255.0 / (levels(color) - 1) as f32;
        estimate += n as f32 * channels * step_c * step_c / 18.0;
        scored.push((estimate, mode));
    }
    scored.sort_by(|a, b| a.0.total_cmp(&b.0));
    scored.into_iter().take(keep).map(|s| s.1).collect()
}

/// A configuration's ideal weights decimated to one grid.
#[derive(Clone)]
struct Decimated {
    /// Per plane, the grid weights in `0..=1`.
    weights: [[f32; 64]; 2],
    /// The squared error of their infill against the ideal texel weights,
    /// each texel's error scaled by its span.
    floor: f32,
    /// Per plane and entry, what a unit of error there costs.
    cost: [[f32; 64]; 2],
}

impl Decimated {
    fn new(block: &Block, config: &Config, fit: &Fit, grid: &Grid) -> Self {
        let n = block.n;
        let mut out = Self {
            weights: [[0.0; 64]; 2],
            floor: 0.0,
            cost: [[0.0; 64]; 2],
        };
        for pl in 0..config.planes() {
            let g = &mut out.weights[pl];
            *g = grid.decimate(&fit.ideal[pl][..n]);
            if pl == config.plane(3) {
                for t in (0..n).filter(|&t| block.clear[t]) {
                    let Some(end) = fit.clear_end[usize::from(config.part[t])] else {
                        continue;
                    };
                    let (idx, w) = &grid.taps[t];
                    for k in (0..4).filter(|&k| w[k] > 0) {
                        g[usize::from(idx[k])] = end as f32;
                    }
                }
            }
            for t in 0..n {
                let (idx, w) = &grid.taps[t];
                let y: f32 = (0..4)
                    .map(|k| g[usize::from(idx[k])] * f32::from(w[k]))
                    .sum();
                let d = y / 16.0 - fit.ideal[pl][t];
                out.floor += d * d * fit.span[pl][t];
            }
            for (j, reach) in grid.reach.iter().enumerate() {
                out.cost[pl][j] = reach
                    .iter()
                    .map(|&(t, w)| fit.span[pl][usize::from(t)] * (f32::from(w) / 16.0).powi(2))
                    .sum();
            }
        }
        out
    }
}

/// `config` encoded with `mode`, `refine` adding the local weight search
/// and the wider endpoint search: the closest of its passes, as its squared
/// error and the block.
fn trial(block: &Block, config: &Config, fit: &Fit, mode: &Mode, refine: bool) -> (u64, u128) {
    let n = block.n;
    let grid = &block.shape.grids[mode.grid];
    let ladders = ladders();
    let wl = &ladders.weights[mode.range];
    let remaining = 128 - config.config_bits() as i32 - mode.weight_bits as i32;
    let color = color_range(config.values(), remaining).expect("a ranked mode has a color range");
    let cl = &ladders.colors[color];
    let planes = config.planes();

    let mut enc = Encoding {
        ends: [[[0; 4]; 2]; 2],
        ranks: [[0; 64]; 2],
        clear_end: fit.clear_end,
    };
    quantize_weights(grid, wl, &fit.ideal, planes, n, &mut enc.ranks);
    hold_clear(block, config, grid, wl, &mut enc);
    let mut best = (u64::MAX, 0u128);
    for pass in 0..2 {
        let weights = texel_weights(grid, wl, &enc.ranks, planes, n);
        enc.ends = fit_endpoints(block, config, &weights, cl, enc.clear_end, refine);
        orient(block, config, cl, wl, &weights, &mut enc);
        if refine {
            refine_weights(block, config, grid, wl, cl, &mut enc);
            let weights = texel_weights(grid, wl, &enc.ranks, planes, n);
            enc.ends = fit_endpoints(block, config, &weights, cl, enc.clear_end, refine);
            orient(block, config, cl, wl, &weights, &mut enc);
        }
        let bits = pack(config, mode, color, &enc);
        let err = error(block, bits);
        if err < best.0 {
            best = (err, bits);
        }
        if pass == 0 {
            let ideal = reproject(block, config, cl, &enc.ends);
            quantize_weights(grid, wl, &ideal, planes, n, &mut enc.ranks);
            hold_clear(block, config, grid, wl, &mut enc);
        }
    }
    best
}

/// What an alpha step on a transparent texel costs, relative to a color
/// step: transparency is kept exact unless no candidate can keep it.
const CLEAR_PENALTY: u64 = 255;

/// The grid weights every transparent texel reaches pinned to the endpoint
/// its partition's transparent texels sit on, so they decode to alpha 0.
fn hold_clear(block: &Block, config: &Config, grid: &Grid, wl: &Ladder, enc: &mut Encoding) {
    let pl = config.plane(3);
    for t in 0..block.n {
        let Some(end) = enc.clear_end[usize::from(config.part[t])] else {
            continue;
        };
        if !block.clear[t] {
            continue;
        }
        let (idx, w) = &grid.taps[t];
        for k in 0..4 {
            if w[k] > 0 {
                enc.ranks[pl][usize::from(idx[k])] =
                    if end == 0 { 0 } else { (wl.len() - 1) as u8 };
            }
        }
    }
}

/// Ideal texel weights decimated to the grid and quantized to ranks.
fn quantize_weights(
    grid: &Grid,
    wl: &Ladder,
    ideal: &[[f32; MAX_TEXELS]; 2],
    planes: usize,
    n: usize,
    ranks: &mut [[u8; 64]; 2],
) {
    for pl in 0..planes {
        let g = grid.decimate(&ideal[pl][..n]);
        for j in 0..grid.count {
            ranks[pl][j] = wl.nearest(g[j] * 64.0) as u8;
        }
    }
}

/// Every texel's weight (`0..=64`) per plane as the decoder infills it.
fn texel_weights(
    grid: &Grid,
    wl: &Ladder,
    ranks: &[[u8; 64]; 2],
    planes: usize,
    n: usize,
) -> [[u8; MAX_TEXELS]; 2] {
    let mut out = [[0u8; MAX_TEXELS]; 2];
    for (plane, ranks) in out.iter_mut().zip(ranks).take(planes) {
        let values = grid_values(wl, ranks);
        for (t, w) in plane[..n].iter_mut().enumerate() {
            *w = ((grid.accumulate(t, &values) + 8) >> 4) as u8;
        }
    }
    out
}

/// A plane's grid weights unquantized.
fn grid_values(wl: &Ladder, ranks: &[u8; 64]) -> [u8; 64] {
    std::array::from_fn(|j| wl.value[usize::from(ranks[j]).min(wl.len() - 1)])
}

/// The decoded endpoint colors of every partition.
fn colors(config: &Config, cl: &Ladder, ends: &Ends) -> [[[u8; 4]; 2]; 2] {
    ends.map(|pair| {
        pair.map(|e| {
            let v = e.map(|r| cl.value.get(usize::from(r)).copied().unwrap_or(0));
            let a = if config.alpha() { v[3] } else { 255 };
            if config.luminance() {
                [v[0], v[0], v[0], a]
            } else {
                [v[0], v[1], v[2], a]
            }
        })
    })
}

/// Endpoints fitted by least squares to the decoded texel `weights`, each
/// channel quantized to the level pair decoding closest to its texels.
fn fit_endpoints(
    block: &Block,
    config: &Config,
    weights: &[[u8; MAX_TEXELS]; 2],
    cl: &Ladder,
    clear_end: [Option<usize>; 2],
    wide: bool,
) -> Ends {
    let mut ends: Ends = [[[0; 4]; 2]; 2];
    let fitted: &[usize] = match config.cem {
        0 => &[0],
        4 => &[3, 0],
        8 => &[0, 1, 2],
        _ => &[3, 0, 1, 2],
    };
    let mut samples = [(0u8, 0u8); MAX_TEXELS];
    for p in 0..config.partitions {
        let members = &config.members[p];
        for &c in fitted {
            let pl = config.plane(c);
            for (s, &t) in samples.iter_mut().zip(members) {
                let t = usize::from(t);
                *s = (block.src[t][c], weights[pl][t]);
            }
            let samples = &samples[..members.len()];
            let moments = Moments::of(samples);
            let mut cap = if c < 3 && config.alpha() && config.ccs != Some(3) {
                [ends[p][0][3], ends[p][1][3]].map(|r| cl.value[usize::from(r)])
            } else {
                [255, 255]
            };
            if c == 3
                && let Some(end) = clear_end[p]
            {
                cap[end] = 0;
            }
            let (ra, rb) = quantize_pair(cl, samples, &moments, cap, wide);
            ends[p][0][c] = ra as u8;
            ends[p][1][c] = rb as u8;
        }
    }
    ends
}

/// The sums over one channel's samples, `(x, w of 64)`, that its
/// least-squares endpoints and the squared error of any endpoint pair are
/// found from, with `u = 1 − w`.
#[derive(Default)]
struct Moments {
    uu: f32,
    uw: f32,
    ww: f32,
    ux: f32,
    wx: f32,
    xx: f32,
    n: f32,
}

impl Moments {
    fn of(samples: &[(u8, u8)]) -> Self {
        let mut m = Self::default();
        for &(x, w) in samples {
            let (x, w) = (f32::from(x), f32::from(w) / 64.0);
            let u = 1.0 - w;
            m.uu += u * u;
            m.uw += u * w;
            m.ww += w * w;
            m.ux += u * x;
            m.wx += w * x;
            m.xx += x * x;
            m.n += 1.0;
        }
        m
    }

    /// The endpoint pair `(a, b)` minimizing `Σ ((1 − w)·a + w·b − x)²`.
    fn solve(&self) -> (f32, f32) {
        let det = self.uu * self.ww - self.uw * self.uw;
        if det.abs() < 1e-4 * (self.uu + self.ww).max(1.0) {
            let m = (self.ux + self.wx) / self.n.max(1.0);
            return (m, m);
        }
        let a = (self.ux * self.ww - self.wx * self.uw) / det;
        let b = (self.wx * self.uu - self.ux * self.uw) / det;
        (a.clamp(0.0, 255.0), b.clamp(0.0, 255.0))
    }

    /// `Σ ((1 − w)·a + w·b − x)²`, the error of `(a, b)` before rounding.
    fn error(&self, a: f32, b: f32) -> f32 {
        a * a * self.uu + 2.0 * a * b * self.uw + b * b * self.ww
            - 2.0 * (a * self.ux + b * self.wx)
            + self.xx
    }
}

/// The ranks of an endpoint pair near the least-squares one, each at most
/// `cap`, that decode closest to `samples`: the pairs one level either side
/// (two for `wide`) ranked by their error before rounding, and the best few
/// measured as decoded.
fn quantize_pair(
    cl: &Ladder,
    samples: &[(u8, u8)],
    moments: &Moments,
    cap: [u8; 2],
    wide: bool,
) -> (usize, usize) {
    let (a, b) = moments.solve();
    let candidates = |v: f32, cap: u8| {
        let f = cl.floor(v) as i32;
        let range = if wide { -1..=2 } else { 0..=1 };
        let mut out = [usize::MAX; 4];
        let mut k = 0;
        for d in range {
            let r = f + d;
            if r >= 0 && (r as usize) < cl.len() && cl.value[r as usize] <= cap {
                out[k] = r as usize;
                k += 1;
            }
        }
        if k == 0 {
            out[0] = cl.floor(f32::from(cap));
        }
        out
    };
    let (ca, cb) = (candidates(a, cap[0]), candidates(b, cap[1]));
    let mut ranked = [(f32::MAX, 0usize, 0usize); 3];
    for &ra in ca.iter().filter(|&&r| r != usize::MAX) {
        for &rb in cb.iter().filter(|&&r| r != usize::MAX) {
            let e = moments.error(f32::from(cl.value[ra]), f32::from(cl.value[rb]));
            if let Some(at) = ranked.iter().position(|r| e < r.0) {
                ranked.copy_within(at..2, at + 1);
                ranked[at] = (e, ra, rb);
            }
        }
    }
    let measured = ranked[..if wide { 3 } else { 2 }]
        .iter()
        .filter(|r| r.0 < f32::MAX)
        .map(|&(_, ra, rb)| (channel_error(samples, cl.value[ra], cl.value[rb]), ra, rb));
    let best = measured.min_by_key(|m| m.0).expect("a candidate pair");
    (best.1, best.2)
}

/// The squared error of one channel decoded from endpoints `va`, `vb`.
fn channel_error(samples: &[(u8, u8)], va: u8, vb: u8) -> u64 {
    samples
        .iter()
        .map(|&(x, w)| {
            let d = i64::from(interpolate(va, vb, u32::from(w))) - i64::from(x);
            (d * d) as u64
        })
        .sum()
}

/// Endpoints ordered as the decoder keeps them: for RGB endpoint modes, the
/// second endpoint's color sum at least the first's, or the decoder would
/// blue-contract and swap them. One partition swaps its endpoints and
/// inverts the weights, an exact equivalent; with more, the weights are
/// shared, so the endpoints are nudged apart instead.
fn orient(
    block: &Block,
    config: &Config,
    cl: &Ladder,
    wl: &Ladder,
    weights: &[[u8; MAX_TEXELS]; 2],
    enc: &mut Encoding,
) {
    if config.luminance() {
        return;
    }
    let sum = |e: [u8; 4]| e[..3].iter().map(|&v| u32::from(v)).sum::<u32>();
    for p in 0..config.partitions {
        loop {
            let [c0, c1] = colors(config, cl, &enc.ends)[p];
            if sum(c1) >= sum(c0) {
                break;
            }
            if config.partitions == 1 {
                enc.ends[0].swap(0, 1);
                enc.clear_end[0] = enc.clear_end[0].map(|e| 1 - e);
                for plane in &mut enc.ranks[..config.planes()] {
                    for r in plane.iter_mut() {
                        *r = (wl.len() - 1 - usize::from(*r).min(wl.len() - 1)) as u8;
                    }
                }
                break;
            }
            // The least harmful single step that closes the gap: the second
            // endpoint up a level or the first down one, in some channel.
            let mut step = (u64::MAX, 0, 0, 0u8);
            for c in 0..3 {
                let pl = config.plane(c);
                let samples: Vec<(u8, u8)> = config.members[p]
                    .iter()
                    .map(|&t| (block.src[usize::from(t)][c], weights[pl][usize::from(t)]))
                    .collect();
                let [r0, r1] = [enc.ends[p][0][c], enc.ends[p][1][c]].map(usize::from);
                if r1 + 1 < cl.len() && cl.value[r1 + 1] <= c1[3] {
                    let err = channel_error(&samples, cl.value[r0], cl.value[r1 + 1]);
                    if err < step.0 {
                        step = (err, 1, c, (r1 + 1) as u8);
                    }
                }
                if r0 > 0 {
                    let err = channel_error(&samples, cl.value[r0 - 1], cl.value[r1]);
                    if err < step.0 {
                        step = (err, 0, c, (r0 - 1) as u8);
                    }
                }
            }
            if step.0 == u64::MAX {
                break;
            }
            enc.ends[p][step.1][step.2] = step.3;
        }
    }
}

/// Texel weights projected onto the quantized endpoints.
fn reproject(block: &Block, config: &Config, cl: &Ladder, ends: &Ends) -> [[f32; MAX_TEXELS]; 2] {
    let decoded = colors(config, cl, ends);
    let mut out = [[0f32; MAX_TEXELS]; 2];
    for (members, decoded) in config.members.iter().zip(decoded).take(config.partitions) {
        let e = decoded.map(|e| e.map(f32::from));
        for (pl, out) in out.iter_mut().enumerate().take(config.planes()) {
            let axis: [f32; 4] = std::array::from_fn(|c| {
                if config.plane(c) == pl {
                    e[1][c] - e[0][c]
                } else {
                    0.0
                }
            });
            let len = dot(axis, axis);
            for &t in members {
                let t = usize::from(t);
                out[t] = if len > 0.0 {
                    (dot(sub(block.px[t], e[0]), axis) / len).clamp(0.0, 1.0)
                } else {
                    0.0
                };
            }
        }
    }
    out
}

/// A local search over the grid weights with the endpoints fixed: each
/// weight moved a level either way while that lowers the decoded error.
fn refine_weights(
    block: &Block,
    config: &Config,
    grid: &Grid,
    wl: &Ladder,
    cl: &Ladder,
    enc: &mut Encoding,
) {
    let n = block.n;
    let decoded = colors(config, cl, &enc.ends);
    // Every channel of every partition decoded at each weight.
    let table: [[[u8; 65]; 4]; 2] = std::array::from_fn(|p| {
        let [e0, e1] = decoded[p];
        std::array::from_fn(|c| std::array::from_fn(|w| interpolate(e0[c], e1[c], w as u32)))
    });
    for pl in 0..config.planes() {
        let mut channels = [0usize; 4];
        let mut k = 0;
        for c in (0..4).filter(|&c| config.plane(c) == pl) {
            channels[k] = c;
            k += 1;
        }
        let channels = &channels[..k];
        let texel_error = |t: usize, acc: u32| -> i64 {
            let row = &table[usize::from(config.part[t])];
            let w = ((acc + 8) >> 4) as usize;
            channels
                .iter()
                .map(|&c| cost(block.src[t], c, row[c][w]) as i64)
                .sum()
        };
        let mut values = grid_values(wl, &enc.ranks[pl]);
        let mut acc = [0u32; MAX_TEXELS];
        let mut err = [0i64; MAX_TEXELS];
        for t in 0..n {
            acc[t] = grid.accumulate(t, &values);
            err[t] = texel_error(t, acc[t]);
        }
        for _ in 0..2 {
            let mut moved = false;
            for (j, reach) in grid.reach.iter().enumerate() {
                let cur = usize::from(enc.ranks[pl][j]);
                let mut best = (0i64, cur);
                for cand in [cur.wrapping_sub(1), cur + 1] {
                    if cand >= wl.len() {
                        continue;
                    }
                    let dv = i32::from(wl.value[cand]) - i32::from(values[j]);
                    let delta: i64 = reach
                        .iter()
                        .map(|&(t, w)| {
                            let t = usize::from(t);
                            let new = (acc[t] as i32 + dv * i32::from(w)) as u32;
                            texel_error(t, new) - err[t]
                        })
                        .sum();
                    if delta < best.0 {
                        best = (delta, cand);
                    }
                }
                if best.1 != cur {
                    let dv = i32::from(wl.value[best.1]) - i32::from(values[j]);
                    for &(t, w) in reach {
                        let t = usize::from(t);
                        acc[t] = (acc[t] as i32 + dv * i32::from(w)) as u32;
                        err[t] = texel_error(t, acc[t]);
                    }
                    values[j] = wl.value[best.1];
                    enc.ranks[pl][j] = best.1 as u8;
                    moved = true;
                }
            }
            if !moved {
                break;
            }
        }
    }
}

/// The block `enc` packs to with `mode` and color range `color`.
fn pack(config: &Config, mode: &Mode, color: usize, enc: &Encoding) -> u128 {
    let ladders = ladders();
    let (cl, wl) = (&ladders.colors[color], &ladders.weights[mode.range]);
    let mut block = u128::from(mode.bits) | ((config.partitions as u128 - 1) << 11);
    let start = if config.partitions == 1 {
        block |= u128::from(config.cem) << 13;
        17
    } else {
        block |= u128::from(config.seed) << 13 | u128::from(config.cem) << 25;
        29
    };
    let order: &[(usize, usize)] = match config.cem {
        0 => &[(0, 0), (1, 0)],
        4 => &[(0, 0), (1, 0), (0, 3), (1, 3)],
        8 => &[(0, 0), (1, 0), (0, 1), (1, 1), (0, 2), (1, 2)],
        _ => &[
            (0, 0),
            (1, 0),
            (0, 1),
            (1, 1),
            (0, 2),
            (1, 2),
            (0, 3),
            (1, 3),
        ],
    };
    let mut values = [0u8; 18];
    let mut k = 0;
    for p in 0..config.partitions {
        for &(e, c) in order {
            values[k] = cl.code[usize::from(enc.ends[p][e][c])];
            k += 1;
        }
    }
    block |= ise::encode(&values[..k], color) << start;
    let (planes, count) = (config.planes(), mode.weights);
    let mut weights = [0u8; 64];
    for (i, w) in weights[..count].iter_mut().enumerate() {
        *w = wl.code[usize::from(enc.ranks[i % planes][i / planes])];
    }
    block |= ise::encode(&weights[..count], mode.range).reverse_bits();
    if let Some(c) = config.ccs {
        block |= (c as u128) << (128 - mode.weight_bits - 2);
    }
    block
}

/// The squared error of `bits` decoded against the block's texels.
fn error(block: &Block, bits: u128) -> u64 {
    let mut out = [[0u16; 4]; MAX_TEXELS];
    let out = &mut out[..block.n];
    if decode_block(bits, block.shape.footprint, out).is_none() {
        debug_assert!(false, "the encoder packed an illegal block");
        return u64::MAX;
    }
    out.iter()
        .zip(block.src)
        .map(|(d, &s)| (0..4).map(|c| cost(s, c, unorm8(d[c]))).sum::<u64>())
        .sum()
}

/// The cost of channel `c` of texel `s` decoding as `d`: the squared error,
/// weighted up for the alpha of a transparent texel.
fn cost(s: [u8; 4], c: usize, d: u8) -> u64 {
    let e = u64::from(d.abs_diff(s[c]));
    if c == 3 && s[3] == 0 {
        e * e * CLEAR_PENALTY
    } else {
        e * e
    }
}
