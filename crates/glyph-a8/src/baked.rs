//! Baked A8 glyph pages: a face's glyph coverage rasterized at build time for
//! a fixed set of sizes, stored compressed and read in place.
//!
//! A product whose UI font sizes are fixed bakes its packaged faces at those
//! sizes. The runtime glyph cache reads a glyph's
//! coverage from here before it would rasterize it, and a build that bakes
//! every face links no rasterizer at all.
//!
//! # Layout
//!
//! One blob per face, little-endian, laid out so it can stay where the linker
//! put it (flash, or a mapped file) and be read without copying. The reader
//! takes bytes and never casts, so the blob needs no alignment; the emitter
//! still places it at an 8-byte boundary, like every other embedded blob.
//!
//! ```text
//! header        16 B   magic "VBA8", version u16 = 1, strike count u16,
//!                      blob length u32, reserved u32 = 0
//! strike × S    16 B   ppem u16 (ascending), reserved u16 = 0, glyph count u32,
//!                      records offset u32 (4-aligned), data offset u32
//! record × G    20 B   glyph u16 (ascending), width u16, height u16,
//!                      reserved u16 = 0, left f32, top f32,
//!                      end u32 — the glyph's code ends at `data offset + end`
//!                      and starts where the previous record's ends
//! data                 each glyph's run-length code (`rle`)
//! ```
//!
//! A strike holds every glyph of the face with ink at its size; a glyph a
//! strike does not list draws nothing there (a space, or a glyph the font
//! subset emptied). [`BakedFace::parse`] checks the whole layout once, so a
//! lookup after it is a binary search and a decode is bounds-checked against
//! a table already known to fit.

use alloc::vec;
use alloc::vec::Vec;

use crate::{CoverageBitmap, resample, rle};

/// The blob's first four bytes.
pub const MAGIC: [u8; 4] = *b"VBA8";
/// The layout version this reader reads.
pub const VERSION: u16 = 1;

const HEADER: usize = 16;
const STRIKE: usize = 16;
const RECORD: usize = 20;

fn u16_at(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

/// One face's baked strikes, borrowed from the blob.
#[derive(Debug, Clone, Copy)]
pub struct BakedFace<'a> {
    blob: &'a [u8],
    strikes: usize,
}

/// One size of a [`BakedFace`].
#[derive(Debug, Clone, Copy)]
struct Strike {
    ppem: u16,
    glyphs: usize,
    records: usize,
    data: usize,
}

/// What a strike holds for a glyph.
#[derive(Debug, Clone, Copy)]
pub enum Lookup<'a> {
    /// The face is not baked at that size.
    NoStrike,
    /// The glyph draws nothing at that size.
    Empty,
    /// The glyph's coverage.
    Glyph(BakedGlyph<'a>),
}

/// One glyph's baked coverage: its geometry and its code.
#[derive(Debug, Clone, Copy)]
pub struct BakedGlyph<'a> {
    pub width: u16,
    pub height: u16,
    /// As [`CoverageBitmap::left`].
    pub left: f32,
    /// As [`CoverageBitmap::top`].
    pub top: f32,
    code: &'a [u8],
}

impl BakedGlyph<'_> {
    /// The bytes the glyph's code takes in the blob.
    pub fn code_len(&self) -> usize {
        self.code.len()
    }

    /// Decode the coverage into `out`, `width × height` bytes, row-major.
    /// `false` when `out` has another length or the code is malformed.
    pub fn decode_into(&self, out: &mut [u8]) -> bool {
        out.len() == usize::from(self.width) * usize::from(self.height)
            && rle::decode(self.code, usize::from(self.width), out)
    }

    /// The coverage as a bitmap.
    pub fn decode(&self) -> Option<CoverageBitmap> {
        let mut coverage = vec![0; usize::from(self.width) * usize::from(self.height)];
        self.decode_into(&mut coverage).then(|| CoverageBitmap {
            width: u32::from(self.width),
            height: u32::from(self.height),
            left: self.left,
            top: self.top,
            coverage,
        })
    }
}

