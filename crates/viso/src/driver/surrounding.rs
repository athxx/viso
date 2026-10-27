//! The stretch of a focused field's text its input method reads.

use viso_platform::SurroundingText;

/// The text shown before the selection, in bytes.
const BEFORE: usize = 1024;

/// The text shown after the selection, in bytes.
const AFTER: usize = 1024;

/// How far the selection may move on past the start of the stretch before
/// the stretch moves after it. While the stretch holds still an input
/// method's offsets keep their meaning from one edit to the next.
const SLACK: usize = 3 * BEFORE;

/// The longest selection shown; a longer one shows no text at all.
const MOST_SELECTED: usize = 16 * 1024;

/// The stretch of `text` around the selection from `anchor` to `focus` and
/// the composition `composition`, all bytes. It starts at `kept`, where the
/// last one started, while the selection is still close enough after it;
/// else a little before the selection. `None` when the selection is too
/// long to show.
pub(crate) fn surrounding(
    text: &str,
    anchor: usize,
    focus: usize,
    composition: Option<(usize, usize)>,
    kept: Option<usize>,
) -> Option<SurroundingText> {
    let (mut lo, mut hi) = (anchor.min(focus), anchor.max(focus));
    if let Some((c0, c1)) = composition {
        lo = lo.min(c0);
        hi = hi.max(c1);
    }
    let hi = hi.min(text.len());
    let lo = lo.min(hi);
    if hi - lo > MOST_SELECTED {
        return None;
    }
    let start = match kept {
        Some(at) if at <= lo && lo - at <= SLACK && text.is_char_boundary(at) => at,
        _ => text.floor_char_boundary(lo.saturating_sub(BEFORE)),
    };
    let end = text.ceil_char_boundary(hi.saturating_add(AFTER).min(text.len()));
    let local = |at: usize| at.min(text.len()).saturating_sub(start).min(end - start);
    Some(SurroundingText {
        text: text[start..end].to_owned(),
        offset: start,
        anchor: local(anchor),
        focus: local(focus),
        composition: composition.map(|(c0, c1)| (local(c0), local(c1))),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_short_text_shows_whole() {
        let s = surrounding("hello world", 6, 11, None, None).unwrap();
        assert_eq!(s.text, "hello world");
        assert_eq!((s.offset, s.anchor, s.focus), (0, 6, 11));
        assert_eq!(s.composition, None);
    }

    #[test]
    fn a_long_text_shows_a_stretch_on_char_boundaries() {
        let text = "é".repeat(4000);
        let s = surrounding(&text, 4000, 4000, Some((3998, 4000)), None).unwrap();
        assert!(text.is_char_boundary(s.offset));
        assert_eq!(&text[s.offset..s.offset + s.text.len()], s.text);
        assert!(s.text.len() <= BEFORE + AFTER + 2);
        assert_eq!(s.offset + s.focus, 4000);
        assert_eq!(s.composition, Some((3998 - s.offset, 4000 - s.offset)));
    }

    #[test]
    fn the_stretch_holds_still_while_typing_moves_the_caret_on() {
        let text = "a".repeat(10_000);
        let first = surrounding(&text, 5000, 5000, None, None).unwrap();
        let next = surrounding(&text, 5100, 5100, None, Some(first.offset)).unwrap();
        assert_eq!(next.offset, first.offset);
        assert_eq!(next.focus, 5100 - first.offset);
        let far = surrounding(&text, 9000, 9000, None, Some(first.offset)).unwrap();
        assert_eq!(far.offset, 9000 - BEFORE);
        let back = surrounding(&text, 100, 100, None, Some(far.offset)).unwrap();
        assert_eq!(back.offset, 0);
    }

    #[test]
    fn a_huge_selection_shows_nothing() {
        let text = "a".repeat(MOST_SELECTED + 10);
        assert_eq!(surrounding(&text, 0, text.len(), None, None), None);
    }
}
