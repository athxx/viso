//! The BC1 color block: two little-endian RGB565 endpoints (red in the high
//! bits) and a 2-bit index per texel, little-endian from texel 0. Endpoints
//! widen by bit replication. In descending order they pick four opaque
//! colors, the endpoints then the mixes `(2·c0 + c1 + 1) / 3` and
//! `(c0 + 2·c1 + 1) / 3`; otherwise three, the half mix `(c0 + c1 + 1) / 2`
//! third, and index 3 is transparent black. BC3's color block decodes four
//! colors in either order. The mixes round to nearest; hardware lands within
//! a level of them.

use std::array::from_fn;
use std::sync::OnceLock;

use super::{Effort, PENALTY, members, principal_axis};

/// The palette a color block decodes with.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Palette {
    /// The endpoints and their two thirds, opaque.
    Four,
    /// The endpoints, their half and transparent black.
    Three,
}

impl Palette {
    /// How far each index lies from the first endpoint toward the second.
    fn weights(self) -> &'static [f32] {
        match self {
            Palette::Four => &[0.0, 1.0, 1.0 / 3.0, 2.0 / 3.0],
            Palette::Three => &[0.0, 1.0, 0.5],
        }
    }
}

/// The format a color block is encoded for, and the roles its texels play.
#[derive(Clone, Copy, Debug)]
pub(super) enum Format {
    /// BC1: `holes` decode as transparent black, through the three-color
    /// palette.
    Bc1 { holes: u16 },
    /// BC3's color block, always four colors: `black` must decode black.
    Bc3 { black: u16 },
}

/// An RGB565 endpoint widened to 8 bits a channel.
fn widen(c: u16) -> [i32; 3] {
    let (r, g, b) = (
        i32::from(c >> 11),
        i32::from(c >> 5 & 63),
        i32::from(c & 31),
    );
    [r << 3 | r >> 2, g << 2 | g >> 4, b << 3 | b >> 2]
}

/// The colors of a block with endpoints `c0` and `c1` under `palette`, by
/// index.
fn colors(c0: u16, c1: u16, palette: Palette) -> [[u8; 4]; 4] {
    let (a, b) = (widen(c0), widen(c1));
    let mix = |f: fn(i32, i32) -> i32| [f(a[0], b[0]), f(a[1], b[1]), f(a[2], b[2]), 255];
    let out = match palette {
        Palette::Four => [
            mix(|a, _| a),
            mix(|_, b| b),
            mix(|a, b| (2 * a + b + 1) / 3),
            mix(|a, b| (a + 2 * b + 1) / 3),
        ],
        Palette::Three => [
            mix(|a, _| a),
            mix(|_, b| b),
            mix(|a, b| (a + b + 1) / 2),
            [0; 4],
        ],
    };
    out.map(|c| c.map(|v| v as u8))
}

/// A color block decoded, by texel: four colors always when `four` (BC3),
/// else as the endpoint order picks (BC1).
pub(super) fn decode(block: &[u8; 8], four: bool) -> [[u8; 4]; 16] {
    let c0 = u16::from_le_bytes([block[0], block[1]]);
    let c1 = u16::from_le_bytes([block[2], block[3]]);
    let palette = if four || c0 > c1 {
        Palette::Four
    } else {
        Palette::Three
    };
    let colors = colors(c0, c1, palette);
    let bits = u32::from_le_bytes([block[4], block[5], block[6], block[7]]);
    from_fn(|i| colors[(bits >> (2 * i) & 3) as usize])
}

/// One block's texels as a color block for `format`.
pub(super) fn encode(texels: &[[u8; 4]; 16], format: Format, effort: Effort) -> [u8; 8] {
    let (holes, black) = match format {
        Format::Bc1 { holes } => (holes, 0),
        Format::Bc3 { black } => (0, black),
    };
    let fitter = Fitter {
        texels,
        holes,
        black,
        fitted: !holes,
        effort,
    };
    let best = match format {
        Format::Bc1 { holes } if holes != 0 => fitter.fit(Palette::Three),
        Format::Bc1 { .. } if effort == Effort::Thorough => {
            let four = fitter.fit(Palette::Four);
            let three = fitter.fit(Palette::Three);
            if three.error < four.error {
                three
            } else {
                four
            }
        }
        Format::Bc1 { .. } | Format::Bc3 { .. } => fitter.fit(Palette::Four),
    };
    pack(&best, matches!(format, Format::Bc1 { .. }))
}

