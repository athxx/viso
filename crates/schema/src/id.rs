//! Durable declaration identity.

use viso_ende::{DecodeError, Decoder, Encoder};

/// The durable identity of a declaration: the 128-bit fingerprint of its
/// package, module, kind and path, unchanged by edits elsewhere and by
/// source reordering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct StableId {
    /// The high 64 bits.
    pub hi: u64,
    /// The low 64 bits.
    pub lo: u64,
}

/// Writes `id` as its two halves.
pub fn write_stable_id(enc: &mut Encoder, id: StableId) {
    enc.write_u64(id.hi);
    enc.write_u64(id.lo);
}

/// Reads an id [`write_stable_id`] wrote.
pub fn read_stable_id(dec: &mut Decoder<'_>) -> Result<StableId, DecodeError> {
    Ok(StableId {
        hi: dec.read_u64()?,
        lo: dec.read_u64()?,
    })
}
