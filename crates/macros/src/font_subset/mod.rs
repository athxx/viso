//! Build-time font subsetting for embedded targets (§37.8).
//!
//! `viso build` for an embedded target sets two variables for the compiler:
//!
//! - `VISO_FONT_CHARSET`: the characters dynamic text may need, as
//!   comma-separated items — `ascii`, `latin`, `greek`, `cyrillic`, `gb2312`,
//!   a code point `U+XXXX` or a range `U+XXXX-YYYY` (see [`charset`]);
//! - `VISO_FONT_TEXT`: a UTF-8 file holding the static text of the build,
//!   every character of which some packaged face must map.
//!
//! With either set, each packaged face keeps the declared characters, the
//! text's characters, and space, no-break space and U+FFFD; with neither, the
//! faces are embedded as they are. A subset keeps glyph ids: dropped glyphs
//! become empty, so `GSUB`, `GPOS`, `kern`, `hmtx` and every other table
//! indexed by glyph stay valid byte for byte.

mod cff;
mod charset;
mod closure;
mod cmap;
#[rustfmt::skip]
mod gb2312_table;
mod sfnt;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use ttf_parser::Face;

use closure::GlyphSet;
use sfnt::{Tag, read_u16, read_u32};

pub(crate) const CHARSET_VAR: &str = "VISO_FONT_CHARSET";
pub(crate) const TEXT_VAR: &str = "VISO_FONT_TEXT";

/// Code points every subset keeps where the face maps them.
const ALWAYS_KEPT: [char; 3] = [' ', '\u{A0}', '\u{FFFD}'];

/// Tables a subset drops: device metrics per glyph, which a renderer derives
/// again, and a signature the new bytes would break.
const DROPPED: [Tag; 4] = [*b"hdmx", *b"LTSH", *b"VDMX", *b"DSIG"];

/// Tables that keep a face whole, with what they hold.
const KEPT_WHOLE: [(Tag, &str); 5] = [
    (*b"CFF2", "CFF2 outlines"),
    (*b"COLR", "COLR color glyphs"),
    (*b"CBDT", "CBDT color bitmaps"),
    (*b"sbix", "sbix color bitmaps"),
    (*b"SVG ", "SVG glyphs"),
];

/// Offset of `indexToLocFormat` in `head`.
const HEAD_LOCA_FORMAT: usize = 50;

/// What a build asks the packaged faces to keep.
#[derive(Debug)]
pub(crate) struct Request {
    /// Every character a subset keeps where a face maps it.
    pub kept: BTreeSet<char>,
    /// The static text file and the characters of it that need a glyph.
    pub text: Option<(PathBuf, BTreeSet<char>)>,
}

impl Request {
    /// The request the build environment makes: `None` when neither variable
    /// is set (or both are empty). A relative text path is taken from
    /// `manifest_dir`.
    pub fn from_env(manifest_dir: &Path) -> Result<Option<Self>, String> {
        let var = |name: &str| match std::env::var(name) {
            Ok(value) if value.trim().is_empty() => Ok(None),
            Ok(value) => Ok(Some(value)),
            Err(std::env::VarError::NotPresent) => Ok(None),
            Err(std::env::VarError::NotUnicode(_)) => Err(format!("{name} is not UTF-8")),
        };
        let charset = var(CHARSET_VAR)?;
        let text = var(TEXT_VAR)?.map(|path| manifest_dir.join(path));
        Self::new(charset.as_deref(), text)
    }

    pub fn new(charset: Option<&str>, text: Option<PathBuf>) -> Result<Option<Self>, String> {
        if charset.is_none() && text.is_none() {
            return Ok(None);
        }
        let mut kept = match charset {
            Some(spec) => charset::parse(spec)?,
            None => BTreeSet::new(),
        };
        kept.extend(ALWAYS_KEPT);
        let text = match text {
            Some(path) => {
                let bytes = std::fs::read(&path).map_err(|error| {
                    format!("{TEXT_VAR}: cannot read `{}`: {error}", path.display())
                })?;
                let text = String::from_utf8(bytes).map_err(|error| {
                    format!("{TEXT_VAR}: `{}` is not UTF-8: {error}", path.display())
                })?;
                kept.extend(text.chars());
                let needed = text.chars().filter(|&ch| needs_glyph(ch)).collect();
                Some((path, needed))
            }
            None => None,
        };
        Ok(Some(Self { kept, text }))
    }
}

