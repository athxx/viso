//! The document UIKit's text system edits through `UITextInput`.
//!
//! The app owns the real text, so the view presents UIKit a shadow document
//! holding only the composition in progress: empty between compositions, the
//! marked text while one runs. Offsets are UTF-16 code units, as UIKit
//! counts them; every change is turned into the preedit and commit events
//! the app applies to its own buffer. Pure, so tested on every host.

#![cfg_attr(not(target_os = "ios"), allow(dead_code))]

/// What a change to the document means for the app.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Edit {
    /// Show `text` as the composition, caret at byte `caret`; empty clears it.
    Preedit { text: String, caret: usize },
    /// Insert `text` at the app's caret.
    Commit(String),
}

#[derive(Default)]
pub(crate) struct Composition {
    text: String,
    marked: bool,
    /// The selection, UTF-16 offsets into `text`.
    selection: (usize, usize),
}

impl Composition {
    /// The document's length in UTF-16 units.
    pub(crate) fn len(&self) -> usize {
        self.text.encode_utf16().count()
    }

    pub(crate) fn is_marked(&self) -> bool {
        self.marked
    }

    pub(crate) fn marked_range(&self) -> Option<(usize, usize)> {
        self.marked.then(|| (0, self.len()))
    }

    pub(crate) fn selection(&self) -> (usize, usize) {
        self.selection
    }

    pub(crate) fn set_selection(&mut self, start: usize, end: usize) {
        self.selection = self.clamp_range(start, end);
    }

    fn clamp_range(&self, start: usize, end: usize) -> (usize, usize) {
        let len = self.len();
        let (a, b) = (start.min(len), end.min(len));
        (a.min(b), a.max(b))
    }

    /// The text between two UTF-16 offsets.
    pub(crate) fn text_in(&self, start: usize, end: usize) -> String {
        let (a, b) = self.clamp_range(start, end);
        let (a, b) = (byte_offset(&self.text, a), byte_offset(&self.text, b));
        self.text[a..b].to_owned()
    }

    /// `setMarkedText:selectedRange:` — the composition becomes `text` with
    /// the selection `selected` relative to it. An empty text ends the
    /// composition without committing.
    pub(crate) fn set_marked(&mut self, text: &str, selected: (usize, usize)) -> Edit {
        if text.is_empty() {
            *self = Self::default();
            return Edit::Preedit {
                text: String::new(),
                caret: 0,
            };
        }
        self.text = text.to_owned();
        self.marked = true;
        let (start, len) = selected;
        self.selection = self.clamp_range(start, start.saturating_add(len));
        self.preedit()
    }

    /// `unmarkText` — the composition is accepted as it stands.
    pub(crate) fn unmark(&mut self) -> Vec<Edit> {
        let text = std::mem::take(&mut self.text);
        let was_marked = std::mem::take(&mut self.marked);
        self.selection = (0, 0);
        let mut out = Vec::new();
        if was_marked {
            out.push(Edit::Preedit {
                text: String::new(),
                caret: 0,
            });
            if !text.is_empty() {
                out.push(Edit::Commit(text));
            }
        }
        out
    }

    /// `insertText:` — replaces the composition (if any) and commits.
    pub(crate) fn insert(&mut self, text: &str) -> Vec<Edit> {
        let was_marked = self.marked;
        *self = Self::default();
        let mut out = Vec::new();
        if was_marked {
            out.push(Edit::Preedit {
                text: String::new(),
                caret: 0,
            });
        }
        if !text.is_empty() {
            out.push(Edit::Commit(text.to_owned()));
        }
        out
    }

    /// `replaceRange:withText:` — inside a composition the marked text is
    /// edited in place; with none, the replacement is a commit.
    pub(crate) fn replace(&mut self, start: usize, end: usize, text: &str) -> Vec<Edit> {
        if !self.marked {
            return self.insert(text);
        }
        let (a, b) = self.clamp_range(start, end);
        let (ba, bb) = (byte_offset(&self.text, a), byte_offset(&self.text, b));
        self.text.replace_range(ba..bb, text);
        let caret = a + text.encode_utf16().count();
        self.selection = (caret, caret);
        if self.text.is_empty() {
            *self = Self::default();
            return vec![Edit::Preedit {
                text: String::new(),
                caret: 0,
            }];
        }
        vec![self.preedit()]
    }

