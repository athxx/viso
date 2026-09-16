//! The 1D gradient LUT atlas: an RGBA8 texture the renderer bakes gradient
//! ramps into, one row per distinct gradient, plus the CPU backing it uploads
//! from.
//!
//! A gradient with three or more stops (or a non-linear
//! [`InterpolationSpace`]) is too expensive to evaluate per fragment, so its
//! ramp is baked once into a [`LUT_WIDTH`]-texel row of premultiplied linear
//! RGBA and sampled at `(t, lut_v)` thereafter (§12.2 tier three). Two-stop
//! linear-space gradients skip this atlas entirely and lerp inline in the
//! shader.
//!
//! The bake is keyed ([`LutKey`]): a repeated gradient hits the same row with
//! **zero** rebaking, so a static gradient is built once and never recreated
//! per frame. Rows are assigned top to bottom; when the atlas fills it wipes
//! generationally (the [`epoch`](GradientLutAtlas::epoch) bumps) and callers
//! re-key.
//!
//! Structurally a sibling of [`ColorAtlas`](crate::color_atlas::ColorAtlas):
//! the same CPU-backing / dirty-rect / generational-wipe lifecycle, but the
//! packing is a trivial row cursor rather than a max-rects packer, and rows are
//! content-addressed by [`LutKey`] instead of blitted from an external bitmap.
//! Like the other atlases, the GPU [`TextureId`] is created once by the caller;
//! this type never touches the device.

use std::collections::HashMap;

use viso_gpu::{TextureFormat, TextureId};
use viso_math::{ExtendMode, InterpolationSpace, LinearStraight, linear_to_srgb, srgb_to_linear};

use crate::primitive::GradientStop;

/// Texels per baked ramp row. 256 gives 8-bit-per-channel ramps a distinct
/// texel per output level with bilinear sampling smoothing between — enough
/// that banding is invisible at any on-screen gradient length while keeping the
/// row a single cache-friendly line.
pub const LUT_WIDTH: u32 = 256;

/// Bytes per texel of the RGBA8 backing.
const BPT: u32 = 4;

/// A baked-ramp cache key: two gradients that bake to identical rows share one.
///
/// The stops are hashed by their exact `f32` bit patterns (offset and each
/// straight-linear channel), so the key is a pure function of what the bake
/// reads — bit-exact, never approximate, matching the brush-store diff
/// discipline. `interp` and `extend` change the baked texels (`extend` folds
/// `t` into `[0, 1]` before the ramp lookup, so it is baked in, not applied at
/// sample time), so both are part of the identity.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LutKey {
    /// Each stop as `(offset_bits, [r_bits, g_bits, b_bits, a_bits])` — the raw
    /// `f32` bit patterns, for bit-exact identity.
    stops: Vec<(u32, [u32; 4])>,
    /// The color space the per-texel lerp runs in.
    interp: InterpolationSpace,
    /// How `t` outside `[0, 1]` is folded before the ramp lookup.
    extend: ExtendMode,
}

impl LutKey {
    /// Build a key from a gradient's stops, interpolation space, and extend
    /// mode. The stops are captured by exact bit pattern.
    pub fn new(stops: &[GradientStop], interp: InterpolationSpace, extend: ExtendMode) -> Self {
        let stops = stops
            .iter()
            .map(|s| {
                (
                    s.offset.to_bits(),
                    [
                        s.color.r.to_bits(),
                        s.color.g.to_bits(),
                        s.color.b.to_bits(),
                        s.color.a.to_bits(),
                    ],
                )
            })
            .collect();
        Self {
            stops,
            interp,
            extend,
        }
    }
}

/// The result of [`GradientLutAtlas::alloc`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LutAlloc {
    /// The ramp occupies this row; sample the atlas at texture-`v` `v`. `epoch`
    /// is the generation the row was baked in — callers holding a cached `v`
    /// across frames must re-alloc if the atlas epoch has moved past it.
    Row {
        /// The row's normalized texture-`v` (center of the row).
        v: f32,
        /// The generation this row belongs to.
        epoch: u32,
    },
    /// The atlas has no free row even after a wipe (more distinct live
    /// gradients than [`GradientLutAtlas`] has rows). The atlas wiped
    /// generationally; the caller re-keys its live gradients and retries.
    Overflow,
}

