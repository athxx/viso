//! Terrain: a ground block and, for hills, terraced layers of blocks over a
//! seeded value-noise heightfield.
//!
//! The field is sampled at cell centres, normalized to its own extents so
//! `height` is the literal peak-to-floor relief, pushed toward its floor so
//! plains sit between the hills, flattened to a plaza around the origin
//! where a game puts its player, and quantized to terraces of `step`. Each
//! terrace level becomes the fewest rectangles that cover it, so a hill of
//! three levels is three stacked plates, not one block per cell. Every
//! operation is IEEE arithmetic or an integer hash, so a seed builds the
//! same terrain on every target.

use std::rc::Rc;

use super::Model;
use crate::game::world::{BodyKind, SpawnDesc};
use crate::native::{NativeError, NativeValue, SchemaTy, Vec3F32};
use crate::value::{Aggregate, Value};

/// The most cells a side of a terrain has.
const MAX_CELLS: u32 = 256;
/// The thickness of the ground block, below `y = 0`.
const GROUND: f32 = 1.0;
/// Octaves of noise summed; each adds detail at half the size.
const OCTAVES: u32 = 4;

/// A terrain to build: a square of `size` metres centred on the origin, its
/// ground at `y = 0`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Terrain {
    size: f32,
    height: f32,
    cell: f32,
    step: f32,
    feature: f32,
    seed: u64,
}

impl Terrain {
    /// The full path of its native value type.
    pub const PATH: &'static str = "viso::game::kit::Terrain";

    /// Flat ground `size` metres across.
    pub fn flat(size: f32) -> Terrain {
        Terrain {
            size,
            height: 0.0,
            cell: 2.0,
            step: 0.5,
            feature: 24.0,
            seed: 0,
        }
    }

    /// Ground `size` metres across with hills up to `height` metres.
    pub fn hills(size: f32, height: f32) -> Terrain {
        Terrain {
            height,
            ..Terrain::flat(size)
        }
    }

    /// Built from the noise of `seed`.
    pub fn seed(self, seed: u64) -> Terrain {
        Terrain { seed, ..self }
    }

    /// Sampled every `cell` metres.
    pub fn cell(self, cell: f32) -> Terrain {
        Terrain { cell, ..self }
    }

    /// Terraced in steps of `step` metres.
    pub fn step(self, step: f32) -> Terrain {
        Terrain { step, ..self }
    }

    /// With hills about `feature` metres apart.
    pub fn feature(self, feature: f32) -> Terrain {
        Terrain { feature, ..self }
    }

    /// The blocks that build it: the ground, then each terrace level's
    /// rectangles, row by row.
    ///
    /// # Errors
    ///
    /// A size, height, cell, step or feature size that is not finite and
    /// positive (a height may be zero).
    pub fn blocks(&self) -> Result<Vec<SpawnDesc>, NativeError> {
        let positive = |name: &str, v: f32| {
            if v.is_finite() && v > 0.0 {
                Ok(())
            } else {
                Err(NativeError::new(format!(
                    "a terrain's {name} is finite and positive, not {v}"
                )))
            }
        };
        positive("size", self.size)?;
        positive("cell", self.cell)?;
        positive("step", self.step)?;
        positive("feature size", self.feature)?;
        if !(self.height.is_finite() && self.height >= 0.0) {
            return Err(NativeError::new(format!(
                "a terrain's height is finite and not negative, not {}",
                self.height
            )));
        }
        let block = |at: [f32; 3], size: [f32; 3]| {
            SpawnDesc::new(BodyKind::Block, Vec3F32::new(size[0], size[1], size[2]))
                .at(Vec3F32::from_array(at))
                .model(Model::Ground)
        };
        let mut blocks = vec![block(
            [0.0, -GROUND * 0.5, 0.0],
            [self.size, GROUND, self.size],
        )];
        let top = (self.height / self.step) as u32;
        if top == 0 {
            return Ok(blocks);
        }
        let n = ((self.size / self.cell).ceil() as u32).clamp(1, MAX_CELLS) as usize;
        let cell = self.size / n as f32;
        let levels = self.levels(n, cell, top);
        let origin = -self.size * 0.5;
        let mut used = vec![false; n * n];
        for level in 1..=top as u8 {
            used.fill(false);
            let open =
                |used: &[bool], i: usize, j: usize| levels[j * n + i] >= level && !used[j * n + i];
            for j in 0..n {
                for i in 0..n {
                    if !open(&used, i, j) {
                        continue;
                    }
                    let mut w = 1;
                    while i + w < n && open(&used, i + w, j) {
                        w += 1;
                    }
                    let mut d = 1;
                    while j + d < n && (i..i + w).all(|k| open(&used, k, j + d)) {
                        d += 1;
                    }
                    for row in j..j + d {
                        used[row * n + i..row * n + i + w].fill(true);
                    }
                    let (w, d) = (w as f32 * cell, d as f32 * cell);
                    let at = [
                        origin + i as f32 * cell + w * 0.5,
                        (f32::from(level) - 0.5) * self.step,
                        origin + j as f32 * cell + d * 0.5,
                    ];
                    blocks.push(block(at, [w, self.step, d]));
                }
            }
        }
        Ok(blocks)
    }

