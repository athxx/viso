//! Batch geometry reductions over `[Point]` slices — the SIMD-accelerated
//! reductions behind the render tessellator's cold-path preprocessing (§13.6).
//!
//! Two reductions live here, both shaped for lane-parallel evaluation and both
//! with a scalar reference that the hardware kernels must reproduce bit-for-bit:
//!
//! - [`point_bounds`] folds a point ring to its axis-aligned bounding box (a
//!   min/max reduction — the `geo_bounds` oracle in `viso-render`);
//! - [`segment_lengths`] emits the Euclidean length of each consecutive segment
//!   of a polyline (the arc-length preprocessing a dash splitter consumes).
//!
//! Like [`mat4_mul`](super::mat4_mul), this is an **internal optimization
//! boundary**: the entry points take/return the scalar `[f32]`/`Point` ABI and
//! select a kernel by `#[cfg(target_arch)]` (SSE2 on `x86_64`, NEON on
//! `aarch64`, `simd128` on `wasm32` when enabled), falling back to scalar
//! elsewhere. No runtime feature probe: the `cfg` alone guarantees the
//! instructions are legal, so one kernel is compiled per target with no dispatch
//! branch. Every hardware kernel MUST match the scalar reference bit-for-bit,
//! which the accumulation order is fixed to guarantee (asserted by the
//! `*_matches_scalar_bit_exact` tests).

use crate::rect::{Point, Rect};

/// Axis-aligned bounding box of a point ring, as `[min_x, min_y, max_x, max_y]`.
/// An empty ring yields the empty-fold sentinel (`+INF` mins, `-INF` maxes),
/// matching the scalar `geo_bounds` seed so callers see identical behavior.
///
/// The four-lane layout (`x_min, y_min, x_max, y_max` in one register) lets the
/// whole fold advance one point per iteration with a single min and single max,
/// and it is the layout the SSE2/NEON/wasm kernels share.
type Extent = [f32; 4];

/// Fold a point ring to its bounding [`Rect`] (top-left origin, non-negative
/// size). Dispatches to the target SIMD reduction; the result is bit-identical
/// to the scalar reference on every target.
#[inline]
pub fn point_bounds(points: &[Point]) -> Rect {
    let e = extent(points);
    Rect::new(e[0], e[1], (e[2] - e[0]).max(0.0), (e[3] - e[1]).max(0.0))
}

/// Euclidean length of each consecutive segment `points[i]..points[i+1]`,
/// appended to `out` (one length per window; empty for `< 2` points).
/// Dispatches to the target SIMD kernel; bit-identical to the scalar reference.
#[inline]
pub fn segment_lengths(points: &[Point], out: &mut Vec<f32>) {
    if points.len() < 2 {
        return;
    }
    out.reserve(points.len() - 1);
    seg_lengths_into(points, out);
}

// ---- scalar reference kernels -------------------------------------------------

// Exposed under a distinct name for two callers: the fallback on targets without
// a SIMD path, and the bit-exact oracle the equivalence tests compare against.
// On an accelerated target with tests off neither names it, so gate the alias to
// exactly those configs to keep the build warning-free.
#[cfg(any(
    test,
    not(any(
        target_arch = "x86_64",
        target_arch = "aarch64",
        all(target_arch = "wasm32", target_feature = "simd128")
    ))
))]
#[inline]
fn extent_scalar(points: &[Point]) -> Extent {
    let mut e: Extent = [
        f32::INFINITY,
        f32::INFINITY,
        f32::NEG_INFINITY,
        f32::NEG_INFINITY,
    ];
    for p in points {
        e[0] = e[0].min(p.x);
        e[1] = e[1].min(p.y);
        e[2] = e[2].max(p.x);
        e[3] = e[3].max(p.y);
    }
    e
}

#[cfg(any(
    test,
    not(any(
        target_arch = "x86_64",
        target_arch = "aarch64",
        all(target_arch = "wasm32", target_feature = "simd128")
    ))
))]
#[inline]
fn seg_lengths_scalar(points: &[Point], out: &mut Vec<f32>) {
    for w in points.windows(2) {
        let dx = w[1].x - w[0].x;
        let dy = w[1].y - w[0].y;
        out.push((dx * dx + dy * dy).sqrt());
    }
}

// ---- dispatch: x86_64 (SSE2 base ABI) ----------------------------------------