/// A 1D gradient LUT atlas: a row cursor, a keyed row cache, a CPU pixel
/// backing, and the accumulated dirty rect the renderer uploads.
///
/// The GPU [`TextureId`] is created once by the caller and handed in; this type
/// never touches the device.
#[derive(Debug)]
pub struct GradientLutAtlas {
    /// Number of rows (the texture height). Each row is one baked ramp.
    rows: u32,
    /// Row-major RGBA8 pixels, `LUT_WIDTH × rows × 4` bytes; the CPU source of
    /// truth.
    pixels: Vec<u8>,
    /// The GPU texture (`Rgba8Unorm`, `LUT_WIDTH × rows`) these pixels back.
    texture: TextureId,
    /// Cache of baked keys → row index, so a repeated gradient never rebakes.
    cache: HashMap<LutKey, u32>,
    /// Next free row (rows are assigned top to bottom until full).
    next_row: u32,
    /// Dirty row span since the last [`take_dirty`](Self::take_dirty), as
    /// `(first_row, row_count)`, or `None` when nothing changed.
    dirty: Option<(u32, u32)>,
    /// Generational epoch; bumped on every overflow wipe so callers invalidate
    /// row caches keyed on it.
    epoch: u32,
}

impl GradientLutAtlas {
    /// The RGBA8 pixel format a gradient-LUT texture must be created with.
    pub const FORMAT: TextureFormat = TextureFormat::Rgba8Unorm;

    /// A fresh atlas of `rows` ramp rows, backing the given GPU texture.
    ///
    /// The texture must be created as [`Self::FORMAT`] at `LUT_WIDTH × rows`.
    /// The backing starts fully zero (transparent).
    pub fn new(rows: u32, texture: TextureId) -> Self {
        Self {
            rows,
            pixels: vec![0u8; (LUT_WIDTH as usize) * (rows as usize) * BPT as usize],
            texture,
            cache: HashMap::new(),
            next_row: 0,
            dirty: None,
            epoch: 0,
        }
    }

    /// Row count (the texture height in texels).
    pub fn rows(&self) -> u32 {
        self.rows
    }

    /// The GPU texture handle these pixels back.
    pub fn texture(&self) -> TextureId {
        self.texture
    }

    /// The current generation. Bumps on every overflow wipe; callers key their
    /// cached `lut_v` on it and re-alloc after a change.
    pub fn epoch(&self) -> u32 {
        self.epoch
    }

    /// The full CPU pixel backing (row-major RGBA8, `LUT_WIDTH × rows × 4`
    /// bytes).
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// Resolve `key` to a baked row, baking it on a cache miss.
    ///
    /// A cache hit returns the existing row's texture-`v` with **no** rebake —
    /// this is the "static gradient built once, never recreated per frame"
    /// guarantee. A miss bakes the ramp into the next free row (dirty rect
    /// grown) and caches it. When no free row remains the atlas wipes
    /// generationally and returns [`LutAlloc::Overflow`]; the caller re-keys and
    /// retries.
    pub fn alloc(&mut self, key: LutKey) -> LutAlloc {
        if let Some(&row) = self.cache.get(&key) {
            return LutAlloc::Row {
                v: self.row_v(row),
                epoch: self.epoch,
            };
        }
        if self.next_row >= self.rows {
            self.wipe();
            return LutAlloc::Overflow;
        }
        let row = self.next_row;
        self.next_row += 1;
        self.bake(row, &key);
        self.cache.insert(key, row);
        self.grow_dirty(row);
        LutAlloc::Row {
            v: self.row_v(row),
            epoch: self.epoch,
        }
    }

    /// Take the accumulated dirty row span (in texels) and its RGBA bytes,
    /// resetting the dirty state. Returns `None` when nothing changed.
    ///
    /// The result is `(x, y, w, h, bytes)` with `x = 0`, `w = LUT_WIDTH`, the
    /// bytes being the tightly-packed rows of the dirty span, suitable for
    /// [`viso_gpu::GpuBackend::write_texture`].
    pub fn take_dirty(&mut self) -> Option<(u32, u32, u32, u32, Vec<u8>)> {
        let (first, count) = self.dirty.take()?;
        let row_bytes = (LUT_WIDTH * BPT) as usize;
        let start = first as usize * row_bytes;
        let end = start + count as usize * row_bytes;
        Some((0, first, LUT_WIDTH, count, self.pixels[start..end].to_vec()))
    }

