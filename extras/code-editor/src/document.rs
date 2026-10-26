//! The document: text in a rope, the line ending it is saved with, and the
//! edits that change it.
//!
//! The text is kept with `'\n'` line breaks only, whatever the file had: a
//! file read with `"\r\n"` is kept as `'\n'` and remembered as
//! [`LineEnding::CrLf`], so every offset, line and column means one thing
//! and [`Document::save_text`] writes it back as it came.
//!
//! Offsets are bytes into that text. An edit is a [`ChangeSet`]: replacements
//! of byte ranges of the text before it, sorted and apart, one per cursor.
//! Applying it hands back the set that undoes it and the [`LineSplice`]s that
//! say which lines it replaced with how many, so every array kept a line at a
//! time — line ids, highlight states, wrap counts — follows the text without
//! being rebuilt.

use std::borrow::Cow;
use std::ops::Range;

use crate::rope::Rope;

/// How a document's lines end when saved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LineEnding {
    /// `'\n'`.
    #[default]
    Lf,
    /// `"\r\n"`.
    CrLf,
}

impl LineEnding {
    /// The ending `text` uses: the one its first line break has.
    pub fn of(text: &str) -> LineEnding {
        match text.find('\n') {
            Some(i) if i > 0 && text.as_bytes()[i - 1] == b'\r' => LineEnding::CrLf,
            _ => LineEnding::Lf,
        }
    }

    /// Its bytes.
    pub fn as_str(self) -> &'static str {
        match self {
            LineEnding::Lf => "\n",
            LineEnding::CrLf => "\r\n",
        }
    }
}

/// `text` with every `"\r\n"` as `'\n'`, borrowed when it has none.
pub fn normalize(text: &str) -> Cow<'_, str> {
    if !text.contains("\r\n") {
        return Cow::Borrowed(text);
    }
    Cow::Owned(text.replace("\r\n", "\n"))
}

/// Whether side of an edit a position stays on when the edit is at it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Assoc {
    /// Before text inserted at it: a position that does not move.
    Before,
    /// After text inserted at it: a caret that typed it.
    After,
}

/// Bytes `range` of a text replaced by `text`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub range: Range<usize>,
    pub text: String,
}

impl Change {
    /// `text` inserted at `at`.
    pub fn insert(at: usize, text: impl Into<String>) -> Change {
        Change {
            range: at..at,
            text: text.into(),
        }
    }

    /// `range` removed.
    pub fn delete(range: Range<usize>) -> Change {
        Change {
            range,
            text: String::new(),
        }
    }

    /// `range` replaced by `text`.
    pub fn replace(range: Range<usize>, text: impl Into<String>) -> Change {
        Change {
            range,
            text: text.into(),
        }
    }

    /// Whether it changes nothing.
    pub fn is_noop(&self) -> bool {
        self.range.is_empty() && self.text.is_empty()
    }
}

/// Changes to one text, made at once: sorted by where they start and apart,
/// each in the offsets of the text before any of them.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ChangeSet {
    changes: Vec<Change>,
}

impl ChangeSet {
    /// The set of `changes`, in any order. Changes that overlap are cut so
    /// each keeps the range the one before it left: they must not, and debug
    /// builds say so.
    pub fn new(mut changes: Vec<Change>) -> ChangeSet {
        changes.retain(|c| !c.is_noop());
        changes.sort_by_key(|c| (c.range.start, c.range.end));
        for i in 1..changes.len() {
            let end = changes[i - 1].range.end;
            debug_assert!(changes[i].range.start >= end, "overlapping changes");
            let c = &mut changes[i];
            c.range.start = c.range.start.max(end);
            c.range.end = c.range.end.max(c.range.start);
        }
        ChangeSet { changes }
    }

    /// The set of one change.
    pub fn one(change: Change) -> ChangeSet {
        ChangeSet::new(vec![change])
    }

    /// Its changes, in order.
    pub fn changes(&self) -> &[Change] {
        &self.changes
    }

