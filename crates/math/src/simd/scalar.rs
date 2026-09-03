//! The scalar reference kernel — the bit-exact contract every SIMD kernel in
//! this module must reproduce.
//!
//! The accumulation order here (`a[r]*b[c] + a[4+r]*b[c+1] + …`) is the canonical
//! one: the SIMD kernels are written to sum their lanes in the same order, so
//! their results match this one bit-for-bit rather than merely approximately.

/// Column-major `4×4 * 4×4` product `a * b`, scalar reference.
///
/// Column `c`, row `r` of the result is `sum_k a[k][r] * b[c][k]`, with element
/// `[col][row]` stored at index `col * 4 + row`.
///
/// Kept as the equivalence oracle and the no-SIMD fallback; on an accelerated
/// target with tests off nothing calls it, hence `#[allow(dead_code)]`.
#[allow(dead_code)]
#[inline]
pub(crate) fn mat4_mul(a: &[f32; 16], b: &[f32; 16]) -> crate::mat::Mat4 {
    let mut out = [0.0f32; 16];
    for c in 0..4 {
        let bc = c * 4;
        for r in 0..4 {
            out[bc + r] =
                a[r] * b[bc] + a[4 + r] * b[bc + 1] + a[8 + r] * b[bc + 2] + a[12 + r] * b[bc + 3];
        }
    }
    crate::mat::Mat4 { v: out }
}