    /// The normalized texture-`v` of a row's center.
    fn row_v(&self, row: u32) -> f32 {
        (row as f32 + 0.5) / self.rows as f32
    }

    /// Bake `key`'s ramp into `row`: for each of [`LUT_WIDTH`] texels, fold the
    /// parameter through `extend`, evaluate the stops in the chosen
    /// interpolation space, and store the result **premultiplied** linear RGBA8
    /// (the sample path, like the color atlas, is premultiplied and branchless).
    fn bake(&mut self, row: u32, key: &LutKey) {
        let base = row as usize * (LUT_WIDTH * BPT) as usize;
        for texel in 0..LUT_WIDTH {
            // Texel centers span (0, 1): the row is a lookup over `t`, so the
            // first and last texels sit half a step in, matching bilinear
            // sampling at the row's edges.
            let t = (texel as f32 + 0.5) / LUT_WIDTH as f32;
            let folded = fold_extend(t, key.extend);
            let color = eval_stops(&key.stops, folded, key.interp);
            let premul = color.premultiply();
            let off = base + texel as usize * BPT as usize;
            self.pixels[off] = to_u8(premul.r);
            self.pixels[off + 1] = to_u8(premul.g);
            self.pixels[off + 2] = to_u8(premul.b);
            self.pixels[off + 3] = to_u8(premul.a);
        }
    }

    /// Union `row` into the accumulated dirty span.
    fn grow_dirty(&mut self, row: u32) {
        self.dirty = Some(match self.dirty {
            None => (row, 1),
            Some((first, count)) => {
                let lo = first.min(row);
                let hi = (first + count).max(row + 1);
                (lo, hi - lo)
            }
        });
    }

    /// Generational wipe: drop the cache, reset the cursor, clear the backing,
    /// mark every row dirty, and bump the epoch.
    fn wipe(&mut self) {
        self.cache.clear();
        self.next_row = 0;
        self.pixels.iter_mut().for_each(|p| *p = 0);
        self.dirty = Some((0, self.rows));
        self.epoch = self.epoch.wrapping_add(1);
    }
}

/// Fold a gradient parameter `t` into `[0, 1]` per the extend mode. This is
/// baked into the LUT (not applied at sample time), so a repeated/mirrored
/// gradient still samples a clamped texture.
fn fold_extend(t: f32, extend: ExtendMode) -> f32 {
    match extend {
        ExtendMode::Clamp => t.clamp(0.0, 1.0),
        ExtendMode::Repeat => t - t.floor(),
        ExtendMode::Mirror => {
            // Triangle wave: 0→1→0 over each 2-unit period.
            let u = (t * 0.5).rem_euclid(1.0) * 2.0;
            if u > 1.0 { 2.0 - u } else { u }
        }
    }
}

/// Evaluate the stop list at parameter `t ∈ [0, 1]` in `interp` space, returning
/// straight linear RGBA. Stops are assumed ordered by ascending `offset`; `t`
/// before the first / after the last clamps to that stop's color.
fn eval_stops(stops: &[(u32, [u32; 4])], t: f32, interp: InterpolationSpace) -> LinearStraight {
    if stops.is_empty() {
        return LinearStraight::TRANSPARENT;
    }
    let color_of = |i: usize| {
        let [r, g, b, a] = stops[i].1;
        LinearStraight::new(
            f32::from_bits(r),
            f32::from_bits(g),
            f32::from_bits(b),
            f32::from_bits(a),
        )
    };
    let offset_of = |i: usize| f32::from_bits(stops[i].0);

    if t <= offset_of(0) {
        return color_of(0);
    }
    let last = stops.len() - 1;
    if t >= offset_of(last) {
        return color_of(last);
    }
    // Find the span [i, i+1] containing t.
    let mut i = 0;
    while i < last && offset_of(i + 1) < t {
        i += 1;
    }
    let a = offset_of(i);
    let b = offset_of(i + 1);
    let span = b - a;
    let local = if span > 0.0 { (t - a) / span } else { 0.0 };
    lerp_color(color_of(i), color_of(i + 1), local, interp)
}

