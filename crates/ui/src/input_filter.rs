//! What a text field takes of the text typed, pasted or committed into it.
//!
//! Every piece of text entering a field passes three rules in turn: the one
//! its [`InputKind`] sets (a digits field keeps digits, a number field digits,
//! one sign at the start and, for a decimal, one separator), the field's own [`InputFilter`], and
//! its [`max_length`](crate::FieldLook::max_length), which keeps as much of
//! the piece as fits. Control characters other than a tab and a held line
//! break never enter. A piece the rules take nothing of is refused whole: the
//! selection it would replace stays. Text the field is given from outside —
//! its value, an undo — passes no rule.

use std::borrow::Cow;
use std::fmt;
use std::rc::Rc;

use viso_runtime::InputKind;
use viso_schema::number::number_char;
use viso_text::Segmenter;

use crate::text_edit::FieldLook;

/// A field's own rule for what it takes: maps each piece of text entering it
/// to what it keeps, after the rule of its [`InputKind`]. Two filters are the
/// same when they are one function.
#[derive(Clone)]
pub struct InputFilter(Rc<dyn Fn(&str) -> String>);

impl InputFilter {
    /// A filter that keeps `f(piece)` of each piece.
    pub fn new(f: impl Fn(&str) -> String + 'static) -> Self {
        InputFilter(Rc::new(f))
    }

    /// What it keeps of `piece`.
    pub fn apply(&self, piece: &str) -> String {
        (self.0)(piece)
    }
}

impl PartialEq for InputFilter {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::addr_eq(Rc::as_ptr(&self.0), Rc::as_ptr(&other.0))
    }
}

impl Eq for InputFilter {}

impl fmt::Debug for InputFilter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("InputFilter(..)")
    }
}

/// What a field shown by `look` and holding `text` takes of `piece` in
/// place of `text[start..end]`.
pub(crate) fn admit<'s>(
    look: &FieldLook,
    text: &str,
    start: usize,
    end: usize,
    piece: Cow<'s, str>,
) -> Cow<'s, str> {
    let (before, after) = (&text[..start], &text[end..]);
    let mut piece = keep_printable(piece);
    piece = match look.kind {
        InputKind::Digits => digits(piece),
        InputKind::Integer => number(piece, before, after, false),
        InputKind::Decimal => number(piece, before, after, true),
        InputKind::Phone => phone(piece),
        InputKind::Text | InputKind::Email | InputKind::Url | InputKind::Search => piece,
    };
    if let Some(filter) = &look.filter
        && !piece.is_empty()
    {
        piece = Cow::Owned(filter.apply(&piece));
    }
    match look.max_length {
        Some(max) => fit(piece, before, after, max as usize),
        None => piece,
    }
}

/// `piece` without control characters other than a tab or a line break.
fn keep_printable(piece: Cow<'_, str>) -> Cow<'_, str> {
    let control = |c: char| c.is_control() && c != '\t' && c != '\n';
    if !piece.contains(control) {
        return piece;
    }
    Cow::Owned(piece.chars().filter(|&c| !control(c)).collect())
}

/// Keeps `s` borrowed when `out` came to the same text.
fn same_or<'s>(s: Cow<'s, str>, out: String) -> Cow<'s, str> {
    if out == *s { s } else { Cow::Owned(out) }
}

/// What a number field takes of `piece` between `before` and `after`: digits,
/// a minus sign only at the very start, and, with `fraction`, one decimal
/// point in the whole text. Nothing goes in front of a sign.
fn number<'s>(piece: Cow<'s, str>, before: &str, after: &str, fraction: bool) -> Cow<'s, str> {
    let signed_after = after.starts_with('-');
    let mut at_start = before.is_empty();
    let mut point = fraction && !before.contains('.') && !after.contains('.');
    let mut out = String::with_capacity(piece.len());
    for c in piece.chars().map(number_char) {
        let keep = match c {
            '-' => at_start && !signed_after,
            '0'..='9' => !(at_start && signed_after),
            '.' if point && !(at_start && signed_after) => {
                point = false;
                true
            }
            _ => false,
        };
        if keep {
            out.push(c);
            at_start = false;
        }
    }
    same_or(piece, out)
}

