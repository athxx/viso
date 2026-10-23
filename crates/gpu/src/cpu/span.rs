//! Row kernels of the 8-bit pipeline: premultiplied `f32` color packed into
//! bytes, and bytes composited source-over in integers.
//!
//! Every kernel has a scalar definition and, where the target has one, an
//! SSE2 / NEON / SIMD128 implementation of the *same* integer arithmetic, so
//! the vector and scalar results are bit-identical (the tests compare them on
//! random rows). The arithmetic:
//!
//! - **pack:** `trunc(min(max(v, 0), 1) * 255 + 0.5)` per channel, `max`
//!   returning the non-NaN operand, so NaN packs to 0;
//! - **over:** `d' = sat(s + div255(d * (255 - sa)))` per channel, with
//!   `div255(x) = (x + 128 + ((x + 128) >> 8)) >> 8`, the exact rounded
//!   quotient for every product of two bytes.
//!
//! An sRGB target runs the scalar kernels at the end instead: a pixel is
//! decoded to linear light, composited in `f32`, and encoded, its alpha
//! packed as above. An opaque source only encodes; a transparent
//! black one leaves the pixel as it is.

use crate::srgb;

/// The exact rounded `x / 255` for `x <= 255 * 255`.
#[inline(always)]
pub(super) fn div255(x: u32) -> u32 {
    let y = x + 128;
    (y + (y >> 8)) >> 8
}

/// One channel packed into a byte.
#[inline(always)]
pub(super) fn pack1(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
}

/// One premultiplied color packed into bytes, red and blue swapped for a
/// BGRA target.
#[inline(always)]
pub(super) fn pack(c: [f32; 4], bgra: bool) -> [u8; 4] {
    let [r, g, b, a] = c.map(pack1);
    if bgra { [b, g, r, a] } else { [r, g, b, a] }
}

/// `s` over `d`, one pixel.
#[inline(always)]
pub(super) fn over1(d: [u8; 4], s: [u8; 4]) -> [u8; 4] {
    let inv = 255 - u32::from(s[3]);
    std::array::from_fn(|c| (u32::from(s[c]) + div255(u32::from(d[c]) * inv)).min(255) as u8)
}

/// `src` packed into `out` (equal lengths).
pub(super) fn pack_row(src: &[[f32; 4]], out: &mut [[u8; 4]], bgra: bool) {
    debug_assert_eq!(src.len(), out.len());
    let done = simd::pack(src, out, bgra);
    for (o, &s) in out[done..].iter_mut().zip(&src[done..]) {
        *o = pack(s, bgra);
    }
}

/// `src` composited over `dst` (equal lengths).
pub(super) fn over_row(dst: &mut [[u8; 4]], src: &[[u8; 4]]) {
    debug_assert_eq!(src.len(), dst.len());
    let done = simd::over(dst, src);
    for (d, &s) in dst[done..].iter_mut().zip(&src[done..]) {
        *d = over1(*d, s);
    }
}

/// One premultiplied linear color encoded into sRGB bytes, red and blue
/// swapped for a BGRA target.
#[inline(always)]
pub(super) fn encode(c: [f32; 4], bgra: bool) -> [u8; 4] {
    let e = srgb::encode;
    let [r, g, b, a] = [e(c[0]), e(c[1]), e(c[2]), pack1(c[3])];
    if bgra { [b, g, r, a] } else { [r, g, b, a] }
}

/// sRGB bytes decoded to premultiplied linear color.
#[inline(always)]
pub(super) fn decode(d: [u8; 4], bgra: bool) -> [f32; 4] {
    let [r, g, b, a] = if bgra { [d[2], d[1], d[0], d[3]] } else { d };
    let dec = srgb::decode;
    [dec(r), dec(g), dec(b), f32::from(a) / 255.0]
}

