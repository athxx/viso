//! The BC4 alpha block of BC3: two 8-bit endpoints and a 3-bit index per
//! texel, little-endian from texel 0. Endpoints in descending order spread
//! eight levels from the first to the second; otherwise six, then 0 and 255.
//! The mixes round to nearest: `((7 - k)·a0 + k·a1 + 3) / 7` and
//! `((5 - k)·a0 + k·a1 + 2) / 5` for the `k`th level past the first
//! endpoint.

use super::{Effort, PENALTY};

/// The levels of a block with endpoints `a0` and `a1`, by index.
fn levels(a0: u8, a1: u8) -> [u8; 8] {
    let (a, b) = (u32::from(a0), u32::from(a1));
    let mut out = [a0, a1, 0, 0, 0, 0, 0, 255];
    if a0 > a1 {
        for k in 1..7u32 {
            out[k as usize + 1] = (((7 - k) * a + k * b + 3) / 7) as u8;
        }
    } else {
        for k in 1..5u32 {
            out[k as usize + 1] = (((5 - k) * a + k * b + 2) / 5) as u8;
        }
    }
    out
}

/// An alpha block decoded, by texel.
pub(super) fn decode(block: &[u8; 8]) -> [u8; 16] {
    let levels = levels(block[0], block[1]);
    let mut bits = [0u8; 8];
    bits[..6].copy_from_slice(&block[2..]);
    let bits = u64::from_le_bytes(bits);
    std::array::from_fn(|i| levels[(bits >> (3 * i) & 7) as usize])
}

/// One block's alpha, by texel, as an alpha block.
pub(super) fn encode(alpha: &[u8; 16], effort: Effort) -> [u8; 8] {
    let (lo, hi) = (
        *alpha.iter().min().expect("16"),
        *alpha.iter().max().expect("16"),
    );
    let inner = alpha.iter().filter(|&&a| a != 0 && a != 255);
    let (inner_lo, inner_hi) = (inner.clone().min().copied(), inner.max().copied());
    let (inner_lo, inner_hi) = (inner_lo.unwrap_or(lo), inner_hi.unwrap_or(hi));
    let mut best = pack(inner_lo, inner_hi, alpha);
    let mut consider = |a0: u8, a1: u8| {
        let candidate = pack(a0, a1, alpha);
        if candidate.1 < best.1 {
            best = candidate;
        }
    };
    if hi > lo {
        consider(hi, lo);
    }
    if effort == Effort::Thorough {
        let window = |v: u8| v.saturating_sub(4)..=v.saturating_add(4);
        for a0 in window(hi) {
            for a1 in window(lo).filter(|&a1| a1 < a0) {
                consider(a0, a1);
            }
        }
        for a0 in window(inner_lo) {
            for a1 in window(inner_hi).filter(|&a1| a1 >= a0) {
                consider(a0, a1);
            }
        }
    }
    best.0
}

/// The block with endpoints `a0` and `a1` closest to `alpha`, and its
/// squared error.
fn pack(a0: u8, a1: u8, alpha: &[u8; 16]) -> ([u8; 8], u32) {
    let levels = levels(a0, a1);
    let mut bits = 0u64;
    let mut error = 0;
    for (i, &a) in alpha.iter().enumerate() {
        let (k, e) = levels
            .iter()
            .map(|&l| (i32::from(l) - i32::from(a)).pow(2) as u32)
            .enumerate()
            .min_by_key(|&(_, e)| e)
            .expect("8 levels");
        error += if a == 0 && e > 0 { PENALTY } else { e };
        bits |= (k as u64) << (3 * i);
    }
    let mut out = [a0, a1, 0, 0, 0, 0, 0, 0];
    out[2..].copy_from_slice(&bits.to_le_bytes()[..6]);
    (out, error)
}
