//! Line-break opportunities: where a paragraph line *may* wrap when it exceeds
//! the available width. This is the pure segmentation layer — it decides break
//! candidates from text alone, independent of any shaping or pixel widths, so it
//! is unit-testable without a font.
//!
//! Two granularities, matching the reference layouter's word→grapheme fallback:
//!
//! - **Word** opportunities come from UAX#29 word boundaries
//!   ([`unicode_segmentation`]), then merged so trailing/closing punctuation
//!   stays with the preceding word and opening punctuation stays with the
//!   following one (a period or close-paren never wraps alone; an open-paren is
//!   never stranded at a line end). These are the primary wrap points, so an
//!   English line breaks between words and a CJK line breaks per ideograph (each
//!   is its own word boundary).
//! - **Grapheme** opportunities come from UAX#29 grapheme clusters — the
//!   fallback when a single word is itself wider than the line, so it can break
//!   mid-word rather than overflow unboundedly.
//!
//! Both are reported as **byte offsets into the input** at which a break may
//! occur: the offset *after* each segment. The layout pass maps a break offset
//! to the shaped-glyph boundary at that cluster, so a single whole-line shape
//! (correct shaping context and BiDi) is reused across every candidate rather
//! than reshaping per segment.

use unicode_segmentation::UnicodeSegmentation;

/// The byte offsets in `text` at which a line may break, at word granularity:
/// the end offset of each word segment after punctuation merging. The final
/// offset (`text.len()`) is always present as the end-of-line opportunity;
/// offset `0` is never reported (a line cannot break before its first segment).
///
/// Whitespace runs are their own word segments, so a break offset can fall
/// after a space; the layout pass trims a trailing space's advance from a
/// wrapped row rather than carrying it to the next line.
pub fn word_break_offsets(text: &str) -> Vec<usize> {
    // Per-segment byte lengths from UAX#29 word bounds, then punctuation merge.
    let mut lens: Vec<usize> = text.split_word_bounds().map(str::len).collect();
    merge_punctuation(text, &mut lens);
    cumulative_offsets(&lens)
}

/// The byte offsets in `text` at which a line may break, at grapheme
/// granularity: the end offset of each extended grapheme cluster. Used only as
/// the fallback when one word overflows the line, so a break can land mid-word
/// without splitting a combining sequence. Never breaks inside a grapheme.
pub fn grapheme_break_offsets(text: &str) -> Vec<usize> {
    let lens: Vec<usize> = text.graphemes(true).map(str::len).collect();
    cumulative_offsets(&lens)
}

/// Turn per-segment byte lengths into the cumulative end offsets, dropping the
/// leading `0`. An empty input yields no opportunities.
fn cumulative_offsets(lens: &[usize]) -> Vec<usize> {
    let mut offsets = Vec::with_capacity(lens.len());
    let mut acc = 0;
    for &len in lens {
        acc += len;
        offsets.push(acc);
    }
    offsets
}

/// Merge word segments so line breaks avoid typographically wrong positions,
/// following UAX#14 / CSS Text conventions (mirrors the reference layouter):
///
/// 1. A trailing/closing-punctuation segment (`.`, `,`, `)`, `…`, …) merges
///    into the *preceding* segment, so it never wraps to a line by itself.
/// 2. An opening-punctuation segment (`(`, `[`, `«`, …) merges into the
///    *following* segment, so it is never stranded at a line end.
fn merge_punctuation(text: &str, lens: &mut Vec<usize>) {
    // Pass 1: fold "no-break-before" (closing) punctuation into the segment
    // before it.
    if lens.len() >= 2 {
        let mut i = 1;
        let mut offset = lens[0];
        while i < lens.len() {
            let end = offset + lens[i];
            if text[offset..end].chars().all(is_no_break_before) {
                lens[i - 1] += lens[i];
                lens.remove(i);
            } else {
                offset = end;
                i += 1;
            }
        }
    }
    // Pass 2: fold "no-break-after" (opening) punctuation into the segment
    // after it.
    if lens.len() >= 2 {
        let mut i = 0;
        let mut offset = 0;
        while i + 1 < lens.len() {
            let end = offset + lens[i];
            if text[offset..end].chars().all(is_no_break_after) {
                lens[i + 1] += lens[i];
                lens.remove(i);
            } else {
                offset = end;
                i += 1;
            }
        }
    }
}

