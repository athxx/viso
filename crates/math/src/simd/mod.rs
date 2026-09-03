//! Internal SIMD kernels for the hot `Mat4` paths.
//!
//! The public matrix ABI is always the scalar `[f32; 16]` layout (see
//! [`mat`](crate::mat)); this module holds the accelerated implementations that
//! back `Mat4::mul`. It is an **internal optimization boundary**: it never
//! appears in a public signature, and any hardware kernel added here MUST match
//! the scalar reference bit-for-bit (asserted by a test when kernels land).
//!
//! For now this is the scalar reference only. Target-feature-gated SSE2/NEON/
//! wasm128 kernels land in a later slice behind the same [`mat4_mul`] signature,
//! together with the scalar-equivalence test.

/// Column-major `4×4 * 4×4` product `a * b`, scalar reference implementation.
///
/// Column `c`, row `r` of the result is `sum_k a[k][r] * b[c][k]`, with element
/// `[col][row]` stored at index `col * 4 + row`.
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