/// `candidate` as a block; `ordered` puts its endpoints in the order that
/// selects its palette (BC1), remapping the indices to match.
fn pack(candidate: &Candidate, ordered: bool) -> [u8; 8] {
    let (mut c0, mut c1, mut indices) = (candidate.c0, candidate.c1, candidate.indices);
    if ordered {
        let four = candidate.palette == Palette::Four;
        if (four && c0 < c1) || (!four && c0 > c1) {
            std::mem::swap(&mut c0, &mut c1);
            indices = indices.map(|k| if four || k < 2 { k ^ 1 } else { k });
        }
        if four && c0 == c1 {
            // Equal endpoints decode three colors, all of them the endpoint.
            indices = [0; 16];
        }
    }
    let bits = indices
        .iter()
        .enumerate()
        .fold(0u32, |bits, (i, &k)| bits | u32::from(k) << (2 * i));
    let mut out = [0; 8];
    out[..2].copy_from_slice(&c0.to_le_bytes());
    out[2..4].copy_from_slice(&c1.to_le_bytes());
    out[4..].copy_from_slice(&bits.to_le_bytes());
    out
}

/// Endpoints, the palette they decode with, the indices they select and the
/// error of the texels they code.
#[derive(Clone, Copy, Debug)]
struct Candidate {
    c0: u16,
    c1: u16,
    palette: Palette,
    indices: [u8; 16],
    error: u32,
}

/// A color block being fitted: its texels and the roles they play.
struct Fitter<'a> {
    texels: &'a [[u8; 4]; 16],
    /// Texels on the transparent entry.
    holes: u16,
    /// Texels that must decode black.
    black: u16,
    /// Texels the endpoints are fitted to: all but the holes.
    fitted: u16,
    effort: Effort,
}

