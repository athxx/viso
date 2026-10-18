use std::collections::BTreeSet;

use ttf_parser::{Face, GlyphId, OutlineBuilder, Rect};

use super::cff::{Entry, Operand, PRIVATE_OFFSETS, TOP_OFFSETS, write_dict, write_index};
use super::*;

/// Noto Sans cut to Latin-1 and a few quotes; has `fi`/`ffi` ligatures and
/// composite accented letters.
const NOTO: &[u8] = include_bytes!("../../tests/fixtures/NotoSans-Latin.ttf");
/// Bravura cut to a few dozen SMuFL symbols and renamed: CFF outlines with
/// subroutines.
const SYMBOLS: &[u8] = include_bytes!("../../tests/fixtures/VisoSym-subset.otf");
const DEJAVU: &[u8] = include_bytes!("../../../text/tests/fixtures/DejaVuSans-subset.ttf");
const COLR: &[u8] = include_bytes!("../../../text/tests/fixtures/ColrFixture.ttf");

fn kept(text: &str) -> BTreeSet<char> {
    text.chars().chain(ALWAYS_KEPT).collect()
}

fn subset(data: &[u8], text: &str) -> Vec<u8> {
    let file = subset_file(data, &kept(text)).unwrap();
    assert!(file.whole.is_empty(), "{:?}", file.whole);
    file.bytes
}

struct Bounds;

impl OutlineBuilder for Bounds {
    fn move_to(&mut self, _: f32, _: f32) {}
    fn line_to(&mut self, _: f32, _: f32) {}
    fn quad_to(&mut self, _: f32, _: f32, _: f32, _: f32) {}
    fn curve_to(&mut self, _: f32, _: f32, _: f32, _: f32, _: f32, _: f32) {}
    fn close(&mut self) {}
}

fn bounds(face: &Face<'_>, glyph: u16) -> Option<Rect> {
    face.outline_glyph(GlyphId(glyph), &mut Bounds)
}

fn glyph(face: &Face<'_>, ch: char) -> Option<u16> {
    face.glyph_index(ch).map(|glyph| glyph.0)
}

/// Every table record's checksum matches its table, and the whole font sums
/// to the `head` magic number.
fn assert_checksums(data: &[u8]) {
    let tables = sfnt::face_tables(data, 0).unwrap();
    let count = usize::from(read_u16(data, 4).unwrap());
    for record in 0..count {
        let at = 12 + 16 * record;
        let (tag, table) = tables.tables[record];
        let mut table = table.to_vec();
        if &tag == b"head" {
            table[8..12].fill(0);
        }
        assert_eq!(
            read_u32(data, at + 4).unwrap(),
            sfnt::checksum(&table),
            "{}",
            sfnt::tag_name(&tag)
        );
        assert_eq!(read_u32(data, at + 8).unwrap() % 4, 0, "aligned");
    }
    assert_eq!(sfnt::checksum(data), 0xB1B0_AFBA);
}

/// The ligature `GSUB` forms from `components`, if any.
fn ligature(face: &Face<'_>, components: &[u16]) -> Option<u16> {
    use ttf_parser::gsub::SubstitutionSubtable;
    let gsub = face.tables().gsub?;
    for lookup in gsub.lookups {
        for subtable in lookup.subtables.into_iter::<SubstitutionSubtable>() {
            let SubstitutionSubtable::Ligature(ligatures) = subtable else {
                continue;
            };
            let index = ligatures.coverage.get(GlyphId(components[0]));
            let Some(set) = index.and_then(|index| ligatures.ligature_sets.get(index)) else {
                continue;
            };
            for candidate in set {
                let rest: Vec<u16> = candidate.components.into_iter().map(|g| g.0).collect();
                if rest == components[1..] {
                    return Some(candidate.glyph.0);
                }
            }
        }
    }
    None
}

/// A subset `glyf` entry is the original padded to 4 bytes.
fn assert_same_outline(subset: &[u8], original: &[u8]) {
    assert_eq!(subset.len(), original.len().next_multiple_of(4));
    assert_eq!(&subset[..original.len()], original);
    assert!(subset[original.len()..].iter().all(|&byte| byte == 0));
}