/// Interpolate two straight-linear colors by `f ∈ [0, 1]` in `interp` space,
/// returning straight linear. Alpha is always linear. `OkLab` is reserved and
/// panics: constructing a gradient with it is rejected before a bake reaches
/// here.
fn lerp_color(
    a: LinearStraight,
    b: LinearStraight,
    f: f32,
    interp: InterpolationSpace,
) -> LinearStraight {
    let g = 1.0 - f;
    let alpha = a.a * g + b.a * f;
    match interp {
        InterpolationSpace::LinearRgb => LinearStraight::new(
            a.r * g + b.r * f,
            a.g * g + b.g * f,
            a.b * g + b.b * f,
            alpha,
        ),
        InterpolationSpace::Srgb => {
            // Lerp in gamma-encoded sRGB, then decode each channel back to
            // linear for storage (the LUT is always linear).
            let mix = |ca: f32, cb: f32| {
                let s = linear_to_srgb(ca) * g + linear_to_srgb(cb) * f;
                srgb_to_linear(s)
            };
            LinearStraight::new(mix(a.r, b.r), mix(a.g, b.g), mix(a.b, b.b), alpha)
        }
        InterpolationSpace::OkLab => {
            panic!("OkLab gradient interpolation is reserved and not implemented (D2.1)")
        }
    }
}

