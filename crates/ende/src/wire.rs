//! The canonical wire representation of core typed IDs, plus the protocol tag.
//!
//! Viso's typed identifiers (the `NameId` / `SymbolId` family) are defined in
//! the crates that own their identity semantics, not here. This module owns only
//! the **format** those IDs travel in: a stable one-byte kind tag followed by a
//! fixed-width payload. Defining the format rather than the type keeps `ende` a
//! leaf — it never needs an edge back into the identity-owning crates, and those
//! crates encode through this format instead of inventing their own.
//!
//! A [`WireId`] is that tagged pair. It round-trips through the binary
//! [`encode`](crate::encode) / [`decode`](crate::decode) codec like any other
//! value, so a producer and consumer that disagree on the kind tag fail loudly
//! at decode time rather than silently reinterpreting bytes.

use crate::decode::{Decode, DecodeError, Decoder};
use crate::encode::{Encode, Encoder};

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
        let magic = dec.read_raw(Self::MAGIC.len())?;
        if magic != Self::MAGIC {
            // A wrong magic is not our stream at all; surface it as EOF-shaped
            // corruption at the header offset rather than adding a variant for a
            // case that only occurs on a completely foreign input.
            return Err(DecodeError::UnexpectedEof {
                offset: 0,
                needed: Self::MAGIC.len(),
                available: Self::MAGIC.len(),
            });
        }
        let version = dec.read_u16()?;
        Ok(Self { version })
    }
}

/// The kind of identifier a [`WireId`] carries.
///
/// The tag is the discriminant written on the wire; the identity crates map
/// their concrete ID types onto these kinds. `Custom` leaves room for a caller
/// to carry its own kind space without a format change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum IdKind {
    /// An interned name identifier.
    Name = 0,
    /// A resolved symbol identifier.
    Symbol = 1,
    /// A caller-defined kind, distinguished by the `Custom(u8)` payload.
    Custom(u8) = 255,
}

impl IdKind {
    /// The one-byte tag written on the wire for this kind.
    #[inline]
    fn tag(self) -> u8 {
        match self {
            IdKind::Name => 0,
            IdKind::Symbol => 1,
            IdKind::Custom(_) => 255,
        }
    }
}

/// A typed identifier in wire form: a [`IdKind`] tag and a `u64` value.
///
/// This is the format carried across encode/decode; the identity crates convert
/// their own ID types to and from it at the wire boundary. Keeping the value a
/// plain `u64` (not a `usize`) fixes the width regardless of target pointer size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WireId {
    /// Which identifier space `value` belongs to.
    pub kind: IdKind,
    /// The raw identifier value.
    pub value: u64,
}

impl WireId {
    /// A wire ID of the given kind and value.
    #[inline]
    pub fn new(kind: IdKind, value: u64) -> Self {
        Self { kind, value }
    }
}

impl Encode for WireId {
    #[inline]
    fn encode(&self, enc: &mut Encoder) {
        enc.write_u8(self.kind.tag());
        // A custom kind carries its sub-tag before the value so the reader can
        // reconstruct the exact `IdKind::Custom(n)` it was given.
        if let IdKind::Custom(sub) = self.kind {
            enc.write_u8(sub);
        }
        enc.write_varint(self.value);
    }
}

impl Decode for WireId {
    fn decode(dec: &mut Decoder<'_>) -> Result<Self, DecodeError> {
        let tag = dec.read_u8()?;
        let kind = match tag {
            0 => IdKind::Name,
            1 => IdKind::Symbol,
            _ => IdKind::Custom(dec.read_u8()?),
        };
        let value = dec.read_varint()?;
        Ok(Self { kind, value })
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
        assert!(ProtocolTag::decode_from_slice(&bytes).is_err());
    }

    #[test]
    fn wire_id_round_trips_each_kind() {
        for id in [
            WireId::new(IdKind::Name, 0),
            WireId::new(IdKind::Symbol, 42),
            WireId::new(IdKind::Name, u64::MAX),
            WireId::new(IdKind::Custom(7), 1234),
            WireId::new(IdKind::Custom(255), 9),
        ] {
            let bytes = id.encode_to_vec();
            let decoded = WireId::decode_from_slice(&bytes).unwrap();
            assert_eq!(decoded, id);
        }
    }
}