/// Whether a missing glyph for `ch` would show: not a control, separator or
/// default-ignorable code point, which render as nothing either way.
fn needs_glyph(ch: char) -> bool {
    let code = ch as u32;
    !(ch.is_control()
        || matches!(
            code,
            0xAD | 0x34F
                | 0x61C
                | 0x115F..=0x1160
                | 0x17B4..=0x17B5
                | 0x180B..=0x180F
                | 0x200B..=0x200F
                | 0x2028..=0x202E
                | 0x2060..=0x206F
                | 0x3164
                | 0xFE00..=0xFE0F
                | 0xFEFF
                | 0xFFA0
                | 0xFFF0..=0xFFF8
                | 0x1BCA0..=0x1BCA3
                | 0x1D173..=0x1D17A
                | 0xE0000..=0xE0FFF
        ))
}

/// Whether `face` maps `ch` to a glyph other than `.notdef`.
pub(crate) fn maps(face: &Face<'_>, ch: char) -> bool {
    face.glyph_index(ch).is_some_and(|glyph| glyph.0 != 0)
}

/// A packaged file after subsetting.
#[derive(Debug)]
pub(crate) struct SubsetFile {
    /// The file to embed: one sfnt, or a collection of the same faces in the
    /// same order.
    pub bytes: Vec<u8>,
    /// Faces embedded whole, with why.
    pub whole: Vec<(u32, String)>,
    /// Each face's drawable glyphs, ascending: `.notdef`, the kept
    /// characters' glyphs and everything `GSUB` reaches from them; not the
    /// components a composite outline borrows, which never draw on their own.
    /// `None` for a face embedded whole.
    pub drawn: Vec<Option<Vec<u16>>>,
}

enum Outcome {
    Subset { bytes: Vec<u8>, drawn: Vec<u16> },
    Whole(String),
}

/// Tables holding outlines or their variations: what a face whose glyphs are
/// all baked no longer needs once shaping is all that reads it.
const OUTLINES: [Tag; 4] = [*b"glyf", *b"loca", *b"CFF ", *b"gvar"];

/// `data`, a single face or a collection, without its outline tables: every
/// table shaping reads (`cmap`, `GSUB`, `GPOS`, `hmtx`, …) stays byte for byte
/// and glyph ids keep their meaning, but no glyph has an outline to draw.
pub(crate) fn strip_outlines(data: &[u8]) -> Result<Vec<u8>, String> {
    let strip = |index: u32| -> Result<Vec<u8>, String> {
        let tables = sfnt::face_tables(data, index)?;
        let kept = tables
            .tables
            .iter()
            .filter(|(tag, _)| !OUTLINES.contains(tag))
            .map(|&(tag, table)| (tag, table.to_vec()))
            .collect();
        Ok(sfnt::write_face(tables.version, kept))
    };
    if !sfnt::is_collection(data) {
        return strip(0);
    }
    let count = read_u32(data, 8).ok_or("truncated collection header")?;
    let faces = (0..count).map(strip).collect::<Result<Vec<_>, _>>()?;
    sfnt::write_collection(&faces)
}

