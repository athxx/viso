//! `CFF ` (version 1) subset.
//!
//! Glyph ids stay as they are: a dropped glyph's charstring becomes a bare
//! `endchar`, and every kept charstring, subroutine, charset, encoding and
//! FDSelect is copied unchanged. The table is laid out again — header, Name,
//! Top DICT, String and Global Subr INDEXes, then charset, Encoding,
//! FDSelect, CharStrings, Private DICTs with their local Subrs, and FDArray —
//! with every offset operand written in the fixed five-byte form, so the
//! DICTs' lengths do not depend on the offsets they carry. Subroutines are
//! kept whole: a kept glyph may call any of them.

use std::ops::Range;

use super::closure::GlyphSet;
use super::sfnt::{read_u16, read_u32};

const CHARSET: u16 = 15;
const ENCODING: u16 = 16;
const CHAR_STRINGS: u16 = 17;
const PRIVATE: u16 = 18;
const SUBRS: u16 = 19;
const FD_ARRAY: u16 = 0x0C24;
const FD_SELECT: u16 = 0x0C25;

/// Top and Font DICT operators whose operands are offsets.
pub(super) const TOP_OFFSETS: [u16; 6] = [
    CHARSET,
    ENCODING,
    CHAR_STRINGS,
    PRIVATE,
    FD_ARRAY,
    FD_SELECT,
];
pub(super) const PRIVATE_OFFSETS: [u16; 1] = [SUBRS];

/// A Type 2 charstring that draws nothing.
const ENDCHAR: &[u8] = &[14];

/// An INDEX: its byte range and each item's range, both within the table.
pub(super) struct Index {
    pub start: usize,
    pub end: usize,
    pub items: Vec<Range<usize>>,
}

fn truncated() -> String {
    "the CFF table is truncated".to_owned()
}

fn offset_at(data: &[u8], at: usize, size: u8) -> Option<usize> {
    let bytes = data.get(at..at + usize::from(size))?;
    Some(
        bytes
            .iter()
            .fold(0usize, |value, &byte| value << 8 | usize::from(byte)),
    )
}

pub(super) fn parse_index(data: &[u8], start: usize) -> Result<Index, String> {
    let count = usize::from(read_u16(data, start).ok_or_else(truncated)?);
    if count == 0 {
        return Ok(Index {
            start,
            end: start + 2,
            items: Vec::new(),
        });
    }
    let size = *data.get(start + 2).ok_or_else(truncated)?;
    if !(1..=4).contains(&size) {
        return Err(format!("a CFF INDEX has offset size {size}"));
    }
    let offsets = start + 3;
    // Offsets count from the byte before the item data.
    let base = offsets + (count + 1) * usize::from(size) - 1;
    let mut items = Vec::with_capacity(count);
    let mut previous = offset_at(data, offsets, size).ok_or_else(truncated)?;
    for item in 1..=count {
        let next =
            offset_at(data, offsets + item * usize::from(size), size).ok_or_else(truncated)?;
        if next < previous || previous == 0 {
            return Err("a CFF INDEX has unordered offsets".to_owned());
        }
        items.push(base + previous..base + next);
        previous = next;
    }
    let end = base + previous;
    if end > data.len() {
        return Err(truncated());
    }
    Ok(Index { start, end, items })
}

