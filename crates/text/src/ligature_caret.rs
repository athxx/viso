//! GDEF ligature caret lookup (OpenType `LigCaretList`).
//!
//! A ligature glyph draws several characters as one shape; the font may record
//! where a caret between those characters belongs, as x coordinates from the
//! glyph origin. The shaper reads them here so a caret stop inside a ligature
//! sits where the font designer put it rather than at an even split.
//!
//! Only the coordinate forms are read: CaretValue formats 1 and 3 (format 3's
//! device/variation adjustment is ignored). Format 2 ties the caret to a contour
//! point of the hinted outline, which this runtime does not load; a ligature
//! using it reports no carets and the layout falls back to an even split.

/// The caret coordinates, in font design units from the glyph origin, of
/// `glyph` in the GDEF table bytes `gdef`, sorted ascending. `None` when the
/// table has no ligature caret list, does not cover `glyph`, or uses a caret
/// form this reader does not resolve.
pub(crate) fn ligature_carets(gdef: &[u8], glyph: u16) -> Option<Vec<i16>> {
    let list = offset_table(gdef, 0, 8)?;
    let coverage = offset_table(list, 0, 0)?;
    let index = coverage_index(coverage, glyph)?;
    if index >= usize::from(read_u16(list, 2)?) {
        return None;
    }
    let lig_glyph = offset_table(list, 0, 4 + 2 * index)?;
    let count = usize::from(read_u16(lig_glyph, 0)?);
    let mut carets = Vec::with_capacity(count);
    for i in 0..count {
        let value = offset_table(lig_glyph, 0, 2 + 2 * i)?;
        match read_u16(value, 0)? {
            1 | 3 => carets.push(read_u16(value, 2)? as i16),
            _ => return None,
        }
    }
    carets.sort_unstable();
    Some(carets)
}

/// The sub-table at the 16-bit offset stored at `data[at]`, measured from
/// `data[base]`. A zero offset means the sub-table is absent.
fn offset_table(data: &[u8], base: usize, at: usize) -> Option<&[u8]> {
    let offset = usize::from(read_u16(data, at)?);
    if offset == 0 {
        return None;
    }
    data.get(base + offset..)
}

/// The coverage index of `glyph` in a Coverage table (format 1 glyph array or
/// format 2 range records), both sorted by glyph id.
fn coverage_index(coverage: &[u8], glyph: u16) -> Option<usize> {
    let count = usize::from(read_u16(coverage, 2)?);
    match read_u16(coverage, 0)? {
        1 => {
            let (mut lo, mut hi) = (0, count);
            while lo < hi {
                let mid = (lo + hi) / 2;
                let id = read_u16(coverage, 4 + 2 * mid)?;
                match id.cmp(&glyph) {
                    std::cmp::Ordering::Equal => return Some(mid),
                    std::cmp::Ordering::Less => lo = mid + 1,
                    std::cmp::Ordering::Greater => hi = mid,
                }
            }
            None
        }
        2 => {
            let (mut lo, mut hi) = (0, count);
            while lo < hi {
                let mid = (lo + hi) / 2;
                let record = 4 + 6 * mid;
                let start = read_u16(coverage, record)?;
                let end = read_u16(coverage, record + 2)?;
                if glyph < start {
                    hi = mid;
                } else if glyph > end {
                    lo = mid + 1;
                } else {
                    let first = usize::from(read_u16(coverage, record + 4)?);
                    return Some(first + usize::from(glyph - start));
                }
            }
            None
        }
        _ => None,
    }
}

fn read_u16(data: &[u8], at: usize) -> Option<u16> {
    let bytes = data.get(at..at.checked_add(2)?)?;
    Some(u16::from_be_bytes([bytes[0], bytes[1]]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn push(out: &mut Vec<u8>, v: u16) {
        out.extend_from_slice(&v.to_be_bytes());
    }

    /// A GDEF v1.0 table whose LigCaretList covers `ligatures` (glyph id and
    /// its CaretValues as `(format, coordinate)`), with the coverage table in
    /// the given format.
    fn gdef(ligatures: &[(u16, &[(u16, i16)])], coverage_format: u16) -> Vec<u8> {
        // LigCaretList: coverage offset, count, one offset per LigGlyph, then
        // the LigGlyphs, then the coverage table.
        let mut lig_glyphs: Vec<Vec<u8>> = Vec::new();
        for (_, carets) in ligatures {
            let mut g = Vec::new();
            push(&mut g, carets.len() as u16);
            let header = 2 + 2 * carets.len();
            for i in 0..carets.len() {
                push(&mut g, (header + 6 * i) as u16);
            }
            for &(format, coord) in carets.iter() {
                push(&mut g, format);
                push(&mut g, coord as u16);
                push(&mut g, 0);
            }
            lig_glyphs.push(g);
        }
        let mut coverage = Vec::new();
        push(&mut coverage, coverage_format);
        if coverage_format == 1 {
            push(&mut coverage, ligatures.len() as u16);
            for (id, _) in ligatures {
                push(&mut coverage, *id);
            }
        } else {
            push(&mut coverage, ligatures.len() as u16);
            for (i, (id, _)) in ligatures.iter().enumerate() {
                push(&mut coverage, *id);
                push(&mut coverage, *id);
                push(&mut coverage, i as u16);
            }
        }
        let header = 4 + 2 * ligatures.len();
        let mut list = Vec::new();
        let glyph_bytes: usize = lig_glyphs.iter().map(Vec::len).sum();
        push(&mut list, (header + glyph_bytes) as u16);
        push(&mut list, ligatures.len() as u16);
        let mut at = header;
        for g in &lig_glyphs {
            push(&mut list, at as u16);
            at += g.len();
        }
        for g in &lig_glyphs {
            list.extend_from_slice(g);
        }
        list.extend_from_slice(&coverage);

        let mut table = Vec::new();
        push(&mut table, 1);
        push(&mut table, 0);
        push(&mut table, 0);
        push(&mut table, 0);
        push(&mut table, 12);
        push(&mut table, 0);
        table.extend_from_slice(&list);
        table
    }

    #[test]
    fn reads_format1_and_format3_carets_through_glyph_array_coverage() {
        let table = gdef(&[(5, &[(1, 300)]), (9, &[(3, 700), (1, 350)])], 1);
        assert_eq!(ligature_carets(&table, 5), Some(vec![300]));
        assert_eq!(ligature_carets(&table, 9), Some(vec![350, 700]));
        assert_eq!(ligature_carets(&table, 6), None);
    }

    #[test]
    fn reads_carets_through_range_coverage() {
        let table = gdef(&[(4, &[(1, 100)]), (40, &[(1, 200), (1, 400)])], 2);
        assert_eq!(ligature_carets(&table, 4), Some(vec![100]));
        assert_eq!(ligature_carets(&table, 40), Some(vec![200, 400]));
        assert_eq!(ligature_carets(&table, 41), None);
    }

    #[test]
    fn contour_point_carets_fall_back() {
        let table = gdef(&[(5, &[(2, 0)])], 1);
        assert_eq!(ligature_carets(&table, 5), None);
    }

    #[test]
    fn missing_or_truncated_tables_answer_none() {
        assert_eq!(ligature_carets(&[], 1), None);
        let mut no_list = gdef(&[(5, &[(1, 300)])], 1);
        no_list[8] = 0;
        no_list[9] = 0;
        assert_eq!(ligature_carets(&no_list, 5), None);
        let table = gdef(&[(5, &[(1, 300)])], 1);
        assert_eq!(ligature_carets(&table[..table.len() - 2], 5), None);
    }
}