/// A glyph's `glyf` entry.
fn outline(data: &[u8], glyph: u16) -> Vec<u8> {
    let tables = sfnt::face_tables(data, 0).unwrap();
    let face = Face::parse(data, 0).unwrap();
    let head = tables.get(b"head").unwrap();
    let glyf = tables.get(b"glyf").unwrap();
    let long = read_u16(head, HEAD_LOCA_FORMAT) == Some(1);
    let offsets = loca_offsets(
        tables.get(b"loca").unwrap(),
        long,
        face.number_of_glyphs(),
        glyf.len(),
    )
    .unwrap();
    glyf[offsets[usize::from(glyph)]..offsets[usize::from(glyph) + 1]].to_vec()
}

#[test]
fn kept_characters_keep_their_glyph_ids_and_outlines() {
    let bytes = subset(NOTO, "Afi");
    let original = Face::parse(NOTO, 0).unwrap();
    let face = Face::parse(&bytes, 0).unwrap();
    assert_eq!(face.number_of_glyphs(), original.number_of_glyphs());
    for ch in ['A', 'f', 'i', ' ', '\u{A0}'] {
        let id = glyph(&original, ch).unwrap();
        assert_eq!(glyph(&face, ch), Some(id), "{ch}");
        assert_eq!(bounds(&face, id), bounds(&original, id), "{ch}");
        assert_eq!(
            face.glyph_hor_advance(GlyphId(id)),
            original.glyph_hor_advance(GlyphId(id))
        );
    }
    for ch in ['B', 'z', 'é'] {
        let id = glyph(&original, ch).unwrap();
        assert!(bounds(&original, id).is_some());
        assert_eq!(glyph(&face, ch), None, "{ch}");
        assert_eq!(bounds(&face, id), None, "{ch}");
    }
    assert!(bytes.len() < NOTO.len());
    assert_checksums(&bytes);
    let tables = sfnt::face_tables(&bytes, 0).unwrap();
    let before = sfnt::face_tables(NOTO, 0).unwrap();
    let glyf = |tables: &sfnt::FaceTables<'_>| tables.get(b"glyf").unwrap().len();
    assert!(
        glyf(&tables) * 10 < glyf(&before),
        "glyf {} of {}",
        glyf(&tables),
        glyf(&before)
    );
    assert!(DROPPED.iter().all(|tag| !tables.has(tag)));
    // Glyph-indexed tables travel unchanged.
    for tag in [b"GSUB", b"GPOS", b"hmtx", b"maxp"] {
        assert_eq!(tables.get(tag), before.get(tag), "{}", sfnt::tag_name(tag));
    }
}

#[test]
fn a_ligature_stays_when_all_its_components_do() {
    let original = Face::parse(NOTO, 0).unwrap();
    let (f, i) = (
        glyph(&original, 'f').unwrap(),
        glyph(&original, 'i').unwrap(),
    );
    let fi = ligature(&original, &[f, i]).expect("the fixture has an fi ligature");
    let ffi = ligature(&original, &[f, f, i]).expect("the fixture has an ffi ligature");
    assert!(bounds(&original, fi).is_some());

    let bytes = subset(NOTO, "fi");
    let face = Face::parse(&bytes, 0).unwrap();
    assert_eq!(bounds(&face, fi), bounds(&original, fi));
    assert_eq!(bounds(&face, ffi), bounds(&original, ffi));

    let bytes = subset(NOTO, "f");
    let face = Face::parse(&bytes, 0).unwrap();
    assert_eq!(
        bounds(&face, fi),
        None,
        "`i` is not kept, so neither is `fi`"
    );
}

#[test]
fn a_composite_keeps_its_components() {
    let original = Face::parse(NOTO, 0).unwrap();
    let (ch, id, parts) = ('\u{C0}'..='\u{FF}')
        .find_map(|ch| {
            let id = glyph(&original, ch)?;
            let parts = closure::components(&outline(NOTO, id));
            (!parts.is_empty()).then_some((ch, id, parts))
        })
        .expect("the fixture has a composite glyph");

    let bytes = subset(NOTO, &ch.to_string());
    let face = Face::parse(&bytes, 0).unwrap();
    assert_eq!(bounds(&face, id), bounds(&original, id), "{ch}");
    assert_same_outline(&outline(&bytes, id), &outline(NOTO, id));
    for part in parts {
        assert!(!outline(NOTO, part).is_empty());
        assert_same_outline(&outline(&bytes, part), &outline(NOTO, part));
    }
    // The base letter's glyph is kept, its character is not.
    assert_eq!(glyph(&face, 'e'), None);
    assert_checksums(&bytes);
}

