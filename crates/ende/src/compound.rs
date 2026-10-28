//! [`Encode`] and [`Decode`] for the shapes a message is built of, each
//! written as the hand-written codecs write it:
//!
//! - a string is [`write_str`](Encoder::write_str)'s, a list its count then
//!   its items (a list of bytes one run), an array its items alone;
//! - an `Option` a byte, `0` for `None` and `1` before the value;
//! - a tuple its fields in turn;
//! - a 128-bit integer its high then its low `u64`.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;

use crate::{Decode, DecodeError, Decoder, Encode, Encoder};

impl Encode for String {
    #[inline]
    fn encode(&self, enc: &mut Encoder) {
        enc.write_str(self);
    }
}

impl Decode for String {
    #[inline]
    fn decode(dec: &mut Decoder<'_>) -> Result<Self, DecodeError> {
        dec.read_str().map(String::from)
    }
}

impl Encode for Box<str> {
    #[inline]
    fn encode(&self, enc: &mut Encoder) {
        enc.write_str(self);
    }
}

impl Decode for Box<str> {
    #[inline]
    fn decode(dec: &mut Decoder<'_>) -> Result<Self, DecodeError> {
        dec.read_str().map(Box::from)
    }
}

impl<T: Encode> Encode for Vec<T> {
    #[inline]
    fn encode(&self, enc: &mut Encoder) {
        self.as_slice().encode(enc);
    }
}

/// A list of any count the input holds: each item takes a byte at least.
impl<T: Decode> Decode for Vec<T> {
    #[inline]
    fn decode(dec: &mut Decoder<'_>) -> Result<Self, DecodeError> {
        dec.read_vec(usize::MAX)
    }
}

impl<T: Encode> Encode for Box<[T]> {
    #[inline]
    fn encode(&self, enc: &mut Encoder) {
        (**self).encode(enc);
    }
}

impl<T: Decode> Decode for Box<[T]> {
    #[inline]
    fn decode(dec: &mut Decoder<'_>) -> Result<Self, DecodeError> {
        Vec::decode(dec).map(Vec::into_boxed_slice)
    }
}

impl<T: Encode, const N: usize> Encode for [T; N] {
    #[inline]
    fn encode(&self, enc: &mut Encoder) {
        T::encode_slice(self, enc);
    }
}

impl<T: Decode, const N: usize> Decode for [T; N] {
    fn decode(dec: &mut Decoder<'_>) -> Result<Self, DecodeError> {
        let items = T::decode_many(dec, N)?;
        match items.try_into() {
            Ok(array) => Ok(array),
            Err(_) => unreachable!("`decode_many` reads exactly the count it is given"),
        }
    }
}

impl<T: Encode> Encode for Box<T> {
    #[inline]
    fn encode(&self, enc: &mut Encoder) {
        (**self).encode(enc);
    }
}

impl<T: Decode> Decode for Box<T> {
    #[inline]
    fn decode(dec: &mut Decoder<'_>) -> Result<Self, DecodeError> {
        T::decode(dec).map(Box::new)
    }
}

impl<T: Encode> Encode for Option<T> {
    #[inline]
    fn encode(&self, enc: &mut Encoder) {
        match self {
            Some(value) => {
                enc.write_u8(1);
                value.encode(enc);
            }
            None => enc.write_u8(0),
        }
    }
}

impl<T: Decode> Decode for Option<T> {
    #[inline]
    fn decode(dec: &mut Decoder<'_>) -> Result<Self, DecodeError> {
        let offset = dec.position();
        match dec.read_u8()? {
            0 => Ok(None),
            1 => T::decode(dec).map(Some),
            _ => Err(DecodeError::Malformed { offset }),
        }
    }
}

impl Encode for u128 {
    #[inline]
    fn encode(&self, enc: &mut Encoder) {
        enc.write_u64((self >> 64) as u64);
        enc.write_u64(*self as u64);
    }
}

impl Decode for u128 {
    #[inline]
    fn decode(dec: &mut Decoder<'_>) -> Result<Self, DecodeError> {
        let hi = dec.read_u64()?;
        let lo = dec.read_u64()?;
        Ok((u128::from(hi) << 64) | u128::from(lo))
    }
}

impl Encode for i128 {
    #[inline]
    fn encode(&self, enc: &mut Encoder) {
        (*self as u128).encode(enc);
    }
}

impl Decode for i128 {
    #[inline]
    fn decode(dec: &mut Decoder<'_>) -> Result<Self, DecodeError> {
        u128::decode(dec).map(|v| v as i128)
    }
}

impl Encode for () {
    #[inline]
    fn encode(&self, _: &mut Encoder) {}
}

impl Decode for () {
    #[inline]
    fn decode(_: &mut Decoder<'_>) -> Result<Self, DecodeError> {
        Ok(())
    }
}

macro_rules! tuple {
    ($($name:ident $index:tt),+) => {
        impl<$($name: Encode),+> Encode for ($($name,)+) {
            #[inline]
            fn encode(&self, enc: &mut Encoder) {
                $(self.$index.encode(enc);)+
            }
        }

        impl<$($name: Decode),+> Decode for ($($name,)+) {
            #[inline]
            fn decode(dec: &mut Decoder<'_>) -> Result<Self, DecodeError> {
                Ok(($($name::decode(dec)?,)+))
            }
        }
    };
}

tuple!(A 0);
tuple!(A 0, B 1);
tuple!(A 0, B 1, C 2);
tuple!(A 0, B 1, C 2, D 3);

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn round_trip<T: Encode + Decode + PartialEq + core::fmt::Debug>(value: T) -> Vec<u8> {
        let bytes = value.encode_to_vec();
        assert_eq!(T::decode_from_slice(&bytes), Ok(value));
        bytes
    }

    #[test]
    fn each_shape_writes_as_the_hand_written_codecs_do() {
        let mut enc = Encoder::new();
        enc.write_bytes(&[1, 2, 3]);
        assert_eq!(
            round_trip(vec![1u8, 2, 3]),
            enc.into_bytes(),
            "bytes are one run"
        );
        let mut enc = Encoder::new();
        enc.write_varint(2);
        enc.write_str("a");
        enc.write_str("bc");
        assert_eq!(
            round_trip(vec![String::from("a"), String::from("bc")]),
            enc.into_bytes()
        );
        assert_eq!(round_trip(Some(7u16)), [1, 7, 0]);
        assert_eq!(round_trip(None::<u16>), [0]);
        assert_eq!(round_trip([4u8, 5]), [4, 5], "an array has no count");
        let mut enc = Encoder::new();
        enc.write_u64(1);
        enc.write_u64(2);
        assert_eq!(round_trip((1u128 << 64) | 2), enc.into_bytes());
        assert_eq!(round_trip((1u8, String::from("x"))), [1, 1, b'x']);
        round_trip(Box::<[u32]>::from([9, 8]));
    }

    #[test]
    fn an_option_tag_past_one_is_malformed() {
        assert_eq!(
            Option::<u8>::decode_from_slice(&[2, 0]),
            Err(DecodeError::Malformed { offset: 0 })
        );
    }

    #[test]
    fn a_list_count_past_the_input_fails_before_reading() {
        let mut enc = Encoder::new();
        enc.write_varint(1 << 40);
        assert!(matches!(
            Vec::<u32>::decode_from_slice(&enc.into_bytes()),
            Err(DecodeError::UnexpectedEof { .. })
        ));
    }
}
