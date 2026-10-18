//! The run-length code a baked glyph's coverage is stored in.
//!
//! A8 glyph coverage is mostly runs of empty (`0`) and solid (`255`) pixels
//! with anti-aliased edges between them, and a vertical stem repeats the row
//! above it. The code has four tokens, each one byte followed by its payload:
//!
//! ```text
//! 00nnnnnn            n + 1 (1–64) zero pixels
//! 01nnnnnn            n + 1 (1–64) solid pixels
//! 10nnnnnn            n + 1 (1–64) pixels equal to the ones a row above
//! 11nnnnnn  b × n+1   n + 1 (1–64) literal pixels
//! ```
//!
//! The pixels run row after row with no row breaks, so a run carries across
//! the end of a row, and "a row above" is `width` pixels back. Decoding is a
//! fill or a copy per token, with no state across tokens and no allocation;
//! it is lossless.

use alloc::vec::Vec;

const RUN: usize = 64;
const COUNT: u8 = 0x3F;
const TAG: u8 = 0xC0;
const ZERO: u8 = 0x00;
const SOLID: u8 = 0x40;
const ABOVE: u8 = 0x80;
const LITERAL: u8 = 0xC0;

/// A run shorter than this is cheaper inside a literal than as its own token
/// followed by a new literal header.
const MIN_RUN: usize = 3;

/// Append the code of `pixels`, rows of `width`, to `out`.
pub(crate) fn encode(pixels: &[u8], width: usize, out: &mut Vec<u8>) {
    let mut at = 0;
    while at < pixels.len() {
        if let Some((tag, run)) = run_at(pixels, width, at) {
            let len = run.min(RUN);
            out.push(tag | (len - 1) as u8);
            at += len;
            continue;
        }
        let start = at;
        while at < pixels.len() && at - start < RUN && run_at(pixels, width, at).is_none() {
            at += 1;
        }
        out.push(LITERAL | (at - start - 1) as u8);
        out.extend_from_slice(&pixels[start..at]);
    }
}

/// The longest run starting at `at` — of `0`, of `255`, or of pixels equal
/// to the row above — when it is worth a token (at least [`MIN_RUN`] long,
/// or reaching the end). A fill wins a tie: it reads nothing back.
fn run_at(pixels: &[u8], width: usize, at: usize) -> Option<(u8, usize)> {
    let rest = &pixels[at..];
    let fill = match pixels[at] {
        0 => Some(ZERO),
        u8::MAX => Some(SOLID),
        _ => None,
    }
    .map(|tag| (tag, rest.iter().take_while(|&&p| p == pixels[at]).count()));
    let above = (width > 0 && at >= width).then(|| {
        let len = rest
            .iter()
            .zip(&pixels[at - width..])
            .take_while(|(p, q)| p == q)
            .count();
        (ABOVE, len)
    });
    let (tag, len) = match (fill, above) {
        (Some(fill), Some(above)) if above.1 > fill.1 => above,
        (Some(fill), _) => fill,
        (None, above) => above?,
    };
    (len >= MIN_RUN || len == rest.len()).then_some((tag, len))
}

/// Decode `packed` into `out`, rows of `width`, which it must fill exactly.
/// `false` when the code is malformed or decodes to another length.
pub(crate) fn decode(packed: &[u8], width: usize, out: &mut [u8]) -> bool {
    let mut read = 0;
    let mut written = 0;
    while read < packed.len() {
        let token = packed[read];
        read += 1;
        let len = usize::from(token & COUNT) + 1;
        let end = written + len;
        if end > out.len() {
            return false;
        }
        match token & TAG {
            ZERO => out[written..end].fill(0),
            SOLID => out[written..end].fill(u8::MAX),
            ABOVE => {
                if width == 0 || written < width {
                    return false;
                }
                // A run longer than a row reads pixels it writes, so it
                // copies at most a row at a time.
                let mut at = written;
                while at < end {
                    let step = (end - at).min(width);
                    out.copy_within(at - width..at - width + step, at);
                    at += step;
                }
            }
            _ => {
                let Some(source) = packed.get(read..read + len) else {
                    return false;
                };
                out[written..end].copy_from_slice(source);
                read += len;
            }
        }
        written = end;
    }
    written == out.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn round_trip(pixels: &[u8], width: usize) -> Vec<u8> {
        let mut packed = Vec::new();
        encode(pixels, width, &mut packed);
        let mut out = vec![0xAA; pixels.len()];
        assert!(decode(&packed, width, &mut out), "{pixels:?}");
        assert_eq!(out, pixels);
        packed
    }

    #[test]
    fn every_shape_of_input_round_trips() {
        round_trip(&[], 0);
        round_trip(&[0], 1);
        round_trip(&[255], 1);
        round_trip(&[7], 1);
        round_trip(&[0, 0, 255, 255, 0, 9, 0, 0, 0, 255], 5);
        let mut long = vec![0u8; 300];
        long.extend(vec![255u8; 200]);
        long.extend((0..=255).cycle().take(1000));
        long.extend(vec![0u8; 65]);
        for width in [1, 2, 7, 64, 100, long.len()] {
            round_trip(&long, width);
        }
    }

    #[test]
    fn runs_take_one_byte_per_sixty_four_pixels() {
        assert_eq!(round_trip(&[0; 64], 64), [0x3F]);
        assert_eq!(round_trip(&[255; 65], 65), [0x7F, 0x40]);
        assert_eq!(round_trip(&[1, 2, 3], 3), [0xC2, 1, 2, 3]);
        // A pair of zeros inside edge pixels stays in the literal.
        assert_eq!(round_trip(&[5, 0, 0, 6], 4), [0xC3, 5, 0, 0, 6]);
    }

    #[test]
    fn a_stem_repeats_the_row_above() {
        let stem = [0, 40, 255, 255, 200, 0];
        let pixels: Vec<u8> = stem.iter().copied().cycle().take(stem.len() * 4).collect();
        let packed = round_trip(&pixels, stem.len());
        // The first row, then one copy of the three rows under it.
        assert_eq!(packed, [0xC5, 0, 40, 255, 255, 200, 0, 0x80 | 17]);
        // A row of zeros under zeros is a fill, not a copy.
        assert_eq!(round_trip(&[0; 8], 4), [0x07]);
    }

    #[test]
    fn malformed_code_is_refused() {
        let mut out = [0u8; 4];
        assert!(!decode(&[0x3F], 4, &mut out), "runs past the end");
        assert!(!decode(&[0x01], 4, &mut out), "stops short");
        assert!(!decode(&[0xC3, 1, 2], 4, &mut out), "literal truncated");
        assert!(!decode(&[0x83], 2, &mut out), "copies above the first row");
        assert!(!decode(&[0x01, 0x81], 0, &mut out), "copies with no row");
        assert!(decode(&[0x03], 4, &mut out));
        assert!(decode(&[0xC1, 1, 2, 0x81], 2, &mut out));
        assert_eq!(out, [1, 2, 1, 2]);
    }
}
