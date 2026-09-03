//! wasm `simd128` `Mat4` product for `wasm32` (built with `+simd128`).
//!
//! Column-major storage makes each of A's four columns exactly one `v128`
//! (`a[4k .. 4k+4]`). Output column `c` is `A_col0*b[bc] + A_col1*b[bc+1] +
//! A_col2*b[bc+2] + A_col3*b[bc+3]`, summed in that fixed order with separate
//! multiply and add, reproducing the scalar reference bit-for-bit.
//!
//! The wasm SIMD load/store intrinsics are safe (bounds are the pointer's, and
//! we pass in-bounds pointers), so this kernel needs no `unsafe`.

use core::arch::wasm32::{f32x4_add, f32x4_mul, f32x4_splat, v128, v128_load, v128_store};

/// Column-major `4×4 * 4×4` product `a * b` using wasm `simd128`.
#[inline]
#[target_feature(enable = "simd128")]
pub(crate) fn mat4_mul(a: &[f32; 16], b: &[f32; 16]) -> crate::mat::Mat4 {
    let mut out = [0.0f32; 16];
    // v128_load / v128_store are unsafe intrinsics (raw pointer reads); each
    // access below is 16 bytes at an in-bounds offset of `[f32; 16]`.
    unsafe {
        let a0: v128 = v128_load(a.as_ptr().cast());
        let a1: v128 = v128_load(a.as_ptr().add(4).cast());
        let a2: v128 = v128_load(a.as_ptr().add(8).cast());
        let a3: v128 = v128_load(a.as_ptr().add(12).cast());

        for c in 0..4 {
            let bc = c * 4;
            let mut acc = f32x4_mul(a0, f32x4_splat(b[bc]));
            acc = f32x4_add(acc, f32x4_mul(a1, f32x4_splat(b[bc + 1])));
            acc = f32x4_add(acc, f32x4_mul(a2, f32x4_splat(b[bc + 2])));
            acc = f32x4_add(acc, f32x4_mul(a3, f32x4_splat(b[bc + 3])));
            // SAFETY: writes 16 bytes at in-bounds offset `bc` of `[f32; 16]`.
            v128_store(out.as_mut_ptr().add(bc).cast(), acc);
        }
    }
    crate::mat::Mat4 { v: out }
}
