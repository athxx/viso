//! The subset `cmap`: format 4 for the BMP, format 12 for the full
//! repertoire, and the original format 14 variation sequences.

use super::sfnt::{read_u16, read_u32};

/// Two array segments closer than this many unmapped code points merge: an
/// array slot costs 2 bytes, a segment 8.
const MERGE_GAP: u32 = 3;
/// A run of code points with consecutive glyphs at least this long gets a
/// delta segment of its own instead of array slots.
const DELTA_RUN: usize = 4;

/// A `cmap` mapping `mappings` (sorted by code point, glyph ids non-zero):
/// format 4 under platform 0 encoding 3 and platform 3 encoding 1, format 12
/// under platform 0 encoding 4 and platform 3 encoding 10, and `variations`
/// (a format 14 subtable) under platform 0 encoding 5. Format 4 is left out
/// when the BMP mapping does not fit its 16-bit length.
pub(super) fn build(mappings: &[(u32, u16)], variations: Option<&[u8]>) -> Vec<u8> {
    let format4 = format4(mappings);
    let format12 = format12(mappings);
    let mut subtables: Vec<(u16, u16, usize)> = Vec::new();
    let mut bodies: Vec<&[u8]> = Vec::new();
    let format4_at = format4.as_ref().map(|body| {
        bodies.push(body);
        bodies.len() - 1
    });
    bodies.push(&format12);
    let format12_at = bodies.len() - 1;
    let variations_at = variations.map(|body| {
        bodies.push(body);
        bodies.len() - 1
    });
    if let Some(at) = format4_at {
        subtables.push((0, 3, at));
    }
    subtables.push((0, 4, format12_at));
    if let Some(at) = variations_at {
        subtables.push((0, 5, at));
    }
    if let Some(at) = format4_at {
        subtables.push((3, 1, at));
    }
    subtables.push((3, 10, format12_at));

    let mut offsets = Vec::with_capacity(bodies.len());
    let mut offset = 4 + 8 * subtables.len();
    for body in &bodies {
        offsets.push(offset as u32);
        offset += body.len().next_multiple_of(4);
    }
    let mut out = Vec::with_capacity(offset);
    push16(&mut out, 0);
    push16(&mut out, subtables.len() as u16);
    for &(platform, encoding, body) in &subtables {
        push16(&mut out, platform);
        push16(&mut out, encoding);
        push32(&mut out, offsets[body]);
    }
    for body in bodies {
        out.extend_from_slice(body);
        out.resize(out.len().next_multiple_of(4), 0);
    }
    out
}

/// The first format 14 subtable of `cmap`.
pub(super) fn variations(cmap: &[u8]) -> Option<&[u8]> {
    let count = read_u16(cmap, 2)?;
    (0..usize::from(count)).find_map(|record| {
        let offset = read_u32(cmap, 4 + 8 * record + 4)? as usize;
        if read_u16(cmap, offset)? != 14 {
            return None;
        }
        let length = read_u32(cmap, offset + 2)? as usize;
        cmap.get(offset..offset.checked_add(length)?)
    })
}

/// The glyphs format 14 subtable `variations` maps a variation sequence of a
/// base code point accepted by `kept` to.
pub(super) fn variation_glyphs(variations: &[u8], kept: impl Fn(u32) -> bool) -> Vec<u16> {
    let mut glyphs = Vec::new();
    let records = read_u32(variations, 6).unwrap_or(0) as usize;
    for record in 0..records {
        let at = 10 + 11 * record;
        let Some(non_default) = read_u32(variations, at + 7) else {
            break;
        };
        let table = non_default as usize;
        if table == 0 {
            continue;
        }
        let mappings = read_u32(variations, table).unwrap_or(0) as usize;
        for mapping in 0..mappings {
            let entry = table + 4 + 5 * mapping;
            let (Some(base), Some(glyph)) =
                (read_u24(variations, entry), read_u16(variations, entry + 3))
            else {
                break;
            };
            if kept(base) {
                glyphs.push(glyph);
            }
        }
    }
    glyphs
}

fn read_u24(data: &[u8], at: usize) -> Option<u32> {
    data.get(at..at + 3)
        .map(|bytes| u32::from_be_bytes([0, bytes[0], bytes[1], bytes[2]]))
}

