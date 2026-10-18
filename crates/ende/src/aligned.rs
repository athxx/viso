//! Bytes at an aligned address, so a blob embedded as a `static` can be read
//! in place from memory-mapped flash.

use core::ops::Deref;

/// The alignment of an [`Aligned`] blob and of every section a
/// [`SectionWriter`](crate::SectionWriter) lays out.
pub const SECTION_ALIGN: usize = 8;

/// `B` at an address aligned to [`SECTION_ALIGN`].
///
/// A blob whose sections are aligned relative to its start (see
/// [`Encoder::align`](crate::Encoder::align)) has them at aligned addresses
/// only when the blob itself starts at one; a `static` byte array is aligned
/// to 1. Wrapping it keeps every section where a word read of mapped flash or
/// a future zero-copy view expects it:
///
/// ```
/// use viso_ende::Aligned;
///
/// static BLOB: Aligned<[u8; 4]> = Aligned(*b"VE\x01\x00");
/// let bytes: &'static [u8] = BLOB.as_slice();
/// assert_eq!(bytes.as_ptr().addr() % viso_ende::SECTION_ALIGN, 0);
/// ```
#[repr(C, align(8))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Aligned<B: ?Sized>(pub B);

const _: () = assert!(core::mem::align_of::<Aligned<[u8; 0]>>() == SECTION_ALIGN);

impl<const N: usize> Aligned<[u8; N]> {
    /// The bytes, as a slice starting at the aligned address.
    #[inline]
    pub const fn as_slice(&self) -> &[u8] {
        &self.0
    }
}

impl<B: ?Sized> Deref for Aligned<B> {
    type Target = B;

    #[inline]
    fn deref(&self) -> &B {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_static_blob_starts_aligned() {
        static ONE: Aligned<[u8; 1]> = Aligned([7]);
        static ODD: Aligned<[u8; 13]> = Aligned(*b"thirteen byte");
        for bytes in [ONE.as_slice(), ODD.as_slice()] {
            assert_eq!(bytes.as_ptr().addr() % SECTION_ALIGN, 0);
        }
        assert_eq!(ODD.as_slice(), b"thirteen byte");
        let unsized_view: &Aligned<[u8]> = &ODD;
        assert_eq!(unsized_view.len(), 13);
    }
}