/// `s` over the linear color `d`, both premultiplied.
#[inline(always)]
pub(super) fn over_linear(d: [f32; 4], s: [f32; 4]) -> [f32; 4] {
    let inv = 1.0 - s[3];
    std::array::from_fn(|c| s[c] + d[c] * inv)
}

/// Whether `s` leaves what it is drawn over as it is.
#[inline(always)]
pub(super) fn clear(s: [f32; 4]) -> bool {
    s == [0.0; 4]
}

/// `src` encoded into `out` (equal lengths). A run of one color — a fill —
/// encodes once.
pub(super) fn encode_row(src: &[[f32; 4]], out: &mut [[u8; 4]], bgra: bool) {
    debug_assert_eq!(src.len(), out.len());
    let mut last: Option<([f32; 4], [u8; 4])> = None;
    for (o, &s) in out.iter_mut().zip(src) {
        *o = match last {
            Some((ls, lo)) if ls == s => lo,
            _ => {
                let e = encode(s, bgra);
                last = Some((s, e));
                e
            }
        };
    }
}

/// `src` composited over the sRGB pixels `dst` in linear light (equal
/// lengths). A pixel drawn as its neighbour was — the same source over the
/// same destination, or the same opaque source — takes its result, so a fill
/// over a fill decodes and encodes once a run.
pub(super) fn over_srgb_row(dst: &mut [[u8; 4]], src: &[[f32; 4]], bgra: bool) {
    debug_assert_eq!(src.len(), dst.len());
    // The last source drawn, the pixel it was drawn over, and the result.
    let mut last: Option<([f32; 4], [u8; 4], [u8; 4])> = None;
    for (d, &s) in dst.iter_mut().zip(src) {
        if let Some((ls, ld, lo)) = last
            && ls == s
            && (ld == *d || s[3] >= 1.0)
        {
            *d = lo;
            continue;
        }
        let under = *d;
        if s[3] >= 1.0 {
            *d = encode(s, bgra);
        } else if !clear(s) {
            *d = encode(over_linear(decode(under, bgra), s), bgra);
        } else {
            continue;
        }
        last = Some((s, under, *d));
    }
}

#[cfg(target_arch = "x86_64")]
mod simd {
    use std::arch::x86_64::*;

    /// Packs the leading multiple of four pixels; returns how many.
    pub(super) fn pack(src: &[[f32; 4]], out: &mut [[u8; 4]], bgra: bool) -> usize {
        let n = src.len() & !3;
        // SAFETY: SSE2 is part of the x86_64 baseline; every load reads four
        // in-bounds pixels of `src` (16 floats) and every store writes four
        // in-bounds pixels of `out` (16 bytes), both unaligned.
        unsafe {
            let zero = _mm_setzero_ps();
            let one = _mm_set1_ps(1.0);
            let k = _mm_set1_ps(255.0);
            let half = _mm_set1_ps(0.5);
            let quant = |v: __m128| {
                let v = if bgra {
                    _mm_shuffle_ps::<0b11_00_01_10>(v, v)
                } else {
                    v
                };
                let v = _mm_min_ps(_mm_max_ps(v, zero), one);
                _mm_cvttps_epi32(_mm_add_ps(_mm_mul_ps(v, k), half))
            };
            for i in (0..n).step_by(4) {
                let p = src.as_ptr().add(i).cast::<f32>();
                let a = quant(_mm_loadu_ps(p));
                let b = quant(_mm_loadu_ps(p.add(4)));
                let c = quant(_mm_loadu_ps(p.add(8)));
                let d = quant(_mm_loadu_ps(p.add(12)));
                let bytes = _mm_packus_epi16(_mm_packs_epi32(a, b), _mm_packs_epi32(c, d));
                _mm_storeu_si128(out.as_mut_ptr().add(i).cast(), bytes);
            }
        }
        n
    }

