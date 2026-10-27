//! The text an input method edits: the stretch of the focused field's text
//! the app shows it, with the input method's own edits applied until the
//! app shows it again.
//!
//! The app owns the text; this mirror answers the input method's questions
//! (what lies before the caret, what is selected, what is composing) at
//! once, while the edits it makes travel to the app as events. Each edit is
//! applied here as the app will apply it, so the mirror and the app agree
//! until the app shows its text again. The input method owns the
//! composition's life: whether one runs is the mirror's to say, what
//! surrounds it the app's. With no text shown (a secret, a control that
//! keeps its own document) the mirror holds only the composition.
//! Offsets are bytes; [`units`](ImeText::units) and
//! [`byte`](ImeText::byte) map the UTF-16 offsets most platforms count.

// Each platform asks its own share of these questions.
#![cfg_attr(not(test), allow(dead_code))]

use crate::{ImeEdit, RawEvent, RawImeEdit, RawImePreedit, RawText, SurroundingText, WindowId};

#[derive(Debug, Default, Clone)]
pub(crate) struct ImeText {
    /// The stretch of the field's text, with the composition as it stands.
    text: String,
    /// Where `text` starts in the field's text.
    offset: usize,
    /// Whether the app showed its text; else `text` holds only the
    /// composition.
    shown: bool,
    anchor: usize,
    focus: usize,
    /// The composition under way.
    marked: Option<(usize, usize)>,
}

impl ImeText {
    /// Take the text the app shows, or with `None` show none. A composition
    /// under way stays, over the app's own or else over its selection.
    pub(crate) fn show(&mut self, shown: Option<&SurroundingText>) {
        let marked = self.marked.map(|(m0, m1)| {
            let rel = |at: usize| at.clamp(m0, m1) - m0;
            (
                self.text[m0..m1].to_owned(),
                rel(self.anchor),
                rel(self.focus),
            )
        });
        match shown {
            Some(s) => {
                self.text.clone_from(&s.text);
                self.offset = s.offset;
                self.shown = true;
                self.anchor = s.anchor.min(s.text.len());
                self.focus = s.focus.min(s.text.len());
                self.marked = None;
                match (marked, s.composition) {
                    (Some((m, a, f)), over) => {
                        let (c0, c1) = over.unwrap_or_else(|| self.selection());
                        self.place_marked(c0, c1.min(self.text.len()), &m, a, f);
                    }
                    (None, Some(_)) => {}
                    (None, None) => {}
                }
            }
            None => {
                *self = Self::default();
                if let Some((m, a, f)) = marked {
                    self.place_marked(0, 0, &m, a, f);
                }
            }
        }
    }

    /// Put composition `m` over `[start, end)`, its selection `a`..`f` into it.
    fn place_marked(&mut self, start: usize, end: usize, m: &str, a: usize, f: usize) {
        self.text.replace_range(start..end, m);
        self.marked = Some((start, start + m.len()));
        self.anchor = start + a;
        self.focus = start + f;
    }

    /// The text, the composition included.
    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    /// The selection's ordered bounds.
    pub(crate) fn selection(&self) -> (usize, usize) {
        (self.anchor.min(self.focus), self.anchor.max(self.focus))
    }

    /// The selection's anchor and focus.
    pub(crate) fn ends(&self) -> (usize, usize) {
        (self.anchor, self.focus)
    }

    pub(crate) fn marked(&self) -> Option<(usize, usize)> {
        self.marked
    }

    pub(crate) fn is_marked(&self) -> bool {
        self.marked.is_some()
    }

    /// Whether the app showed its text.
    pub(crate) fn is_shown(&self) -> bool {
        self.shown
    }

    /// Byte `at` as UTF-16 units from the start.
    pub(crate) fn units(&self, at: usize) -> usize {
        let at = self.boundary(at);
        self.text[..at].encode_utf16().count()
    }

    /// UTF-16 offset `units` as a byte offset, rounded down to a character.
    pub(crate) fn byte(&self, units: usize) -> usize {
        super::utf16::byte_offset(&self.text, units)
    }