impl<'a> BakedFace<'a> {
    /// The face `blob` holds, its whole layout checked; `None` when it is not
    /// a version-1 blob or any table, record or code lies outside it, out of
    /// order, or overlapping.
    pub fn parse(blob: &'a [u8]) -> Option<Self> {
        if blob.get(..4)? != MAGIC
            || u16_at(blob, 4)? != VERSION
            || u32_at(blob, 8)? as usize != blob.len()
            || u32_at(blob, 12)? != 0
        {
            return None;
        }
        let face = Self {
            blob,
            strikes: usize::from(u16_at(blob, 6)?),
        };
        let table_end = HEADER + face.strikes * STRIKE;
        if table_end > blob.len() {
            return None;
        }
        let mut previous = 0;
        for index in 0..face.strikes {
            let at = HEADER + index * STRIKE;
            if u16_at(blob, at + 2)? != 0 {
                return None;
            }
            let strike = face.strike(index)?;
            if strike.ppem <= previous
                || strike.records % 4 != 0
                || strike.records < table_end
                || strike.records + strike.glyphs * RECORD > blob.len()
            {
                return None;
            }
            previous = strike.ppem;
            let mut last_glyph = None;
            let mut last_end = 0;
            for record in 0..strike.glyphs {
                let at = strike.records + record * RECORD;
                let glyph = u16_at(blob, at)?;
                let end = u32_at(blob, at + 16)? as usize;
                if last_glyph.is_some_and(|last| glyph <= last)
                    || u16_at(blob, at + 2)? == 0
                    || u16_at(blob, at + 4)? == 0
                    || u16_at(blob, at + 6)? != 0
                    || end <= last_end
                {
                    return None;
                }
                last_glyph = Some(glyph);
                last_end = end;
            }
            if strike.data < table_end || strike.data + last_end > blob.len() {
                return None;
            }
        }
        Some(face)
    }

    fn strike(&self, index: usize) -> Option<Strike> {
        let at = HEADER + index * STRIKE;
        Some(Strike {
            ppem: u16_at(self.blob, at)?,
            glyphs: u32_at(self.blob, at + 4)? as usize,
            records: u32_at(self.blob, at + 8)? as usize,
            data: u32_at(self.blob, at + 12)? as usize,
        })
    }

    fn strike_for(&self, ppem: u16) -> Option<Strike> {
        (0..self.strikes)
            .filter_map(|index| self.strike(index))
            .find(|strike| strike.ppem == ppem)
    }

    /// The baked sizes, in pixels per em, ascending.
    pub fn sizes(&self) -> impl Iterator<Item = u16> + '_ {
        (0..self.strikes).filter_map(|index| Some(self.strike(index)?.ppem))
    }

    /// The bytes the blob takes.
    pub fn byte_len(&self) -> usize {
        self.blob.len()
    }

    /// What the strike at `ppem` holds for `glyph`.
    pub fn glyph(&self, glyph: u16, ppem: u16) -> Lookup<'a> {
        let Some(strike) = self.strike_for(ppem) else {
            return Lookup::NoStrike;
        };
        let record = |index: usize| strike.records + index * RECORD;
        let (mut low, mut high) = (0, strike.glyphs);
        while low < high {
            let middle = (low + high) / 2;
            match u16_at(self.blob, record(middle)) {
                Some(id) if id < glyph => low = middle + 1,
                Some(id) if id > glyph => high = middle,
                Some(_) => {
                    return self
                        .record(&strike, middle)
                        .map_or(Lookup::Empty, Lookup::Glyph);
                }
                None => return Lookup::Empty,
            }
        }
        Lookup::Empty
    }

    fn record(&self, strike: &Strike, index: usize) -> Option<BakedGlyph<'a>> {
        let at = strike.records + index * RECORD;
        let start = match index {
            0 => 0,
            _ => u32_at(self.blob, at - RECORD + 16)? as usize,
        };
        let end = u32_at(self.blob, at + 16)? as usize;
        Some(BakedGlyph {
            width: u16_at(self.blob, at + 2)?,
            height: u16_at(self.blob, at + 4)?,
            left: f32::from_bits(u32_at(self.blob, at + 8)?),
            top: f32::from_bits(u32_at(self.blob, at + 12)?),
            code: self.blob.get(strike.data + start..strike.data + end)?,
        })
    }

    /// `glyph`'s coverage at `ppem`: a zero-area bitmap when it draws nothing
    /// there, `None` when the face is not baked at `ppem`.
    pub fn coverage(&self, glyph: u16, ppem: u16) -> Option<CoverageBitmap> {
        match self.glyph(glyph, ppem) {
            Lookup::NoStrike => None,
            Lookup::Empty => Some(CoverageBitmap::empty(0.0, 0.0)),
            Lookup::Glyph(baked) => baked.decode(),
        }
    }

    /// The baked size `ppem` draws from when it is not baked itself: the
    /// smallest larger one, which scales down, else the largest.
    pub fn nearest_size(&self, ppem: u16) -> Option<u16> {
        self.sizes()
            .find(|&size| size >= ppem)
            .or_else(|| self.sizes().last())
    }

    /// `glyph`'s coverage at `ppem`, resampled from [`Self::nearest_size`]
    /// when `ppem` is not baked; `None` when nothing is.
    pub fn resampled(&self, glyph: u16, ppem: u16) -> Option<CoverageBitmap> {
        let from = self.nearest_size(ppem)?;
        let bitmap = self.coverage(glyph, from)?;
        Some(if from == ppem {
            bitmap
        } else {
            resample::resample(&bitmap, from, ppem)
        })
    }
}