    /// Composites the leading multiple of four pixels; returns how many.
    pub(super) fn over(dst: &mut [[u8; 4]], src: &[[u8; 4]]) -> usize {
        let n = dst.len() & !3;
        // SAFETY: SSE2 is part of the x86_64 baseline; each iteration loads and
        // stores four in-bounds pixels (16 bytes) of `src` and `dst`.
        unsafe {
            let zero = _mm_setzero_si128();
            let k255 = _mm_set1_epi16(255);
            let k128 = _mm_set1_epi16(128);
            // Alpha of each of two pixels, replicated over its four lanes.
            let inv = |s: __m128i| {
                let a =
                    _mm_shufflehi_epi16::<0b11_11_11_11>(_mm_shufflelo_epi16::<0b11_11_11_11>(s));
                _mm_sub_epi16(k255, a)
            };
            let div255 = |x: __m128i| {
                let y = _mm_add_epi16(x, k128);
                _mm_srli_epi16::<8>(_mm_add_epi16(y, _mm_srli_epi16::<8>(y)))
            };
            for i in (0..n).step_by(4) {
                let s = _mm_loadu_si128(src.as_ptr().add(i).cast());
                let d = _mm_loadu_si128(dst.as_ptr().add(i).cast());
                let (s_lo, s_hi) = (_mm_unpacklo_epi8(s, zero), _mm_unpackhi_epi8(s, zero));
                let (d_lo, d_hi) = (_mm_unpacklo_epi8(d, zero), _mm_unpackhi_epi8(d, zero));
                let lo = div255(_mm_mullo_epi16(d_lo, inv(s_lo)));
                let hi = div255(_mm_mullo_epi16(d_hi, inv(s_hi)));
                let out = _mm_adds_epu8(s, _mm_packus_epi16(lo, hi));
                _mm_storeu_si128(dst.as_mut_ptr().add(i).cast(), out);
            }
        }
        n
    }
}

#[cfg(target_arch = "aarch64")]
mod simd {
    use std::arch::aarch64::*;

    /// Packs the leading multiple of eight pixels; returns how many.
    pub(super) fn pack(src: &[[f32; 4]], out: &mut [[u8; 4]], bgra: bool) -> usize {
        let n = src.len() & !7;
        // SAFETY: NEON is part of the aarch64 baseline; each iteration reads
        // eight in-bounds pixels of `src` (two `vld4q` of 16 floats, split per
        // channel) and writes eight of `out` (32 bytes, interleaved by `vst4`).
        unsafe {
            let zero = vdupq_n_f32(0.0);
            let one = vdupq_n_f32(1.0);
            let k = vdupq_n_f32(255.0);
            let half = vdupq_n_f32(0.5);
            let quant = |v: float32x4_t| {
                let v = vminnmq_f32(vmaxnmq_f32(v, zero), one);
                vmovn_u32(vcvtq_u32_f32(vaddq_f32(vmulq_f32(v, k), half)))
            };
            let channel =
                |lo: float32x4_t, hi: float32x4_t| vmovn_u16(vcombine_u16(quant(lo), quant(hi)));
            for i in (0..n).step_by(8) {
                let p = src.as_ptr().add(i).cast::<f32>();
                let lo = vld4q_f32(p);
                let hi = vld4q_f32(p.add(16));
                let (r, g, b, a) = (
                    channel(lo.0, hi.0),
                    channel(lo.1, hi.1),
                    channel(lo.2, hi.2),
                    channel(lo.3, hi.3),
                );
                let (r, b) = if bgra { (b, r) } else { (r, b) };
                vst4_u8(out.as_mut_ptr().add(i).cast(), uint8x8x4_t(r, g, b, a));
            }
        }
        n
    }