    /// The text's length in UTF-16 units.
    pub(crate) fn len_units(&self) -> usize {
        self.text.encode_utf16().count()
    }

    /// Byte `at`, within the text and on a `char` boundary.
    fn boundary(&self, at: usize) -> usize {
        self.text.floor_char_boundary(at.min(self.text.len()))
    }

    /// The ordered range between bytes `a` and `b`, on boundaries.
    pub(crate) fn span(&self, a: usize, b: usize) -> (usize, usize) {
        let (a, b) = (self.boundary(a), self.boundary(b));
        (a.min(b), a.max(b))
    }

    /// What the composition replaces, or else the selection.
    fn target(&self) -> (usize, usize) {
        self.marked.unwrap_or_else(|| self.selection())
    }

    /// Bytes of the field's text, from bytes of this stretch.
    fn field(&self, (a, b): (usize, usize)) -> core::ops::Range<usize> {
        self.offset + a..self.offset + b
    }

    /// Compose `text`, its selection `selected` (bytes into it), over the
    /// composition, else over `over` (an autocorrect or a reconversion
    /// taking up committed text), else over the selection. An empty text
    /// drops the composition.
    pub(crate) fn set_marked(
        &mut self,
        window: WindowId,
        text: &str,
        selected: (usize, usize),
        over: Option<(usize, usize)>,
    ) -> Option<RawEvent> {
        if text.is_empty() {
            return self.cancel(window);
        }
        let caret = text.floor_char_boundary(selected.1.min(text.len()));
        let start = text.floor_char_boundary(selected.0.min(caret));
        let over = over.map(|(a, b)| self.span(a, b));
        let event = match over {
            Some(range) if self.shown && range != self.target() => {
                self.marked = None;
                RawEvent::ImeEdit(RawImeEdit {
                    window,
                    edit: ImeEdit::Recompose {
                        range: self.field(range),
                        text: text.to_owned(),
                        caret,
                    },
                })
            }
            _ => RawEvent::ImePreedit(RawImePreedit {
                window,
                text: text.to_owned(),
                caret,
            }),
        };
        let (a, b) = over.filter(|_| self.shown).unwrap_or_else(|| self.target());
        self.place_marked(a, b, text, start, caret);
        Some(event)
    }

    /// Commit `text` over the composition, else over `over`, else over the
    /// selection.
    pub(crate) fn insert(
        &mut self,
        window: WindowId,
        text: &str,
        over: Option<(usize, usize)>,
    ) -> Option<RawEvent> {
        let over = over.map(|(a, b)| self.span(a, b));
        if let Some(range) = over
            && self.shown
            && range != self.target()
        {
            return self.replace(window, range.0, range.1, text);
        }
        let was_marked = self.marked.is_some();
        let (a, b) = self.target();
        self.marked = None;
        self.text.replace_range(a..b, text);
        self.anchor = a + text.len();
        self.focus = self.anchor;
        self.forget_unshown();
        (was_marked || !text.is_empty()).then(|| {
            RawEvent::Text(RawText {
                window,
                text: text.to_owned(),
            })
        })
    }

    /// With no text shown, what was committed is the app's alone.
    fn forget_unshown(&mut self) {
        if !self.shown && self.marked.is_none() {
            *self = Self::default();
        }
    }

    /// The composition as it stands, its caret where the selection ends.
    pub(crate) fn preedit(&self, window: WindowId) -> Option<RawEvent> {
        let (m0, m1) = self.marked?;
        Some(RawEvent::ImePreedit(RawImePreedit {
            window,
            text: self.text[m0..m1].to_owned(),
            caret: self.focus.clamp(m0, m1) - m0,
        }))
    }

    /// What taking `shown` changes: the text, else only the selection.
    pub(crate) fn change(&self, shown: Option<&SurroundingText>) -> (bool, bool) {
        let mut next = self.clone();
        next.show(shown);
        let text = next.text != self.text || next.marked != self.marked;
        (text, !text && next.ends() != self.ends())
    }

