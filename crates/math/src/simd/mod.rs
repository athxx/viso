//! Internal SIMD kernels for the hot `Mat4` paths.
//!
//! The public matrix ABI is always the scalar `[f32; 16]` layout (see
//! [`mat`](crate::mat)); this module holds the accelerated implementations that
//! back `Mat4::mul`. It is an **internal optimization boundary**: it never
//! appears in a public signature, and every hardware kernel here MUST match the
//! scalar reference bit-for-bit (asserted by `simd_matches_scalar_bit_exact`).
//!
//! [`mat4_mul`] is the single entry point. It selects a kernel at compile time
//! from the target's guaranteed features — SSE2 on `x86_64`, NEON on
//! `aarch64`, `simd128` on `wasm32` when enabled — and falls back to the scalar
//! reference everywhere else. Because the choice is a `cfg` (not a runtime
//! check), a build always compiles exactly one kernel and the accelerated path
//! carries no dispatch branch.

mod scalar;

// The scalar reference, exposed under a distinct name for two callers: the
// fallback kernel on targets without a SIMD path, and the bit-exact oracle the
// SIMD-equivalence test compares against. On an accelerated target with tests
// off neither names it, so gate the alias to exactly those configs to keep the
// build warning-free.
#[cfg(any(
    test,
    not(any(
        target_arch = "x86_64",
        target_arch = "aarch64",
        all(target_arch = "wasm32", target_feature = "simd128")
    ))
))]
pub(crate) use scalar::mat4_mul as mat4_mul_scalar;

// x86_64 always has SSE2 (it is part of the base ABI), so this needs no runtime
// probe: the `cfg` alone guarantees the instructions are legal.
#[cfg(target_arch = "x86_64")]
mod sse2;

// aarch64 always has NEON (part of the base ABI), same guarantee.
#[cfg(target_arch = "aarch64")]
mod neon;

// wasm128 is opt-in at build time via `-C target-feature=+simd128`; the `cfg`
// is only set when that feature is actually enabled, so it stays legal.
#[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
mod wasm128;

/// Column-major `4×4 * 4×4` product `a * b`.
///
/// Column `c`, row `r` of the result is `sum_k a[k][r] * b[c][k]`, with element
/// `[col][row]` stored at index `col * 4 + row`. Dispatches to the target's
/// SIMD kernel where one exists, else the scalar reference; all kernels agree
/// bit-for-bit with the scalar path.
#[cfg(target_arch = "x86_64")]
#[inline]
pub(crate) fn mat4_mul(a: &[f32; 16], b: &[f32; 16]) -> crate::mat::Mat4 {
    // SAFETY: SSE2 is part of the x86_64 base ABI, so the intrinsics used by
    // `sse2::mat4_mul` are always available on this target; the `cfg` is
    // sufficient and no runtime feature probe is needed.
    unsafe { sse2::mat4_mul(a, b) }
}

#[cfg(target_arch = "aarch64")]
#[inline]
pub(crate) fn mat4_mul(a: &[f32; 16], b: &[f32; 16]) -> crate::mat::Mat4 {
    // SAFETY: NEON is part of the aarch64 base ABI, so the intrinsics used by
    // `neon::mat4_mul` are always available on this target.
    unsafe { neon::mat4_mul(a, b) }
}

#[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
#[inline]
pub(crate) fn mat4_mul(a: &[f32; 16], b: &[f32; 16]) -> crate::mat::Mat4 {
    wasm128::mat4_mul(a, b)
}

#[cfg(not(any(
    target_arch = "x86_64",
    target_arch = "aarch64",
    all(target_arch = "wasm32", target_feature = "simd128")
)))]
#[inline]
pub(crate) fn mat4_mul(a: &[f32; 16], b: &[f32; 16]) -> crate::mat::Mat4 {
    mat4_mul_scalar(a, b)
}

#[cfg(test)]
mod tests {
    use super::*;

    // A pair of matrices whose product exercises every element, chosen so the
    // sums are not round-number exact — a lax kernel would drift here.
    fn sample() -> ([f32; 16], [f32; 16]) {
        let mut a = [0.0f32; 16];
        let mut b = [0.0f32; 16];
        for i in 0..16 {
            a[i] = (i as f32) * 0.37 - 2.9;
            b[i] = 1.0 / ((i as f32) + 1.3);
        }
        (a, b)
    }

    #[test]
    fn simd_matches_scalar_bit_exact() {
        let (a, b) = sample();
        let scalar = mat4_mul_scalar(&a, &b);
        let dispatched = mat4_mul(&a, &b);
        // Bit-exact: the SIMD kernel must reproduce the scalar reference's
        // exact bit pattern (accumulation order is fixed to match), not merely
        // be close. Compare the raw bits so a NaN or a 1-ulp drift fails.
        for (i, (s, d)) in scalar.v.iter().zip(dispatched.v.iter()).enumerate() {
            assert_eq!(
                s.to_bits(),
                d.to_bits(),
                "element {i} differs: scalar {s} vs dispatched {d}"
            );
        }
    }

    #[test]
    fn identity_is_multiplicative_unit() {
        let id = [
            1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
        ];
        let (a, _) = sample();
        let left = mat4_mul(&id, &a);
        let right = mat4_mul(&a, &id);
        for ((l, r), expected) in left.v.iter().zip(right.v.iter()).zip(a.iter()) {
            assert_eq!(l, expected);
            assert_eq!(r, expected);
        }
    }
}
