//! The append-only binary encoder and the [`Encode`] trait.
//!
//! Every scalar is written little-endian and fixed-width; byte-string and text
//! lengths are written as an unsigned LEB128 varint so a short slice costs one
//! length byte rather than eight. This is the write side of the wire format; the
//! read side in [`decode`](crate::decode) mirrors it exactly, and the pair is
//! covered by the round-trip tests there.
//!
//! The encoder cannot fail: it only ever grows an owned `Vec<u8>`. That keeps
//! the trait signatures free of `Result` and makes `Encode` cheap to implement.

/// An append-only byte writer producing the binary wire format.
///
/// Wraps an owned buffer; every `write_*` appends and never seeks, so encoding
/// is a single forward pass. Reuse one across many values (or call
/// [`Encoder::with_capacity`]) to amortize the backing allocation.
#[derive(Debug, Default, Clone)]
pub struct Encoder {
    buf: Vec<u8>,
}

impl Encoder {
    /// A new encoder with an empty buffer.
    #[inline]
    pub fn new() -> Self {
        Self { buf: Vec::new() }
    }

    /// A new encoder whose buffer is preallocated for at least `capacity` bytes.
    ///
    /// Use this when the encoded size is known or bounded to avoid reallocation
    /// while writing.
    #[inline]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            buf: Vec::with_capacity(capacity),
        }
    }

    /// The bytes written so far.
    #[inline]
    pub fn as_bytes(&self) -> &[u8] {
        &self.buf
    }

    /// Consumes the encoder and returns its buffer.
    #[inline]
    pub fn into_bytes(self) -> Vec<u8> {
        self.buf
    }

    /// The number of bytes written so far.
    #[inline]
    pub fn len(&self) -> usize {
        self.buf.len()
    }

    /// Whether nothing has been written yet.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// Appends a `bool` as a single byte: `1` for true, `0` for false.
    #[inline]
    pub fn write_bool(&mut self, value: bool) {
        self.buf.push(value as u8);
    }

    /// Appends a raw byte.
    #[inline]
    pub fn write_u8(&mut self, value: u8) {
        self.buf.push(value);
    }

    /// Appends a signed byte.
    #[inline]
    pub fn write_i8(&mut self, value: i8) {
        self.buf.push(value as u8);
    }

    /// Appends the raw bytes verbatim, without a length prefix.
    ///
    /// Use this only when the length is fixed by the schema; for a
    /// self-describing field prefer [`write_bytes`](Self::write_bytes).
    #[inline]
    pub fn write_raw(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// Appends a length-prefixed byte string: a varint length then the bytes.
    #[inline]
    pub fn write_bytes(&mut self, bytes: &[u8]) {
        self.write_varint(bytes.len() as u64);
        self.buf.extend_from_slice(bytes);
    }

    /// Appends a length-prefixed UTF-8 string: a varint byte length then the
    /// UTF-8 bytes. The decoder validates the bytes are UTF-8 on the way back.
    #[inline]
    pub fn write_str(&mut self, value: &str) {
        self.write_bytes(value.as_bytes());
    }

    /// Appends an unsigned LEB128 varint.
    ///
    /// Seven bits per byte, little-endian groups, high bit set on every byte
    /// but the last. Used for lengths and any value where small numbers
    /// dominate; the decoder rejects an overlong (more than ten-byte) encoding.
    pub fn write_varint(&mut self, mut value: u64) {
        loop {
            let byte = (value & 0x7f) as u8;
            value >>= 7;
            if value == 0 {
                self.buf.push(byte);
                break;
            }
            self.buf.push(byte | 0x80);
        }
    }

    /// Appends a signed LEB128 varint (zig-zag mapped to unsigned first).
    #[inline]
    pub fn write_varint_signed(&mut self, value: i64) {
        // Zig-zag: map small-magnitude signed values to small unsigned ones so
        // -1, 1, -2, 2 … stay one byte instead of ten for every negative.
        self.write_varint(((value << 1) ^ (value >> 63)) as u64);
    }
}

/// Fixed-width little-endian scalars. A macro keeps every integer/float impl
/// byte-for-byte identical to its `to_le_bytes` so the decoder mirror is
/// mechanical and cannot drift per type.
macro_rules! write_le {
    ($($name:ident => $ty:ty),+ $(,)?) => {$(
        impl Encoder {
            #[doc = concat!("Appends a little-endian `", stringify!($ty), "`.")]
            #[inline]
            pub fn $name(&mut self, value: $ty) {
                self.buf.extend_from_slice(&value.to_le_bytes());
            }
        }
    )+};
}

write_le! {
    write_u16 => u16,
    write_u32 => u32,
    write_u64 => u64,
    write_i16 => i16,
    write_i32 => i32,
    write_i64 => i64,
    write_f32 => f32,
    write_f64 => f64,
}

/// A value that can serialize itself into an [`Encoder`].
///
/// Infallible by construction — the encoder only grows a buffer — so there is
/// no `Result`. Implement it for wire-format types; the mirror is
/// [`Decode`](crate::decode::Decode).
pub trait Encode {
    /// Writes `self` to `enc`.
    fn encode(&self, enc: &mut Encoder);

    /// Encodes `self` into a fresh `Vec<u8>`. A convenience over constructing an
    /// [`Encoder`] for one value.
    #[inline]
    fn encode_to_vec(&self) -> Vec<u8> {
        let mut enc = Encoder::new();
        self.encode(&mut enc);
        enc.into_bytes()
    }
}

/// Scalar `Encode` impls, matching the `Encoder::write_*` methods one-to-one.
macro_rules! encode_scalar {
    ($($ty:ty => $method:ident),+ $(,)?) => {$(
        impl Encode for $ty {
            #[inline]
            fn encode(&self, enc: &mut Encoder) {
                enc.$method(*self);
            }
        }
    )+};
}

encode_scalar! {
    bool => write_bool,
    u8 => write_u8,
    u16 => write_u16,
    u32 => write_u32,
    u64 => write_u64,
    i8 => write_i8,
    i16 => write_i16,
    i32 => write_i32,
    i64 => write_i64,
    f32 => write_f32,
    f64 => write_f64,
}

impl Encode for str {
    #[inline]
    fn encode(&self, enc: &mut Encoder) {
        enc.write_str(self);
    }
}

impl Encode for [u8] {
    #[inline]
    fn encode(&self, enc: &mut Encoder) {
        enc.write_bytes(self);
    }
}

impl<T: Encode + ?Sized> Encode for &T {
    #[inline]
    fn encode(&self, enc: &mut Encoder) {
        (**self).encode(enc);
    }
}