    /// Composites the leading multiple of eight pixels; returns how many.
    pub(super) fn over(dst: &mut [[u8; 4]], src: &[[u8; 4]]) -> usize {
        let n = dst.len() & !7;
        // SAFETY: NEON is part of the aarch64 baseline; each iteration loads
        // and stores eight in-bounds pixels (32 bytes) of `src` and `dst`,
        // de-interleaved per channel by `vld4`.
        unsafe {
            let k255 = vdup_n_u8(255);
            for i in (0..n).step_by(8) {
                let s = vld4_u8(src.as_ptr().add(i).cast());
                let d = vld4_u8(dst.as_ptr().add(i).cast());
                let inv = vsub_u8(k255, s.3);
                // (x + 128 + ((x + 128) >> 8)) >> 8, narrowed: `vrshrq_n_u16`
                // is (x + 128) >> 8 and `vraddhn_u16` adds with the rounding 128.
                let mix = |dc: uint8x8_t, sc: uint8x8_t| {
                    let x = vmull_u8(dc, inv);
                    vqadd_u8(sc, vraddhn_u16(x, vrshrq_n_u16::<8>(x)))
                };
                let out = uint8x8x4_t(mix(d.0, s.0), mix(d.1, s.1), mix(d.2, s.2), mix(d.3, s.3));
                vst4_u8(dst.as_mut_ptr().add(i).cast(), out);
            }
        }
        n
    }
}

#[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
mod simd {
    use std::arch::wasm32::*;

    /// Packs the leading multiple of four pixels; returns how many.
    pub(super) fn pack(src: &[[f32; 4]], out: &mut [[u8; 4]], bgra: bool) -> usize {
        let n = src.len() & !3;
        let zero = f32x4_splat(0.0);
        let one = f32x4_splat(1.0);
        let k = f32x4_splat(255.0);
        let half = f32x4_splat(0.5);
        let quant = |v: v128| {
            let v = if bgra {
                i32x4_shuffle::<2, 1, 0, 3>(v, v)
            } else {
                v
            };
            // `pmax(zero, v)` is `zero < v ? v : zero`, so NaN gives zero.
            let v = f32x4_pmin(one, f32x4_pmax(zero, v));
            i32x4_trunc_sat_f32x4(f32x4_add(f32x4_mul(v, k), half))
        };
        for i in (0..n).step_by(4) {
            // SAFETY: each read is four in-bounds pixels (16 floats) of `src`,
            // each write four in-bounds pixels (16 bytes) of `out`; `v128`
            // loads and stores are unaligned.
            unsafe {
                let p = src.as_ptr().add(i).cast::<v128>();
                let a = quant(v128_load(p));
                let b = quant(v128_load(p.add(1)));
                let c = quant(v128_load(p.add(2)));
                let d = quant(v128_load(p.add(3)));
                let bytes = u8x16_narrow_i16x8(i16x8_narrow_i32x4(a, b), i16x8_narrow_i32x4(c, d));
                v128_store(out.as_mut_ptr().add(i).cast(), bytes);
            }
        }
        n
    }

    /// Composites the leading multiple of four pixels; returns how many.
    pub(super) fn over(dst: &mut [[u8; 4]], src: &[[u8; 4]]) -> usize {
        let n = dst.len() & !3;
        let k255 = u16x8_splat(255);
        let k128 = u16x8_splat(128);
        let inv = |s: v128| {
            let a = i16x8_shuffle::<3, 3, 3, 3, 7, 7, 7, 7>(s, s);
            u16x8_sub(k255, a)
        };
        let div255 = |x: v128| {
            let y = u16x8_add(x, k128);
            u16x8_shr(u16x8_add(y, u16x8_shr(y, 8)), 8)
        };
        for i in (0..n).step_by(4) {
            // SAFETY: each iteration loads and stores four in-bounds pixels
            // (16 bytes) of `src` and `dst`; `v128` loads are unaligned.
            unsafe {
                let s = v128_load(src.as_ptr().add(i).cast());
                let d = v128_load(dst.as_ptr().add(i).cast());
                let (s_lo, s_hi) = (u16x8_extend_low_u8x16(s), u16x8_extend_high_u8x16(s));
                let (d_lo, d_hi) = (u16x8_extend_low_u8x16(d), u16x8_extend_high_u8x16(d));
                let lo = div255(i16x8_mul(d_lo, inv(s_lo)));
                let hi = div255(i16x8_mul(d_hi, inv(s_hi)));
                let out = u8x16_add_sat(s, u8x16_narrow_i16x8(lo, hi));
                v128_store(dst.as_mut_ptr().add(i).cast(), out);
            }
        }
        n
    }
}