#[test]
fn a_cff_face_empties_dropped_charstrings() {
    let original = Face::parse(SYMBOLS, 0).unwrap();
    let kept_chars = ['\u{E050}', '\u{E062}'];
    let bytes = subset(SYMBOLS, &kept_chars.iter().collect::<String>());
    let face = Face::parse(&bytes, 0).unwrap();
    assert_eq!(face.number_of_glyphs(), original.number_of_glyphs());
    for ch in kept_chars {
        let id = glyph(&original, ch).unwrap();
        assert_eq!(glyph(&face, ch), Some(id));
        assert!(bounds(&face, id).is_some());
        assert_eq!(bounds(&face, id), bounds(&original, id), "{ch:?}");
    }
    let dropped = glyph(&original, '\u{E051}').unwrap();
    assert!(bounds(&original, dropped).is_some());
    assert_eq!(glyph(&face, '\u{E051}'), None);
    assert_eq!(bounds(&face, dropped), None);
    let (before, after) = (
        sfnt::face_tables(SYMBOLS, 0)
            .unwrap()
            .get(b"CFF ")
            .unwrap()
            .len(),
        sfnt::face_tables(&bytes, 0)
            .unwrap()
            .get(b"CFF ")
            .unwrap()
            .len(),
    );
    assert!(after < before, "CFF {after} of {before}");
    assert!(bytes.len() < SYMBOLS.len());
    assert_checksums(&bytes);
}

/// A Type 2 charstring number.
fn number(out: &mut Vec<u8>, value: i32) {
    match value {
        -107..=107 => out.push((value + 139) as u8),
        108..=1131 => {
            let value = value - 108;
            out.extend_from_slice(&[(value / 256 + 247) as u8, (value % 256) as u8]);
        }
        _ => {
            out.push(28);
            out.extend_from_slice(&(value as i16).to_be_bytes());
        }
    }
}

fn int_entry(op: u16, values: &[i32]) -> Entry {
    Entry {
        op,
        operands: values.iter().map(|&value| Operand::Int(value)).collect(),
    }
}