    /// The terrace level of each of the `n × n` cells of width `cell`, row
    /// by row, at most `top`.
    fn levels(&self, n: usize, cell: f32, top: u32) -> Vec<u8> {
        let origin = -self.size * 0.5;
        let frequency = 1.0 / self.feature;
        let centre = |k: usize| origin + (k as f32 + 0.5) * cell;
        let mut raw = Vec::with_capacity(n * n);
        for j in 0..n {
            for i in 0..n {
                raw.push(fbm(self.seed, centre(i) * frequency, centre(j) * frequency));
            }
        }
        let (low, high) = raw
            .iter()
            .fold((f32::MAX, f32::MIN), |(lo, hi), &v| (lo.min(v), hi.max(v)));
        let span = high - low;
        let (inner, outer) = (self.size * 0.12, self.size * 0.25);
        let top = top.min(u32::from(u8::MAX));
        let mut levels = Vec::with_capacity(n * n);
        for j in 0..n {
            for i in 0..n {
                let h = if span > 0.0 {
                    (raw[j * n + i] - low) / span
                } else {
                    0.0
                };
                let (x, z) = (centre(i), centre(j));
                let r = (x * x + z * z).sqrt();
                let plaza = smooth(((r - inner) / (outer - inner)).clamp(0.0, 1.0));
                let height = h * h * plaza * self.height;
                let level = (height / self.step + 0.5) as u32;
                levels.push(level.min(top) as u8);
            }
        }
        levels
    }
}

/// `t²(3 − 2t)`.
fn smooth(t: f32) -> f32 {
    t * t * (3.0 - 2.0 * t)
}

/// A lattice point's value in `[0, 1)`.
fn lattice(seed: u64, x: i64, z: i64) -> f32 {
    let mut h = seed ^ (x as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15);
    h ^= (z as u64).wrapping_mul(0xc2b2_ae3d_27d4_eb4f);
    h = (h ^ (h >> 31)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    h = (h ^ (h >> 29)).wrapping_mul(0x94d0_49bb_1331_11eb);
    h ^= h >> 32;
    (h >> 40) as f32 / (1u32 << 24) as f32
}

/// Bilinear value noise in `[0, 1)`, smoothly interpolated.
fn noise(seed: u64, x: f32, z: f32) -> f32 {
    let (x0, z0) = (x.floor(), z.floor());
    let (sx, sz) = (smooth(x - x0), smooth(z - z0));
    let (ix, iz) = (x0 as i64, z0 as i64);
    let h00 = lattice(seed, ix, iz);
    let h10 = lattice(seed, ix + 1, iz);
    let h01 = lattice(seed, ix, iz + 1);
    let h11 = lattice(seed, ix + 1, iz + 1);
    h00 + (h10 - h00) * sx + (h01 - h00) * sz + (h00 - h10 - h01 + h11) * sx * sz
}

/// Octaves of noise, each at twice the frequency and half the amplitude.
fn fbm(seed: u64, x: f32, z: f32) -> f32 {
    let (mut total, mut amplitude, mut scale) = (0.0, 1.0, 1.0);
    for octave in 0..OCTAVES {
        let seed = seed ^ u64::from(octave).wrapping_mul(0x9e37_79b9);
        total += noise(seed, x * scale, z * scale) * amplitude;
        amplitude *= 0.5;
        scale *= 2.0;
    }
    total
}

impl NativeValue for Terrain {
    const TY: SchemaTy = SchemaTy::Value(Terrain::PATH);

    fn from_value(value: &Value) -> Option<Terrain> {
        let Value::Agg(agg) = value else {
            return None;
        };
        let [size, height, cell, step, feature, seed] = &agg.fields[..] else {
            return None;
        };
        let f = |v: &Value| v.as_float().map(|v| v as f32);
        Some(Terrain {
            size: f(size)?,
            height: f(height)?,
            cell: f(cell)?,
            step: f(step)?,
            feature: f(feature)?,
            seed: seed.as_int()? as u64,
        })
    }

    fn into_value(self) -> Value {
        let f = |v: f32| Value::Float(f64::from(v));
        Value::Agg(Rc::new(Aggregate {
            tag: 0,
            fields: Box::new([
                f(self.size),
                f(self.height),
                f(self.cell),
                f(self.step),
                f(self.feature),
                Value::Int(self.seed as i64),
            ]),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flat_ground_is_one_block_below_the_origin() {
        let blocks = Terrain::flat(40.0).blocks().expect("blocks");
        assert_eq!(blocks.len(), 1);
    }

    #[test]
    fn hills_rise_around_a_flat_plaza_and_merge_their_cells() {
        let terrain = Terrain::hills(64.0, 4.0).seed(7);
        let blocks = terrain.blocks().expect("blocks");
        assert_eq!(blocks, terrain.blocks().expect("blocks"), "deterministic");
        let n = 32;
        let levels = terrain.levels(n, 2.0, 8);
        assert!(levels.iter().any(|&l| l >= 4), "some hill reaches the top");
        for (k, &level) in levels.iter().enumerate() {
            let (i, j) = (k % n, k / n);
            let (x, z) = (-31.0 + 2.0 * i as f32, -31.0 + 2.0 * j as f32);
            if (x * x + z * z).sqrt() < 64.0 * 0.12 {
                assert_eq!(level, 0, "the plaza is flat at {x}, {z}");
            }
        }
        let cells: usize = levels.iter().map(|&l| usize::from(l)).sum();
        assert!(blocks.len() - 1 < cells / 2, "{} blocks", blocks.len());
        assert_ne!(blocks, terrain.seed(8).blocks().expect("blocks"));
    }

    #[test]
    fn a_degenerate_terrain_is_an_error() {
        for t in [
            Terrain::flat(0.0),
            Terrain::flat(f32::NAN),
            Terrain::hills(10.0, -1.0),
            Terrain::hills(10.0, 1.0).step(0.0),
        ] {
            assert!(t.blocks().is_err(), "{t:?}");
        }
    }
}
