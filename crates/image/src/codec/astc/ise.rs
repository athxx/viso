//! Integer sequence encoding: a run of values of one range packed into a bit
//! stream, trits five to 8 bits and quints three to 7 bits, each block's
//! packed digits interleaved between its values' low bits.
//!
//! A stream is a `u128` read from bit 0; bits past its length read as zero,
//! so a final block cut short decodes as the spec requires.

use super::tables::{Base, QUINT_PACK, QUINTS, RANGES, TRIT_PACK, TRITS, sequence_bits};

/// The `n` bits of `stream` from bit `pos`, zero past bit 127.
#[inline]
fn read(stream: u128, pos: u32, n: u32) -> u32 {
    if pos >= 128 {
        0
    } else {
        ((stream >> pos) as u32) & ((1 << n) - 1)
    }
}

/// The low `len` bits of `stream`.
#[inline]
pub(super) fn truncate(stream: u128, len: u32) -> u128 {
    if len >= 128 {
        stream
    } else {
        stream & ((1u128 << len) - 1)
    }
}

/// The digit bits each value of a trit block is followed by, and where in
/// the packed byte they land.
const TRIT_SPLIT: [(u32, u32); 5] = [(2, 0), (2, 2), (1, 4), (2, 5), (1, 7)];

/// The same for a quint block's packed 7 bits.
const QUINT_SPLIT: [(u32, u32); 3] = [(3, 0), (2, 3), (2, 5)];

/// `out.len()` values of range `range` from `stream`, which holds exactly
/// [`sequence_bits`] of them (the caller truncates it).
pub(super) fn decode(stream: u128, range: usize, out: &mut [u8]) {
    let (base, bits) = RANGES[range];
    let mut pos = 0;
    match base {
        Base::Bits => {
            for v in out.iter_mut() {
                *v = read(stream, pos, bits) as u8;
                pos += bits;
            }
        }
        Base::Trit => {
            for chunk in out.chunks_mut(5) {
                let mut low = [0u32; 5];
                let mut packed = 0;
                for (k, &(n, at)) in TRIT_SPLIT.iter().enumerate() {
                    low[k] = read(stream, pos, bits);
                    pos += bits;
                    packed |= read(stream, pos, n) << at;
                    pos += n;
                }
                let trits = TRITS[packed as usize];
                for (k, v) in chunk.iter_mut().enumerate() {
                    *v = ((u32::from(trits[k]) << bits) | low[k]) as u8;
                }
            }
        }
        Base::Quint => {
            for chunk in out.chunks_mut(3) {
                let mut low = [0u32; 3];
                let mut packed = 0;
                for (k, &(n, at)) in QUINT_SPLIT.iter().enumerate() {
                    low[k] = read(stream, pos, bits);
                    pos += bits;
                    packed |= read(stream, pos, n) << at;
                    pos += n;
                }
                let quints = QUINTS[packed as usize];
                for (k, v) in chunk.iter_mut().enumerate() {
                    *v = ((u32::from(quints[k]) << bits) | low[k]) as u8;
                }
            }
        }
    }
}

/// `values`, each below the levels of range `range`, packed into a stream
/// [`sequence_bits`] long.
pub(super) fn encode(values: &[u8], range: usize) -> u128 {
    let (base, bits) = RANGES[range];
    let mut stream = 0u128;
    let mut pos = 0;
    let mut put = |v: u32, n: u32| {
        if pos < 128 && n > 0 {
            stream |= u128::from(v) << pos;
        }
        pos += n;
    };
    let low = |v: u8| u32::from(v) & ((1 << bits) - 1);
    match base {
        Base::Bits => values.iter().for_each(|&v| put(u32::from(v), bits)),
        Base::Trit => {
            for chunk in values.chunks(5) {
                let digit = |k: usize| chunk.get(k).map_or(0, |&v| usize::from(v) >> bits);
                let index = digit(0) + 3 * digit(1) + 9 * digit(2) + 27 * digit(3) + 81 * digit(4);
                let packed = u32::from(TRIT_PACK[index]);
                for (k, &(n, at)) in TRIT_SPLIT.iter().enumerate() {
                    put(chunk.get(k).map_or(0, |&v| low(v)), bits);
                    put((packed >> at) & ((1 << n) - 1), n);
                }
            }
        }
        Base::Quint => {
            for chunk in values.chunks(3) {
                let digit = |k: usize| chunk.get(k).map_or(0, |&v| usize::from(v) >> bits);
                let index = digit(0) + 5 * digit(1) + 25 * digit(2);
                let packed = u32::from(QUINT_PACK[index]);
                for (k, &(n, at)) in QUINT_SPLIT.iter().enumerate() {
                    put(chunk.get(k).map_or(0, |&v| low(v)), bits);
                    put((packed >> at) & ((1 << n) - 1), n);
                }
            }
        }
    }
    truncate(stream, sequence_bits(range, values.len() as u32))
}