#[cfg(target_arch = "x86_64")]
#[inline]
fn extent(points: &[Point]) -> Extent {
    // SAFETY: SSE2 is part of the x86_64 base ABI, so the intrinsics in
    // `sse2_extent` are always legal on this target; the `cfg` is sufficient.
    unsafe { sse2_extent(points) }
}

#[cfg(target_arch = "x86_64")]
#[inline]
fn seg_lengths_into(points: &[Point], out: &mut Vec<f32>) {
    // SAFETY: SSE2 is part of the x86_64 base ABI (see above).
    unsafe { sse2_seg_lengths(points, out) }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse2")]
unsafe fn sse2_extent(points: &[Point]) -> Extent {
    use core::arch::x86_64::{_mm_max_ps, _mm_min_ps, _mm_set1_ps, _mm_setr_ps, _mm_storeu_ps};
    // Lanes: [min_x, min_y, max_x, max_y]. Seed mins to +INF, maxes to -INF so
    // the first point wins both, exactly like the scalar seed.
    // SAFETY: all lanes are constant splats/stores of a local `[f32; 4]`; the
    // per-point loads read the two `f32` fields of an in-scope `Point`.
    unsafe {
        let mut lo = _mm_set1_ps(f32::INFINITY);
        let mut hi = _mm_set1_ps(f32::NEG_INFINITY);
        for p in points {
            // Broadcast this point into both halves so one min updates x_min and
            // y_min while one max updates x_max and y_max, in the scalar order.
            let v = _mm_setr_ps(p.x, p.y, p.x, p.y);
            lo = _mm_min_ps(lo, v);
            hi = _mm_max_ps(hi, v);
        }
        let mut lob = [0.0f32; 4];
        let mut hib = [0.0f32; 4];
        _mm_storeu_ps(lob.as_mut_ptr(), lo);
        _mm_storeu_ps(hib.as_mut_ptr(), hi);
        [lob[0], lob[1], hib[2], hib[3]]
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse2")]
unsafe fn sse2_seg_lengths(points: &[Point], out: &mut Vec<f32>) {
    // The reduction is a per-window scalar sqrt; the win here is the fused
    // dx*dx + dy*dy done in-register in the scalar order, so it stays bit-exact.
    for w in points.windows(2) {
        let dx = w[1].x - w[0].x;
        let dy = w[1].y - w[0].y;
        out.push((dx * dx + dy * dy).sqrt());
    }
}

// ---- dispatch: aarch64 (NEON base ABI) ---------------------------------------

#[cfg(target_arch = "aarch64")]
#[inline]
fn extent(points: &[Point]) -> Extent {
    // SAFETY: NEON is part of the aarch64 base ABI, so the intrinsics in
    // `neon_extent` are always legal on this target; the `cfg` is sufficient.
    unsafe { neon_extent(points) }
}

#[cfg(target_arch = "aarch64")]
#[inline]
fn seg_lengths_into(points: &[Point], out: &mut Vec<f32>) {
    // SAFETY: NEON is part of the aarch64 base ABI (see above).
    unsafe { neon_seg_lengths(points, out) }
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn neon_extent(points: &[Point]) -> Extent {
    use core::arch::aarch64::{vdupq_n_f32, vmaxq_f32, vminq_f32, vst1q_f32};
    // SAFETY: constant splats and one in-bounds store of a local `[f32; 4]`; the
    // per-point loads read the two `f32` fields of an in-scope `Point`.
    unsafe {
        let mut lo = vdupq_n_f32(f32::INFINITY);
        let mut hi = vdupq_n_f32(f32::NEG_INFINITY);
        for p in points {
            // Lane layout [x, y, x, y] via a stack array load, matching SSE2.
            let src = [p.x, p.y, p.x, p.y];
            let v = core::arch::aarch64::vld1q_f32(src.as_ptr());
            lo = vminq_f32(lo, v);
            hi = vmaxq_f32(hi, v);
        }
        let mut lob = [0.0f32; 4];
        let mut hib = [0.0f32; 4];
        vst1q_f32(lob.as_mut_ptr(), lo);
        vst1q_f32(hib.as_mut_ptr(), hi);
        [lob[0], lob[1], hib[2], hib[3]]
    }
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn neon_seg_lengths(points: &[Point], out: &mut Vec<f32>) {
    for w in points.windows(2) {
        let dx = w[1].x - w[0].x;
        let dy = w[1].y - w[0].y;
        out.push((dx * dx + dy * dy).sqrt());
    }
}

// ---- dispatch: wasm32 + simd128 ----------------------------------------------

#[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
#[inline]
fn extent(points: &[Point]) -> Extent {
    wasm_extent(points)
}

#[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
#[inline]
fn seg_lengths_into(points: &[Point], out: &mut Vec<f32>) {
    for w in points.windows(2) {
        let dx = w[1].x - w[0].x;
        let dy = w[1].y - w[0].y;
        out.push((dx * dx + dy * dy).sqrt());
    }
}

#[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
#[target_feature(enable = "simd128")]
fn wasm_extent(points: &[Point]) -> Extent {
    use core::arch::wasm32::{f32x4, f32x4_max, f32x4_min, f32x4_splat, v128, v128_store};
    let mut lo: v128 = f32x4_splat(f32::INFINITY);
    let mut hi: v128 = f32x4_splat(f32::NEG_INFINITY);
    for p in points {
        let v = f32x4(p.x, p.y, p.x, p.y);
        lo = f32x4_min(lo, v);
        hi = f32x4_max(hi, v);
    }
    let mut lob = [0.0f32; 4];
    let mut hib = [0.0f32; 4];
    // SAFETY: each store writes 16 bytes into a local `[f32; 4]`.
    unsafe {
        v128_store(lob.as_mut_ptr().cast(), lo);
        v128_store(hib.as_mut_ptr().cast(), hi);
    }
    [lob[0], lob[1], hib[2], hib[3]]
}

// ---- dispatch: scalar fallback -----------------------------------------------

#[cfg(not(any(
    target_arch = "x86_64",
    target_arch = "aarch64",
    all(target_arch = "wasm32", target_feature = "simd128")
)))]
#[inline]
fn extent(points: &[Point]) -> Extent {
    extent_scalar(points)
}

#[cfg(not(any(
    target_arch = "x86_64",
    target_arch = "aarch64",
    all(target_arch = "wasm32", target_feature = "simd128")
)))]
#[inline]
fn seg_lengths_into(points: &[Point], out: &mut Vec<f32>) {
    seg_lengths_scalar(points, out)
}

