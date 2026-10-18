//! ASTC LDR: one 128-bit block for any footprint from 4×4 to 12×12 texels,
//! the format mobile GPUs sample.
//!
//! - `decode`: the decoder, exact to the Khronos Data Format Specification
//!   for 2D LDR blocks; illegal blocks and HDR content decode to the error
//!   color, opaque magenta.
//! - `encode`: the encoder, a search over partitionings, endpoint modes,
//!   weight planes and block modes, each candidate judged by decoding it.
//! - `ise` and `tables`: the integer sequence encoding and the quantization
//!   tables both sides share.
//!
//! Texels are premultiplied RGBA8 throughout. The decoder interpolates in
//! UNORM16 as the hardware does, then rounds each channel `C` to the 8-bit
//! value nearest `C / 65535`, the float a GPU samples the texel as.

mod decode;
mod encode;
mod ise;
mod tables;
#[cfg(test)]
mod tests;

use super::Effort;

/// One block of premultiplied texels, `footprint.0 × footprint.1` row by row,
/// as an ASTC LDR block of that footprint.
///
/// # Panics
///
/// When a side of `footprint` is outside `4..=12`, or `texels` does not hold
/// its texels.
pub fn encode_astc(texels: &[[u8; 4]], footprint: (u32, u32), effort: Effort) -> [u8; 16] {
    check(footprint, texels.len());
    encode::encode_block(texels, footprint, effort).to_le_bytes()
}

/// An ASTC LDR block of `footprint` decoded into `out`, row by row.
///
/// # Panics
///
/// When a side of `footprint` is outside `4..=12`, or `out` does not hold its
/// texels.
pub fn decode_astc(block: &[u8; 16], footprint: (u32, u32), out: &mut [[u8; 4]]) {
    check(footprint, out.len());
    let mut wide = [[0u16; 4]; 144];
    let wide = &mut wide[..out.len()];
    if decode::decode_block(u128::from_le_bytes(*block), footprint, wide).is_none() {
        wide.fill(decode::ERROR_COLOR);
    }
    for (o, w) in out.iter_mut().zip(wide.iter()) {
        *o = w.map(decode::unorm8);
    }
}

/// Asserts `footprint` is a 2D ASTC footprint of `texels` texels.
fn check(footprint: (u32, u32), texels: usize) {
    assert!(
        (4..=12).contains(&footprint.0) && (4..=12).contains(&footprint.1),
        "ASTC footprint {footprint:?}"
    );
    assert_eq!(
        texels,
        (footprint.0 * footprint.1) as usize,
        "texels in an ASTC block"
    );
}
