//! SSE2 `Mat4` product for `x86_64`.
//!
//! Column-major storage makes each of A's four columns exactly one `__m128`
//! (`a[4k .. 4k+4]`). Output column `c` is the linear combination
//! `A_col0*b[bc] + A_col1*b[bc+1] + A_col2*b[bc+2] + A_col3*b[bc+3]`, summed in
//! that fixed order with **separate** multiply and add (no FMA), which is
//! exactly the scalar reference's per-element order — so the result is
//! bit-identical, not merely close.

use core::arch::x86_64::{
    __m128, _mm_add_ps, _mm_loadu_ps, _mm_mul_ps, _mm_set1_ps, _mm_storeu_ps,
};

/// Column-major `4×4 * 4×4` product `a * b` using SSE2.
///
/// # Safety
///
/// Uses SSE2 intrinsics. Safe to call on any `x86_64` target because SSE2 is
/// part of the base `x86_64` ABI; the caller need only ensure the target is
/// `x86_64` (guaranteed by the `cfg` at the call site).
#[inline]
#[target_feature(enable = "sse2")]
pub(crate) unsafe fn mat4_mul(a: &[f32; 16], b: &[f32; 16]) -> crate::mat::Mat4 {
    let mut out = [0.0f32; 16];
    // SAFETY: `a` is `[f32; 16]`; each `_mm_loadu_ps` reads 4 contiguous f32
    // starting at 0/4/8/12, all in bounds. Unaligned loads/stores are used, so
    // no alignment invariant is required.
    unsafe {
        let a0: __m128 = _mm_loadu_ps(a.as_ptr());
        let a1: __m128 = _mm_loadu_ps(a.as_ptr().add(4));
        let a2: __m128 = _mm_loadu_ps(a.as_ptr().add(8));
        let a3: __m128 = _mm_loadu_ps(a.as_ptr().add(12));

        for c in 0..4 {
            let bc = c * 4;
            // Broadcast each scalar of B's column c and combine A's columns in
            // the scalar reference's order: ((col0*b0 + col1*b1) + col2*b2) ...
            // matches `a[r]*b0 + a[4+r]*b1 + a[8+r]*b2 + a[12+r]*b3` element by
            // element (add is left-associative here, as in the scalar loop).
            let mut acc = _mm_mul_ps(a0, _mm_set1_ps(b[bc]));
            acc = _mm_add_ps(acc, _mm_mul_ps(a1, _mm_set1_ps(b[bc + 1])));
            acc = _mm_add_ps(acc, _mm_mul_ps(a2, _mm_set1_ps(b[bc + 2])));
            acc = _mm_add_ps(acc, _mm_mul_ps(a3, _mm_set1_ps(b[bc + 3])));
            _mm_storeu_ps(out.as_mut_ptr().add(bc), acc);
        }
    }
    crate::mat::Mat4 { v: out }
}
