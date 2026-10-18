//! Declared character sets: the `VISO_FONT_CHARSET` grammar and its named
//! sets.

use std::collections::BTreeSet;

use super::gb2312_table;

/// A named set: its name and its inclusive code point ranges.
type Named = (&'static str, &'static [(u32, u32)]);

const NAMED: [Named; 4] = [
    ("ascii", &[(0x20, 0x7E)]),
    // Basic Latin, Latin-1 Supplement, Latin Extended-A and -B.
    ("latin", &[(0x20, 0x7E), (0xA0, 0x24F)]),
    // Greek and Coptic, Greek Extended.
    ("greek", &[(0x370, 0x3FF), (0x1F00, 0x1FFF)]),
    // Cyrillic, Cyrillic Supplement.
    ("cyrillic", &[(0x400, 0x52F)]),
];

/// `gb2312` is resolved from its own table.
const GB2312: &str = "gb2312";

fn names() -> String {
    NAMED
        .iter()
        .map(|(name, _)| *name)
        .chain([GB2312])
        .collect::<Vec<_>>()
        .join(", ")
}

/// The GB 2312 repertoire: its 682 symbols and 6763 hanzi.
pub(super) fn gb2312() -> impl Iterator<Item = u32> {
    let symbols = gb2312_table::SYMBOLS
        .iter()
        .flat_map(|&(start, end)| start..=end);
    let hanzi = gb2312_table::HANZI
        .iter()
        .enumerate()
        .flat_map(|(word, &bits)| {
            (0..64)
                .filter(move |bit| bits >> bit & 1 != 0)
                .map(move |bit| gb2312_table::HANZI_BASE + 64 * word as u32 + bit)
        });
    symbols.chain(hanzi)
}

fn code_point(text: &str, item: &str) -> Result<u32, String> {
    let digits = text
        .strip_prefix("U+")
        .or_else(|| text.strip_prefix("u+"))
        .ok_or_else(|| invalid(item))?;
    if digits.is_empty() || digits.len() > 6 {
        return Err(invalid(item));
    }
    let value = u32::from_str_radix(digits, 16).map_err(|_| invalid(item))?;
    if value > 0x10_FFFF {
        return Err(format!("VISO_FONT_CHARSET: `{item}` lies past U+10FFFF"));
    }
    Ok(value)
}

fn invalid(item: &str) -> String {
    format!(
        "VISO_FONT_CHARSET: unknown item `{item}`; expected one of {} or a code point \
         `U+XXXX` / range `U+XXXX-YYYY`",
        names()
    )
}