    /// Accept the composition as it stands.
    pub(crate) fn unmark(&mut self, window: WindowId) -> Option<RawEvent> {
        let (m0, m1) = self.marked.take()?;
        self.anchor = m1;
        self.focus = m1;
        let text = self.text[m0..m1].to_owned();
        self.forget_unshown();
        Some(RawEvent::Text(RawText { window, text }))
    }

    /// Drop the composition, taking its text back.
    pub(crate) fn cancel(&mut self, window: WindowId) -> Option<RawEvent> {
        let (m0, m1) = self.marked.take()?;
        self.text.replace_range(m0..m1, "");
        self.anchor = m0;
        self.focus = m0;
        self.forget_unshown();
        Some(RawEvent::ImePreedit(RawImePreedit {
            window,
            text: String::new(),
            caret: 0,
        }))
    }

    /// Replace `[start, end)` with `text`, as the app replaces it: inside
    /// the composition it edits the composition; a composition it keeps
    /// clear of goes on, one it touches ends first; a selection it keeps
    /// clear of keeps its place, else the caret follows `text`. With no
    /// text shown, what lies outside the composition is unknown, so the
    /// edit is a commit.
    pub(crate) fn replace(
        &mut self,
        window: WindowId,
        start: usize,
        end: usize,
        text: &str,
    ) -> Option<RawEvent> {
        let (start, end) = self.span(start, end);
        if let Some((m0, m1)) = self.marked
            && m0 <= start
            && end <= m1
        {
            self.text.replace_range(start..end, text);
            let m1 = m1 + text.len() - (end - start);
            let caret = start + text.len();
            if m0 == m1 {
                self.marked = None;
                self.anchor = m0;
                self.focus = m0;
                return Some(RawEvent::ImePreedit(RawImePreedit {
                    window,
                    text: String::new(),
                    caret: 0,
                }));
            }
            self.marked = Some((m0, m1));
            self.anchor = caret;
            self.focus = caret;
            return Some(RawEvent::ImePreedit(RawImePreedit {
                window,
                text: self.text[m0..m1].to_owned(),
                caret: caret - m0,
            }));
        }
        if !self.shown {
            return self.insert(window, text, None);
        }
        if self.marked.is_some_and(|(m0, m1)| start < m1 && end > m0) {
            self.marked = None;
        }
        let (s0, s1) = self.selection();
        let outside = end < s0 || start > s1 || (start != end && (end <= s0 || start >= s1));
        self.text.replace_range(start..end, text);
        let moved = |at: usize| {
            if at >= end {
                at + text.len() - (end - start)
            } else {
                at
            }
        };
        if outside {
            self.anchor = moved(self.anchor);
            self.focus = moved(self.focus);
        } else {
            self.anchor = start + text.len();
            self.focus = self.anchor;
        }
        if let Some((m0, m1)) = self.marked
            && m0 >= end
        {
            self.marked = Some((moved(m0), moved(m1)));
        }
        Some(RawEvent::ImeEdit(RawImeEdit {
            window,
            edit: ImeEdit::Replace {
                range: self.offset + start..self.offset + end,
                text: text.to_owned(),
            },
        }))
    }

    /// Select from `anchor` to `focus`. Inside the composition the selection
    /// is the composition's own, which its next update carries.
    pub(crate) fn select(
        &mut self,
        window: WindowId,
        anchor: usize,
        focus: usize,
    ) -> Option<RawEvent> {
        let (anchor, focus) = (self.boundary(anchor), self.boundary(focus));
        if (anchor, focus) == (self.anchor, self.focus) {
            return None;
        }
        let inside = self
            .marked
            .is_some_and(|(m0, m1)| (m0..=m1).contains(&anchor) && (m0..=m1).contains(&focus));
        self.anchor = anchor;
        self.focus = focus;
        (self.shown && !inside).then(|| {
            RawEvent::ImeEdit(RawImeEdit {
                window,
                edit: ImeEdit::Select {
                    anchor: self.offset + anchor,
                    focus: self.offset + focus,
                },
            })
        })
    }