    /// Whether it changes nothing.
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }

    /// The bytes it keeps: its texts and ranges.
    pub fn cost(&self) -> usize {
        self.changes
            .iter()
            .map(|c| c.text.len() + std::mem::size_of::<Change>())
            .sum::<usize>()
            + std::mem::size_of::<ChangeSet>()
    }

    /// Where position `at` of the text before is in the text after.
    pub fn map(&self, at: usize, assoc: Assoc) -> usize {
        let mut delta = 0isize;
        for c in &self.changes {
            let (s, e) = (c.range.start, c.range.end);
            if at < s || (at == s && assoc == Assoc::Before) {
                break;
            }
            let grow = c.text.len() as isize - (e - s) as isize;
            if at > e || (at == e && s < e) {
                delta += grow;
                continue;
            }
            if s == e {
                // After an insert at it; another insert may follow at it.
                delta += grow;
                continue;
            }
            // Inside a replaced range: to an end of its replacement.
            let to = match assoc {
                Assoc::Before => s,
                Assoc::After => s + c.text.len(),
            };
            return (to as isize + delta) as usize;
        }
        (at as isize + delta) as usize
    }

    /// The text after it changes `text`, a string.
    pub fn apply_to_string(&self, text: &mut String) {
        for c in self.changes.iter().rev() {
            text.replace_range(c.range.clone(), &c.text);
        }
    }

    /// Where each change's text lands in the text after.
    pub fn ranges_after(&self) -> impl Iterator<Item = Range<usize>> + '_ {
        let mut delta = 0isize;
        self.changes.iter().map(move |c| {
            let start = (c.range.start as isize + delta) as usize;
            delta += c.text.len() as isize - c.range.len() as isize;
            start..start + c.text.len()
        })
    }
}

/// Lines `old..old + removed` of a text before an edit, replaced by lines
/// `new..new + added` after it. An edit's splices are sorted and apart:
/// applied to a line-indexed array last to first, each sees the offsets it
/// was made in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineSplice {
    /// The first line replaced, before the edit.
    pub old: usize,
    /// Where its replacement starts, after the edit.
    pub new: usize,
    /// Lines replaced: at least one, the line an edit inside it changed.
    pub removed: usize,
    /// Lines put in their place: at least one.
    pub added: usize,
}

impl LineSplice {
    /// Applies `splices` to `lines`, a value per line, new lines made by
    /// `fill` from their index after the edit. A replaced line's first new
    /// line keeps its value when `keep_first`.
    pub fn apply<T>(
        splices: &[LineSplice],
        lines: &mut Vec<T>,
        keep_first: bool,
        mut fill: impl FnMut(usize) -> T,
    ) {
        for s in splices.iter().rev() {
            let old = s.old.min(lines.len());
            let end = (s.old + s.removed).min(lines.len());
            let keep = usize::from(keep_first && end > old);
            let from = old + keep;
            let start = s.new + keep;
            let made = (start..s.new + s.added).map(&mut fill);
            lines.splice(from..end.max(from), made);
        }
    }
}

/// What applying an edit did.
#[derive(Debug, Clone)]
pub struct Applied {
    /// The edit that undoes it.
    pub inverse: ChangeSet,
    /// The lines it replaced.
    pub splices: Vec<LineSplice>,
}

/// A document: its text, how its lines end, and a stable id for each line.
#[derive(Debug, Clone)]
pub struct Document {
    text: Rope,
    ending: LineEnding,
    /// Bumped by every edit.
    version: u64,
    /// One per line, kept by a line an edit changes inside: what keeps a
    /// line's drawn row as lines above it come and go.
    ids: Vec<u32>,
    next_id: u32,
}

impl Default for Document {
    fn default() -> Self {
        Document::new("")
    }
}

impl Document {
    /// A document of `text`, whose line ending it takes on and drops.
    pub fn new(text: &str) -> Document {
        let ending = LineEnding::of(text);
        let text = Rope::from(&*normalize(text));
        let lines = text.line_count();
        Document {
            text,
            ending,
            version: 0,
            ids: (0..lines as u32).collect(),
            next_id: lines as u32,
        }
    }

    /// Its text, lines ending in `'\n'`.
    #[inline]
    pub fn text(&self) -> &Rope {
        &self.text
    }

    /// Its length in bytes.
    #[inline]
    pub fn len(&self) -> usize {
        self.text.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Its lines; an empty document has one.
    #[inline]
    pub fn line_count(&self) -> usize {
        self.ids.len()
    }

    /// Line `line`'s text, without its break.
    pub fn line(&self, line: usize) -> Cow<'_, str> {
        self.text.line(line)
    }

    /// The bytes of line `line`, without its break.
    pub fn line_range(&self, line: usize) -> Range<usize> {
        self.text.line_range(line)
    }

    /// The byte line `line` starts at.
    pub fn line_start(&self, line: usize) -> usize {
        self.text.line_start(line)
    }

    /// The byte line `line` ends at, before its break.
    pub fn line_end(&self, line: usize) -> usize {
        self.text.line_end(line)
    }

    /// The line byte `at` is on.
    pub fn line_of(&self, at: usize) -> usize {
        self.text.line_of(at)
    }