fn push16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_be_bytes());
}

fn push32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_be_bytes());
}

/// Maximal runs of consecutive code points mapped to consecutive glyphs, as
/// `(first code point, first glyph, length)`.
fn runs(mappings: &[(u32, u16)]) -> Vec<(u32, u16, usize)> {
    let mut runs: Vec<(u32, u16, usize)> = Vec::new();
    for &(code, glyph) in mappings {
        if let Some((start, first, length)) = runs.last_mut()
            && *start + *length as u32 == code
            && u32::from(*first) + *length as u32 == u32::from(glyph)
        {
            *length += 1;
        } else {
            runs.push((code, glyph, 1));
        }
    }
    runs
}

enum Segment {
    /// Code points `start..=end` map to `code + delta`.
    Delta { start: u32, end: u32, delta: u16 },
    /// Code points `start..` map through `glyphs` (0 where unmapped).
    Array { start: u32, glyphs: Vec<u16> },
}

impl Segment {
    fn end(&self) -> u32 {
        match self {
            Self::Delta { end, .. } => *end,
            Self::Array { start, glyphs } => start + glyphs.len() as u32 - 1,
        }
    }
}

fn format4(mappings: &[(u32, u16)]) -> Option<Vec<u8>> {
    let bmp: Vec<(u32, u16)> = mappings
        .iter()
        .copied()
        .filter(|&(code, _)| code < 0xFFFF)
        .collect();
    let mut segments: Vec<Segment> = Vec::new();
    for (start, first, length) in runs(&bmp) {
        let delta = first.wrapping_sub(start as u16);
        if length >= DELTA_RUN {
            segments.push(Segment::Delta {
                start,
                end: start + length as u32 - 1,
                delta,
            });
            continue;
        }
        let run = (0..length).map(|step| first + step as u16);
        match segments.last_mut() {
            Some(Segment::Array {
                start: open,
                glyphs,
            }) if start - (*open + glyphs.len() as u32) <= MERGE_GAP => {
                let gap = start - (*open + glyphs.len() as u32);
                glyphs.extend(std::iter::repeat_n(0, gap as usize));
                glyphs.extend(run);
            }
            _ => segments.push(Segment::Array {
                start,
                glyphs: run.collect(),
            }),
        }
    }
    // A lone short run reads cheaper as a delta segment.
    for segment in &mut segments {
        if let Segment::Array { start, glyphs } = segment
            && glyphs.iter().enumerate().all(|(step, &glyph)| {
                glyph != 0 && u32::from(glyph) == u32::from(glyphs[0]) + step as u32
            })
        {
            *segment = Segment::Delta {
                start: *start,
                end: *start + glyphs.len() as u32 - 1,
                delta: glyphs[0].wrapping_sub(*start as u16),
            };
        }
    }
    segments.push(Segment::Delta {
        start: 0xFFFF,
        end: 0xFFFF,
        delta: 1,
    });

    let count = segments.len();
    let array_slots: usize = segments
        .iter()
        .map(|segment| match segment {
            Segment::Array { glyphs, .. } => glyphs.len(),
            Segment::Delta { .. } => 0,
        })
        .sum();
    let length = 16 + 8 * count + 2 * array_slots;
    if length > usize::from(u16::MAX) {
        return None;
    }
    let mut selector = 0u16;
    while 2usize << selector <= count {
        selector += 1;
    }
    let range = 2 * (1u16 << selector);
    let mut out = Vec::with_capacity(length);
    push16(&mut out, 4);
    push16(&mut out, length as u16);
    push16(&mut out, 0);
    push16(&mut out, (2 * count) as u16);
    push16(&mut out, range);
    push16(&mut out, selector);
    push16(&mut out, (2 * count) as u16 - range);
    for segment in &segments {
        push16(&mut out, segment.end() as u16);
    }
    push16(&mut out, 0);
    for segment in &segments {
        let start = match segment {
            Segment::Delta { start, .. } | Segment::Array { start, .. } => *start,
        };
        push16(&mut out, start as u16);
    }
    for segment in &segments {
        let delta = match segment {
            Segment::Delta { delta, .. } => *delta,
            Segment::Array { .. } => 0,
        };
        push16(&mut out, delta);
    }
    // An array segment's range offset counts bytes from its own entry to its
    // first slot in the glyph array that follows the offsets.
    let mut slot = 0;
    for (index, segment) in segments.iter().enumerate() {
        match segment {
            Segment::Delta { .. } => push16(&mut out, 0),
            Segment::Array { glyphs, .. } => {
                push16(&mut out, (2 * (count - index + slot)) as u16);
                slot += glyphs.len();
            }
        }
    }
    for segment in &segments {
        if let Segment::Array { glyphs, .. } = segment {
            for &glyph in glyphs {
                push16(&mut out, glyph);
            }
        }
    }
    Some(out)
}