/// Why face `index` of `data` is kept whole rather than subset, if it is.
pub(crate) fn kept_whole(data: &[u8], index: u32) -> Result<Option<String>, String> {
    let tables = sfnt::face_tables(data, index)?;
    if let Some((_, what)) = KEPT_WHOLE.iter().find(|(tag, _)| tables.has(tag)) {
        return Ok(Some(format!("{what} are not subset")));
    }
    let face = Face::parse(data, index).map_err(|error| error.to_string())?;
    let unicode = face.tables().cmap.is_some_and(|cmap| {
        cmap.subtables
            .into_iter()
            .any(|subtable| subtable.is_unicode())
    });
    if !unicode {
        return Ok(Some("the face has no Unicode cmap".to_owned()));
    }
    if !(tables.has(b"glyf") && tables.has(b"loca")) && !tables.has(b"CFF ") {
        return Ok(Some(
            "the face has neither glyf nor CFF outlines".to_owned(),
        ));
    }
    Ok(None)
}

/// Subset every face of `data` to `kept`.
pub(crate) fn subset_file(data: &[u8], kept: &BTreeSet<char>) -> Result<SubsetFile, String> {
    if !sfnt::is_collection(data) {
        return Ok(match subset_face(data, 0, kept)? {
            Outcome::Subset { bytes, drawn } => SubsetFile {
                bytes,
                whole: Vec::new(),
                drawn: vec![Some(drawn)],
            },
            Outcome::Whole(reason) => SubsetFile {
                bytes: data.to_vec(),
                whole: vec![(0, reason)],
                drawn: vec![None],
            },
        });
    }
    // Each face is subset on its own and the collection packed again; tables
    // the subsets still share are stored once.
    let count = read_u32(data, 8).ok_or("truncated collection header")?;
    let mut faces = Vec::with_capacity(count as usize);
    let mut whole = Vec::new();
    let mut drawn = Vec::with_capacity(count as usize);
    for index in 0..count {
        match subset_face(data, index, kept)? {
            Outcome::Subset {
                bytes,
                drawn: glyphs,
            } => {
                faces.push(bytes);
                drawn.push(Some(glyphs));
            }
            Outcome::Whole(reason) => {
                drawn.push(None);
                let tables = sfnt::face_tables(data, index)?;
                let copied = tables
                    .tables
                    .iter()
                    .map(|&(tag, table)| (tag, table.to_vec()))
                    .collect();
                faces.push(sfnt::write_face(tables.version, copied));
                whole.push((index, reason));
            }
        }
    }
    if whole.len() == count as usize {
        return Ok(SubsetFile {
            bytes: data.to_vec(),
            whole,
            drawn,
        });
    }
    Ok(SubsetFile {
        bytes: sfnt::write_collection(&faces)?,
        whole,
        drawn,
    })
}

