//! Folded ranges: lines hidden under the line that heads them, and which
//! lines could fold.
//!
//! A fold keeps its header line in view, with a placeholder at its end, and
//! hides the lines after it to its last. Folds are apart and sorted; folding
//! a range around folded ones takes them in. The lines the editor shows are
//! the lines no fold hides: the view lists those, and maps between the line
//! a row shows and the row a line is on through the hidden counts kept
//! here, a search over the folds rather than over the lines.
//!
//! What can fold is worked out from indentation, as a language's own ranges
//! may say otherwise: a line folds when the next line with text is indented
//! further, down to the last line indented further than it, blank lines at
//! the end left out.

use crate::document::{Document, LineSplice};

/// Lines `start + 1..=end` hidden under line `start`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fold {
    pub start: usize,
    pub end: usize,
}

impl Fold {
    /// The lines it hides.
    #[inline]
    pub fn hidden(&self) -> usize {
        self.end - self.start
    }
}

/// The folds of a document.
#[derive(Debug, Clone, Default)]
pub struct Folds {
    list: Vec<Fold>,
    /// The lines hidden by `list[..i]`, for each `i`, and one more for all.
    before: Vec<usize>,
}

impl Folds {
    /// No folds.
    pub fn new() -> Folds {
        Folds {
            list: Vec::new(),
            before: vec![0],
        }
    }

    /// The folds, in order.
    pub fn list(&self) -> &[Fold] {
        &self.list
    }

    /// Whether nothing is folded.
    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    /// Lines hidden in all.
    pub fn hidden(&self) -> usize {
        self.before.last().copied().unwrap_or(0)
    }

    /// The lines shown of a document of `lines` lines.
    pub fn shown(&self, lines: usize) -> usize {
        lines - self.hidden().min(lines)
    }

    /// The fold whose header is line `line`.
    pub fn at(&self, line: usize) -> Option<Fold> {
        let i = self.list.partition_point(|f| f.start < line);
        self.list.get(i).filter(|f| f.start == line).copied()
    }

    /// The fold hiding line `line`, if one does.
    pub fn hiding(&self, line: usize) -> Option<Fold> {
        let i = self.list.partition_point(|f| f.end < line);
        self.list
            .get(i)
            .filter(|f| f.start < line && line <= f.end)
            .copied()
    }

    /// Whether a fold hides line `line`.
    pub fn is_hidden(&self, line: usize) -> bool {
        self.hiding(line).is_some()
    }

    /// The line in view nearest above or at `line`: its fold's header when
    /// one hides it.
    pub fn shown_line(&self, line: usize) -> usize {
        self.hiding(line).map_or(line, |f| f.start)
    }

    /// The row line `line` is shown on: its fold's header's when hidden.
    pub fn row_of(&self, line: usize) -> usize {
        let line = self.shown_line(line);
        let i = self.list.partition_point(|f| f.start < line);
        line - self.before[i]
    }

