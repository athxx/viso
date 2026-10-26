//! What the editor measures text by: grapheme clusters, the cells each takes
//! in the grid, and the classes words are made of.
//!
//! Code is laid out on a grid of equal cells, as wide as one character of the
//! monospace face: a caret stop, a click and a wrap are worked out in cells,
//! not from a shaped run, so they are known the moment the text changes. A
//! grapheme takes one cell, two when East Asian Width calls it wide or
//! fullwidth (CJK, most emoji) or it asks for emoji presentation; a tab runs
//! to the next tab stop.

use icu_properties::CodePointMapData;
use icu_properties::props::EastAsianWidth;
use unicode_segmentation::UnicodeSegmentation;

/// The cells `c` takes, alone.
#[inline]
pub fn char_cells(c: char) -> u32 {
    if c.is_ascii() {
        return 1;
    }
    match CodePointMapData::<EastAsianWidth>::new().get(c) {
        EastAsianWidth::Wide | EastAsianWidth::Fullwidth => 2,
        _ => 1,
    }
}

/// The cells grapheme cluster `g` takes, not a tab.
pub fn grapheme_cells(g: &str) -> u32 {
    let mut chars = g.chars();
    let Some(first) = chars.next() else {
        return 0;
    };
    if first.is_ascii() && g.len() == 1 {
        return 1;
    }
    // An emoji presentation selector, or a wide character anywhere in a
    // sequence (a flag, a joined family), makes the whole cluster wide.
    if g.contains('\u{FE0F}') {
        return 2;
    }
    let mut cells = char_cells(first);
    for c in chars {
        cells = cells.max(char_cells(c));
    }
    cells
}

/// Whether every byte of `text` is printable ASCII: one cell a byte, one
/// grapheme a byte.
#[inline]
pub fn is_plain_ascii(text: &str) -> bool {
    text.bytes().all(|b| (0x20..0x7f).contains(&b))
}

/// The cells line text `text` takes from cell `col` on, tabs `tab` cells
/// apart.
pub fn cells(text: &str, mut col: u32, tab: u32) -> u32 {
    let start = col;
    if is_plain_ascii(text) {
        return text.len() as u32;
    }
    for g in text.graphemes(true) {
        col += if g == "\t" {
            tab_cells(col, tab)
        } else {
            grapheme_cells(g)
        };
    }
    col - start
}

/// The cells a tab at cell `col` takes: to the next stop `tab` apart.
#[inline]
pub fn tab_cells(col: u32, tab: u32) -> u32 {
    let tab = tab.max(1);
    tab - col % tab
}

/// The byte past the grapheme starting at byte `at` of `text`; the end at
/// the end.
pub fn next_grapheme(text: &str, at: usize) -> usize {
    if at >= text.len() {
        return text.len();
    }
    let b = text.as_bytes()[at];
    // An ASCII byte other than CR before a non-combining byte is its own
    // cluster.
    if b.is_ascii() && b != b'\r' && text.as_bytes().get(at + 1).is_none_or(|n| n.is_ascii()) {
        return at + 1;
    }
    text[at..]
        .graphemes(true)
        .next()
        .map_or(text.len(), |g| at + g.len())
}

/// The byte the grapheme ending at byte `at` of `text` starts at; zero at the
/// start.
pub fn prev_grapheme(text: &str, at: usize) -> usize {
    if at == 0 {
        return 0;
    }
    let bytes = text.as_bytes();
    if bytes[at - 1].is_ascii()
        && bytes[at - 1] != b'\n'
        && at.checked_sub(2).is_none_or(|p| bytes[p].is_ascii())
    {
        return at - 1;
    }
    text[..at]
        .grapheme_indices(true)
        .next_back()
        .map_or(0, |(i, _)| i)
}

/// What a character is, for word motion and double-click selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CharClass {
    /// Spaces and tabs.
    Space,
    /// Letters, digits and `_`: what identifiers are made of.
    Word,
    /// Anything else: operators, brackets, punctuation.
    Punct,
}

/// The class of `c`.
#[inline]
pub fn char_class(c: char) -> CharClass {
    if c == ' ' || c == '\t' || c.is_whitespace() {
        CharClass::Space
    } else if c.is_alphanumeric() || c == '_' {
        CharClass::Word
    } else {
        CharClass::Punct
    }
}

/// The bytes of `text` that indent it: its leading spaces and tabs.
pub fn indent_of(text: &str) -> &str {
    let end = text
        .bytes()
        .position(|b| b != b' ' && b != b'\t')
        .unwrap_or(text.len());
    &text[..end]
}

/// The cells `text`'s indent takes, tabs `tab` apart; `None` for a blank
/// line.
pub fn indent_cells(text: &str, tab: u32) -> Option<u32> {
    let indent = indent_of(text);
    if indent.len() == text.len() {
        return None;
    }
    Some(cells(indent, 0, tab))
}

/// The bracket that pairs with `c`, and whether `c` opens.
pub fn bracket_pair(c: char) -> Option<(char, bool)> {
    Some(match c {
        '(' => (')', true),
        '[' => (']', true),
        '{' => ('}', true),
        ')' => ('(', false),
        ']' => ('[', false),
        '}' => ('{', false),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_characters_take_two_cells() {
        assert_eq!(grapheme_cells("a"), 1);
        assert_eq!(grapheme_cells("é"), 1);
        assert_eq!(grapheme_cells("e\u{301}"), 1);
        assert_eq!(grapheme_cells("日"), 2);
        assert_eq!(grapheme_cells("ｱ"), 1, "halfwidth katakana");
        assert_eq!(grapheme_cells("🦀"), 2);
        assert_eq!(grapheme_cells("❤\u{FE0F}"), 2);
        assert_eq!(grapheme_cells("👨\u{200D}👩\u{200D}👧"), 2);
    }

    #[test]
    fn tabs_run_to_the_next_stop() {
        assert_eq!(cells("\tx", 0, 4), 5);
        assert_eq!(cells("ab\tx", 0, 4), 5);
        assert_eq!(cells("abcd\tx", 0, 4), 9);
        assert_eq!(cells("日本\t", 0, 4), 8);
        assert_eq!(tab_cells(3, 4), 1);
    }

    #[test]
    fn grapheme_steps_cross_clusters_whole() {
        let text = "ae\u{301}🇯🇵x\r\ny";
        let mut stops = vec![0];
        let mut at = 0;
        while at < text.len() {
            at = next_grapheme(text, at);
            stops.push(at);
        }
        assert_eq!(stops, [0, 1, 4, 12, 13, 15, 16]);
        let mut back = vec![text.len()];
        let mut at = text.len();
        while at > 0 {
            at = prev_grapheme(text, at);
            back.push(at);
        }
        back.reverse();
        assert_eq!(back, stops);
    }

    #[test]
    fn indents_are_measured_in_cells() {
        assert_eq!(indent_cells("    let x", 4), Some(4));
        assert_eq!(indent_cells("\t\tx", 4), Some(8));
        assert_eq!(indent_cells("   ", 4), None);
        assert_eq!(indent_of("  \tfoo"), "  \t");
    }
}