/// One linear channel `[0, 1]` (extended range clamped) to an 8-bit texel.
fn to_u8(c: f32) -> u8 {
    (c.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitive::Rgba;

    fn stop(offset: f32, r: f32, g: f32, b: f32, a: f32) -> GradientStop {
        GradientStop {
            offset,
            color: Rgba::new(r, g, b, a),
        }
    }

    fn key(stops: Vec<GradientStop>) -> LutKey {
        LutKey::new(&stops, InterpolationSpace::LinearRgb, ExtendMode::Clamp)
    }

    #[test]
    fn same_key_hits_the_same_row_without_rebaking() {
        let mut atlas = GradientLutAtlas::new(4, TextureId::new(0));
        let stops = vec![stop(0.0, 1.0, 0.0, 0.0, 1.0), stop(1.0, 0.0, 0.0, 1.0, 1.0)];
        let a = atlas.alloc(key(stops.clone()));
        // Baking the first row dirties it; a repeat must not dirty again.
        assert!(atlas.take_dirty().is_some());
        let b = atlas.alloc(key(stops));
        assert_eq!(a, b);
        assert!(atlas.take_dirty().is_none(), "cache hit must not rebake");
    }

    #[test]
    fn distinct_keys_get_distinct_rows() {
        let mut atlas = GradientLutAtlas::new(4, TextureId::new(0));
        let a = atlas.alloc(key(vec![
            stop(0.0, 1.0, 0.0, 0.0, 1.0),
            stop(1.0, 0.0, 1.0, 0.0, 1.0),
        ]));
        let b = atlas.alloc(key(vec![
            stop(0.0, 0.0, 0.0, 1.0, 1.0),
            stop(1.0, 1.0, 1.0, 0.0, 1.0),
        ]));
        let (LutAlloc::Row { v: va, .. }, LutAlloc::Row { v: vb, .. }) = (a, b) else {
            panic!("both should place");
        };
        assert_ne!(va, vb);
    }

    #[test]
    fn extend_and_interp_are_part_of_the_key() {
        let mut atlas = GradientLutAtlas::new(4, TextureId::new(0));
        let stops = vec![stop(0.0, 1.0, 0.0, 0.0, 1.0), stop(1.0, 0.0, 0.0, 1.0, 1.0)];
        let clamp = atlas.alloc(LutKey::new(
            &stops,
            InterpolationSpace::LinearRgb,
            ExtendMode::Clamp,
        ));
        let repeat = atlas.alloc(LutKey::new(
            &stops,
            InterpolationSpace::LinearRgb,
            ExtendMode::Repeat,
        ));
        let srgb = atlas.alloc(LutKey::new(
            &stops,
            InterpolationSpace::Srgb,
            ExtendMode::Clamp,
        ));
        let (LutAlloc::Row { v: vc, .. }, LutAlloc::Row { v: vr, .. }, LutAlloc::Row { v: vs, .. }) =
            (clamp, repeat, srgb)
        else {
            panic!("all should place");
        };
        assert_ne!(vc, vr);
        assert_ne!(vc, vs);
        assert_ne!(vr, vs);
    }

    #[test]
    fn overflow_wipes_and_bumps_epoch() {
        let mut atlas = GradientLutAtlas::new(1, TextureId::new(0));
        assert!(matches!(
            atlas.alloc(key(vec![
                stop(0.0, 1.0, 0.0, 0.0, 1.0),
                stop(1.0, 0.0, 0.0, 1.0, 1.0)
            ])),
            LutAlloc::Row { epoch: 0, .. }
        ));
        // Second distinct key: no free row → wipe.
        assert_eq!(
            atlas.alloc(key(vec![
                stop(0.0, 0.0, 1.0, 0.0, 1.0),
                stop(1.0, 1.0, 0.0, 0.0, 1.0)
            ])),
            LutAlloc::Overflow
        );
        assert_eq!(atlas.epoch(), 1);
        // After the wipe the whole texture is dirty and rows are free again.
        let (x, y, w, h, _) = atlas.take_dirty().expect("wipe dirties all");
        assert_eq!((x, y, w, h), (0, 0, LUT_WIDTH, 1));
        assert!(matches!(
            atlas.alloc(key(vec![
                stop(0.0, 0.0, 1.0, 0.0, 1.0),
                stop(1.0, 1.0, 0.0, 0.0, 1.0)
            ])),
            LutAlloc::Row { epoch: 1, .. }
        ));
    }

    #[test]
    fn take_dirty_returns_full_width_rows() {
        let mut atlas = GradientLutAtlas::new(4, TextureId::new(0));
        atlas.alloc(key(vec![
            stop(0.0, 1.0, 0.0, 0.0, 1.0),
            stop(1.0, 0.0, 0.0, 1.0, 1.0),
        ]));
        let (x, y, w, h, bytes) = atlas.take_dirty().expect("dirty after bake");
        assert_eq!((x, y, w, h), (0, 0, LUT_WIDTH, 1));
        assert_eq!(bytes.len(), (LUT_WIDTH * BPT) as usize);
    }

    #[test]
    fn linear_bake_midpoint_is_halfway_premultiplied() {
        // Red→blue, opaque, linear space: the middle texel is ~(0.5, 0, 0.5)
        // premultiplied (a == 1), stored as 8-bit.
        let mut atlas = GradientLutAtlas::new(1, TextureId::new(0));
        atlas.alloc(key(vec![
            stop(0.0, 1.0, 0.0, 0.0, 1.0),
            stop(1.0, 0.0, 0.0, 1.0, 1.0),
        ]));
        let mid = (LUT_WIDTH / 2) as usize * BPT as usize;
        let px = &atlas.pixels()[mid..mid + 4];
        // Around 0.5 * 255 ≈ 128 (± bilinear texel-center offset).
        assert!((px[0] as i32 - 128).abs() <= 3, "r={}", px[0]);
        assert_eq!(px[1], 0);
        assert!((px[2] as i32 - 128).abs() <= 3, "b={}", px[2]);
        assert_eq!(px[3], 255);
    }

    #[test]
    fn premultiplied_storage_scales_rgb_by_alpha() {
        // A single fully-opaque-white to translucent-white ramp: at the
        // translucent end, premultiplied rgb tracks alpha.
        let mut atlas = GradientLutAtlas::new(1, TextureId::new(0));
        atlas.alloc(key(vec![
            stop(0.0, 1.0, 1.0, 1.0, 1.0),
            stop(1.0, 1.0, 1.0, 1.0, 0.0),
        ]));
        let last = (LUT_WIDTH as usize - 1) * BPT as usize;
        let px = &atlas.pixels()[last..last + 4];
        // Near-zero alpha → near-zero premultiplied rgb.
        assert!(px[3] <= 2, "a={}", px[3]);
        assert!(px[0] <= 2, "r={}", px[0]);
    }
}