/// One size to bake: its glyphs' coverage, in any order; zero-area glyphs are
/// left out of the blob.
#[derive(Debug, Clone, Default)]
pub struct BakedStrike {
    pub ppem: u16,
    pub glyphs: Vec<(u16, CoverageBitmap)>,
}

/// Why a set of strikes cannot be laid out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EncodeError {
    /// Two strikes of one size, or a size of 0.
    Size(u16),
    /// A glyph twice in one strike.
    Glyph { ppem: u16, glyph: u16 },
    /// A bitmap wider or taller than 65535 pixels, or whose coverage is not
    /// `width × height` bytes.
    Bitmap { ppem: u16, glyph: u16 },
    /// The blob would pass 4 GiB.
    TooLarge,
}

impl BakedStrike {
    /// Lay `strikes` out as one blob.
    pub fn encode(strikes: &[BakedStrike]) -> Result<Vec<u8>, EncodeError> {
        let mut order: Vec<&BakedStrike> = strikes.iter().collect();
        order.sort_by_key(|strike| strike.ppem);
        for pair in order.windows(2) {
            if pair[0].ppem == pair[1].ppem {
                return Err(EncodeError::Size(pair[0].ppem));
            }
        }
        if let Some(zero) = order.first().filter(|strike| strike.ppem == 0) {
            return Err(EncodeError::Size(zero.ppem));
        }
        let mut blob = vec![0; HEADER + order.len() * STRIKE];
        blob[..4].copy_from_slice(&MAGIC);
        blob[4..6].copy_from_slice(&VERSION.to_le_bytes());
        blob[6..8].copy_from_slice(&(order.len() as u16).to_le_bytes());
        let offset = |len: usize| u32::try_from(len).map_err(|_| EncodeError::TooLarge);
        for (index, strike) in order.iter().enumerate() {
            let mut glyphs: Vec<&(u16, CoverageBitmap)> = strike
                .glyphs
                .iter()
                .filter(|(_, bitmap)| !bitmap.is_empty())
                .collect();
            glyphs.sort_by_key(|(glyph, _)| *glyph);
            let mut records = Vec::with_capacity(glyphs.len() * RECORD);
            let mut code = Vec::new();
            let mut last = None;
            for &(glyph, ref bitmap) in glyphs.iter().copied() {
                if last == Some(glyph) {
                    return Err(EncodeError::Glyph {
                        ppem: strike.ppem,
                        glyph,
                    });
                }
                last = Some(glyph);
                let bad = || EncodeError::Bitmap {
                    ppem: strike.ppem,
                    glyph,
                };
                let width = u16::try_from(bitmap.width).map_err(|_| bad())?;
                let height = u16::try_from(bitmap.height).map_err(|_| bad())?;
                if bitmap.coverage.len() != usize::from(width) * usize::from(height) {
                    return Err(bad());
                }
                rle::encode(&bitmap.coverage, usize::from(width), &mut code);
                records.extend_from_slice(&glyph.to_le_bytes());
                records.extend_from_slice(&width.to_le_bytes());
                records.extend_from_slice(&height.to_le_bytes());
                records.extend_from_slice(&[0, 0]);
                records.extend_from_slice(&bitmap.left.to_bits().to_le_bytes());
                records.extend_from_slice(&bitmap.top.to_bits().to_le_bytes());
                records.extend_from_slice(&offset(code.len())?.to_le_bytes());
            }
            blob.resize(blob.len().next_multiple_of(4), 0);
            let records_at = offset(blob.len())?;
            blob.extend_from_slice(&records);
            let data_at = offset(blob.len())?;
            blob.extend_from_slice(&code);
            let at = HEADER + index * STRIKE;
            blob[at..at + 2].copy_from_slice(&strike.ppem.to_le_bytes());
            blob[at + 4..at + 8].copy_from_slice(&offset(glyphs.len())?.to_le_bytes());
            blob[at + 8..at + 12].copy_from_slice(&records_at.to_le_bytes());
            blob[at + 12..at + 16].copy_from_slice(&data_at.to_le_bytes());
        }
        let len = offset(blob.len())?;
        blob[8..12].copy_from_slice(&len.to_le_bytes());
        Ok(blob)
    }
}

