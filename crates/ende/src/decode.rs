//! The bounded binary decoder and the [`Decode`] trait.
//!
//! This is the read mirror of [`encode`](crate::encode): the same little-endian
//! fixed-width scalars and LEB128 varint lengths. Its defining property is that
//! it is **bounded** — every `read_*` checks the remaining length before
//! touching a byte and returns a [`DecodeError`] on shortfall, so decoding
//! arbitrary (including adversarial) input can never panic and never read past
//! the input slice. The fuzz-smoke test asserts exactly that over random bytes.

use core::str::Utf8Error;

/// Why decoding failed. Carries no owned data (no `String`) so it stays cheap
/// and heap-free; the numeric context is enough to locate the fault.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DecodeError {
    /// Fewer bytes remained than the read required.
    ///
    /// `needed` bytes were requested at `offset`, but only `available` remained.
    UnexpectedEof {
        /// Cursor position where the read began.
        offset: usize,
        /// Bytes the read needed.
        needed: usize,
        /// Bytes actually left in the input.
        available: usize,
    },
    /// A LEB128 varint ran longer than the ten bytes a `u64` can occupy, or its
    /// final byte set bits above the 64-bit range — a malformed/overlong varint.
    OverlongVarint {
        /// Cursor position where the varint began.
        offset: usize,
    },
    /// A length-prefixed string was not valid UTF-8.
    InvalidUtf8 {
        /// Cursor position where the string bytes began.
        offset: usize,
    },
    /// The whole input was expected to be consumed but bytes remained after the
    /// top-level value. Raised only by [`Decoder::finish`].
    TrailingBytes {
        /// Number of unconsumed bytes.
        remaining: usize,
    },
    /// The bytes were present and well-formed as raw scalars, but they did not
    /// spell a valid value for the type being decoded — an out-of-range enum
    /// discriminant, an incompatible protocol version, or a similar semantic
    /// violation. Distinct from [`UnexpectedEof`](Self::UnexpectedEof) (which
    /// means the input ran short) so a caller can tell a truncated stream from a
    /// corrupt one.
    Malformed {
        /// Cursor position where the invalid value began.
        offset: usize,
    },
}

impl core::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            DecodeError::UnexpectedEof {
                offset,
                needed,
                available,
            } => write!(
                f,
                "unexpected end of input: needed {needed} byte(s) at offset {offset}, {available} available"
            ),
            DecodeError::OverlongVarint { offset } => {
                write!(f, "overlong varint at offset {offset}")
            }
            DecodeError::InvalidUtf8 { offset } => {
                write!(f, "invalid UTF-8 in string at offset {offset}")
            }
            DecodeError::TrailingBytes { remaining } => {
                write!(f, "{remaining} trailing byte(s) after value")
            }
            DecodeError::Malformed { offset } => {
                write!(f, "malformed value at offset {offset}")
            }
        }
    }
}

impl std::error::Error for DecodeError {}

impl From<DecodeError> for std::io::Error {
    fn from(err: DecodeError) -> Self {
        std::io::Error::new(std::io::ErrorKind::InvalidData, err)
    }
}

/// A bounded cursor over a borrowed byte slice.
///
/// Reads advance an internal offset; every read validates that enough bytes
/// remain first, so a `Decoder` over untrusted input is safe — no read panics
/// and none reads past the slice. Construct with [`Decoder::new`], then call the
/// `read_*` methods in the same order the [`Encoder`](crate::encode::Encoder)
/// wrote them.
#[derive(Debug, Clone)]
pub struct Decoder<'a> {
    data: &'a [u8],
    offset: usize,
}