fn subset_face(data: &[u8], index: u32, kept: &BTreeSet<char>) -> Result<Outcome, String> {
    if let Some(reason) = kept_whole(data, index)? {
        return Ok(Outcome::Whole(reason));
    }
    let tables = sfnt::face_tables(data, index)?;
    let face = Face::parse(data, index).map_err(|error| error.to_string())?;
    let glyph_count = face.number_of_glyphs();
    let head = tables.get(b"head").ok_or("the face has no head table")?;
    let cmap_table = tables.get(b"cmap").ok_or("the face has no cmap table")?;

    let mappings: Vec<(u32, u16)> = kept
        .iter()
        .filter_map(|&ch| {
            let glyph = face.glyph_index(ch)?.0;
            (glyph != 0).then_some((ch as u32, glyph))
        })
        .collect();
    let mut glyphs = GlyphSet::new(glyph_count);
    glyphs.insert(0);
    for &(_, glyph) in &mappings {
        glyphs.insert(glyph);
    }
    let variations = cmap::variations(cmap_table);
    if let Some(variations) = variations {
        let base_kept = |code: u32| char::from_u32(code).is_some_and(|ch| kept.contains(&ch));
        for glyph in cmap::variation_glyphs(variations, base_kept) {
            glyphs.insert(glyph);
        }
    }
    if let Some(gsub) = face.tables().gsub {
        closure::close_over_gsub(&gsub, &mut glyphs);
    }
    let drawn: Vec<u16> = glyphs.iter().collect();

    let outlines = match (tables.get(b"glyf"), tables.get(b"loca")) {
        (Some(glyf), Some(loca)) => {
            let long = read_u16(head, HEAD_LOCA_FORMAT) == Some(1);
            let offsets = loca_offsets(loca, long, glyph_count, glyf.len())?;
            let outline = |glyph: u16| {
                let glyph = usize::from(glyph);
                match (offsets.get(glyph), offsets.get(glyph + 1)) {
                    (Some(&start), Some(&end)) => &glyf[start..end],
                    _ => &[][..],
                }
            };
            closure::close_over_components(&mut glyphs, outline);
            Some(rewrite_glyf(glyf, &offsets, &glyphs))
        }
        _ => None,
    };
    let cff = tables
        .get(b"CFF ")
        .map(|table| cff::subset(table, glyph_count, &glyphs))
        .transpose()?;
    let mut written = Vec::with_capacity(tables.tables.len());
    for &(tag, table) in &tables.tables {
        if DROPPED.contains(&tag) {
            continue;
        }
        let bytes = match (&tag, &outlines, &cff) {
            (b"glyf", Some(outlines), _) => outlines.glyf.clone(),
            (b"loca", Some(outlines), _) => outlines.loca.clone(),
            (b"head", Some(outlines), _) => {
                let mut head = table.to_vec();
                if let Some(format) = head.get_mut(HEAD_LOCA_FORMAT..HEAD_LOCA_FORMAT + 2) {
                    format.copy_from_slice(&u16::from(outlines.long).to_be_bytes());
                }
                head
            }
            (b"CFF ", _, Some(cff)) => cff.clone(),
            (b"cmap", _, _) => cmap::build(&mappings, variations),
            _ => table.to_vec(),
        };
        written.push((tag, bytes));
    }
    Ok(Outcome::Subset {
        bytes: sfnt::write_face(tables.version, written),
        drawn,
    })
}

/// Each glyph's start in `glyf`, then the table's end: `glyphs + 1` offsets.
fn loca_offsets(
    loca: &[u8],
    long: bool,
    glyphs: u16,
    glyf_len: usize,
) -> Result<Vec<usize>, String> {
    let mut offsets = Vec::with_capacity(usize::from(glyphs) + 1);
    for glyph in 0..=usize::from(glyphs) {
        let offset = if long {
            read_u32(loca, 4 * glyph).map(|offset| offset as usize)
        } else {
            read_u16(loca, 2 * glyph).map(|offset| 2 * usize::from(offset))
        };
        let offset = offset.ok_or("the loca table is truncated")?;
        if offset > glyf_len || offsets.last().is_some_and(|&last| offset < last) {
            return Err("the loca table is malformed".to_owned());
        }
        offsets.push(offset);
    }
    Ok(offsets)
}

struct Outlines {
    glyf: Vec<u8>,
    loca: Vec<u8>,
    /// Whether `loca` holds 32-bit offsets.
    long: bool,
}

/// `glyf` and `loca` holding only `keep`'s outlines (and `.notdef`'s); every
/// other glyph is empty.
fn rewrite_glyf(glyf: &[u8], offsets: &[usize], keep: &GlyphSet) -> Outlines {
    let mut out = Vec::new();
    let mut starts = Vec::with_capacity(offsets.len());
    for (glyph, window) in offsets.windows(2).enumerate() {
        starts.push(out.len());
        if glyph == 0 || keep.contains(glyph as u16) {
            out.extend_from_slice(&glyf[window[0]..window[1]]);
            out.resize(out.len().next_multiple_of(4), 0);
        }
    }
    starts.push(out.len());
    // A short entry holds half the offset in 16 bits; padding keeps every
    // offset even.
    let long = out.len() > 2 * usize::from(u16::MAX);
    let mut loca = Vec::with_capacity(starts.len() * if long { 4 } else { 2 });
    for start in starts {
        if long {
            loca.extend_from_slice(&(start as u32).to_be_bytes());
        } else {
            loca.extend_from_slice(&((start / 2) as u16).to_be_bytes());
        }
    }
    Outlines {
        glyf: out,
        loca,
        long,
    }
}

#[cfg(test)]
mod tests;