/// Why a face cannot be baked.
#[cfg(feature = "raster")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BakeError {
    /// The rasterizer reads no outline of `glyph`: the face does not parse,
    /// or its outlines are a kind it does not draw (CFF2).
    Outline {
        glyph: u16,
    },
    Encode(EncodeError),
}

#[cfg(feature = "raster")]
impl core::fmt::Display for BakeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            BakeError::Outline { glyph } => {
                write!(f, "glyph {glyph} has no outline the rasterizer reads")
            }
            BakeError::Encode(EncodeError::Size(ppem)) => write!(f, "size {ppem} is not bakeable"),
            BakeError::Encode(EncodeError::Glyph { ppem, glyph }) => {
                write!(f, "glyph {glyph} is baked twice at {ppem} px")
            }
            BakeError::Encode(EncodeError::Bitmap { ppem, glyph }) => {
                write!(f, "glyph {glyph} at {ppem} px is too large to bake")
            }
            BakeError::Encode(EncodeError::TooLarge) => f.write_str("the baked pages pass 4 GiB"),
        }
    }
}

/// Bake `glyphs` of face `index` of `sfnt` at each of `sizes` (pixels per
/// em) with [`crate::rasterize_coverage`], the rasterizer the runtime runs.
#[cfg(feature = "raster")]
pub fn bake_face(
    sfnt: &[u8],
    index: u32,
    glyphs: &[u16],
    sizes: &[u16],
) -> Result<Vec<u8>, BakeError> {
    let strikes = sizes
        .iter()
        .map(|&ppem| {
            let glyphs = glyphs
                .iter()
                .map(|&glyph| {
                    crate::rasterize_coverage(sfnt, index, glyph, f32::from(ppem))
                        .map(|bitmap| (glyph, bitmap))
                        .ok_or(BakeError::Outline { glyph })
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(BakedStrike { ppem, glyphs })
        })
        .collect::<Result<Vec<_>, BakeError>>()?;
    BakedStrike::encode(&strikes).map_err(BakeError::Encode)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bitmap(width: u32, height: u32, coverage: Vec<u8>) -> CoverageBitmap {
        CoverageBitmap {
            width,
            height,
            left: -0.75,
            top: 11.25,
            coverage,
        }
    }

    fn sample() -> Vec<u8> {
        BakedStrike::encode(&[
            BakedStrike {
                ppem: 24,
                glyphs: vec![(9, bitmap(2, 2, vec![0, 255, 128, 0]))],
            },
            BakedStrike {
                ppem: 12,
                glyphs: vec![
                    (40, bitmap(3, 1, vec![1, 2, 3])),
                    (3, CoverageBitmap::empty(0.0, 0.0)),
                    (7, bitmap(1, 2, vec![255, 255])),
                ],
            },
        ])
        .unwrap()
    }

    #[test]
    fn strikes_and_glyphs_read_back_as_written() {
        let blob = sample();
        let face = BakedFace::parse(&blob).expect("valid");
        assert_eq!(face.sizes().collect::<Vec<_>>(), [12, 24]);
        let glyph = face.coverage(40, 12).unwrap();
        assert_eq!(glyph, bitmap(3, 1, vec![1, 2, 3]));
        assert_eq!(face.coverage(7, 12).unwrap().coverage, [255, 255]);
        assert_eq!(face.coverage(9, 24).unwrap().coverage, [0, 255, 128, 0]);
        assert!(matches!(face.glyph(3, 12), Lookup::Empty), "zero area");
        assert!(matches!(face.glyph(8, 12), Lookup::Empty), "not inked");
        assert!(face.coverage(8, 12).unwrap().is_empty());
        assert!(matches!(face.glyph(40, 16), Lookup::NoStrike));
        assert!(face.coverage(40, 16).is_none());
    }

    #[test]
    fn records_and_tables_are_four_byte_aligned() {
        let blob = sample();
        assert_eq!(&blob[..4], b"VBA8");
        let face = BakedFace::parse(&blob).unwrap();
        for index in 0..face.strikes {
            assert_eq!(face.strike(index).unwrap().records % 4, 0);
        }
    }

    #[test]
    fn an_unbaked_size_draws_from_the_nearest_baked_one() {
        let blob = sample();
        let face = BakedFace::parse(&blob).unwrap();
        assert_eq!(face.nearest_size(10), Some(12));
        assert_eq!(face.nearest_size(13), Some(24));
        assert_eq!(face.nearest_size(30), Some(24));
        let scaled = face.resampled(9, 18).unwrap();
        assert_eq!((scaled.width, scaled.height), (2, 2));
        assert_eq!((scaled.left, scaled.top), (-0.5625, 8.4375));
        assert!(
            face.resampled(9, 12).unwrap().is_empty(),
            "a baked size wins"
        );
        assert_eq!(face.resampled(40, 12), face.coverage(40, 12));
    }

    #[test]
    fn a_damaged_blob_is_refused() {
        let blob = sample();
        assert!(BakedFace::parse(&blob[..blob.len() - 1]).is_none());
        for at in [0, 4, 8, 12, 16, HEADER + 2] {
            let mut bad = blob.clone();
            bad[at] ^= 0x10;
            assert!(BakedFace::parse(&bad).is_none(), "byte {at}");
        }
        // Strikes out of order.
        let mut swapped = blob.clone();
        let (first, second) = (HEADER, HEADER + STRIKE);
        let a = swapped[first..first + 2].to_vec();
        let b = swapped[second..second + 2].to_vec();
        swapped[first..first + 2].copy_from_slice(&b);
        swapped[second..second + 2].copy_from_slice(&a);
        assert!(BakedFace::parse(&swapped).is_none());
    }

    #[test]
    fn a_strike_twice_or_a_glyph_twice_does_not_encode() {
        let one = |ppem| BakedStrike {
            ppem,
            glyphs: Vec::new(),
        };
        assert_eq!(
            BakedStrike::encode(&[one(12), one(12)]),
            Err(EncodeError::Size(12))
        );
        assert_eq!(BakedStrike::encode(&[one(0)]), Err(EncodeError::Size(0)));
        let twice = BakedStrike {
            ppem: 12,
            glyphs: vec![(5, bitmap(1, 1, vec![9])), (5, bitmap(1, 1, vec![9]))],
        };
        assert_eq!(
            BakedStrike::encode(&[twice]),
            Err(EncodeError::Glyph { ppem: 12, glyph: 5 })
        );
        let short = BakedStrike {
            ppem: 12,
            glyphs: vec![(5, bitmap(2, 2, vec![9]))],
        };
        assert_eq!(
            BakedStrike::encode(&[short]),
            Err(EncodeError::Bitmap { ppem: 12, glyph: 5 })
        );
    }

    #[cfg(feature = "raster")]
    #[test]
    fn a_baked_face_is_the_rasterizer_s_pixels() {
        const DEJAVU: &[u8] = include_bytes!("../../text/tests/fixtures/DejaVuSans-subset.ttf");
        let glyphs: Vec<u16> = (0..60).collect();
        let blob = bake_face(DEJAVU, 0, &glyphs, &[13, 32]).unwrap();
        let face = BakedFace::parse(&blob).unwrap();
        for ppem in [13, 32] {
            for &glyph in &glyphs {
                let raster = crate::rasterize_coverage(DEJAVU, 0, glyph, f32::from(ppem)).unwrap();
                let baked = face.coverage(glyph, ppem).unwrap();
                if raster.is_empty() {
                    assert!(baked.is_empty(), "glyph {glyph} at {ppem}");
                } else {
                    assert_eq!(baked, raster, "glyph {glyph} at {ppem}");
                }
            }
        }
    }

    #[cfg(feature = "raster")]
    #[test]
    fn an_unreadable_face_does_not_bake() {
        assert_eq!(
            bake_face(b"not a font", 0, &[1], &[12]),
            Err(BakeError::Outline { glyph: 1 })
        );
    }
}