#[cfg(not(any(
    target_arch = "x86_64",
    target_arch = "aarch64",
    all(target_arch = "wasm32", target_feature = "simd128")
)))]
mod simd {
    pub(super) fn pack(_: &[[f32; 4]], _: &mut [[u8; 4]], _: bool) -> usize {
        0
    }

    pub(super) fn over(_: &mut [[u8; 4]], _: &[[u8; 4]]) -> usize {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A deterministic xorshift stream.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }

        fn byte(&mut self) -> u8 {
            self.next() as u8
        }

        fn float(&mut self) -> f32 {
            match self.next() % 16 {
                0 => f32::NAN,
                1 => -1.5,
                2 => 7.0,
                3 => f32::INFINITY,
                4 => 0.5 / 255.0,
                5 => 1.5 / 255.0,
                _ => (self.next() % 1_000_001) as f32 / 1_000_000.0,
            }
        }
    }

    #[test]
    fn div255_is_the_rounded_quotient_of_every_byte_product() {
        for x in 0..=255 * 255u32 {
            assert_eq!(div255(x), (x * 2 + 255) / 510, "{x}");
            // The multiply-high form SSE and NEON variants are proven against.
            assert_eq!(div255(x), ((x + 128) * 257) >> 16, "{x}");
        }
    }

    #[test]
    fn vector_packing_matches_scalar_packing() {
        let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
        for len in [0, 1, 3, 4, 7, 8, 13, 64] {
            for bgra in [false, true] {
                let src: Vec<[f32; 4]> = (0..len)
                    .map(|_| std::array::from_fn(|_| rng.float()))
                    .collect();
                let mut vector = vec![[0u8; 4]; len];
                pack_row(&src, &mut vector, bgra);
                let scalar: Vec<[u8; 4]> = src.iter().map(|&c| pack(c, bgra)).collect();
                assert_eq!(vector, scalar, "len {len} bgra {bgra}");
            }
        }
    }

    #[test]
    fn vector_compositing_matches_scalar_compositing() {
        let mut rng = Rng(0x2545_f491_4f6c_dd1d);
        for len in [0, 1, 3, 4, 7, 8, 9, 15, 16, 64] {
            for _ in 0..50 {
                let src: Vec<[u8; 4]> = (0..len)
                    .map(|_| {
                        let a = rng.byte();
                        // Mostly premultiplied, sometimes not: saturation is
                        // part of the contract.
                        let cap = if rng.next().is_multiple_of(8) { 255 } else { a };
                        [0; 3]
                            .map(|_: u8| (rng.byte() as u32 * cap as u32 / 255) as u8)
                            .into_iter()
                            .chain([a])
                            .collect::<Vec<_>>()
                            .try_into()
                            .unwrap()
                    })
                    .collect();
                let dst: Vec<[u8; 4]> = (0..len)
                    .map(|_| std::array::from_fn(|_| rng.byte()))
                    .collect();
                let mut vector = dst.clone();
                over_row(&mut vector, &src);
                let scalar: Vec<[u8; 4]> =
                    dst.iter().zip(&src).map(|(&d, &s)| over1(d, s)).collect();
                assert_eq!(vector, scalar, "len {len}");
            }
        }
    }

    #[test]
    fn opaque_and_clear_sources_are_exact() {
        assert_eq!(over1([9, 8, 7, 6], [1, 2, 3, 255]), [1, 2, 3, 255]);
        assert_eq!(over1([9, 8, 7, 6], [0, 0, 0, 0]), [9, 8, 7, 6]);
        assert_eq!(pack([1.0, 0.0, 0.5, 1.0], true), [128, 0, 255, 255]);
    }
}
