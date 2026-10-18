//! A section table: a blob split into aligned sections a reader locates
//! directly, without walking the ones before.
//!
//! The table is a `u32` section count followed by one entry per section — its
//! offset from the start of the blob, its byte length and the number of items
//! it holds, each a little-endian `u32` — then padding to [`SECTION_ALIGN`].
//! The sections follow in table order, each starting at the first
//! [`SECTION_ALIGN`] boundary after the previous one ends, the gap zero. The
//! layout is canonical: a reader checks every entry against it, so a table
//! that points outside the blob, overlaps, reorders or leaves stray bytes
//! between sections is rejected before any section is read.
//!
//! Offsets are relative to the blob, which the decoder reading the table
//! spans from its first byte. A blob that sits at an aligned address
//! ([`Aligned`](crate::Aligned)) has each section at one.

use crate::aligned::SECTION_ALIGN;
use crate::decode::{DecodeError, Decoder};
use crate::encode::Encoder;

/// The bytes of one table entry: offset, length and item count.
const ENTRY: usize = 12;

/// Lays out a fixed number of sections behind a table reserved up front.
#[derive(Debug)]
pub struct SectionWriter {
    /// Where the first entry is.
    table: usize,
    /// How many sections the table holds.
    count: u32,
    /// How many have been written.
    written: u32,
}

impl SectionWriter {
    /// Reserves a table of `count` sections at `enc`'s position, which is
    /// relative to the start of the blob the table describes.
    pub fn new(enc: &mut Encoder, count: u32) -> SectionWriter {
        enc.write_u32(count);
        let table = enc.len();
        for _ in 0..count {
            enc.write_raw(&[0; ENTRY]);
        }
        SectionWriter {
            table,
            count,
            written: 0,
        }
    }

    /// Writes the next section with `write`, which returns the number of
    /// items it wrote.
    ///
    /// # Panics
    ///
    /// If every reserved section was written already, or the blob outgrows
    /// a `u32` offset.
    pub fn section(&mut self, enc: &mut Encoder, write: impl FnOnce(&mut Encoder) -> u32) {
        assert!(
            self.written < self.count,
            "more sections than the table holds"
        );
        enc.align(SECTION_ALIGN);
        let start = enc.len();
        let items = write(enc);
        let len = enc.len() - start;
        let at = self.table + self.written as usize * ENTRY;
        let narrow = |n: usize| u32::try_from(n).expect("a section blob fits u32 offsets");
        enc.patch_u32(at, narrow(start));
        enc.patch_u32(at + 4, narrow(len));
        enc.patch_u32(at + 8, items);
        self.written += 1;
    }

    /// Checks every reserved section was written.
    ///
    /// # Panics
    ///
    /// If one was not.
    pub fn finish(self) {
        assert_eq!(
            self.written, self.count,
            "a reserved section was not written"
        );
    }
}

/// One section of a blob.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Section<'a> {
    /// Its bytes.
    pub bytes: &'a [u8],
    /// The number of items it holds, as its writer counted them.
    pub items: u32,
    /// Where it starts in the blob.
    pub offset: usize,
    /// The blob up to the section's end.
    upto: &'a [u8],
}

impl<'a> Section<'a> {
    /// A decoder over its bytes, positioned at its first byte: positions,
    /// alignment and error offsets are the blob's, and
    /// [`finish`](Decoder::finish) checks the section is fully read.
    #[inline]
    pub fn decoder(&self) -> Decoder<'a> {
        Decoder::starting_at(self.upto, self.offset)
    }
}

/// A validated section table.
#[derive(Debug, Clone, Copy)]
pub struct Sections<'a> {
    blob: &'a [u8],
    entries: &'a [u8],
    count: u32,
    end: usize,
}