/// A CID-keyed CFF face of four glyphs: `.notdef` and A in font DICT 0, B
/// and C in font DICT 1, whose glyphs draw their right edge through local
/// subroutine 0. Each glyph is a box `100 + 50 * gid` units wide.
fn cid_font() -> Vec<u8> {
    let charstring = |gid: i32| {
        let mut out = Vec::new();
        let width = 100 + 50 * gid;
        for (dx, dy, op) in [(50, 0, 21), (width, 0, 5)] {
            number(&mut out, dx);
            number(&mut out, dy);
            out.push(op);
        }
        if gid >= 2 {
            // callsubr 0: the bias for fewer than 1240 subroutines is 107.
            number(&mut out, -107);
            out.push(10);
        } else {
            number(&mut out, 0);
            number(&mut out, 200);
            out.push(5);
        }
        number(&mut out, -width);
        number(&mut out, 0);
        out.push(5);
        out.push(14);
        out
    };
    let mut subr = Vec::new();
    number(&mut subr, 0);
    number(&mut subr, 200);
    subr.extend_from_slice(&[5, 11]);

    let mut top = vec![
        int_entry(0x0C1E, &[391, 392, 0]),
        int_entry(0x0C22, &[4]),
        int_entry(15, &[0]),
        int_entry(17, &[0]),
        int_entry(0x0C24, &[0]),
        int_entry(0x0C25, &[0]),
    ];
    let header = [1u8, 0, 4, 4];
    let names = write_index(&[b"CidTest"]);
    let strings = write_index(&[&b"Adobe"[..], b"Identity"]);
    let global_subrs = write_index::<&[u8]>(&[]);
    let top_len = write_index(&[write_dict(&top, &TOP_OFFSETS)]).len();
    let base = header.len() + names.len() + top_len + strings.len() + global_subrs.len();
    let mut body = Vec::new();
    let place = |body: &mut Vec<u8>, bytes: &[u8]| {
        let start = base + body.len();
        body.extend_from_slice(bytes);
        start as i32
    };
    let charset = place(&mut body, &[2, 0, 1, 0, 2]);
    let fd_select = place(&mut body, &[3, 0, 2, 0, 0, 0, 0, 2, 1, 0, 4]);
    let charstrings = place(
        &mut body,
        &write_index(&(0..4).map(charstring).collect::<Vec<_>>()),
    );
    let private0 = write_dict(&[int_entry(21, &[0])], &PRIVATE_OFFSETS);
    let mut private1 = vec![int_entry(19, &[0]), int_entry(21, &[0])];
    let private1_len = write_dict(&private1, &PRIVATE_OFFSETS).len();
    private1[0] = int_entry(19, &[private1_len as i32]);
    let mut private1 = write_dict(&private1, &PRIVATE_OFFSETS);
    private1.extend_from_slice(&write_index(&[&subr]));
    let fonts = |offsets: [i32; 2]| {
        write_index(&[
            write_dict(
                &[int_entry(18, &[private0.len() as i32, offsets[0]])],
                &TOP_OFFSETS,
            ),
            write_dict(
                &[int_entry(18, &[private1_len as i32, offsets[1]])],
                &TOP_OFFSETS,
            ),
        ])
    };
    let fd_array_len = fonts([0, 0]).len();
    let fd_array_at = (base + body.len()) as i32;
    let private0_at = fd_array_at + fd_array_len as i32;
    let private1_at = private0_at + private0.len() as i32;
    place(&mut body, &fonts([private0_at, private1_at]));
    place(&mut body, &private0);
    place(&mut body, &private1);
    for (op, value) in [
        (15, charset),
        (17, charstrings),
        (0x0C24, fd_array_at),
        (0x0C25, fd_select),
    ] {
        let entry = top.iter_mut().find(|entry| entry.op == op).unwrap();
        *entry = int_entry(op, &[value]);
    }
    let mut cff = header.to_vec();
    cff.extend_from_slice(&names);
    cff.extend_from_slice(&write_index(&[write_dict(&top, &TOP_OFFSETS)]));
    cff.extend_from_slice(&strings);
    cff.extend_from_slice(&global_subrs);
    cff.extend_from_slice(&body);

    let mut head = vec![0u8; 54];
    head[0..4].copy_from_slice(&0x0001_0000u32.to_be_bytes());
    head[12..16].copy_from_slice(&0x5F0F_3CF5u32.to_be_bytes());
    head[18..20].copy_from_slice(&1000u16.to_be_bytes());
    let mut hhea = vec![0u8; 36];
    hhea[0..4].copy_from_slice(&0x0001_0000u32.to_be_bytes());
    hhea[4..6].copy_from_slice(&800i16.to_be_bytes());
    hhea[6..8].copy_from_slice(&(-200i16).to_be_bytes());
    hhea[34..36].copy_from_slice(&4u16.to_be_bytes());
    let hmtx: Vec<u8> = (0..4u16)
        .flat_map(|gid| [(300 + 50 * gid).to_be_bytes(), 50u16.to_be_bytes()].concat())
        .collect();
    let mut maxp = 0x0000_5000u32.to_be_bytes().to_vec();
    maxp.extend_from_slice(&4u16.to_be_bytes());
    let cmap = cmap::build(&[(0x41, 1), (0x42, 2), (0x43, 3)], None);
    sfnt::write_face(
        u32::from_be_bytes(*b"OTTO"),
        vec![
            (*b"CFF ", cff),
            (*b"cmap", cmap),
            (*b"head", head),
            (*b"hhea", hhea),
            (*b"hmtx", hmtx),
            (*b"maxp", maxp),
        ],
    )
}

#[test]
fn a_cid_keyed_cff_face_keeps_its_font_dicts_and_local_subroutines() {
    let font = cid_font();
    let original = Face::parse(&font, 0).unwrap();
    let boxes: Vec<Option<Rect>> = (0..4).map(|gid| bounds(&original, gid)).collect();
    for gid in 1..4u16 {
        let rect = boxes[usize::from(gid)].expect("the fixture draws every glyph");
        assert_eq!((rect.x_min, rect.x_max), (50, 50 + 100 + 50 * gid as i16));
        assert_eq!((rect.y_min, rect.y_max), (0, 200));
    }

    let bytes = subset(&font, "AC");
    let face = Face::parse(&bytes, 0).unwrap();
    assert_eq!(bounds(&face, 1), boxes[1]);
    assert_eq!(bounds(&face, 3), boxes[3], "font DICT 1 and its subroutine");
    assert_eq!(bounds(&face, 2), None);
    assert_eq!(glyph(&face, 'B'), None);
    assert_eq!(glyph(&face, 'C'), Some(3));
    assert_checksums(&bytes);
}