/// Characters a line must not break *before* (UAX#14 classes CL/CP/EX/IS plus
/// common typographic conventions): they cling to the preceding word.
fn is_no_break_before(c: char) -> bool {
    matches!(
        c,
        // IS: infix numeric separators.
        '.' | ',' | ':' | ';'
        // EX: exclamation / interrogation.
        | '!' | '?'
        // CP/CL: closing brackets and punctuation.
        | ')' | ']' | '}'
        // CL: closing quotation marks.
        | '\u{2019}' // ’
        | '\u{201D}' // ”
        | '\u{203A}' // ›
        | '\u{00BB}' // »
        // Other common no-break-before.
        | '\u{2026}' // …
        | '%'
        | '\u{00B0}' // °
    )
}

/// Characters a line must not break *after* (UAX#14 class OP): they cling to the
/// following word.
fn is_no_break_after(c: char) -> bool {
    matches!(
        c,
        '(' | '[' | '{'
        // Opening quotation marks.
        | '\u{2018}' // ‘
        | '\u{201C}' // “
        | '\u{2039}' // ‹
        | '\u{00AB}' // «
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_text_has_no_opportunities() {
        assert_eq!(word_break_offsets(""), Vec::<usize>::new());
        assert_eq!(grapheme_break_offsets(""), Vec::<usize>::new());
    }

    #[test]
    fn word_breaks_fall_between_words_and_end() {
        // "ab cd": word segments are "ab", " ", "cd" → offsets after each.
        // The last offset is the whole length (end-of-line opportunity).
        let offsets = word_break_offsets("ab cd");
        assert_eq!(*offsets.last().unwrap(), 5, "end offset is the line length");
        // A break is offered between the two words (after "ab" or after the
        // space), and never before the first character.
        assert!(offsets.iter().all(|&o| o > 0));
        assert!(
            offsets.contains(&3),
            "break offered after 'ab ' (before 'cd'), got {offsets:?}"
        );
    }

    #[test]
    fn trailing_period_does_not_wrap_alone() {
        // "hi." — the period must stay with "hi": no break opportunity between
        // "hi" and ".", so the only offset is the end.
        let offsets = word_break_offsets("hi.");
        assert_eq!(
            offsets,
            vec![3],
            "closing '.' merges into the word, leaving only the end break"
        );
    }

    #[test]
    fn opening_paren_is_not_stranded() {
        // "a (b" — the "(" must attach to "b", so no break falls between "("
        // and "b". A break is still offered after "a ".
        let offsets = word_break_offsets("a (b");
        assert!(
            !offsets.contains(&3),
            "no break between '(' and 'b', got {offsets:?}"
        );
        assert!(
            offsets.contains(&2),
            "break still offered after 'a ', got {offsets:?}"
        );
    }

    #[test]
    fn cjk_breaks_per_ideograph() {
        // Each Han character is its own word boundary, so a CJK run offers a
        // break after every ideograph (3 chars × 3 bytes = 9 bytes).
        let text = "世界人";
        let offsets = word_break_offsets(text);
        assert_eq!(
            offsets,
            vec![3, 6, 9],
            "CJK breaks after each ideograph, got {offsets:?}"
        );
    }

    #[test]
    fn graphemes_do_not_split_combining_sequences() {
        // A base letter + combining acute is one grapheme; the fallback must not
        // offer a break inside it. "e\u{0301}x" → graphemes "é", "x".
        let text = "e\u{0301}x";
        let offsets = grapheme_break_offsets(text);
        assert_eq!(
            offsets,
            vec![3, 4],
            "break after the combined grapheme, not inside it, got {offsets:?}"
        );
    }
}