    /// The line row `row` shows.
    pub fn line_of(&self, row: usize) -> usize {
        // The folds wholly above the row: each one's header row is before it.
        let mut lo = 0;
        let mut hi = self.list.len();
        while lo < hi {
            let mid = (lo + hi) / 2;
            let f = self.list[mid];
            if f.start - self.before[mid] < row {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        row + self.before[lo]
    }

    /// The line shown after `line`, if any of `lines` is.
    pub fn next_shown(&self, line: usize, lines: usize) -> Option<usize> {
        let next = self.at(line).map_or(line + 1, |f| f.end + 1);
        (next < lines).then_some(next)
    }

    /// The line shown before `line`.
    pub fn prev_shown(&self, line: usize) -> Option<usize> {
        let prev = line.checked_sub(1)?;
        Some(self.shown_line(prev))
    }

    /// Folds `fold`, taking in the folds inside it. A fold that would hide
    /// nothing, or that crosses another, is not made.
    pub fn add(&mut self, fold: Fold) -> bool {
        if fold.end <= fold.start {
            return false;
        }
        let crosses = self.list.iter().any(|f| {
            (f.start < fold.start && fold.start <= f.end && f.end < fold.end)
                || (fold.start < f.start && f.start <= fold.end && fold.end < f.end)
                || (f.start <= fold.start && fold.end <= f.end)
        });
        if crosses {
            return false;
        }
        self.list
            .retain(|f| !(fold.start <= f.start && f.end <= fold.end));
        let at = self.list.partition_point(|f| f.start < fold.start);
        self.list.insert(at, fold);
        self.count();
        true
    }

    /// Unfolds the fold headed by line `line`.
    pub fn remove(&mut self, line: usize) -> bool {
        let before = self.list.len();
        self.list.retain(|f| f.start != line);
        let removed = self.list.len() != before;
        if removed {
            self.count();
        }
        removed
    }

    /// Unfolds every fold hiding or heading a line of `lines`.
    pub fn reveal(&mut self, lines: std::ops::RangeInclusive<usize>) -> bool {
        let before = self.list.len();
        let (a, b) = (*lines.start(), *lines.end());
        self.list.retain(|f| f.end < a || b <= f.start);
        let removed = self.list.len() != before;
        if removed {
            self.count();
        }
        removed
    }

    /// Unfolds everything.
    pub fn clear(&mut self) -> bool {
        let any = !self.list.is_empty();
        self.list.clear();
        self.count();
        any
    }

    /// Follows an edit. A fold the edit reaches into the hidden lines of,
    /// or past the end of, opens; one above or below moves with its lines.
    /// Returns whether any opened.
    pub fn splice(&mut self, splices: &[LineSplice]) -> bool {
        if self.list.is_empty() {
            return false;
        }
        let mut opened = false;
        for s in splices.iter().rev() {
            let (a, r, n) = (s.old, s.removed, s.added);
            let last = a + r - 1;
            self.list.retain_mut(|f| {
                if f.end < a {
                    return true;
                }
                if f.start > last {
                    f.start = f.start + n - r;
                    f.end = f.end + n - r;
                    return true;
                }
                // The header line edited alone keeps the fold below it.
                if a == f.start && last == f.start && n == 1 {
                    return true;
                }
                opened = true;
                false
            });
        }
        self.count();
        opened
    }

    fn count(&mut self) {
        self.before.clear();
        let mut sum = 0;
        self.before.push(0);
        for f in &self.list {
            sum += f.hidden();
            self.before.push(sum);
        }
    }
}

/// The range line `line` folds by indentation, if it does: the lines after
/// it indented further, `tab` cells a tab, blank lines at the end left out.
pub fn indent_range(doc: &Document, line: usize, tab: u32) -> Option<Fold> {
    let lines = doc.line_count();
    let base = crate::text::indent_cells(&doc.line(line), tab)?;
    let mut end = None;
    let mut l = line + 1;
    while l < lines {
        match crate::text::indent_cells(&doc.line(l), tab) {
            None => {}
            Some(i) if i > base => end = Some(l),
            Some(_) => break,
        }
        l += 1;
    }
    end.map(|end| Fold { start: line, end })
}

/// Whether line `line` folds by indentation: the next line with text is
/// indented further.
pub fn folds_by_indent(doc: &Document, line: usize, tab: u32) -> bool {
    let Some(base) = crate::text::indent_cells(&doc.line(line), tab) else {
        return false;
    };
    let lines = doc.line_count();
    // Look a little way past blank lines; far enough for any real code.
    for l in line + 1..lines.min(line + 64) {
        if let Some(i) = crate::text::indent_cells(&doc.line(l), tab) {
            return i > base;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::{Change, ChangeSet};

    #[test]
    fn rows_and_lines_map_both_ways_around_folds() {
        let mut folds = Folds::new();
        assert!(folds.add(Fold { start: 2, end: 5 }));
        assert!(folds.add(Fold { start: 8, end: 9 }));
        assert_eq!(folds.hidden(), 4);
        assert_eq!(folds.shown(12), 8);
        let rows: Vec<usize> = (0..8).map(|r| folds.line_of(r)).collect();
        assert_eq!(rows, [0, 1, 2, 6, 7, 8, 10, 11]);
        for (row, &line) in rows.iter().enumerate() {
            assert_eq!(folds.row_of(line), row);
        }
        assert_eq!(folds.row_of(4), 2);
        assert_eq!(folds.next_shown(2, 12), Some(6));
        assert_eq!(folds.prev_shown(6), Some(2));
        assert!(folds.is_hidden(9) && !folds.is_hidden(8));
        // A fold around one takes it in; one crossing it is not made.
        assert!(!folds.add(Fold { start: 4, end: 7 }));
        assert!(folds.add(Fold { start: 1, end: 6 }));
        assert_eq!(
            folds.list(),
            [Fold { start: 1, end: 6 }, Fold { start: 8, end: 9 }]
        );
    }

    #[test]
    fn edits_move_folds_and_open_the_ones_they_reach_into() {
        let text: String = (0..12).map(|i| format!("{i}\n")).collect();
        let mut doc = Document::new(&text);
        let mut folds = Folds::new();
        folds.add(Fold { start: 2, end: 4 });
        folds.add(Fold { start: 7, end: 9 });
        // Two lines typed above both: they move.
        let at = doc.line_start(0);
        let applied = doc.apply(&ChangeSet::one(Change::insert(at, "a\nb\n")));
        assert!(!folds.splice(&applied.splices));
        assert_eq!(
            folds.list(),
            [Fold { start: 4, end: 6 }, Fold { start: 9, end: 11 }]
        );
        // Typing on a header keeps its fold.
        let at = doc.line_start(4);
        let applied = doc.apply(&ChangeSet::one(Change::insert(at, "x")));
        assert!(!folds.splice(&applied.splices));
        assert_eq!(folds.list().len(), 2);
        // A break typed in a header opens it.
        let at = doc.line_end(4);
        let applied = doc.apply(&ChangeSet::one(Change::insert(at, "\n")));
        assert!(folds.splice(&applied.splices));
        assert_eq!(folds.list(), [Fold { start: 10, end: 12 }]);
    }

    #[test]
    fn indentation_says_what_folds() {
        let doc = Document::new("fn a() {\n    x;\n\n    y;\n\n}\nz\n");
        assert_eq!(indent_range(&doc, 0, 4), Some(Fold { start: 0, end: 3 }));
        assert!(folds_by_indent(&doc, 0, 4));
        assert!(!folds_by_indent(&doc, 1, 4));
        assert_eq!(indent_range(&doc, 1, 4), None);
    }
}