pub(super) fn write_index<T: AsRef<[u8]>>(items: &[T]) -> Vec<u8> {
    if items.is_empty() {
        return vec![0, 0];
    }
    let total: usize = items.iter().map(|item| item.as_ref().len()).sum::<usize>() + 1;
    let size: u8 = match total {
        0..=0xFF => 1,
        0x100..=0xFFFF => 2,
        0x1_0000..=0xFF_FFFF => 3,
        _ => 4,
    };
    let mut out = Vec::with_capacity(3 + (items.len() + 1) * usize::from(size) + total);
    out.extend_from_slice(&(items.len() as u16).to_be_bytes());
    out.push(size);
    let mut offset = 1usize;
    let push_offset = |out: &mut Vec<u8>, offset: usize| {
        out.extend_from_slice(&(offset as u32).to_be_bytes()[4 - usize::from(size)..]);
    };
    push_offset(&mut out, offset);
    for item in items {
        offset += item.as_ref().len();
        push_offset(&mut out, offset);
    }
    for item in items {
        out.extend_from_slice(item.as_ref());
    }
    out
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum Operand {
    Int(i32),
    /// A real number, as its encoded bytes (the leading 30 included).
    Real(Vec<u8>),
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Entry {
    pub op: u16,
    pub operands: Vec<Operand>,
}

pub(super) fn parse_dict(data: &[u8]) -> Result<Vec<Entry>, String> {
    let bad = || "a CFF DICT is malformed".to_owned();
    let mut entries = Vec::new();
    let mut operands = Vec::new();
    let mut at = 0;
    while at < data.len() {
        let b0 = data[at];
        let byte = |at: usize| data.get(at).copied().ok_or_else(bad);
        match b0 {
            0..=21 => {
                let op = if b0 == 12 {
                    at += 1;
                    0x0C00 | u16::from(byte(at)?)
                } else {
                    u16::from(b0)
                };
                at += 1;
                entries.push(Entry {
                    op,
                    operands: std::mem::take(&mut operands),
                });
            }
            28 => {
                operands.push(Operand::Int(i32::from(i16::from_be_bytes([
                    byte(at + 1)?,
                    byte(at + 2)?,
                ]))));
                at += 3;
            }
            29 => {
                let value = read_u32(data, at + 1).ok_or_else(bad)? as i32;
                operands.push(Operand::Int(value));
                at += 5;
            }
            30 => {
                let start = at;
                at += 1;
                loop {
                    let nibbles = byte(at)?;
                    at += 1;
                    if nibbles >> 4 == 0xF || nibbles & 0xF == 0xF {
                        break;
                    }
                }
                operands.push(Operand::Real(data[start..at].to_vec()));
            }
            32..=246 => {
                operands.push(Operand::Int(i32::from(b0) - 139));
                at += 1;
            }
            247..=250 => {
                let value = (i32::from(b0) - 247) * 256 + i32::from(byte(at + 1)?) + 108;
                operands.push(Operand::Int(value));
                at += 2;
            }
            251..=254 => {
                let value = -(i32::from(b0) - 251) * 256 - i32::from(byte(at + 1)?) - 108;
                operands.push(Operand::Int(value));
                at += 2;
            }
            _ => return Err(bad()),
        }
    }
    Ok(entries)
}

fn write_int(out: &mut Vec<u8>, value: i32, fixed: bool) {
    match value {
        _ if fixed => {
            out.push(29);
            out.extend_from_slice(&value.to_be_bytes());
        }
        -107..=107 => out.push((value + 139) as u8),
        108..=1131 => {
            let value = value - 108;
            out.extend_from_slice(&[(value / 256 + 247) as u8, (value % 256) as u8]);
        }
        -1131..=-108 => {
            let value = -value - 108;
            out.extend_from_slice(&[(value / 256 + 251) as u8, (value % 256) as u8]);
        }
        -32768..=32767 => {
            out.push(28);
            out.extend_from_slice(&(value as i16).to_be_bytes());
        }
        _ => {
            out.push(29);
            out.extend_from_slice(&value.to_be_bytes());
        }
    }
}

/// `entries` encoded; the operands of `offsets` operators take the fixed
/// five-byte form.
pub(super) fn write_dict(entries: &[Entry], offsets: &[u16]) -> Vec<u8> {
    let mut out = Vec::new();
    for entry in entries {
        let fixed = offsets.contains(&entry.op);
        for operand in &entry.operands {
            match operand {
                Operand::Int(value) => write_int(&mut out, *value, fixed),
                Operand::Real(bytes) => out.extend_from_slice(bytes),
            }
        }
        if entry.op >= 0x0C00 {
            out.extend_from_slice(&[12, entry.op as u8]);
        } else {
            out.push(entry.op as u8);
        }
    }
    out
}

fn ints(entries: &[Entry], op: u16) -> Option<Vec<i32>> {
    let entry = entries.iter().find(|entry| entry.op == op)?;
    entry
        .operands
        .iter()
        .map(|operand| match operand {
            Operand::Int(value) => Some(*value),
            Operand::Real(_) => None,
        })
        .collect()
}

fn offset(entries: &[Entry], op: u16) -> Result<Option<usize>, String> {
    match ints(entries, op).as_deref() {
        None => Ok(None),
        Some([value]) if *value >= 0 => Ok(Some(*value as usize)),
        Some(_) => Err("a CFF DICT offset is malformed".to_owned()),
    }
}

fn set(entries: &mut [Entry], op: u16, values: &[i32]) {
    if let Some(entry) = entries.iter_mut().find(|entry| entry.op == op) {
        entry.operands = values.iter().map(|&value| Operand::Int(value)).collect();
    }
}

fn slice(data: &[u8], range: Range<usize>) -> Result<&[u8], String> {
    data.get(range).ok_or_else(truncated)
}

fn charset_len(data: &[u8], at: usize, glyphs: usize) -> Result<usize, String> {
    let format = *data.get(at).ok_or_else(truncated)?;
    let named = glyphs.saturating_sub(1);
    match format {
        0 => Ok(1 + 2 * named),
        1 | 2 => {
            let mut covered = 0;
            let mut cursor = at + 1;
            while covered < named {
                cursor += 2;
                let left = if format == 1 {
                    usize::from(*data.get(cursor).ok_or_else(truncated)?)
                } else {
                    usize::from(read_u16(data, cursor).ok_or_else(truncated)?)
                };
                cursor += usize::from(format);
                covered += left + 1;
            }
            Ok(cursor - at)
        }
        _ => Err(format!("the CFF charset has format {format}")),
    }
}

fn encoding_len(data: &[u8], at: usize) -> Result<usize, String> {
    let format = *data.get(at).ok_or_else(truncated)?;
    let count = usize::from(*data.get(at + 1).ok_or_else(truncated)?);
    let mut length = match format & 0x7F {
        0 => 2 + count,
        1 => 2 + 2 * count,
        other => return Err(format!("the CFF Encoding has format {other}")),
    };
    if format & 0x80 != 0 {
        let supplements = usize::from(*data.get(at + length).ok_or_else(truncated)?);
        length += 1 + 3 * supplements;
    }
    Ok(length)
}

fn fd_select_len(data: &[u8], at: usize, glyphs: usize) -> Result<usize, String> {
    match *data.get(at).ok_or_else(truncated)? {
        0 => Ok(1 + glyphs),
        3 => {
            let ranges = usize::from(read_u16(data, at + 1).ok_or_else(truncated)?);
            Ok(1 + 2 + 3 * ranges + 2)
        }
        format => Err(format!("the CFF FDSelect has format {format}")),
    }
}

/// A Private DICT with its local Subrs, ready to place: the DICT (Subrs
/// pointing just past it) followed by the Subrs INDEX.
struct Private {
    dict_len: usize,
    bytes: Vec<u8>,
}

fn private(data: &[u8], entries: &[Entry]) -> Result<Option<Private>, String> {
    let Some(values) = ints(entries, PRIVATE) else {
        return Ok(None);
    };
    let [size, at] = values[..] else {
        return Err("the CFF Private operator is malformed".to_owned());
    };
    let (size, at) = (size.max(0) as usize, at.max(0) as usize);
    let mut dict = parse_dict(slice(data, at..at + size)?)?;
    let subrs = match offset(&dict, SUBRS)? {
        Some(relative) => {
            let subrs = parse_index(data, at + relative)?;
            Some(slice(data, subrs.start..subrs.end)?)
        }
        None => None,
    };
    let dict_len = write_dict(&dict, &PRIVATE_OFFSETS).len();
    set(&mut dict, SUBRS, &[dict_len as i32]);
    let mut bytes = write_dict(&dict, &PRIVATE_OFFSETS);
    bytes.extend_from_slice(subrs.unwrap_or_default());
    Ok(Some(Private { dict_len, bytes }))
}

/// The table with every glyph outside `keep` (and not `.notdef`) reduced to
/// an empty charstring.
pub(super) fn subset(data: &[u8], glyphs: u16, keep: &GlyphSet) -> Result<Vec<u8>, String> {
    let major = *data.first().ok_or_else(truncated)?;
    if major != 1 {
        return Err(format!("CFF major version {major} is not CFF 1"));
    }
    let header = usize::from(*data.get(2).ok_or_else(truncated)?);
    let names = parse_index(data, header)?;
    let tops = parse_index(data, names.end)?;
    let [top_range] = &tops.items[..] else {
        return Err("the CFF table does not hold exactly one font".to_owned());
    };
    let strings = parse_index(data, tops.end)?;
    let global_subrs = parse_index(data, strings.end)?;
    let mut top = parse_dict(slice(data, top_range.clone())?)?;
    let count = usize::from(glyphs);

    let charstrings_at = offset(&top, CHAR_STRINGS)?.ok_or("the CFF font has no CharStrings")?;
    let charstrings = parse_index(data, charstrings_at)?;
    if charstrings.items.len() != count {
        return Err(format!(
            "the CFF font has {} charstrings for {count} glyphs",
            charstrings.items.len()
        ));
    }
    let custom = |op: u16, predefined: usize| -> Result<Option<usize>, String> {
        Ok(offset(&top, op)?.filter(|&at| at > predefined))
    };
    let charset = match custom(CHARSET, 2)? {
        Some(at) => Some(slice(data, at..at + charset_len(data, at, count)?)?),
        None => None,
    };
    let encoding = match custom(ENCODING, 1)? {
        Some(at) => Some(slice(data, at..at + encoding_len(data, at)?)?),
        None => None,
    };
    let fd_select = match offset(&top, FD_SELECT)? {
        Some(at) => Some(slice(data, at..at + fd_select_len(data, at, count)?)?),
        None => None,
    };
    let mut fonts = match offset(&top, FD_ARRAY)? {
        Some(at) => parse_index(data, at)?
            .items
            .into_iter()
            .map(|range| parse_dict(slice(data, range)?))
            .collect::<Result<Vec<_>, _>>()?,
        None => Vec::new(),
    };
    let top_private = private(data, &top)?;
    let font_privates = fonts
        .iter()
        .map(|font| private(data, font))
        .collect::<Result<Vec<_>, _>>()?;

    let mut kept = Vec::with_capacity(count);
    for (glyph, range) in charstrings.items.iter().enumerate() {
        kept.push(if glyph == 0 || keep.contains(glyph as u16) {
            slice(data, range.clone())?
        } else {
            ENDCHAR
        });
    }
    let charstrings = write_index(&kept);

    // Fixed-width offsets make the Top DICT's length final before the
    // offsets are known.
    let top_len = write_index(&[write_dict(&top, &TOP_OFFSETS)]).len();
    let prefix = slice(data, 0..header)?.len()
        + (names.end - names.start)
        + top_len
        + (strings.end - strings.start)
        + (global_subrs.end - global_subrs.start);
    let mut body = Vec::new();
    let place = |body: &mut Vec<u8>, bytes: &[u8]| {
        let at = prefix + body.len();
        body.extend_from_slice(bytes);
        at as i32
    };
    if let Some(bytes) = charset {
        let at = place(&mut body, bytes);
        set(&mut top, CHARSET, &[at]);
    }
    if let Some(bytes) = encoding {
        let at = place(&mut body, bytes);
        set(&mut top, ENCODING, &[at]);
    }
    if let Some(bytes) = fd_select {
        let at = place(&mut body, bytes);
        set(&mut top, FD_SELECT, &[at]);
    }
    let at = place(&mut body, &charstrings);
    set(&mut top, CHAR_STRINGS, &[at]);
    if let Some(private) = &top_private {
        let at = place(&mut body, &private.bytes);
        set(&mut top, PRIVATE, &[private.dict_len as i32, at]);
    }
    if !fonts.is_empty() {
        let encoded = |fonts: &[Vec<Entry>]| {
            write_index(
                &fonts
                    .iter()
                    .map(|font| write_dict(font, &TOP_OFFSETS))
                    .collect::<Vec<_>>(),
            )
        };
        let array_at = prefix + body.len();
        let mut private_at = array_at + encoded(&fonts).len();
        for (font, private) in fonts.iter_mut().zip(&font_privates) {
            if let Some(private) = private {
                set(font, PRIVATE, &[private.dict_len as i32, private_at as i32]);
                private_at += private.bytes.len();
            }
        }
        place(&mut body, &encoded(&fonts));
        for private in font_privates.iter().flatten() {
            place(&mut body, &private.bytes);
        }
        set(&mut top, FD_ARRAY, &[array_at as i32]);
    }

    let mut out = Vec::with_capacity(prefix + body.len());
    out.extend_from_slice(slice(data, 0..header)?);
    out.extend_from_slice(slice(data, names.start..names.end)?);
    let top_index = write_index(&[write_dict(&top, &TOP_OFFSETS)]);
    debug_assert_eq!(top_index.len(), top_len);
    out.extend_from_slice(&top_index);
    out.extend_from_slice(slice(data, strings.start..strings.end)?);
    out.extend_from_slice(slice(data, global_subrs.start..global_subrs.end)?);
    out.extend_from_slice(&body);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dict_numbers_round_trip_in_every_encoding() {
        let values = [
            0, 107, -107, 108, 1131, -108, -1131, 1132, -32768, 32767, 70_000, -70_000,
        ];
        let entries = vec![Entry {
            op: 0x0C1E,
            operands: values.iter().map(|&value| Operand::Int(value)).collect(),
        }];
        assert_eq!(parse_dict(&write_dict(&entries, &[])).unwrap(), entries);
        let fixed = write_dict(&entries, &[0x0C1E]);
        assert_eq!(fixed.len(), 5 * values.len() + 2);
        assert_eq!(parse_dict(&fixed).unwrap(), entries);
    }

    #[test]
    fn a_real_operand_is_kept_as_encoded() {
        // -2.25: nibbles e 2 a 2 5 f.
        let dict = [30, 0xE2, 0xA2, 0x5F, 1];
        let entries = parse_dict(&dict).unwrap();
        assert_eq!(
            entries[0].operands,
            [Operand::Real(vec![30, 0xE2, 0xA2, 0x5F])]
        );
        assert_eq!(write_dict(&entries, &[]), dict);
    }

    #[test]
    fn an_index_round_trips() {
        let items: [&[u8]; 3] = [b"a", b"", &[7; 300]];
        let bytes = write_index(&items);
        let index = parse_index(&bytes, 0).unwrap();
        assert_eq!(index.end, bytes.len());
        let read: Vec<&[u8]> = index
            .items
            .iter()
            .map(|range| &bytes[range.clone()])
            .collect();
        assert_eq!(read, items);
        assert_eq!(parse_index(&write_index::<&[u8]>(&[]), 0).unwrap().end, 2);
    }
}