#[test]
fn a_collection_is_subset_face_by_face() {
    let collection = sfnt::write_collection(&[DEJAVU.to_vec(), NOTO.to_vec()]).unwrap();
    assert_eq!(ttf_parser::fonts_in_collection(&collection), Some(2));
    for (index, standalone) in [DEJAVU, NOTO].into_iter().enumerate() {
        let packed = Face::parse(&collection, index as u32).unwrap();
        let alone = Face::parse(standalone, 0).unwrap();
        let id = glyph(&alone, 'A').unwrap();
        assert_eq!(glyph(&packed, 'A'), Some(id));
        assert_eq!(bounds(&packed, id), bounds(&alone, id));
    }

    let bytes = subset(&collection, "A");
    assert_eq!(ttf_parser::fonts_in_collection(&bytes), Some(2));
    assert!(bytes.len() < collection.len());
    for (index, standalone) in [DEJAVU, NOTO].into_iter().enumerate() {
        let face = Face::parse(&bytes, index as u32).unwrap();
        let alone = Face::parse(standalone, 0).unwrap();
        let (a, b) = (glyph(&alone, 'A').unwrap(), glyph(&alone, 'B').unwrap());
        assert_eq!(glyph(&face, 'A'), Some(a));
        assert_eq!(bounds(&face, a), bounds(&alone, a));
        assert_eq!(glyph(&face, 'B'), None);
        assert_eq!(bounds(&face, b), None);
    }
}

#[test]
fn a_collection_stores_equal_tables_once() {
    let twice = sfnt::write_collection(&[DEJAVU.to_vec(), DEJAVU.to_vec()]).unwrap();
    assert!(twice.len() < DEJAVU.len() * 11 / 10, "{}", twice.len());
    assert_eq!(ttf_parser::fonts_in_collection(&twice), Some(2));
    assert!(Face::parse(&twice, 1).is_ok());
}

#[test]
fn a_color_face_is_kept_whole() {
    let file = subset_file(COLR, &kept("O")).unwrap();
    assert_eq!(file.bytes, COLR);
    assert_eq!(file.whole.len(), 1);
    assert!(file.whole[0].1.contains("COLR"), "{:?}", file.whole);
}

#[test]
fn the_drawn_glyphs_hold_ligatures_but_not_borrowed_components() {
    let original = Face::parse(NOTO, 0).unwrap();
    let (f, i) = (
        glyph(&original, 'f').unwrap(),
        glyph(&original, 'i').unwrap(),
    );
    let fi = ligature(&original, &[f, i]).unwrap();
    let (ch, id, parts) = ('\u{C0}'..='\u{FF}')
        .find_map(|ch| {
            let id = glyph(&original, ch)?;
            let parts = closure::components(&outline(NOTO, id));
            (!parts.is_empty()).then_some((ch, id, parts))
        })
        .unwrap();
    let text = format!("fi{ch}");
    let file = subset_file(NOTO, &kept(&text)).unwrap();
    let drawn = file.drawn[0].as_ref().unwrap();
    assert!(drawn.windows(2).all(|pair| pair[0] < pair[1]), "ascending");
    for needed in [0, f, i, fi, id] {
        assert!(drawn.contains(&needed), "glyph {needed}");
    }
    let face = Face::parse(&file.bytes, 0).unwrap();
    for part in parts {
        let mapped = face.tables().cmap.is_some_and(|cmap| {
            cmap.subtables.into_iter().any(|t| {
                let mut hit = false;
                t.codepoints(|code| hit |= t.glyph_index(code) == Some(GlyphId(part)));
                hit
            })
        });
        assert_eq!(drawn.contains(&part), mapped, "component {part}");
    }
    assert!(
        subset_file(COLR, &kept("O")).unwrap().drawn[0].is_none(),
        "kept whole"
    );
}