fn format12(mappings: &[(u32, u16)]) -> Vec<u8> {
    let groups = runs(mappings);
    let mut out = Vec::with_capacity(16 + 12 * groups.len());
    push16(&mut out, 12);
    push16(&mut out, 0);
    push32(&mut out, (16 + 12 * groups.len()) as u32);
    push32(&mut out, 0);
    push32(&mut out, groups.len() as u32);
    for (start, first, length) in groups {
        push32(&mut out, start);
        push32(&mut out, start + length as u32 - 1);
        push32(&mut out, u32::from(first));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lookups(cmap: &[u8]) -> Vec<(u16, Vec<Option<u16>>)> {
        let table = ttf_parser::cmap::Table::parse(cmap).unwrap();
        let probes = [
            0x20, 0x41, 0x42, 0x43, 0x44, 0x4E00, 0x4E03, 0x4E05, 0x1F600,
        ];
        table
            .subtables
            .into_iter()
            .map(|subtable| {
                let format = match subtable.format {
                    ttf_parser::cmap::Format::SegmentMappingToDeltaValues(_) => 4,
                    ttf_parser::cmap::Format::SegmentedCoverage(_) => 12,
                    ttf_parser::cmap::Format::UnicodeVariationSequences(_) => 14,
                    _ => 0,
                };
                let glyphs = probes
                    .iter()
                    .map(|&code| subtable.glyph_index(code).map(|glyph| glyph.0))
                    .collect();
                (format, glyphs)
            })
            .collect()
    }

    #[test]
    fn every_subtable_maps_the_same_glyphs() {
        // A delta run (A–D), scattered ideographs merged into one array
        // segment, and a code point beyond the BMP.
        let mappings = [
            (0x20, 3),
            (0x41, 10),
            (0x42, 11),
            (0x43, 12),
            (0x44, 13),
            (0x4E00, 40),
            (0x4E03, 7),
            (0x4E05, 90),
            (0x1F600, 500),
        ];
        let cmap = build(&mappings, None);
        let expected: Vec<Option<u16>> = vec![
            Some(3),
            Some(10),
            Some(11),
            Some(12),
            Some(13),
            Some(40),
            Some(7),
            Some(90),
            Some(500),
        ];
        let subtables = lookups(&cmap);
        assert_eq!(subtables.len(), 4);
        for (format, glyphs) in subtables {
            let mut wanted = expected.clone();
            if format == 4 {
                wanted[8] = None;
            }
            assert_eq!(glyphs, wanted, "format {format}");
        }
    }

    #[test]
    fn gaps_inside_a_merged_segment_stay_unmapped() {
        let cmap = build(&[(0x4E00, 5), (0x4E02, 9)], None);
        let table = ttf_parser::cmap::Table::parse(&cmap).unwrap();
        let format4 = table.subtables.get(0).unwrap();
        assert_eq!(format4.glyph_index(0x4E01), None);
        assert_eq!(format4.glyph_index(0x4E02).map(|glyph| glyph.0), Some(9));
    }

    #[test]
    fn a_bmp_too_scattered_for_format_4_keeps_format_12() {
        let mappings: Vec<(u32, u16)> = (0..20_000u32)
            .map(|step| (step * 3, 1 + (step % 7) as u16 * 3))
            .filter(|&(code, _)| code < 0xFFFF)
            .collect();
        let cmap = build(&mappings, None);
        let subtables = lookups(&cmap);
        assert!(subtables.iter().all(|(format, _)| *format == 12));
    }
}
