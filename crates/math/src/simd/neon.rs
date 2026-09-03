//! NEON `Mat4` product for `aarch64`.
//!
//! Column-major storage makes each of A's four columns exactly one `float32x4`
//! (`a[4k .. 4k+4]`). Output column `c` is `A_col0*b[bc] + A_col1*b[bc+1] +
//! A_col2*b[bc+2] + A_col3*b[bc+3]`, summed in that fixed order with **separate**
//! multiply and add (`vmulq_f32`/`vaddq_f32`, never a fused `vfmaq`), which
//! reproduces the scalar reference's per-element order bit-for-bit.

use core::arch::aarch64::{float32x4_t, vaddq_f32, vdupq_n_f32, vld1q_f32, vmulq_f32, vst1q_f32};

/// Column-major `4×4 * 4×4` product `a * b` using NEON.
///
/// # Safety
///
/// Uses NEON intrinsics. Safe to call on any `aarch64` target because NEON is
/// part of the base `aarch64` ABI; the caller need only ensure the target is
/// `aarch64` (guaranteed by the `cfg` at the call site).
#[inline]
#[target_feature(enable = "neon")]
pub(crate) unsafe fn mat4_mul(a: &[f32; 16], b: &[f32; 16]) -> crate::mat::Mat4 {
    let mut out = [0.0f32; 16];
    // SAFETY: `a` is `[f32; 16]`; each `vld1q_f32` reads 4 contiguous f32
    // starting at 0/4/8/12, all in bounds; stores write 4 in-bounds f32.
    unsafe {
        let a0: float32x4_t = vld1q_f32(a.as_ptr());
        let a1: float32x4_t = vld1q_f32(a.as_ptr().add(4));
        let a2: float32x4_t = vld1q_f32(a.as_ptr().add(8));
        let a3: float32x4_t = vld1q_f32(a.as_ptr().add(12));

        for c in 0..4 {
            let bc = c * 4;
            // Same left-associative order as the scalar loop, plain mul+add so
            // no fused-multiply-add rounding creeps in.
            let mut acc = vmulq_f32(a0, vdupq_n_f32(b[bc]));
            acc = vaddq_f32(acc, vmulq_f32(a1, vdupq_n_f32(b[bc + 1])));
            acc = vaddq_f32(acc, vmulq_f32(a2, vdupq_n_f32(b[bc + 2])));
            acc = vaddq_f32(acc, vmulq_f32(a3, vdupq_n_f32(b[bc + 3])));
            vst1q_f32(out.as_mut_ptr().add(bc), acc);
        }
    }
    crate::mat::Mat4 { v: out }
}
