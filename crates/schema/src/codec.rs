//! Codec helpers shared by the schema's wire forms.

use alloc::vec::Vec;

use viso_ende::{DecodeError, Decoder, Encoder};

pub(crate) fn write_list<T>(
    enc: &mut Encoder,
    items: &[T],
    mut write: impl FnMut(&mut Encoder, &T),
) {
    enc.write_varint(items.len() as u64);
    for item in items {
        write(enc, item);
    }
}

pub(crate) fn read_list<'a, T>(
    dec: &mut Decoder<'a>,
    read: impl FnMut(&mut Decoder<'a>) -> Result<T, DecodeError>,
) -> Result<Vec<T>, DecodeError> {
    dec.read_list(usize::MAX, read)
}

/// A malformed-input error at the decoder's position.
pub(crate) fn malformed(dec: &Decoder<'_>) -> DecodeError {
    DecodeError::Malformed {
        offset: dec.position(),
    }
}

/// Reads a varint that must fit a `u32`.
pub(crate) fn read_u32_varint(dec: &mut Decoder<'_>) -> Result<u32, DecodeError> {
    let offset = dec.position();
    u32::try_from(dec.read_varint()?).map_err(|_| DecodeError::Malformed { offset })
}
