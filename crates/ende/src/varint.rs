//! Values whose integers are written as LEB128 varints: [`EncodeVarint`]
//! and [`DecodeVarint`], the form a message's counts, ids and positions
//! take, small values in a byte or two. Unsigned integers are written as
//! [`write_varint`](Encoder::write_varint) writes them and signed ones
//! zig-zagged; a tuple, a list, an array or an `Option` of them writes each
//! the same way, in the shape [`Encode`](crate::Encode) gives it.

use alloc::boxed::Box;
use alloc::vec::Vec;

use crate::{DecodeError, Decoder, Encoder};

/// A value written with its integers as varints.
pub trait EncodeVarint {
    /// Writes `self` to `enc`.
    fn encode_varint(&self, enc: &mut Encoder);
}

/// A value read with its integers as varints: the mirror of
/// [`EncodeVarint`].
pub trait DecodeVarint: Sized {
    /// Reads one `Self` from `dec`. A varint past the integer's range is
    /// [`Malformed`](DecodeError::Malformed).
    fn decode_varint(dec: &mut Decoder<'_>) -> Result<Self, DecodeError>;
}

macro_rules! unsigned {
    ($($ty:ty),+) => {$(
        impl EncodeVarint for $ty {
            #[inline]
            fn encode_varint(&self, enc: &mut Encoder) {
                enc.write_varint(*self as u64);
            }
        }

        impl DecodeVarint for $ty {
            #[inline]
            fn decode_varint(dec: &mut Decoder<'_>) -> Result<Self, DecodeError> {
                let offset = dec.position();
                <$ty>::try_from(dec.read_varint()?).map_err(|_| DecodeError::Malformed { offset })
            }
        }
    )+};
}

macro_rules! signed {
    ($($ty:ty),+) => {$(
        impl EncodeVarint for $ty {
            #[inline]
            fn encode_varint(&self, enc: &mut Encoder) {
                enc.write_varint_signed(*self as i64);
            }
        }

        impl DecodeVarint for $ty {
            #[inline]
            fn decode_varint(dec: &mut Decoder<'_>) -> Result<Self, DecodeError> {
                let offset = dec.position();
                <$ty>::try_from(dec.read_varint_signed()?)
                    .map_err(|_| DecodeError::Malformed { offset })
            }
        }
    )+};
}

unsigned!(u8, u16, u32, u64, usize);
signed!(i8, i16, i32, i64, isize);

impl<T: EncodeVarint> EncodeVarint for [T] {
    #[inline]
    fn encode_varint(&self, enc: &mut Encoder) {
        enc.write_varint(self.len() as u64);
        for item in self {
            item.encode_varint(enc);
        }
    }
}

impl<T: EncodeVarint> EncodeVarint for Vec<T> {
    #[inline]
    fn encode_varint(&self, enc: &mut Encoder) {
        self.as_slice().encode_varint(enc);
    }
}

impl<T: DecodeVarint> DecodeVarint for Vec<T> {
    #[inline]
    fn decode_varint(dec: &mut Decoder<'_>) -> Result<Self, DecodeError> {
        dec.read_vec_varint(usize::MAX)
    }
}

impl<T: EncodeVarint> EncodeVarint for Box<[T]> {
    #[inline]
    fn encode_varint(&self, enc: &mut Encoder) {
        (**self).encode_varint(enc);
    }
}

impl<T: DecodeVarint> DecodeVarint for Box<[T]> {
    #[inline]
    fn decode_varint(dec: &mut Decoder<'_>) -> Result<Self, DecodeError> {
        Vec::decode_varint(dec).map(Vec::into_boxed_slice)
    }
}

impl<T: EncodeVarint, const N: usize> EncodeVarint for [T; N] {
    #[inline]
    fn encode_varint(&self, enc: &mut Encoder) {
        for item in self {
            item.encode_varint(enc);
        }
    }
}

impl<T: DecodeVarint, const N: usize> DecodeVarint for [T; N] {
    fn decode_varint(dec: &mut Decoder<'_>) -> Result<Self, DecodeError> {
        let mut items = Vec::with_capacity(N);
        for _ in 0..N {
            items.push(T::decode_varint(dec)?);
        }
        match items.try_into() {
            Ok(array) => Ok(array),
            Err(_) => unreachable!("exactly `N` items were read"),
        }
    }
}

impl<T: EncodeVarint> EncodeVarint for Option<T> {
    #[inline]
    fn encode_varint(&self, enc: &mut Encoder) {
        match self {
            Some(value) => {
                enc.write_u8(1);
                value.encode_varint(enc);
            }
            None => enc.write_u8(0),
        }
    }
}

impl<T: DecodeVarint> DecodeVarint for Option<T> {
    #[inline]
    fn decode_varint(dec: &mut Decoder<'_>) -> Result<Self, DecodeError> {
        let offset = dec.position();
        match dec.read_u8()? {
            0 => Ok(None),
            1 => T::decode_varint(dec).map(Some),
            _ => Err(DecodeError::Malformed { offset }),
        }
    }
}

macro_rules! tuple {
    ($($name:ident $index:tt),+) => {
        impl<$($name: EncodeVarint),+> EncodeVarint for ($($name,)+) {
            #[inline]
            fn encode_varint(&self, enc: &mut Encoder) {
                $(self.$index.encode_varint(enc);)+
            }
        }

        impl<$($name: DecodeVarint),+> DecodeVarint for ($($name,)+) {
            #[inline]
            fn decode_varint(dec: &mut Decoder<'_>) -> Result<Self, DecodeError> {
                Ok(($($name::decode_varint(dec)?,)+))
            }
        }
    };
}

tuple!(A 0);
tuple!(A 0, B 1);
tuple!(A 0, B 1, C 2);
tuple!(A 0, B 1, C 2, D 3);

impl Decoder<'_> {
    /// Reads a list of `T` with its integers as varints: a
    /// [`read_count`](Self::read_count) of at most `max`, then that many
    /// items.
    pub fn read_vec_varint<T: DecodeVarint>(&mut self, max: usize) -> Result<Vec<T>, DecodeError> {
        self.read_list(max, T::decode_varint)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn integers_are_varints_in_every_shape() {
        let mut enc = Encoder::new();
        vec![(1u32, 300u32)].encode_varint(&mut enc);
        Some(-1i32).encode_varint(&mut enc);
        let bytes = enc.into_bytes();
        assert_eq!(bytes, [1, 1, 0xac, 0x02, 1, 1]);
        let mut dec = Decoder::new(&bytes);
        assert_eq!(
            Vec::<(u32, u32)>::decode_varint(&mut dec),
            Ok(vec![(1, 300)])
        );
        assert_eq!(Option::<i32>::decode_varint(&mut dec), Ok(Some(-1)));
        assert!(dec.is_empty());
    }

    #[test]
    fn a_varint_past_its_integer_is_malformed() {
        let mut enc = Encoder::new();
        enc.write_varint(256);
        let bytes = enc.into_bytes();
        assert_eq!(
            u8::decode_varint(&mut Decoder::new(&bytes)),
            Err(DecodeError::Malformed { offset: 0 })
        );
    }
}