    /// The line byte `at` is on and how far into it, in bytes.
    pub fn position(&self, at: usize) -> (usize, usize) {
        let line = self.line_of(at);
        (line, at.min(self.len()) - self.line_start(line))
    }

    /// The byte at `column` bytes into line `line`, held inside the line.
    pub fn offset(&self, line: usize, column: usize) -> usize {
        let range = self.line_range(line.min(self.line_count() - 1));
        let mut at = (range.start + column).min(range.end);
        while !self.text.is_char_boundary(at) {
            at -= 1;
        }
        at
    }

    /// Text `range`.
    pub fn slice(&self, range: Range<usize>) -> Cow<'_, str> {
        self.text.slice(range)
    }

    /// The whole text, lines ending in `'\n'`.
    pub fn to_text(&self) -> String {
        self.text.to_text()
    }

    /// The whole text, lines ending as the document's do.
    pub fn save_text(&self) -> String {
        let text = self.text.to_text();
        match self.ending {
            LineEnding::Lf => text,
            LineEnding::CrLf => text.replace('\n', "\r\n"),
        }
    }

    /// How its lines end when saved.
    pub fn ending(&self) -> LineEnding {
        self.ending
    }

    /// Make its lines end in `ending` when saved.
    pub fn set_ending(&mut self, ending: LineEnding) {
        self.ending = ending;
    }

    /// Bumped by every edit: what an answer computed for one text checks
    /// it still holds.
    #[inline]
    pub fn version(&self) -> u64 {
        self.version
    }

    /// Line `line`'s stable id.
    #[inline]
    pub fn line_id(&self, line: usize) -> u32 {
        self.ids[line]
    }

    /// The line whose id is `id`, looked up from line `near`.
    pub fn line_of_id(&self, id: u32, near: usize) -> Option<usize> {
        let near = near.min(self.ids.len().saturating_sub(1));
        if self.ids.get(near) == Some(&id) {
            return Some(near);
        }
        self.ids.iter().position(|&i| i == id)
    }

    /// Applies `changes`, made against the text as it is.
    pub fn apply(&mut self, changes: &ChangeSet) -> Applied {
        let mut splices: Vec<LineSplice> = Vec::with_capacity(changes.changes.len());
        let mut inverse = Vec::with_capacity(changes.changes.len());
        let mut lines_delta = 0isize;
        let mut bytes_delta = 0isize;
        for c in &changes.changes {
            let range = c.range.start.min(self.len())..c.range.end.min(self.len());
            let first = self.text.line_of(range.start);
            let last = if range.is_empty() {
                first
            } else {
                first + self.text.newlines_in(range.clone())
            };
            let added_breaks = crate::rope::count_newlines(c.text.as_bytes());
            let start = (range.start as isize + bytes_delta) as usize;
            inverse.push(Change {
                range: start..start + c.text.len(),
                text: self.text.slice(range.clone()).into_owned(),
            });
            bytes_delta += c.text.len() as isize - range.len() as isize;
            let grow = added_breaks as isize - (last - first) as isize;
            match splices.last_mut() {
                // Touches the line the change before left: one splice.
                Some(prev) if first < prev.old + prev.removed => {
                    let removed = last + 1 - prev.old;
                    prev.added =
                        (prev.added as isize + grow + (removed - prev.removed) as isize) as usize;
                    prev.removed = removed;
                }
                _ => splices.push(LineSplice {
                    old: first,
                    new: (first as isize + lines_delta) as usize,
                    removed: last + 1 - first,
                    added: added_breaks + 1,
                }),
            }
            lines_delta += grow;
        }
        for c in changes.changes.iter().rev() {
            self.text.replace(c.range.clone(), &c.text);
        }
        let next = &mut self.next_id;
        LineSplice::apply(&splices, &mut self.ids, true, |_| {
            let id = *next;
            *next = next.wrapping_add(1);
            id
        });
        debug_assert_eq!(self.ids.len(), self.text.line_count());
        self.version += 1;
        Applied {
            inverse: ChangeSet { changes: inverse },
            splices,
        }
    }

    /// Replace the whole text with `text`, taking on its line ending: a file
    /// read again. Every line gets a new id.
    pub fn reset(&mut self, text: &str) -> Applied {
        self.ending = LineEnding::of(text);
        let all = ChangeSet::one(Change::replace(0..self.len(), normalize(text)));
        self.apply(&all)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crlf_is_kept_as_lf_and_saved_back() {
        let doc = Document::new("a\r\nbc\r\n");
        assert_eq!(doc.ending(), LineEnding::CrLf);
        assert_eq!(doc.to_text(), "a\nbc\n");
        assert_eq!(doc.line_count(), 3);
        assert_eq!(doc.save_text(), "a\r\nbc\r\n");
    }

    #[test]
    fn a_change_set_maps_positions_by_side() {
        let cs = ChangeSet::new(vec![Change::insert(2, "xy"), Change::replace(4..6, "z")]);
        assert_eq!(cs.map(1, Assoc::After), 1);
        assert_eq!(cs.map(2, Assoc::Before), 2);
        assert_eq!(cs.map(2, Assoc::After), 4);
        assert_eq!(cs.map(3, Assoc::After), 5);
        assert_eq!(cs.map(5, Assoc::Before), 6);
        assert_eq!(cs.map(5, Assoc::After), 7);
        assert_eq!(cs.map(6, Assoc::Before), 7);
        assert_eq!(cs.map(8, Assoc::Before), 9);
        let mut s = String::from("abcdefgh");
        cs.apply_to_string(&mut s);
        assert_eq!(s, "abxycdzgh");
        assert_eq!(cs.ranges_after().collect::<Vec<_>>(), [2..4, 6..7]);
    }

    #[test]
    fn applying_hands_back_the_inverse() {
        let mut doc = Document::new("one\ntwo\nthree");
        let cs = ChangeSet::new(vec![
            Change::replace(1..2, "N\nE"),
            Change::delete(5..10),
            Change::insert(13, "!"),
        ]);
        let mut model = doc.to_text();
        cs.apply_to_string(&mut model);
        let applied = doc.apply(&cs);
        assert_eq!(doc.to_text(), model);
        doc.apply(&applied.inverse);
        assert_eq!(doc.to_text(), "one\ntwo\nthree");
    }

    #[test]
    fn line_splices_keep_ids_where_lines_stay() {
        let mut doc = Document::new("a\nb\nc\nd");
        let ids: Vec<u32> = (0..4).map(|l| doc.line_id(l)).collect();
        // A break typed in line b, and lines c..d joined.
        let cs = ChangeSet::new(vec![Change::insert(3, "\n"), Change::delete(5..6)]);
        let applied = doc.apply(&cs);
        assert_eq!(doc.to_text(), "a\nb\n\ncd");
        assert_eq!(
            applied.splices,
            [
                LineSplice {
                    old: 1,
                    new: 1,
                    removed: 1,
                    added: 2
                },
                LineSplice {
                    old: 2,
                    new: 3,
                    removed: 2,
                    added: 1
                }
            ]
        );
        assert_eq!(doc.line_id(0), ids[0]);
        assert_eq!(doc.line_id(1), ids[1]);
        assert_eq!(doc.line_id(3), ids[2]);
        assert!(!ids.contains(&doc.line_id(2)));
    }

    #[test]
    fn changes_on_one_line_make_one_splice() {
        let mut doc = Document::new("abc\ndef");
        let cs = ChangeSet::new(vec![
            Change::insert(0, "x\n"),
            Change::insert(1, "y"),
            Change::delete(2..5),
        ]);
        let applied = doc.apply(&cs);
        assert_eq!(doc.to_text(), "x\naybef");
        assert_eq!(
            applied.splices,
            [LineSplice {
                old: 0,
                new: 0,
                removed: 2,
                added: 2
            }]
        );
    }

    #[test]
    fn random_edits_keep_line_arrays_in_step() {
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = |n: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % n.max(1) as u64) as usize
        };
        let mut doc = Document::new("fn main() {\n    x\n}\n");
        // A line array kept by splices: each line's text.
        let mut mirror: Vec<String> = (0..doc.line_count())
            .map(|l| doc.line(l).into_owned())
            .collect();
        for _ in 0..300 {
            let len = doc.len();
            let mut changes = Vec::new();
            let mut at = 0;
            while at < len && changes.len() < 3 {
                let start = at + next(len - at + 1);
                let end = (start + next(4)).min(len);
                let text = ["", "\n", "ab", "\n\n", "x\ny"][next(5)];
                changes.push(Change::replace(start..end, text));
                at = end + 1;
            }
            let applied = doc.apply(&ChangeSet::new(changes));
            let text = doc.to_text();
            let lines: Vec<&str> = text.split('\n').collect();
            LineSplice::apply(&applied.splices, &mut mirror, false, |l| {
                lines[l].to_owned()
            });
            assert_eq!(mirror.len(), lines.len());
            // A line no splice touched is the same line.
            for (l, line) in lines.iter().enumerate() {
                let touched = applied
                    .splices
                    .iter()
                    .any(|s| (s.new..s.new + s.added).contains(&l));
                if touched {
                    mirror[l] = (*line).to_owned();
                }
                assert_eq!(mirror[l], *line, "line {l}");
            }
        }
    }
}
