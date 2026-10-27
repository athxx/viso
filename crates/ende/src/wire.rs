//! The protocol tag every Viso wire payload starts with, and the stable
//! content hash payloads are told apart by.

use crate::decode::{Decode, DecodeError, Decoder};
use crate::encode::{Encode, Encoder};

/// The stable 64-bit FNV-1a hash of `bytes`, the same whatever the toolchain
/// or target: what tells two payloads apart across processes and builds.
pub const fn content_hash(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut i = 0;
    while i < bytes.len() {
        hash = (hash ^ bytes[i] as u64).wrapping_mul(0x0000_0100_0000_01b3);
        i += 1;
    }
    hash
}

/// The wire format version this build reads and writes.
///
/// Bumped when the byte layout of the format changes incompatibly. A stream may
/// lead with this tag (see [`ProtocolTag`]) so a reader can reject a version it
/// does not understand before decoding anything else.
pub const WIRE_VERSION: u16 = 1;

/// A four-byte stream preamble: a fixed magic plus the [`WIRE_VERSION`].
///
/// Optional — small internal messages need not carry it — but a persisted
/// snapshot or a cross-process stream should lead with one so a mismatched peer
/// or a stale on-disk blob is detected up front instead of mis-decoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProtocolTag {
    /// Format version the stream was written with.
    pub version: u16,
}

impl ProtocolTag {
    /// A two-byte magic identifying a Viso `ende` stream: `b"VE"`.
    pub const MAGIC: [u8; 2] = *b"VE";

    /// A tag for the current [`WIRE_VERSION`].
    #[inline]
    pub fn current() -> Self {
        Self {
            version: WIRE_VERSION,
        }
    }

    /// Whether this tag's version matches what this build understands.
    #[inline]
    pub fn is_compatible(&self) -> bool {
        self.version == WIRE_VERSION
    }
}

impl Encode for ProtocolTag {
    #[inline]
    fn encode(&self, enc: &mut Encoder) {
        enc.write_raw(&Self::MAGIC);
        enc.write_u16(self.version);
    }
}

impl Decode for ProtocolTag {
    fn decode(dec: &mut Decoder<'_>) -> Result<Self, DecodeError> {
        let offset = dec.position();
        if *dec.read_array::<2>()? != Self::MAGIC {
            // Not an `ende` stream at all.
            return Err(DecodeError::Malformed { offset });
        }
        let version = dec.read_u16()?;
        Ok(Self { version })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_tag_round_trips_and_flags_version() {
        let tag = ProtocolTag::current();
        let bytes = tag.encode_to_vec();
        let decoded = ProtocolTag::decode_from_slice(&bytes).unwrap();
        assert_eq!(decoded, tag);
        assert!(decoded.is_compatible());

        let stale = ProtocolTag {
            version: WIRE_VERSION + 1,
        };
        assert!(!stale.is_compatible());
    }

    #[test]
    fn bad_magic_is_rejected() {
        let bytes = [b'X', b'Y', 1, 0];
        assert_eq!(
            ProtocolTag::decode_from_slice(&bytes),
            Err(DecodeError::Malformed { offset: 0 })
        );
    }
}