    /// Delete `before` bytes before the composition, or else the selection,
    /// and `after` bytes after it, keeping it.
    pub(crate) fn delete_around(
        &mut self,
        window: WindowId,
        before: usize,
        after: usize,
    ) -> Vec<RawEvent> {
        let mut out = Vec::new();
        let (s0, s1) = self.target();
        if after > 0 {
            let end = self
                .text
                .ceil_char_boundary(s1.saturating_add(after).min(self.text.len()));
            out.extend(self.replace(window, s1, end, ""));
        }
        if before > 0 {
            let start = self.boundary(s0.saturating_sub(before));
            out.extend(self.replace(window, start, s0, ""));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: WindowId = WindowId(1);

    fn shown(text: &str, offset: usize, anchor: usize, focus: usize) -> SurroundingText {
        SurroundingText {
            text: text.to_owned(),
            offset,
            anchor,
            focus,
            composition: None,
        }
    }

    fn preedit(text: &str, caret: usize) -> RawEvent {
        RawEvent::ImePreedit(RawImePreedit {
            window: W,
            text: text.to_owned(),
            caret,
        })
    }

    fn commit(text: &str) -> RawEvent {
        RawEvent::Text(RawText {
            window: W,
            text: text.to_owned(),
        })
    }

    fn edit(edit: ImeEdit) -> RawEvent {
        RawEvent::ImeEdit(RawImeEdit { window: W, edit })
    }

    #[test]
    fn a_composition_previews_then_commits_over_the_selection() {
        let mut t = ImeText::default();
        t.show(Some(&shown("ab cd", 100, 3, 5)));
        assert_eq!(t.set_marked(W, "に", (3, 3), None), Some(preedit("に", 3)));
        assert_eq!(t.text(), "ab に");
        assert_eq!(t.marked(), Some((3, 6)));
        assert_eq!(t.insert(W, "二", None), Some(commit("二")));
        assert_eq!(
            (t.text(), t.selection(), t.marked()),
            ("ab 二", (6, 6), None)
        );
    }

    #[test]
    fn reconversion_composes_over_committed_text() {
        // The input method reads the selection, then composes over it again.
        let mut t = ImeText::default();
        t.show(Some(&shown("今日は漢字", 10, 9, 15)));
        let (s0, s1) = t.selection();
        assert_eq!(&t.text()[s0..s1], "漢字");
        assert_eq!(
            t.set_marked(W, "かんじ", (0, 9), Some((s0, s1))),
            Some(preedit("かんじ", 9))
        );
        // Over the word before the caret instead of the selection.
        let mut t = ImeText::default();
        t.show(Some(&shown("今日は漢字", 10, 15, 15)));
        assert_eq!(
            t.set_marked(W, "かんじ", (9, 9), Some((9, 15))),
            Some(edit(ImeEdit::Recompose {
                range: 19..25,
                text: "かんじ".into(),
                caret: 9,
            }))
        );
        assert_eq!(t.text(), "今日はかんじ");
        assert_eq!(t.marked(), Some((9, 18)));
        assert_eq!(t.insert(W, "感じ", None), Some(commit("感じ")));
        assert_eq!(t.text(), "今日は感じ");
    }

    #[test]
    fn an_autocorrect_replaces_the_word_before_the_caret() {
        let mut t = ImeText::default();
        t.show(Some(&shown("say teh ", 0, 8, 8)));
        assert_eq!(
            t.insert(W, "the", Some((4, 7))),
            Some(edit(ImeEdit::Replace {
                range: 4..7,
                text: "the".into(),
            }))
        );
        // The caret after the space keeps its place.
        assert_eq!((t.text(), t.selection()), ("say the ", (8, 8)));
    }

    #[test]
    fn deleting_around_keeps_the_selection_and_the_composition() {
        let mut t = ImeText::default();
        t.show(Some(&shown("ab cd ef", 5, 3, 5)));
        let out = t.delete_around(W, 1, 1);
        assert_eq!(
            out,
            vec![
                edit(ImeEdit::Replace {
                    range: 10..11,
                    text: String::new(),
                }),
                edit(ImeEdit::Replace {
                    range: 7..8,
                    text: String::new(),
                }),
            ]
        );
        assert_eq!((t.text(), t.selection()), ("abcdef", (2, 4)));
        // A composition after the deleted text moves with it.
        let mut t = ImeText::default();
        t.show(Some(&shown("ab ", 0, 3, 3)));
        t.set_marked(W, "cd", (2, 2), None);
        t.delete_around(W, 1, 0);
        assert_eq!(
            (t.text(), t.marked(), t.selection()),
            ("abcd", Some((2, 4)), (4, 4))
        );
    }

    #[test]
    fn a_composition_outlives_the_app_showing_text_from_before_it() {
        let mut t = ImeText::default();
        t.show(Some(&shown("ab", 0, 2, 2)));
        t.set_marked(W, "x", (1, 1), None);
        // Shown again before the app took the preedit.
        t.show(Some(&shown("ab", 0, 2, 2)));
        assert_eq!((t.text(), t.marked()), ("abx", Some((2, 3))));
        // And after, with the composition in it.
        t.show(Some(&SurroundingText {
            composition: Some((2, 3)),
            ..shown("abx", 0, 3, 3)
        }));
        assert_eq!(
            (t.text(), t.marked(), t.selection()),
            ("abx", Some((2, 3)), (3, 3))
        );
        // With no text shown, only the composition is left.
        t.show(None);
        assert_eq!((t.text(), t.marked()), ("x", Some((0, 1))));
        assert!(!t.is_shown());
    }

    #[test]
    fn with_no_text_shown_ranged_edits_commit() {
        let mut t = ImeText::default();
        t.show(None);
        assert_eq!(t.insert(W, "a", Some((0, 0))), Some(commit("a")));
        assert_eq!(t.select(W, 0, 0), None);
        t.set_marked(W, "ab", (2, 2), None);
        assert_eq!(t.replace(W, 1, 2, "c"), Some(preedit("ac", 2)));
        assert_eq!(t.unmark(W), Some(commit("ac")));
    }

    #[test]
    fn selecting_reports_outside_the_composition() {
        let mut t = ImeText::default();
        t.show(Some(&shown("hello", 40, 5, 5)));
        assert_eq!(
            t.select(W, 0, 5),
            Some(edit(ImeEdit::Select {
                anchor: 40,
                focus: 45,
            }))
        );
        assert_eq!(t.ends(), (0, 5));
        t.set_marked(W, "ab", (2, 2), None);
        assert_eq!(t.select(W, 1, 1), None);
    }

    #[test]
    fn showing_text_tells_what_it_changes() {
        let mut t = ImeText::default();
        t.show(Some(&shown("ab", 0, 2, 2)));
        assert_eq!(t.change(Some(&shown("ab", 0, 2, 2))), (false, false));
        assert_eq!(t.change(Some(&shown("ab", 0, 0, 2))), (false, true));
        assert_eq!(t.change(Some(&shown("abc", 0, 2, 2))), (true, false));
        // The app showing what the input method's edit made is no change.
        t.insert(W, "c", None);
        assert_eq!(t.change(Some(&shown("abc", 0, 3, 3))), (false, false));
        t.set_marked(W, "xy", (1, 1), None);
        assert_eq!(t.preedit(W), Some(preedit("xy", 1)));
    }

    #[test]
    fn utf16_offsets_map_onto_bytes() {
        let mut t = ImeText::default();
        t.show(Some(&shown("a你😀b", 0, 0, 0)));
        assert_eq!(t.len_units(), 5);
        assert_eq!(t.units(4), 2);
        assert_eq!(t.byte(4), 8);
        assert_eq!(t.units(5), 2);
    }
}