/// What a digits field takes of `piece`: its digits.
fn digits(piece: Cow<'_, str>) -> Cow<'_, str> {
    let out = piece
        .chars()
        .map(number_char)
        .filter(char::is_ascii_digit)
        .collect();
    same_or(piece, out)
}

/// What a phone field takes of `piece`: digits and what dialing and writing
/// a number use besides.
fn phone(piece: Cow<'_, str>) -> Cow<'_, str> {
    let out = piece
        .chars()
        .map(number_char)
        .filter(|c| {
            c.is_ascii_digit() || matches!(c, '+' | '-' | '.' | ' ' | '(' | ')' | '*' | '#')
        })
        .collect();
    same_or(piece, out)
}

/// The graphemes of `s`.
fn graphemes(s: &str) -> usize {
    Segmenter::new(s).grapheme_boundaries().count() - 1
}

/// The graphemes of `before`, `middle` and `after` joined.
fn joined(before: &str, middle: &str, after: &str) -> usize {
    let mut text = String::with_capacity(before.len() + middle.len() + after.len());
    text.push_str(before);
    text.push_str(middle);
    text.push_str(after);
    graphemes(&text)
}

/// As much of the start of `piece` as keeps the text between `before` and
/// `after` within `max` graphemes.
fn fit<'s>(piece: Cow<'s, str>, before: &str, after: &str, max: usize) -> Cow<'s, str> {
    // Joining only merges graphemes, so pieces that fit apart fit together.
    if graphemes(before) + graphemes(&piece) + graphemes(after) <= max {
        return piece;
    }
    let ends: Vec<usize> = Segmenter::new(&piece)
        .grapheme_boundaries()
        .map(|b| b.0)
        .collect();
    let mut take = max
        .saturating_sub(joined(before, "", after))
        .min(ends.len() - 1);
    // A grapheme that merges into its neighbor costs none.
    while take + 1 < ends.len() && joined(before, &piece[..ends[take + 1]], after) <= max {
        take += 1;
    }
    match piece {
        Cow::Borrowed(s) => Cow::Borrowed(&s[..ends[take]]),
        Cow::Owned(mut s) => {
            s.truncate(ends[take]);
            Cow::Owned(s)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn take(look: &FieldLook, text: &str, at: usize, piece: &str) -> String {
        admit(look, text, at, at, Cow::Borrowed(piece)).into_owned()
    }

    fn kind(kind: InputKind) -> FieldLook {
        FieldLook {
            kind,
            ..FieldLook::default()
        }
    }

    #[test]
    fn an_integer_takes_digits_and_a_leading_sign() {
        let look = kind(InputKind::Integer);
        assert_eq!(take(&look, "", 0, "-12a3.4"), "-1234");
        assert_eq!(take(&look, "12", 1, "-5"), "5", "no sign inside");
        assert_eq!(take(&look, "-12", 0, "3"), "", "nothing before the sign");
        assert_eq!(take(&look, "-12", 0, "-"), "", "one sign");
        assert_eq!(take(&look, "", 0, "\u{FF11}\u{FF12}"), "12", "full-width");
        assert!(matches!(
            admit(&look, "", 0, 0, Cow::Borrowed("42")),
            Cow::Borrowed(_)
        ));
    }

    #[test]
    fn digits_take_no_sign() {
        assert_eq!(take(&kind(InputKind::Digits), "", 0, "-1.5\u{FF19}"), "159");
    }

    #[test]
    fn a_decimal_takes_one_point_and_reads_a_comma_as_one() {
        let look = kind(InputKind::Decimal);
        assert_eq!(take(&look, "", 0, "3,14.15"), "3.1415");
        assert_eq!(take(&look, "3.1", 3, "."), "");
        assert_eq!(
            admit(&look, "3.1", 1, 3, Cow::Borrowed(".5")),
            ".5",
            "the point replaced"
        );
    }

    #[test]
    fn a_phone_takes_what_dialing_uses() {
        let look = kind(InputKind::Phone);
        assert_eq!(take(&look, "", 0, "+1 (555) 01-23#x"), "+1 (555) 01-23#");
    }

    #[test]
    fn control_characters_never_enter() {
        let look = FieldLook::default();
        assert_eq!(take(&look, "", 0, "a\u{1B}[b\u{7}\tc"), "a[b\tc");
    }

    #[test]
    fn the_filter_maps_what_the_kind_keeps() {
        let look = FieldLook {
            kind: InputKind::Text,
            filter: Some(InputFilter::new(|s| s.to_uppercase())),
            ..FieldLook::default()
        };
        assert_eq!(take(&look, "", 0, "abc"), "ABC");
        assert_eq!(look.clone(), look, "a clone is the same filter");
        let other = FieldLook {
            filter: Some(InputFilter::new(|s| s.to_uppercase())),
            ..FieldLook::default()
        };
        assert_ne!(look, other);
    }

    #[test]
    fn a_max_length_keeps_what_fits_in_graphemes() {
        let look = FieldLook {
            max_length: Some(4),
            ..FieldLook::default()
        };
        assert_eq!(take(&look, "ab", 2, "cdef"), "cd");
        assert_eq!(take(&look, "abcd", 4, "e"), "");
        assert_eq!(
            admit(&look, "abcd", 1, 3, Cow::Borrowed("xyz")),
            "xy",
            "the selection makes room"
        );
        // A flag is one grapheme of eight bytes; a combining mark merges.
        assert_eq!(take(&look, "ab", 2, "🇯🇵🇯🇵🇯🇵"), "🇯🇵🇯🇵");
        assert_eq!(take(&look, "abcd", 4, "\u{301}"), "\u{301}");
        assert_eq!(take(&look, "abcdef", 6, "g"), "", "already over");
    }
}