impl<'a> Sections<'a> {
    /// Reads and validates the table at `dec`'s position, requiring exactly
    /// `count` sections; `dec` spans the blob from its first byte. Leaves the
    /// cursor at the end of the table.
    ///
    /// # Errors
    ///
    /// [`DecodeError::Malformed`] when the table holds another number of
    /// sections, or an entry breaks the canonical layout; the
    /// [`DecodeError::UnexpectedEof`] of a section that runs past the input.
    pub fn read(dec: &mut Decoder<'a>, count: u32) -> Result<Sections<'a>, DecodeError> {
        let offset = dec.position();
        if dec.read_u32()? != count {
            return Err(DecodeError::Malformed { offset });
        }
        let entries = dec.read_raw(count as usize * ENTRY)?;
        let blob = dec.input();
        let mut end = dec.position();
        for (index, entry) in entries.chunks_exact(ENTRY).enumerate() {
            let field = |at: usize| u32::from_le_bytes(entry[at..at + 4].try_into().unwrap());
            let (start, len) = (field(0) as usize, field(4) as usize);
            let at = offset + 4 + index * ENTRY;
            let aligned = end.next_multiple_of(SECTION_ALIGN);
            if start != aligned {
                return Err(DecodeError::Malformed { offset: at });
            }
            let mut gap = Decoder::new(blob);
            gap.read_raw(end)?;
            gap.align(SECTION_ALIGN)?;
            end = start
                .checked_add(len)
                .filter(|&end| end <= blob.len())
                .ok_or(DecodeError::UnexpectedEof {
                    offset: start,
                    needed: len,
                    available: blob.len().saturating_sub(start),
                })?;
        }
        Ok(Sections {
            blob,
            entries,
            count,
            end,
        })
    }

    /// The number of sections.
    #[inline]
    pub fn len(&self) -> u32 {
        self.count
    }

    /// Whether the table holds no section.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Section `index`.
    ///
    /// # Panics
    ///
    /// If `index` is out of range.
    #[inline]
    pub fn get(&self, index: u32) -> Section<'a> {
        assert!(index < self.count, "section {index} is out of range");
        let entry = &self.entries[index as usize * ENTRY..][..ENTRY];
        let field = |at: usize| u32::from_le_bytes(entry[at..at + 4].try_into().unwrap());
        let start = field(0) as usize;
        let upto = &self.blob[..start + field(4) as usize];
        Section {
            bytes: &upto[start..],
            items: field(8),
            offset: start,
            upto,
        }
    }

    /// Where the last section ends: the length of the blob the table
    /// describes.
    #[inline]
    pub fn end(&self) -> usize {
        self.end
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    /// A blob of three sections after a two-byte header: 3 bytes, none, 9.
    fn blob() -> Vec<u8> {
        let mut enc = Encoder::new();
        enc.write_raw(b"HD");
        let mut sections = SectionWriter::new(&mut enc, 3);
        sections.section(&mut enc, |enc| {
            enc.write_raw(&[1, 2, 3]);
            3
        });
        sections.section(&mut enc, |_| 0);
        sections.section(&mut enc, |enc| {
            enc.write_raw(&[9; 9]);
            1
        });
        sections.finish();
        enc.into_bytes()
    }

    fn read(bytes: &[u8]) -> Result<Sections<'_>, DecodeError> {
        let mut dec = Decoder::new(bytes);
        dec.read_raw(2)?;
        Sections::read(&mut dec, 3)
    }

    #[test]
    fn sections_round_trip_aligned() {
        let bytes = blob();
        let sections = read(&bytes).unwrap();
        assert_eq!(sections.end(), bytes.len());
        let first = sections.get(0);
        assert_eq!((first.bytes, first.items), (&[1, 2, 3][..], 3));
        assert!(sections.get(1).bytes.is_empty());
        assert_eq!(sections.get(2).bytes, &[9; 9]);
        for index in 0..3 {
            let at = sections.get(index).bytes.as_ptr().addr() - bytes.as_ptr().addr();
            assert_eq!(at % SECTION_ALIGN, 0, "section {index} starts aligned");
        }
    }

    #[test]
    fn a_broken_table_is_rejected() {
        let bytes = blob();
        // Another section count.
        let mut dec = Decoder::new(&bytes);
        dec.read_raw(2).unwrap();
        assert!(matches!(
            Sections::read(&mut dec, 2),
            Err(DecodeError::Malformed { offset: 2 })
        ));
        // An offset off the canonical layout, and one past the blob.
        let first = 2 + 4;
        for offset in [17u32, 1 << 20] {
            let mut bad = bytes.clone();
            bad[first..first + 4].copy_from_slice(&offset.to_le_bytes());
            assert!(read(&bad).is_err(), "offset {offset}");
        }
        // A length past the blob.
        let mut bad = bytes.clone();
        let last = first + 2 * ENTRY;
        bad[last + 4..last + 8].copy_from_slice(&64u32.to_le_bytes());
        assert!(matches!(read(&bad), Err(DecodeError::UnexpectedEof { .. })));
        // A stray byte in the padding between two sections.
        let sections = read(&bytes).unwrap();
        let gap = sections.get(0).bytes.as_ptr().addr() - bytes.as_ptr().addr() + 3;
        let mut bad = bytes.clone();
        bad[gap] = 1;
        assert!(matches!(
            read(&bad),
            Err(DecodeError::Malformed { offset }) if offset == gap
        ));
        // Every truncation fails cleanly.
        for len in 0..bytes.len() {
            assert!(read(&bytes[..len]).is_err(), "prefix {len}");
        }
    }
}