impl Fitter<'_> {
    /// The best endpoints under `palette`.
    fn fit(&self, palette: Palette) -> Candidate {
        let mut points = [[0.0f32; 3]; 16];
        let mut n = 0;
        for i in members(self.fitted) {
            let t = self.texels[i];
            points[n] = [t[0], t[1], t[2]].map(f32::from);
            n += 1;
        }
        let points = &points[..n];
        if points.is_empty() {
            return self.evaluate(0, 0, palette);
        }
        let (mean, axis, _) = principal_axis(points);
        let (c0, c1) = single(mean, palette);
        let mut best = self.evaluate(c0, c1, palette);
        let along = |p: &[f32; 3]| (0..3).map(|c| (p[c] - mean[c]) * axis[c]).sum::<f32>();
        let lo = points.iter().map(along).fold(f32::INFINITY, f32::min);
        let hi = points.iter().map(along).fold(f32::NEG_INFINITY, f32::max);
        let at = |s: f32| from_fn(|c| mean[c] + axis[c] * s);
        self.refine(&mut best, palette, at(lo), at(hi), false);
        if self.black != 0 {
            // The second endpoint black, the first as far out along the mean
            // as the texels reach.
            let norm = mean.iter().map(|v| v * v).sum::<f32>();
            let reach = if norm > 0.0 {
                let dot = |p: &[f32; 3]| (0..3).map(|c| p[c] * mean[c]).sum::<f32>() / norm;
                points.iter().map(dot).fold(0.0, f32::max)
            } else {
                0.0
            };
            self.refine(&mut best, palette, mean.map(|v| v * reach), [0.0; 3], true);
        }
        if self.effort == Effort::Thorough {
            if let Some((e0, e1)) = cluster(points, axis, palette) {
                self.refine(&mut best, palette, e0, e1, false);
            }
            self.polish(&mut best);
        }
        best
    }

    /// Endpoints `e0` and `e1` quantized, then refit by least squares to the
    /// indices they select while that lowers the error; the second endpoint
    /// stays black when `pin`. Into `best` if better.
    fn refine(
        &self,
        best: &mut Candidate,
        palette: Palette,
        e0: [f32; 3],
        e1: [f32; 3],
        pin: bool,
    ) {
        let iterations = if self.effort == Effort::Thorough {
            6
        } else {
            2
        };
        let quantize_pair = |e0, e1| (quantize(e0), if pin { 0 } else { quantize(e1) });
        let (c0, c1) = quantize_pair(e0, e1);
        let mut current = self.evaluate(c0, c1, palette);
        for _ in 0..iterations {
            let (e0, e1) = self.least_squares(&current.indices, palette, pin);
            let (c0, c1) = quantize_pair(e0, e1);
            if (c0, c1) == (current.c0, current.c1) {
                break;
            }
            let next = self.evaluate(c0, c1, palette);
            if next.error >= current.error {
                break;
            }
            current = next;
        }
        if current.error < best.error {
            *best = current;
        }
    }

    /// The endpoints that best fit the texels to `indices` in least squares;
    /// the second black when `pin`.
    fn least_squares(
        &self,
        indices: &[u8; 16],
        palette: Palette,
        pin: bool,
    ) -> ([f32; 3], [f32; 3]) {
        let weights = palette.weights();
        let (mut aa, mut bb, mut ab, mut n) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
        let (mut ax, mut bx, mut sum) = ([0.0f32; 3], [0.0f32; 3], [0.0f32; 3]);
        for i in members(self.fitted) {
            let t = weights[indices[i] as usize];
            let s = 1.0 - t;
            aa += s * s;
            bb += t * t;
            ab += s * t;
            n += 1.0;
            for c in 0..3 {
                let x = f32::from(self.texels[i][c]);
                ax[c] += s * x;
                bx[c] += t * x;
                sum[c] += x;
            }
        }
        if pin {
            return (
                if aa > 0.0 {
                    ax.map(|v| v / aa)
                } else {
                    sum.map(|v| v / n)
                },
                [0.0; 3],
            );
        }
        let det = aa * bb - ab * ab;
        if det.abs() < 1e-3 {
            let mean = sum.map(|v| v / n);
            return (mean, mean);
        }
        (
            from_fn(|c| (bb * ax[c] - ab * bx[c]) / det),
            from_fn(|c| (aa * bx[c] - ab * ax[c]) / det),
        )
    }

    /// Each endpoint channel stepped a code up or down while that lowers the
    /// error.
    fn polish(&self, best: &mut Candidate) {
        const FIELDS: [(u16, u16); 3] = [(11, 31), (5, 63), (0, 31)];
        for _ in 0..4 {
            let mut improved = false;
            for endpoint in 0..2 {
                for (shift, max) in FIELDS {
                    for step in [-1i32, 1] {
                        let c = if endpoint == 0 { best.c0 } else { best.c1 };
                        let field = i32::from(c >> shift & max) + step;
                        if field < 0 || field > i32::from(max) {
                            continue;
                        }
                        let c = c & !(max << shift) | (field as u16) << shift;
                        let (c0, c1) = if endpoint == 0 {
                            (c, best.c1)
                        } else {
                            (best.c0, c)
                        };
                        let next = self.evaluate(c0, c1, best.palette);
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

    /// Endpoints `c0` and `c1` under `palette` with the index nearest each
    /// texel, and the error.
    fn evaluate(&self, c0: u16, c1: u16, palette: Palette) -> Candidate {
        let colors = colors(c0, c1, palette);
        let entries = palette.weights().len();
        let mut indices = [3u8; 16];
        let mut error = 0;
        for i in members(self.fitted) {
            let t = self.texels[i];
            let (k, e) = colors[..entries]
                .iter()
                .map(|p| {
                    (0..3)
                        .map(|c| (i32::from(t[c]) - i32::from(p[c])).pow(2) as u32)
                        .sum()
                })
                .enumerate()
                .min_by_key(|&(_, e): &(usize, u32)| e)
                .expect("an entry");
            indices[i] = k as u8;
            error += if self.black >> i & 1 != 0 && e > 0 {
                PENALTY
            } else {
                e
            };
        }
        debug_assert!(members(self.holes).all(|i| indices[i] == 3));
        Candidate {
            c0,
            c1,
            palette,
            indices,
            error,
        }
    }
}

/// `e`, a color in `0.0..=255.0`, as the nearest RGB565 endpoint.
fn quantize(e: [f32; 3]) -> u16 {
    let code = |v: f32, bits: u32| -> u16 {
        let max = (1i32 << bits) - 1;
        let guess = (v.clamp(0.0, 255.0) * max as f32 / 255.0).round() as i32;
        let widen = |c: i32| {
            if bits == 5 {
                c << 3 | c >> 2
            } else {
                c << 2 | c >> 4
            }
        };
        (guess - 1..=guess + 1)
            .filter(|c| (0..=max).contains(c))
            .min_by(|&a, &b| {
                (widen(a) as f32 - v)
                    .abs()
                    .total_cmp(&(widen(b) as f32 - v).abs())
            })
            .expect("a code") as u16
    };
    code(e[0], 5) << 11 | code(e[1], 6) << 5 | code(e[2], 5)
}

/// The endpoints whose index 2 under `palette` lands nearest the color `e`.
fn single(e: [f32; 3], palette: Palette) -> (u16, u16) {
    static TABLES: OnceLock<[[[u8; 2]; 256]; 4]> = OnceLock::new();
    let tables = TABLES.get_or_init(|| {
        [(5, false), (6, false), (5, true), (6, true)].map(|(bits, half)| single_table(bits, half))
    });
    let half = usize::from(palette == Palette::Three) * 2;
    let v = e.map(|v| v.clamp(0.0, 255.0).round() as usize);
    let (r, g, b) = (
        tables[half][v[0]],
        tables[half + 1][v[1]],
        tables[half][v[2]],
    );
    let pack = |k: usize| u16::from(r[k]) << 11 | u16::from(g[k]) << 5 | u16::from(b[k]);
    (pack(0), pack(1))
}

/// For each 8-bit level, the `bits`-bit endpoint pair whose index-2 mix (the
/// near third, or the half when `half`) widens nearest it, endpoints as
/// close together as that allows.
fn single_table(bits: u32, half: bool) -> [[u8; 2]; 256] {
    let widen = |c: i32| {
        if bits == 5 {
            c << 3 | c >> 2
        } else {
            c << 2 | c >> 4
        }
    };
    let codes = 0..1i32 << bits;
    from_fn(|v| {
        let v = v as i32;
        let mut best = (i32::MAX, i32::MAX, [0u8; 2]);
        for a in codes.clone() {
            for b in codes.clone() {
                let (wa, wb) = (widen(a), widen(b));
                let mix = if half {
                    (wa + wb + 1) / 2
                } else {
                    (2 * wa + wb + 1) / 3
                };
                let key = ((mix - v).abs(), (wa - wb).abs());
                if key < (best.0, best.1) {
                    best = (key.0, key.1, [a as u8, b as u8]);
                }
            }
        }
        best.2
    })
}

/// The endpoints of the best least-squares fit over every split of `points`,
/// ordered along `axis`, into runs on consecutive palette entries.
fn cluster(points: &[[f32; 3]], axis: [f32; 3], palette: Palette) -> Option<([f32; 3], [f32; 3])> {
    let n = points.len();
    let mut order = [[0.0f32; 3]; 16];
    order[..n].copy_from_slice(points);
    let along = |p: &[f32; 3]| p[0] * axis[0] + p[1] * axis[1] + p[2] * axis[2];
    order[..n].sort_unstable_by(|a, b| along(a).total_cmp(&along(b)));
    let mut prefix = [[0.0f32; 3]; 17];
    for (i, p) in order[..n].iter().enumerate() {
        prefix[i + 1] = from_fn(|c| prefix[i][c] + p[c]);
    }
    // The entries in order along the line from the first endpoint.
    let steps: &[f32] = match palette {
        Palette::Four => &[0.0, 1.0 / 3.0, 2.0 / 3.0, 1.0],
        Palette::Three => &[0.0, 0.5, 1.0],
    };
    let mut best: Option<(f32, [f32; 3], [f32; 3])> = None;
    let mut bounds = [0usize; 5];
    let runs = steps.len();
    bounds[runs] = n;
    let mut consider = |bounds: &[usize; 5]| {
        let (mut aa, mut bb, mut ab) = (0.0f32, 0.0f32, 0.0f32);
        let (mut ax, mut bx) = ([0.0f32; 3], [0.0f32; 3]);
        for (r, &t) in steps.iter().enumerate() {
            let count = (bounds[r + 1] - bounds[r]) as f32;
            let s = 1.0 - t;
            aa += count * s * s;
            bb += count * t * t;
            ab += count * s * t;
            for c in 0..3 {
                let sum = prefix[bounds[r + 1]][c] - prefix[bounds[r]][c];
                ax[c] += s * sum;
                bx[c] += t * sum;
            }
        }
        let det = aa * bb - ab * ab;
        if det.abs() < 1e-3 {
            return;
        }
        let e0: [f32; 3] = from_fn(|c| (bb * ax[c] - ab * bx[c]) / det);
        let e1: [f32; 3] = from_fn(|c| (aa * bx[c] - ab * ax[c]) / det);
        let score = (0..3).map(|c| e0[c] * ax[c] + e1[c] * bx[c]).sum::<f32>();
        if best.is_none_or(|(s, _, _)| score > s) {
            best = Some((score, e0, e1));
        }
    };
    for i in 0..=n {
        bounds[1] = i;
        for j in i..=n {
            bounds[2] = j;
            if runs == 3 {
                consider(&bounds);
                continue;
            }
            for k in j..=n {
                bounds[3] = k;
                consider(&bounds);
            }
        }
    }
    best.map(|(_, e0, e1)| {
        (
            e0.map(|v| v.clamp(0.0, 255.0)),
            e1.map(|v| v.clamp(0.0, 255.0)),
        )
    })
}