#[test]
fn stripped_outlines_keep_every_table_shaping_reads() {
    let bytes = subset(NOTO, "Hif");
    let stripped = strip_outlines(&bytes).unwrap();
    assert!(stripped.len() < bytes.len());
    assert_checksums(&stripped);
    let (before, after) = (
        sfnt::face_tables(&bytes, 0).unwrap(),
        sfnt::face_tables(&stripped, 0).unwrap(),
    );
    for tag in OUTLINES {
        assert!(!after.has(&tag), "{}", sfnt::tag_name(&tag));
    }
    for &(tag, table) in &before.tables {
        if !OUTLINES.contains(&tag) && &tag != b"head" {
            assert_eq!(after.get(&tag), Some(table), "{}", sfnt::tag_name(&tag));
        }
    }
    let (face, original) = (
        Face::parse(&stripped, 0).unwrap(),
        Face::parse(&bytes, 0).unwrap(),
    );
    let h = glyph(&original, 'H').unwrap();
    assert_eq!(glyph(&face, 'H'), Some(h));
    assert_eq!(
        face.glyph_hor_advance(GlyphId(h)),
        original.glyph_hor_advance(GlyphId(h))
    );
    assert!(bounds(&original, h).is_some());
    assert_eq!(bounds(&face, h), None, "no outline left");

    let cff = strip_outlines(SYMBOLS).unwrap();
    assert!(sfnt::face_tables(&cff, 0).unwrap().get(b"CFF ").is_none());
    assert!(Face::parse(&cff, 0).is_ok());

    let collection = sfnt::write_collection(&[bytes.clone(), DEJAVU.to_vec()]).unwrap();
    let stripped = strip_outlines(&collection).unwrap();
    assert_eq!(ttf_parser::fonts_in_collection(&stripped), Some(2));
    for index in 0..2 {
        let face = Face::parse(&stripped, index).unwrap();
        let a = glyph(&face, 'H').or_else(|| glyph(&face, 'A')).unwrap();
        assert_eq!(bounds(&face, a), None);
    }
}

#[test]
fn outlines_past_the_short_loca_range_switch_to_long_offsets() {
    let glyf = vec![0u8; 140_000];
    let mut keep = GlyphSet::new(2);
    keep.insert(1);
    let outlines = rewrite_glyf(&glyf, &[0, 70_000, 140_000], &keep);
    assert!(outlines.long);
    assert_eq!(
        loca_offsets(&outlines.loca, true, 2, outlines.glyf.len()).unwrap(),
        [0, 70_000, 140_000]
    );

    keep = GlyphSet::new(2);
    let outlines = rewrite_glyf(&glyf, &[0, 70_000, 140_000], &keep);
    assert!(!outlines.long);
    assert_eq!(
        loca_offsets(&outlines.loca, false, 2, outlines.glyf.len()).unwrap(),
        [0, 70_000, 70_000]
    );
}

#[test]
fn the_gb2312_repertoire_fits_one_format_4_subtable() {
    let mappings: Vec<(u32, u16)> = charset::gb2312()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .chain(0x20..0x7F)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .enumerate()
        .map(|(index, code)| (code, 1 + index as u16))
        .collect();
    let built = cmap::build(&mappings, None);
    let table = ttf_parser::cmap::Table::parse(&built).unwrap();
    let format4 = table.subtables.get(0).unwrap();
    assert!(matches!(
        format4.format,
        ttf_parser::cmap::Format::SegmentMappingToDeltaValues(_)
    ));
    for &(code, glyph) in &mappings {
        assert_eq!(format4.glyph_index(code).map(|glyph| glyph.0), Some(glyph));
    }
    let unmapped = (0x4E00..0x9FA5)
        .find(|code| {
            mappings
                .binary_search_by_key(code, |&(code, _)| code)
                .is_err()
        })
        .unwrap();
    assert_eq!(format4.glyph_index(unmapped), None);
}

#[test]
fn a_request_needs_either_variable() {
    assert!(Request::new(None, None).unwrap().is_none());
    let request = Request::new(Some("U+4E2D"), None).unwrap().unwrap();
    assert!(request.kept.contains(&'中') && request.kept.contains(&' '));
    assert!(request.text.is_none());
    let error = Request::new(Some("hanzi"), None).unwrap_err();
    assert!(error.contains("`hanzi`"), "{error}");
}

#[test]
fn the_text_file_adds_characters_and_requires_the_visible_ones() {
    let path = std::env::temp_dir().join(format!("viso_font_text_{}.txt", std::process::id()));
    std::fs::write(&path, "确定\tOK\n\u{200B}").unwrap();
    let request = Request::new(None, Some(path.clone())).unwrap().unwrap();
    let _ = std::fs::remove_file(&path);
    assert!(request.kept.contains(&'确') && request.kept.contains(&'K'));
    let (_, needed) = request.text.unwrap();
    assert_eq!(needed, BTreeSet::from(['确', '定', 'O', 'K']));

    let missing = Request::new(None, Some(path.clone())).unwrap_err();
    assert!(missing.contains("VISO_FONT_TEXT"), "{missing}");
}