    /// Drop the composition without committing it; the edit clearing the
    /// preedit, if one was showing.
    pub(crate) fn cancel(&mut self) -> Option<Edit> {
        let was_marked = self.marked;
        *self = Self::default();
        was_marked.then(|| Edit::Preedit {
            text: String::new(),
            caret: 0,
        })
    }

    /// The preedit showing the composition as it stands, if one runs.
    pub(crate) fn current(&self) -> Option<Edit> {
        self.marked.then(|| self.preedit())
    }

    fn preedit(&self) -> Edit {
        Edit::Preedit {
            text: self.text.clone(),
            caret: byte_offset(&self.text, self.selection.1),
        }
    }
}

/// The byte offset of UTF-16 offset `units` in `text`, rounded down to a
/// character boundary (an offset between the halves of a surrogate pair
/// names the character they form).
pub(crate) fn byte_offset(text: &str, units: usize) -> usize {
    let mut seen = 0;
    for (byte, c) in text.char_indices() {
        let next = seen + c.len_utf16();
        if next > units {
            return byte;
        }
        seen = next;
    }
    text.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn preedit(text: &str, caret: usize) -> Edit {
        Edit::Preedit {
            text: text.into(),
            caret,
        }
    }

    #[test]
    fn utf16_offsets_become_byte_offsets() {
        assert_eq!(byte_offset("a你😀b", 0), 0);
        assert_eq!(byte_offset("a你😀b", 1), 1);
        assert_eq!(byte_offset("a你😀b", 2), 4);
        // Between the surrogate halves of the emoji.
        assert_eq!(byte_offset("a你😀b", 3), 4);
        assert_eq!(byte_offset("a你😀b", 4), 8);
        assert_eq!(byte_offset("a你😀b", 99), 9);
    }

    #[test]
    fn a_composition_previews_then_commits() {
        let mut doc = Composition::default();
        assert_eq!(doc.set_marked("ni", (2, 0)), preedit("ni", 2));
        assert_eq!(doc.marked_range(), Some((0, 2)));
        assert_eq!(doc.set_marked("你", (1, 0)), preedit("你", 3));
        assert_eq!(doc.unmark(), [preedit("", 0), Edit::Commit("你".into())]);
        assert_eq!(doc.len(), 0);
        assert!(!doc.is_marked());
    }

    #[test]
    fn inserting_replaces_the_composition() {
        let mut doc = Composition::default();
        doc.set_marked("zhong", (5, 0));
        assert_eq!(
            doc.insert("中"),
            [preedit("", 0), Edit::Commit("中".into())]
        );
        assert_eq!(doc.insert("a"), [Edit::Commit("a".into())]);
    }

    #[test]
    fn an_empty_marked_text_cancels() {
        let mut doc = Composition::default();
        doc.set_marked("ka", (2, 0));
        assert_eq!(doc.set_marked("", (0, 0)), preedit("", 0));
        assert!(doc.unmark().is_empty());
        assert_eq!(doc.cancel(), None);
        assert_eq!(doc.current(), None);
    }

    #[test]
    fn replacing_inside_a_composition_edits_it() {
        let mut doc = Composition::default();
        doc.set_marked("かな", (2, 0));
        assert_eq!(doc.replace(1, 2, "ン"), [preedit("かン", 6)]);
        assert_eq!(doc.text_in(0, 1), "か");
        assert_eq!(doc.replace(0, 2, ""), [preedit("", 0)]);
        assert!(!doc.is_marked());
        assert_eq!(doc.replace(0, 0, "x"), [Edit::Commit("x".into())]);
    }

    #[test]
    fn selections_clamp_and_order() {
        let mut doc = Composition::default();
        doc.set_marked("abc", (1, 1));
        assert_eq!(doc.selection(), (1, 2));
        doc.set_selection(9, 0);
        assert_eq!(doc.selection(), (0, 3));
        assert_eq!(doc.current(), Some(preedit("abc", 3)));
    }
}