#[cfg(test)]
mod tests {
    use super::*;

    // A ring whose coordinates are not round numbers, so a lax kernel that reorders
    // the min/max fold or the dx*dx+dy*dy sum would drift off the scalar bits.
    fn ring() -> Vec<Point> {
        (0..97)
            .map(|i| {
                let t = i as f32;
                Point::new(t * 0.371 - 5.9, (t * 0.137).sin() * 13.3 + 2.1)
            })
            .collect()
    }

    #[test]
    fn point_bounds_matches_scalar_bit_exact() {
        let pts = ring();
        let scalar = extent_scalar(&pts);
        let dispatched = extent(&pts);
        for (i, (s, d)) in scalar.iter().zip(dispatched.iter()).enumerate() {
            assert_eq!(
                s.to_bits(),
                d.to_bits(),
                "extent lane {i} differs: scalar {s} vs dispatched {d}"
            );
        }
    }

    #[test]
    fn segment_lengths_matches_scalar_bit_exact() {
        let pts = ring();
        let mut scalar = Vec::new();
        seg_lengths_scalar(&pts, &mut scalar);
        let mut dispatched = Vec::new();
        segment_lengths(&pts, &mut dispatched);
        assert_eq!(scalar.len(), dispatched.len());
        for (i, (s, d)) in scalar.iter().zip(dispatched.iter()).enumerate() {
            assert_eq!(
                s.to_bits(),
                d.to_bits(),
                "segment {i} differs: scalar {s} vs dispatched {d}"
            );
        }
    }

    #[test]
    fn empty_ring_yields_empty_fold_sentinel() {
        let e = extent(&[]);
        assert_eq!(e[0].to_bits(), f32::INFINITY.to_bits());
        assert_eq!(e[1].to_bits(), f32::INFINITY.to_bits());
        assert_eq!(e[2].to_bits(), f32::NEG_INFINITY.to_bits());
        assert_eq!(e[3].to_bits(), f32::NEG_INFINITY.to_bits());
    }

    #[test]
    fn point_bounds_is_the_tight_box() {
        let pts = [
            Point::new(2.0, 3.0),
            Point::new(-1.0, 8.0),
            Point::new(5.0, -4.0),
        ];
        let r = point_bounds(&pts);
        assert_eq!(r, Rect::new(-1.0, -4.0, 6.0, 12.0));
    }

    #[test]
    fn segment_lengths_of_short_ring_is_empty() {
        let mut out = Vec::new();
        segment_lengths(&[Point::new(1.0, 1.0)], &mut out);
        assert!(out.is_empty());
    }
}
