//! Batch geometry reductions over point slices — the public, scalar-ABI entry
//! points to the SIMD-accelerated kernels in [`simd`](crate::simd).
//!
//! These back the render tessellator's cold-path preprocessing (§13.6): folding
//! a flattened outline to its bounding box, and computing per-segment arc
//! lengths for dash splitting. The signatures are the plain scalar `[Point]` /
//! `Vec<f32>` ABI; a target-appropriate SIMD kernel is selected behind them at
//! compile time and is guaranteed bit-for-bit identical to the scalar reference.
//! The caller keeps its own scalar implementation as the correctness oracle.

use crate::rect::{Point, Rect};

/// Axis-aligned bounding [`Rect`] of a point ring (top-left origin, non-negative
/// size). An empty ring yields an empty rect at the `+INF`/`-INF` fold sentinel,
/// matching a scalar min/max fold seeded the same way.
#[inline]
pub fn point_bounds(points: &[Point]) -> Rect {
    crate::simd::point_bounds(points)
}

/// Append the Euclidean length of each consecutive segment `points[i]..[i+1]`
/// to `out` (one length per window; nothing for `< 2` points). Bit-identical to
/// the scalar `sqrt(dx*dx + dy*dy)` per window.
#[inline]
pub fn segment_lengths(points: &[Point], out: &mut Vec<f32>) {
    crate::simd::segment_lengths(points, out);
}
