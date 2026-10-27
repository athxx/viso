//! State identity and values.

use viso_ende::{Decode, DecodeError, Decoder, Encode, Encoder};

/// The version of the algorithm that mints symbol identities: the
/// [`StateKey`]s of states and the stable identities of the records and enums
/// a stored value's type reaches. Artifacts that outlive a build record it; a
/// change to the algorithm or its constants bumps it, and identities of two
/// versions are never compared.
pub const FINGERPRINT_VERSION: u32 = 1;

/// A durable, position-independent identity for a state cell, used to match a
/// cell across a hot-reload recompile.
///
/// It is `#[repr(C)]` with the same `{ hi, lo }` layout the compiler's stable
/// symbol identity uses, so the hot-reload engine can hand its symbol identity
/// in without the UI crate depending on the compiler. The store only ever
/// compares and hashes it; it never interprets the bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(C)]
pub struct StateKey {
    /// High 64 bits of the identity.
    pub hi: u64,
    /// Low 64 bits of the identity.
    pub lo: u64,
}

impl StateKey {
    /// Construct a key from its two halves.
    #[inline]
    pub fn from_parts(hi: u64, lo: u64) -> Self {
        Self { hi, lo }
    }
}

/// A scalar state value. Type-erased for the store; the variants cover
/// the scalars the reactive link needs to carry (numbers, a flag, a color).
///
/// A color is kept as four straight `f32` channels — the same shape the paint
/// tier already uses — so a bound color writes through without a pack/unpack.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum StateValue {
    /// A signed integer (counters, indices).
    Int(i32),
    /// A floating-point number (progress, positions).
    Float(f32),
    /// A boolean flag (toggles, visibility).
    Bool(bool),
    /// A straight-alpha RGBA color, channels in `0.0..=1.0`.
    Color(f32, f32, f32, f32),
}

/// A tag byte, then the payload.
impl Encode for StateValue {
    fn encode(&self, enc: &mut Encoder) {
        match *self {
            StateValue::Int(n) => {
                enc.write_u8(0);
                enc.write_i32(n);
            }
            StateValue::Float(x) => {
                enc.write_u8(1);
                enc.write_f32(x);
            }
            StateValue::Bool(b) => {
                enc.write_u8(2);
                enc.write_bool(b);
            }
            StateValue::Color(r, g, b, a) => {
                enc.write_u8(3);
                for channel in [r, g, b, a] {
                    enc.write_f32(channel);
                }
            }
        }
    }
}

impl Decode for StateValue {
    fn decode(dec: &mut Decoder<'_>) -> Result<Self, DecodeError> {
        let offset = dec.position();
        Ok(match dec.read_u8()? {
            0 => StateValue::Int(dec.read_i32()?),
            1 => StateValue::Float(dec.read_f32()?),
            2 => StateValue::Bool(dec.read_bool()?),
            3 => StateValue::Color(
                dec.read_f32()?,
                dec.read_f32()?,
                dec.read_f32()?,
                dec.read_f32()?,
            ),
            _ => return Err(DecodeError::Malformed { offset }),
        })
    }
}