/// The characters `spec` declares: comma-separated items, each a set name
/// (case-insensitive), a code point `U+XXXX` or a range `U+XXXX-YYYY`.
/// Surrogate code points in a range are skipped; empty items are ignored.
pub(crate) fn parse(spec: &str) -> Result<BTreeSet<char>, String> {
    let mut chars = BTreeSet::new();
    let mut add = |start: u32, end: u32| {
        chars.extend((start..=end).filter_map(char::from_u32));
    };
    for item in spec
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
    {
        let lower = item.to_ascii_lowercase();
        if lower == GB2312 {
            for code in gb2312() {
                add(code, code);
            }
        } else if let Some((_, ranges)) = NAMED.iter().find(|(name, _)| *name == lower) {
            for &(start, end) in *ranges {
                add(start, end);
            }
        } else if let Some((first, last)) = item.split_once('-') {
            let start = code_point(first.trim(), item)?;
            let last = last.trim();
            let end = if last.starts_with("U+") || last.starts_with("u+") {
                code_point(last, item)?
            } else {
                code_point(&format!("U+{last}"), item)?
            };
            if start > end {
                return Err(format!(
                    "VISO_FONT_CHARSET: range `{item}` ends before it starts"
                ));
            }
            add(start, end);
        } else {
            let code = code_point(item, item)?;
            add(code, code);
        }
    }
    Ok(chars)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_ranges_and_points_combine() {
        let chars = parse(" ASCII , U+4E2D, u+00e9-00EA,,U+1F600-U+1F601 ").unwrap();
        assert_eq!(chars.len(), 95 + 1 + 2 + 2);
        for ch in ['A', '~', ' ', '中', 'é', 'ê', '😀', '😁'] {
            assert!(chars.contains(&ch), "{ch}");
        }
        assert!(!chars.contains(&'\u{7F}'));
    }

    #[test]
    fn the_named_sets_hold_their_blocks() {
        assert_eq!(parse("latin").unwrap().len(), 95 + (0x24F - 0xA0 + 1));
        assert!(parse("greek").unwrap().contains(&'Ω'));
        assert!(parse("cyrillic").unwrap().contains(&'Ж'));
        assert!(parse("").unwrap().is_empty());
    }

    #[test]
    fn a_range_over_surrogates_skips_them() {
        assert_eq!(parse("U+D7FF-E000").unwrap().len(), 2);
    }

    #[test]
    fn an_unknown_item_names_itself_and_the_valid_names() {
        let error = parse("ascii,latin1").unwrap_err();
        assert!(error.contains("`latin1`"), "{error}");
        assert!(
            error.contains("ascii, latin, greek, cyrillic, gb2312"),
            "{error}"
        );
        for bad in ["U+", "U+XYZ", "U+1234567", "4E00", "U+110000", "U+20-10"] {
            assert!(parse(bad).is_err(), "{bad}");
        }
    }

    /// The assigned cells of GB 2312's symbol rows: (row, first cell, last
    /// cell), as EUC-CN bytes.
    const SYMBOL_CELLS: [(u8, u8, u8); 13] = [
        (0xA1, 0xA1, 0xFE),
        (0xA2, 0xB1, 0xE2),
        (0xA2, 0xE5, 0xEE),
        (0xA2, 0xF1, 0xFC),
        (0xA3, 0xA1, 0xFE),
        (0xA4, 0xA1, 0xF3),
        (0xA5, 0xA1, 0xF6),
        (0xA6, 0xA1, 0xB8),
        (0xA6, 0xC1, 0xD8),
        (0xA7, 0xA1, 0xC1),
        (0xA7, 0xD1, 0xF1),
        (0xA8, 0xA1, 0xBA),
        (0xA8, 0xC5, 0xE9),
    ];

    /// Every assigned GB 2312 code decoded through GBK, as (symbols, hanzi).
    /// Rows 0xA1–0xA9 hold symbols in the listed cells; rows 0xB0–0xF7 hold
    /// hanzi in every cell 0xA1–0xFE but 0xD7FA–0xD7FE.
    fn decode() -> (Vec<u32>, Vec<u32>) {
        let decode = |row: u8, cell: u8| {
            let code = [row, cell];
            let (text, _, failed) = encoding_rs::GBK.decode(&code);
            assert!(!failed, "{row:02X}{cell:02X}");
            let mut chars = text.chars();
            let ch = chars.next().unwrap();
            assert!(chars.next().is_none());
            assert!(
                !('\u{E000}'..='\u{F8FF}').contains(&ch),
                "{row:02X}{cell:02X}"
            );
            ch as u32
        };
        let mut symbols: Vec<u32> = SYMBOL_CELLS
            .iter()
            .flat_map(|&(row, first, last)| (first..=last).map(move |cell| decode(row, cell)))
            .collect();
        // Box drawing: row 9, cells 0xA4–0xEF.
        symbols.extend((0xA4..=0xEF).map(|cell| decode(0xA9, cell)));
        let hanzi: Vec<u32> = (0xB0..=0xF7u8)
            .flat_map(|row| (0xA1..=0xFEu8).map(move |cell| (row, cell)))
            .filter(|&(row, cell)| !(row == 0xD7 && cell >= 0xFA))
            .map(|(row, cell)| decode(row, cell))
            .collect();
        symbols.sort_unstable();
        symbols.dedup();
        (symbols, hanzi)
    }

    fn ranges(sorted: &[u32]) -> Vec<(u32, u32)> {
        let mut ranges: Vec<(u32, u32)> = Vec::new();
        for &code in sorted {
            match ranges.last_mut() {
                Some((_, end)) if *end + 1 == code => *end = code,
                _ => ranges.push((code, code)),
            }
        }
        ranges
    }

    fn render(symbols: &[(u32, u32)], hanzi: &[u64]) -> String {
        let mut out = String::from(
            "//! The GB 2312 repertoire, generated from GBK by the `gb2312_table_matches_gbk`\n\
             //! test (`VISO_REGENERATE_GB2312=1 cargo test -p viso-macros gb2312` rewrites it).\n\
             \n\
             /// The 682 symbols of rows 1–9, as inclusive code point ranges.\n\
             pub(super) const SYMBOLS: &[(u32, u32)] = &[\n",
        );
        for (start, end) in symbols {
            out.push_str(&format!("    (0x{start:04X}, 0x{end:04X}),\n"));
        }
        out.push_str(
            "];\n\
             \n\
             /// The first code point [`HANZI`] covers.\n\
             pub(super) const HANZI_BASE: u32 = 0x4E00;\n\
             \n\
             /// The 6763 hanzi of rows 16–87: bit `b` of word `w` is code point\n\
             /// `HANZI_BASE + 64 * w + b`.\n",
        );
        out.push_str(&format!(
            "pub(super) const HANZI: [u64; {}] = [\n",
            hanzi.len()
        ));
        for word in hanzi {
            out.push_str(&format!("    0x{word:016X},\n"));
        }
        out.push_str("];\n");
        out
    }

    #[test]
    fn gb2312_table_matches_gbk() {
        let (symbols, hanzi) = decode();
        assert_eq!(symbols.len(), 682);
        assert_eq!(hanzi.len(), 6763);
        assert!(hanzi.iter().all(|&code| (0x4E00..=0x9FFF).contains(&code)));
        let mut words = vec![0u64; (0xA000 - 0x4E00) / 64];
        for &code in &hanzi {
            let bit = code - 0x4E00;
            words[(bit / 64) as usize] |= 1 << (bit % 64);
        }
        let symbol_ranges = ranges(&symbols);
        if std::env::var_os("VISO_REGENERATE_GB2312").is_some() {
            let path = concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/src/font_subset/gb2312_table.rs"
            );
            std::fs::write(path, render(&symbol_ranges, &words)).unwrap();
        }
        assert_eq!(
            gb2312_table::SYMBOLS,
            &symbol_ranges[..],
            "rerun with VISO_REGENERATE_GB2312=1"
        );
        assert_eq!(gb2312_table::HANZI_BASE, 0x4E00);
        assert_eq!(
            &gb2312_table::HANZI[..],
            &words[..],
            "rerun with VISO_REGENERATE_GB2312=1"
        );

        let all: BTreeSet<u32> = gb2312().collect();
        assert_eq!(all.len(), 7445);
        let chars = parse("gb2312").unwrap();
        assert_eq!(chars.len(), 7445);
        assert!(chars.contains(&'中') && chars.contains(&'Ａ') && chars.contains(&'、'));
    }
}
