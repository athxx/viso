//! sfnt containers: a face's table directory, a standalone face written from
//! its tables, and standalone faces packed into a collection.

use std::collections::HashMap;

pub(super) type Tag = [u8; 4];

/// The sum `head.checkSumAdjustment` brings a whole font to.
const CHECKSUM_MAGIC: u32 = 0xB1B0_AFBA;
/// Offset of `checkSumAdjustment` in `head`.
const HEAD_ADJUSTMENT: usize = 8;

/// One face of a font file: its sfnt version and tables, in directory order.
pub(super) struct FaceTables<'a> {
    pub version: u32,
    pub tables: Vec<(Tag, &'a [u8])>,
}

impl<'a> FaceTables<'a> {
    pub fn get(&self, tag: &Tag) -> Option<&'a [u8]> {
        self.tables
            .iter()
            .find(|(held, _)| held == tag)
            .map(|&(_, data)| data)
    }

    pub fn has(&self, tag: &Tag) -> bool {
        self.get(tag).is_some()
    }
}

pub(super) fn read_u16(data: &[u8], at: usize) -> Option<u16> {
    data.get(at..at + 2)
        .map(|bytes| u16::from_be_bytes([bytes[0], bytes[1]]))
}

pub(super) fn read_u32(data: &[u8], at: usize) -> Option<u32> {
    data.get(at..at + 4)
        .map(|bytes| u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

pub(super) fn is_collection(data: &[u8]) -> bool {
    data.starts_with(b"ttcf")
}

/// The tables of face `index` of `data`, a single sfnt or a collection.
pub(super) fn face_tables(data: &[u8], index: u32) -> Result<FaceTables<'_>, String> {
    let start = if is_collection(data) {
        let count = read_u32(data, 8).ok_or("truncated collection header")?;
        if index >= count {
            return Err(format!("the collection has no face {index}"));
        }
        read_u32(data, 12 + 4 * index as usize).ok_or("truncated collection header")? as usize
    } else if index == 0 {
        0
    } else {
        return Err(format!("a single face has no face {index}"));
    };
    let truncated = || "truncated table directory".to_owned();
    let version = read_u32(data, start).ok_or_else(truncated)?;
    let count = read_u16(data, start + 4).ok_or_else(truncated)?;
    let mut tables = Vec::with_capacity(count.into());
    for record in 0..usize::from(count) {
        let at = start + 12 + 16 * record;
        let tag: Tag = data
            .get(at..at + 4)
            .ok_or_else(truncated)?
            .try_into()
            .map_err(|_| truncated())?;
        let offset = read_u32(data, at + 8).ok_or_else(truncated)? as usize;
        let length = read_u32(data, at + 12).ok_or_else(truncated)? as usize;
        let table = offset
            .checked_add(length)
            .and_then(|end| data.get(offset..end))
            .ok_or_else(|| format!("table `{}` lies outside the file", tag_name(&tag)))?;
        tables.push((tag, table));
    }
    Ok(FaceTables { version, tables })
}

pub(super) fn tag_name(tag: &Tag) -> String {
    String::from_utf8_lossy(tag).into_owned()
}

/// The sfnt checksum of `data`: the wrapping sum of its big-endian words, the
/// last zero-padded.
pub(super) fn checksum(data: &[u8]) -> u32 {
    data.chunks(4).fold(0u32, |sum, chunk| {
        let mut word = [0; 4];
        word[..chunk.len()].copy_from_slice(chunk);
        sum.wrapping_add(u32::from_be_bytes(word))
    })
}

fn pad4(out: &mut Vec<u8>) {
    out.resize(out.len().next_multiple_of(4), 0);
}

/// `searchRange`, `entrySelector` and `rangeShift` for `count` table records.
fn search_fields(count: u16) -> [u16; 3] {
    let mut selector = 0u16;
    while 2u32 << selector <= u32::from(count) {
        selector += 1;
    }
    let range = (1u16 << selector) * 16;
    [range, selector, (count * 16).saturating_sub(range)]
}

fn directory_header(out: &mut Vec<u8>, version: u32, count: u16) {
    out.extend_from_slice(&version.to_be_bytes());
    out.extend_from_slice(&count.to_be_bytes());
    for field in search_fields(count) {
        out.extend_from_slice(&field.to_be_bytes());
    }
}

/// A standalone sfnt of `tables`: records sorted by tag, each table 4-byte
/// aligned, table checksums and `head.checkSumAdjustment` computed.
pub(super) fn write_face(version: u32, mut tables: Vec<(Tag, Vec<u8>)>) -> Vec<u8> {
    tables.sort_by_key(|(tag, _)| *tag);
    if let Some((_, head)) = tables.iter_mut().find(|(tag, _)| tag == b"head")
        && head.len() >= HEAD_ADJUSTMENT + 4
    {
        head[HEAD_ADJUSTMENT..HEAD_ADJUSTMENT + 4].fill(0);
    }
    let count = tables.len() as u16;
    let mut out = Vec::new();
    directory_header(&mut out, version, count);
    let mut offset = 12 + 16 * tables.len();
    for (tag, data) in &tables {
        out.extend_from_slice(tag);
        out.extend_from_slice(&checksum(data).to_be_bytes());
        out.extend_from_slice(&(offset as u32).to_be_bytes());
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        offset += data.len().next_multiple_of(4);
    }
    let mut head_at = None;
    for (tag, data) in &tables {
        if tag == b"head" {
            head_at = Some(out.len());
        }
        out.extend_from_slice(data);
        pad4(&mut out);
    }
    if let Some(at) = head_at
        && out.len() >= at + HEAD_ADJUSTMENT + 4
    {
        let adjustment = CHECKSUM_MAGIC.wrapping_sub(checksum(&out));
        out[at + HEAD_ADJUSTMENT..at + HEAD_ADJUSTMENT + 4]
            .copy_from_slice(&adjustment.to_be_bytes());
    }
    out
}

/// A collection of standalone `faces`, in order. Tables with identical bytes
/// are stored once and shared by every face that holds them.
pub(super) fn write_collection(faces: &[Vec<u8>]) -> Result<Vec<u8>, String> {
    let parsed = faces
        .iter()
        .map(|face| face_tables(face, 0))
        .collect::<Result<Vec<_>, _>>()?;
    let mut out = Vec::new();
    out.extend_from_slice(b"ttcf");
    out.extend_from_slice(&0x0001_0000u32.to_be_bytes());
    out.extend_from_slice(&(faces.len() as u32).to_be_bytes());
    let mut directory = 12 + 4 * faces.len();
    for face in &parsed {
        out.extend_from_slice(&(directory as u32).to_be_bytes());
        directory += 12 + 16 * face.tables.len();
    }
    // Table data follows every directory; equal tables share one copy.
    let mut stored: HashMap<&[u8], u32> = HashMap::new();
    let mut data = Vec::new();
    let mut records = Vec::new();
    for face in &parsed {
        let mut entries = Vec::with_capacity(face.tables.len());
        for &(tag, table) in &face.tables {
            let offset = *stored.entry(table).or_insert_with(|| {
                let offset = (directory + data.len()) as u32;
                data.extend_from_slice(table);
                pad4(&mut data);
                offset
            });
            entries.push((tag, checksum(table), offset, table.len() as u32));
        }
        records.push((face.version, entries));
    }
    for (version, entries) in records {
        directory_header(&mut out, version, entries.len() as u16);
        for (tag, sum, offset, length) in entries {
            out.extend_from_slice(&tag);
            out.extend_from_slice(&sum.to_be_bytes());
            out.extend_from_slice(&offset.to_be_bytes());
            out.extend_from_slice(&length.to_be_bytes());
        }
    }
    out.extend_from_slice(&data);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_fields_follow_the_largest_power_of_two() {
        assert_eq!(search_fields(1), [16, 0, 0]);
        assert_eq!(search_fields(9), [128, 3, 16]);
        assert_eq!(search_fields(16), [256, 4, 0]);
    }

    #[test]
    fn a_written_face_sums_to_the_magic_number() {
        let mut head = vec![0; 54];
        head[12..16].copy_from_slice(&0x5F0F_3CF5u32.to_be_bytes());
        let face = write_face(
            0x0001_0000,
            vec![(*b"zzzz", vec![1, 2, 3]), (*b"head", head)],
        );
        assert_eq!(checksum(&face), CHECKSUM_MAGIC);
        let tables = face_tables(&face, 0).unwrap();
        let tags: Vec<_> = tables.tables.iter().map(|(tag, _)| *tag).collect();
        assert_eq!(tags, [*b"head", *b"zzzz"]);
        assert_eq!(tables.get(b"zzzz"), Some(&[1, 2, 3][..]));
    }
}