impl<'a> Decoder<'a> {
    /// A decoder positioned at the start of `data`.
    #[inline]
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, offset: 0 }
    }

    /// The current cursor position (bytes consumed so far).
    #[inline]
    pub fn position(&self) -> usize {
        self.offset
    }

    /// The number of unconsumed bytes.
    #[inline]
    pub fn remaining(&self) -> usize {
        self.data.len() - self.offset
    }

    /// Whether the cursor has reached the end of the input.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.offset >= self.data.len()
    }

    /// Succeeds only if the input is fully consumed, else reports the leftover.
    ///
    /// Call after decoding a top-level value to reject trailing garbage.
    #[inline]
    pub fn finish(&self) -> Result<(), DecodeError> {
        let remaining = self.remaining();
        if remaining == 0 {
            Ok(())
        } else {
            Err(DecodeError::TrailingBytes { remaining })
        }
    }

    /// Reads exactly `len` bytes, advancing the cursor, or fails if fewer remain.
    ///
    /// This is the single bounds gate every fixed-width read funnels through: it
    /// is the reason no read can over-read the slice.
    #[inline]
    pub fn read_raw(&mut self, len: usize) -> Result<&'a [u8], DecodeError> {
        let available = self.remaining();
        if len > available {
            return Err(DecodeError::UnexpectedEof {
                offset: self.offset,
                needed: len,
                available,
            });
        }
        let start = self.offset;
        self.offset += len;
        Ok(&self.data[start..start + len])
    }

    /// Reads a `bool`: `0` is false, any nonzero byte is true.
    #[inline]
    pub fn read_bool(&mut self) -> Result<bool, DecodeError> {
        Ok(self.read_u8()? != 0)
    }

    /// Reads a raw byte.
    #[inline]
    pub fn read_u8(&mut self) -> Result<u8, DecodeError> {
        Ok(self.read_raw(1)?[0])
    }

    /// Reads a signed byte.
    #[inline]
    pub fn read_i8(&mut self) -> Result<i8, DecodeError> {
        Ok(self.read_u8()? as i8)
    }

    /// Reads a length-prefixed byte string: a varint length then that many bytes.
    #[inline]
    pub fn read_bytes(&mut self) -> Result<&'a [u8], DecodeError> {
        let len = self.read_varint()?;
        // `len` is a u64 off the wire; the read_raw bound rejects any value
        // larger than what remains, so an absurd length fails cleanly as EOF
        // rather than allocating or panicking.
        self.read_raw(usize_from_u64(len, self.offset, self.remaining())?)
    }

    /// Reads a length-prefixed UTF-8 string, validating the bytes.
    #[inline]
    pub fn read_str(&mut self) -> Result<&'a str, DecodeError> {
        let start = self.offset;
        let bytes = self.read_bytes()?;
        core::str::from_utf8(bytes)
            .map_err(|_: Utf8Error| DecodeError::InvalidUtf8 { offset: start })
    }

    /// Reads an unsigned LEB128 varint, rejecting an overlong encoding.
    pub fn read_varint(&mut self) -> Result<u64, DecodeError> {
        let start = self.offset;
        let mut result: u64 = 0;
        let mut shift: u32 = 0;
        loop {
            let byte = self.read_u8()?;
            // Ten 7-bit groups cover 70 bits; the tenth may only carry the top
            // bit of the u64. Anything beyond that is overlong/out of range.
            if shift >= 64 || (shift == 63 && byte > 0x01) {
                return Err(DecodeError::OverlongVarint { offset: start });
            }
            result |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Ok(result);
            }
            shift += 7;
        }
    }

    /// Reads a signed LEB128 varint (zig-zag decoded).
    #[inline]
    pub fn read_varint_signed(&mut self) -> Result<i64, DecodeError> {
        let raw = self.read_varint()?;
        Ok(((raw >> 1) as i64) ^ -((raw & 1) as i64))
    }
}

/// Narrows a wire `u64` length to `usize`, failing as EOF when it cannot
/// possibly fit the remaining input (also covers 32-bit targets where a large
/// `u64` overflows `usize`). Keeps the "never panics" contract for hostile
/// lengths without a separate error variant.
#[inline]
fn usize_from_u64(value: u64, offset: usize, available: usize) -> Result<usize, DecodeError> {
    if value > available as u64 {
        return Err(DecodeError::UnexpectedEof {
            offset,
            // Report the request saturated to usize; the point is that it
            // exceeds what remains, which the numbers already show.
            needed: value.min(usize::MAX as u64) as usize,
            available,
        });
    }
    Ok(value as usize)
}

/// Fixed-width little-endian scalar reads, the mirror of the encoder's
/// `write_le!` block. The array length comes from the type so the bound is
/// exact.
macro_rules! read_le {
    ($($name:ident => $ty:ty),+ $(,)?) => {$(
        impl Decoder<'_> {
            #[doc = concat!("Reads a little-endian `", stringify!($ty), "`.")]
            #[inline]
            pub fn $name(&mut self) -> Result<$ty, DecodeError> {
                const WIDTH: usize = core::mem::size_of::<$ty>();
                let bytes = self.read_raw(WIDTH)?;
                let mut arr = [0u8; WIDTH];
                arr.copy_from_slice(bytes);
                Ok(<$ty>::from_le_bytes(arr))
            }
        }
    )+};
}

read_le! {
    read_u16 => u16,
    read_u32 => u32,
    read_u64 => u64,
    read_i16 => i16,
    read_i32 => i32,
    read_i64 => i64,
    read_f32 => f32,
    read_f64 => f64,
}

/// A value that can deserialize itself from a [`Decoder`]. The mirror of
/// [`Encode`](crate::encode::Encode).
pub trait Decode: Sized {
    /// Reads one `Self` from `dec`, advancing its cursor.
    fn decode(dec: &mut Decoder<'_>) -> Result<Self, DecodeError>;

    /// Decodes one `Self` from a full slice and requires the input to be fully
    /// consumed, rejecting trailing bytes.
    #[inline]
    fn decode_from_slice(data: &[u8]) -> Result<Self, DecodeError> {
        let mut dec = Decoder::new(data);
        let value = Self::decode(&mut dec)?;
        dec.finish()?;
        Ok(value)
    }
}

/// Scalar `Decode` impls, matching the `Decoder::read_*` methods one-to-one.
macro_rules! decode_scalar {
    ($($ty:ty => $method:ident),+ $(,)?) => {$(
        impl Decode for $ty {
            #[inline]
            fn decode(dec: &mut Decoder<'_>) -> Result<Self, DecodeError> {
                dec.$method()
            }
        }
    )+};
}

decode_scalar! {
    bool => read_bool,
    u8 => read_u8,
    u16 => read_u16,
    u32 => read_u32,
    u64 => read_u64,
    i8 => read_i8,
    i16 => read_i16,
    i32 => read_i32,
    i64 => read_i64,
    f32 => read_f32,
    f64 => read_f64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encode::{Encode, Encoder};

    // Encode a value, decode it back requiring full consumption, and assert the
    // round trip is the identity. The whole point of the mirrored codec.
    fn round_trip<T: Encode + Decode + PartialEq + core::fmt::Debug>(value: T) {
        let bytes = value.encode_to_vec();
        let decoded = T::decode_from_slice(&bytes).expect("decode of own encoding");
        assert_eq!(value, decoded);
    }

    #[test]
    fn scalars_round_trip() {
        round_trip(true);
        round_trip(false);
        round_trip(0u8);
        round_trip(u8::MAX);
        round_trip(12345u16);
        round_trip(0xdead_beefu32);
        round_trip(0x0123_4567_89ab_cdefu64);
        round_trip(-1i8);
        round_trip(i16::MIN);
        round_trip(i32::MIN);
        round_trip(i64::MIN);
        round_trip(core::f32::consts::PI);
        round_trip(-core::f64::consts::E);
    }

    #[test]
    fn str_and_bytes_round_trip() {
        for s in ["", "hello", "unicode → ✓ 世界", "\0\n\t"] {
            let bytes = {
                let mut enc = Encoder::new();
                enc.write_str(s);
                enc.into_bytes()
            };
            let mut dec = Decoder::new(&bytes);
            assert_eq!(dec.read_str().unwrap(), s);
            dec.finish().unwrap();
        }

        let payload: &[u8] = &[0, 1, 2, 250, 255];
        let mut enc = Encoder::new();
        enc.write_bytes(payload);
        let bytes = enc.into_bytes();
        let mut dec = Decoder::new(&bytes);
        assert_eq!(dec.read_bytes().unwrap(), payload);
        dec.finish().unwrap();
    }

    #[test]
    fn varint_round_trips_across_widths() {
        for v in [0u64, 1, 127, 128, 300, 16_384, u32::MAX as u64, u64::MAX] {
            let mut enc = Encoder::new();
            enc.write_varint(v);
            let mut dec = Decoder::new(enc.as_bytes());
            assert_eq!(dec.read_varint().unwrap(), v);
            assert_eq!(dec.remaining(), 0);
        }
        for v in [0i64, -1, 1, i64::MIN, i64::MAX, -300, 300] {
            let mut enc = Encoder::new();
            enc.write_varint_signed(v);
            let mut dec = Decoder::new(enc.as_bytes());
            assert_eq!(dec.read_varint_signed().unwrap(), v);
        }
    }

    #[test]
    fn short_input_reports_eof_not_panic() {
        // A u32 read against 3 bytes must fail cleanly, not read past the slice.
        let mut dec = Decoder::new(&[1, 2, 3]);
        assert!(matches!(
            dec.read_u32(),
            Err(DecodeError::UnexpectedEof {
                needed: 4,
                available: 3,
                ..
            })
        ));
        // A varint that claims a huge length but has no bytes fails as EOF.
        let mut enc = Encoder::new();
        enc.write_varint(1_000_000);
        let bytes = enc.into_bytes();
        let mut dec = Decoder::new(&bytes);
        assert!(matches!(
            dec.read_bytes(),
            Err(DecodeError::UnexpectedEof { .. })
        ));
    }

    #[test]
    fn overlong_varint_is_rejected() {
        // Eleven continuation bytes cannot be a valid u64 varint.
        let overlong = [0x80u8; 11];
        let mut dec = Decoder::new(&overlong);
        assert!(matches!(
            dec.read_varint(),
            Err(DecodeError::OverlongVarint { .. })
        ));
    }

    #[test]
    fn invalid_utf8_string_is_rejected() {
        let mut enc = Encoder::new();
        enc.write_bytes(&[0xff, 0xfe]); // not valid UTF-8
        let bytes = enc.into_bytes();
        let mut dec = Decoder::new(&bytes);
        assert!(matches!(
            dec.read_str(),
            Err(DecodeError::InvalidUtf8 { .. })
        ));
    }

    #[test]
    fn trailing_bytes_are_rejected_by_finish() {
        let mut enc = Encoder::new();
        enc.write_u8(7);
        enc.write_u8(9); // one byte too many for a single u8 value
        let bytes = enc.into_bytes();
        assert!(matches!(
            u8::decode_from_slice(&bytes),
            Err(DecodeError::TrailingBytes { remaining: 1 })
        ));
    }

    // Fuzz smoke: a deterministic pseudo-random byte stream fed to every reader
    // must never panic and never read past the slice (per the bounded-decoder
    // contract). We drive a mix of readers over many random inputs; success is
    // simply "did not panic and did not exceed the length".
    #[test]
    fn random_bytes_never_panic_or_over_read() {
        // A small xorshift PRNG keeps the test dependency-free and reproducible.
        let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };

        for _ in 0..20_000 {
            let len = (next() % 64) as usize;
            let mut data = Vec::with_capacity(len);
            for _ in 0..len {
                data.push((next() & 0xff) as u8);
            }
            let mut dec = Decoder::new(&data);
            // Interleave every reader kind; each returns Result and advances at
            // most `remaining()`, so the loop can only end at or before EOF.
            while !dec.is_empty() {
                let before = dec.position();
                let op = next() % 8;
                let ok = match op {
                    0 => dec.read_u8().is_ok(),
                    1 => dec.read_u32().is_ok(),
                    2 => dec.read_u64().is_ok(),
                    3 => dec.read_f64().is_ok(),
                    4 => dec.read_varint().is_ok(),
                    5 => dec.read_varint_signed().is_ok(),
                    6 => dec.read_bytes().is_ok(),
                    _ => dec.read_str().is_ok(),
                };
                // The cursor must never move backward and never past the end.
                assert!(dec.position() >= before);
                assert!(dec.position() <= data.len());
                if !ok {
                    // A failed read stops progress; break to avoid spinning on
                    // the same short/invalid tail forever.
                    break;
                }
            }
        }
    }
}
