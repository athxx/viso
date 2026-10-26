//! Moving the cursors: by grapheme, word, row, page and line, keeping the
//! cell an up or down run aims for, stepping over folded lines, and
//! selecting by word, line and box.

use viso_ui::text_edit::Motion;

use crate::layout::{self, RowSpan};
use crate::selection::{Selection, Selections};
use crate::session::Session;
use crate::text::{CharClass, char_class, next_grapheme, prev_grapheme};

/// Which way along the text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Back,
    Forward,
}

impl Session {
    /// Moves every cursor by `motion`, its anchor too unless `extend`.
    pub fn move_cursors(&mut self, motion: Motion, extend: bool) {
        let sel = self.sel.clone();
        let moved = sel.map_each(|s| self.moved(s, motion, extend));
        self.set_cursors(moved);
    }

    /// Puts the cursors at `sel` after a motion.
    fn set_cursors(&mut self, sel: Selections) {
        let changed = sel != self.sel;
        self.sel = sel.clamp(self.doc.len());
        if changed {
            self.cursors_moved();
        } else {
            self.pending.reveal = true;
        }
    }

    /// Where `s` goes by `motion`.
    fn moved(&self, s: Selection, motion: Motion, extend: bool) -> Selection {
        let len = self.doc.len();
        // A selection collapses to its edge on a horizontal step.
        if !extend && !s.is_empty() {
            match motion {
                Motion::Left => return Selection::caret(s.start()),
                Motion::Right => return Selection::caret(s.end()),
                _ => {}
            }
        }
        let vertical = matches!(
            motion,
            Motion::Up | Motion::Down | Motion::PageUp | Motion::PageDown
        );
        if vertical {
            let rows = match motion {
                Motion::PageUp | Motion::PageDown => self.page_rows.max(1),
                _ => 1,
            };
            let dir = match motion {
                Motion::Up | Motion::PageUp => Direction::Back,
                _ => Direction::Forward,
            };
            let (head, goal) = self.vertical(s.head, s.goal, dir, rows);
            let mut out = s.moved(head, extend);
            out.goal = Some(goal);
            return out;
        }
        let head = match motion {
            Motion::Left => self.step(s.head, Direction::Back),
            Motion::Right => self.step(s.head, Direction::Forward),
            Motion::WordLeft => self.word(s.head, Direction::Back),
            Motion::WordRight => self.word(s.head, Direction::Forward),
            Motion::LineStart => self.home(s.head),
            Motion::LineEnd => self.end(s.head),
            Motion::ParagraphStart => self.paragraph(s.head, Direction::Back),
            Motion::ParagraphEnd => self.paragraph(s.head, Direction::Forward),
            Motion::DocStart => 0,
            Motion::DocEnd => len,
            _ => s.head,
        };
        s.moved(head, extend)
    }

    /// One grapheme along from `at`, a line break being one, over folded
    /// lines.
    pub fn step(&self, at: usize, dir: Direction) -> usize {
        let line = self.doc.line_of(at);
        let range = self.doc.line_range(line);
        match dir {
            Direction::Forward => {
                if at >= range.end {
                    // Past the break, and past a fold this line heads.
                    let next = self.folds.next_shown(line, self.doc.line_count());
                    return match next {
                        Some(next) => self.doc.line_start(next),
                        None if self.folds.at(line).is_some() => at,
                        None => (at + 1).min(self.doc.len()),
                    };
                }
                let text = self.doc.line(line);
                range.start + next_grapheme(&text, at - range.start)
            }
            Direction::Back => {
                if at <= range.start {
                    return match self.folds.prev_shown(line) {
                        Some(prev) => self.doc.line_end(prev),
                        None => 0,
                    };
                }
                let text = self.doc.line(line);
                range.start + prev_grapheme(&text, at - range.start)
            }
        }
    }

    /// To the word boundary along from `at`: past spaces, then past a run
    /// of one class; a line break is a stop of its own.
    pub fn word(&self, at: usize, dir: Direction) -> usize {
        let line = self.doc.line_of(at);
        let range = self.doc.line_range(line);
        let text = self.doc.line(line);
        let col = at - range.start;
        match dir {
            Direction::Forward => {
                if col >= text.len() {
                    return self.step(at, dir);
                }
                let rest = &text[col..];
                let mut chars = rest.char_indices().peekable();
                let mut end = rest.len();
                // Past spaces.
                while let Some(&(_, c)) = chars.peek() {
                    if char_class(c) != CharClass::Space {
                        break;
                    }
                    chars.next();
                }
                if let Some(&(_, c)) = chars.peek() {
                    let class = char_class(c);
                    for (i, c) in chars {
                        if char_class(c) != class {
                            end = i;
                            break;
                        }
                    }
                }
                range.start + col + end
            }
            Direction::Back => {
                if col == 0 {
                    return self.step(at, dir);
                }
                let before = &text[..col];
                let mut chars = before.char_indices().rev().peekable();
                let mut start = 0;
                while let Some(&(_, c)) = chars.peek() {
                    if char_class(c) != CharClass::Space {
                        break;
                    }
                    chars.next();
                }
                if let Some(&(_, c)) = chars.peek() {
                    let class = char_class(c);
                    for (i, c) in chars {
                        if char_class(c) != class {
                            start = i + c.len_utf8();
                            break;
                        }
                    }
                }
                range.start + start
            }
        }
    }

    /// Home: the start of the row; on a line's first row, its first
    /// character, or its start from there.
    fn home(&self, at: usize) -> usize {
        let (line, rows, start) = self.rows_at(at);
        let col = at - start;
        let r = layout::row_of(&rows, col);
        if r > 0 {
            return start + rows[r].start;
        }
        let text = self.doc.line(line);
        let indent = crate::text::indent_of(&text).len();
        if col == indent || indent == text.len() {
            start
        } else {
            start + indent
        }
    }

    /// End: the end of the row; of the line from there.
    fn end(&self, at: usize) -> usize {
        let (line, rows, start) = self.rows_at(at);
        let col = at - start;
        let r = layout::row_of(&rows, col);
        let text = self.doc.line(line);
        if r + 1 < rows.len() {
            let end = prev_grapheme(&text, rows[r].end).max(rows[r].start);
            if col != end {
                return start + end;
            }
        }
        start + text.len()
    }

    /// The start of the run of lines with text around `at`, or of the run
    /// before; the end likewise forward.
    fn paragraph(&self, at: usize, dir: Direction) -> usize {
        let lines = self.doc.line_count();
        let blank = |l: usize| self.doc.line(l).trim().is_empty();
        let mut line = self.doc.line_of(at);
        match dir {
            Direction::Back => {
                if at == self.doc.line_start(line) && line > 0 {
                    line -= 1;
                }
                while line > 0 && blank(line) {
                    line -= 1;
                }
                while line > 0 && !blank(line - 1) {
                    line -= 1;
                }
                self.doc.line_start(line)
            }
            Direction::Forward => {
                if at == self.doc.line_end(line) && line + 1 < lines {
                    line += 1;
                }
                while line + 1 < lines && blank(line) {
                    line += 1;
                }
                while line + 1 < lines && !blank(line + 1) {
                    line += 1;
                }
                self.doc.line_end(line)
            }
        }
    }

    /// The line of `at`, its rows and where it starts.
    fn rows_at(&self, at: usize) -> (usize, Vec<RowSpan>, usize) {
        let line = self.doc.line_of(at);
        (line, self.line_rows(line), self.doc.line_start(line))
    }

    /// `rows` rows up or down from `at`, aiming for cell `goal` (the cell at
    /// `at` when `None`); returns where it lands and the goal.
    pub fn vertical(
        &self,
        at: usize,
        goal: Option<u32>,
        dir: Direction,
        rows: usize,
    ) -> (usize, u32) {
        let tab = self.rules().tab;
        let (mut line, row, col) = self.point_of(at);
        let goal = goal.unwrap_or(col);
        let mut row = row;
        let mut spans = self.line_rows(line);
        for _ in 0..rows {
            match dir {
                Direction::Back if row > 0 => row -= 1,
                Direction::Back => match self.folds.prev_shown(line) {
                    Some(prev) => {
                        line = prev;
                        spans = self.line_rows(line);
                        row = spans.len() - 1;
                    }
                    None => return (0, goal),
                },
                Direction::Forward if row + 1 < spans.len() => row += 1,
                Direction::Forward => match self.folds.next_shown(line, self.doc.line_count()) {
                    Some(next) => {
                        line = next;
                        spans = self.line_rows(line);
                        row = 0;
                    }
                    None => return (self.doc.len(), goal),
                },
            }
        }
        let text = self.doc.line(line);
        let inlays = self.inlay_spans(line);
        let byte = layout::byte_at_col(&text, spans[row], &inlays, goal, tab);
        (self.doc.line_start(line) + byte, goal)
    }

    // ----- selecting -----

    /// The word, or run of spaces or punctuation, around `at`.
    pub fn word_at(&self, at: usize) -> std::ops::Range<usize> {
        let line = self.doc.line_of(at);
        let start = self.doc.line_start(line);
        let text = self.doc.line(line);
        let col = at - start;
        let class_at = |i: usize| text[i..].chars().next().map(char_class);
        let class = class_at(col)
            .filter(|&c| c != CharClass::Space || col == 0)
            .or_else(|| {
                // At a word's end: the word before.
                text[..col].chars().next_back().map(char_class)
            });
        let Some(class) = class else {
            return at..at;
        };
        let mut a = col;
        for (i, c) in text[..col].char_indices().rev() {
            if char_class(c) != class {
                break;
            }
            a = i;
        }
        let mut b = col;
        for (i, c) in text[col..].char_indices() {
            if char_class(c) != class {
                break;
            }
            b = col + i + c.len_utf8();
        }
        start + a..start + b
    }

    /// Only the primary cursor, selecting what it did.
    pub fn single_cursor(&mut self) -> bool {
        if self.sel.len() == 1 {
            return false;
        }
        let sel = self.sel.only_primary();
        self.set_cursors(sel);
        true
    }

    /// Collapses: to one cursor, or a selection to its caret.
    pub fn collapse(&mut self) -> bool {
        if self.single_cursor() {
            return true;
        }
        let p = self.sel.primary();
        if p.is_empty() {
            return false;
        }
        self.set_cursors(Selections::one(Selection::caret(p.head)));
        true
    }

    /// Selects the whole text.
    pub fn select_all(&mut self) {
        let len = self.doc.len();
        self.set_cursors(Selections::one(Selection::new(0, len)));
    }

    /// Extends every cursor over its whole lines, their breaks with them.
    pub fn select_lines(&mut self) {
        let sel = self.sel.clone().map_each(|s| {
            let first = self.doc.line_of(s.start());
            let last = self.doc.line_of(s.end());
            let end = if last + 1 < self.doc.line_count() {
                self.doc.line_start(last + 1)
            } else {
                self.doc.len()
            };
            Selection::new(self.doc.line_start(first), end)
        });
        self.set_cursors(sel);
    }

    /// Places one caret at `at`, or with `extend` moves the primary head
    /// there.
    pub fn click(&mut self, at: usize, extend: bool) {
        let sel = if extend {
            let mut p = self.sel.primary();
            p.head = at;
            p.goal = None;
            Selections::one(p)
        } else {
            Selections::one(Selection::caret(at))
        };
        self.set_cursors(sel);
    }

    /// Adds a caret at `at`, or takes away the cursor there.
    pub fn toggle_cursor(&mut self, at: usize) {
        let hit = self
            .sel
            .list()
            .iter()
            .position(|s| s.range().contains(&at) || s.head == at);
        let sel = match hit {
            Some(i) if self.sel.len() > 1 => self.sel.without(i),
            Some(_) => return,
            None => self.sel.with(Selection::caret(at)),
        };
        self.set_cursors(sel);
    }

    /// Selects the word around `at`, extending from the primary anchor's
    /// word with `extend_from`.
    pub fn select_word(&mut self, at: usize, extend_from: Option<std::ops::Range<usize>>) {
        let word = self.word_at(at);
        let sel = match extend_from {
            Some(from) if word.start < from.start => Selection::new(from.end, word.start),
            Some(from) => Selection::new(from.start, word.end.max(from.end)),
            None => Selection::new(word.start, word.end),
        };
        self.set_cursors(Selections::one(sel));
    }

    /// Selects the line at `at`, its break with it, extending from lines
    /// `extend_from`.
    pub fn select_line_at(&mut self, at: usize, extend_from: Option<std::ops::Range<usize>>) {
        let line = self.doc.line_of(at);
        let start = self.doc.line_start(line);
        let end = if line + 1 < self.doc.line_count() {
            self.doc.line_start(line + 1)
        } else {
            self.doc.len()
        };
        let sel = match extend_from {
            Some(from) if start < from.start => Selection::new(from.end, start),
            Some(from) => Selection::new(from.start, end.max(from.end)),
            None => Selection::new(start, end),
        };
        self.set_cursors(Selections::one(sel));
    }

    /// One cursor a line from line `from` to line `to`, each over cells
    /// `a` to `b` of its first row: a box selection.
    pub fn select_box(&mut self, from: (usize, f32), to: (usize, f32)) {
        let (l0, l1) = (from.0.min(to.0), from.0.max(to.0));
        let mut list = Vec::with_capacity(l1 - l0 + 1);
        let mut line = l0;
        let mut primary = 0;
        loop {
            let anchor = self.position_at(line, 0, from.1);
            let head = self.position_at(line, 0, to.1);
            // A line too short for the box gets no cursor, unless it is the
            // one the box started on.
            let start = self.doc.line_start(line);
            let short = anchor == head
                && layout::line_cells(&self.doc.line(line), &[], self.rules().tab)
                    < from.1.min(to.1) as u32;
            if !short || line == to.0 {
                if line == to.0 {
                    primary = list.len();
                }
                list.push(Selection::new(anchor.max(start), head.max(start)));
            }
            match self.folds.next_shown(line, self.doc.line_count()) {
                Some(next) if next <= l1 => line = next,
                _ => break,
            }
        }
        self.set_cursors(Selections::new(list, primary));
    }

    /// Adds a caret a row above or below each cursor's head, at its goal.
    pub fn add_cursor_vertically(&mut self, dir: Direction) {
        let mut list = self.sel.list().to_vec();
        let edge = match dir {
            Direction::Back => list.first().copied(),
            Direction::Forward => list.last().copied(),
        };
        let Some(edge) = edge else {
            return;
        };
        let (head, goal) = self.vertical(edge.head, edge.goal, dir, 1);
        if head == edge.head {
            return;
        }
        let mut caret = Selection::caret(head);
        caret.goal = Some(goal);
        list.push(caret);
        let primary = list.len() - 1;
        self.set_cursors(Selections::new(list, primary));
    }

    /// Selects the next match of the primary selection after the last
    /// cursor and makes it primary; with a caret, first selects the word
    /// under it. Returns whether a cursor was added or a word selected.
    pub fn add_next_match(&mut self) -> bool {
        let p = self.sel.primary();
        if p.is_empty() {
            let word = self.word_at(p.head);
            if word.is_empty() {
                return false;
            }
            let sel = self.sel.map_each(|s| {
                if s == p {
                    Selection::new(word.start, word.end)
                } else {
                    s
                }
            });
            self.set_cursors(sel);
            return true;
        }
        let needle = self.doc.slice(p.range()).into_owned();
        let after = self
            .sel
            .list()
            .iter()
            .map(Selection::end)
            .max()
            .unwrap_or(0);
        let rope = self.doc.text();
        let found = crate::search::find_literal(rope, &needle, after)
            .or_else(|| crate::search::find_literal(rope, &needle, 0));
        let Some(at) = found else {
            return false;
        };
        let new = Selection::new(at, at + needle.len());
        if self.sel.list().iter().any(|s| s.range() == new.range()) {
            return false;
        }
        let sel = self.sel.with(new);
        self.set_cursors(sel);
        true
    }

    /// A cursor on every match of the primary selection, or of the word
    /// under the primary caret.
    pub fn select_all_matches(&mut self) -> bool {
        let p = self.sel.primary();
        let range = if p.is_empty() {
            self.word_at(p.head)
        } else {
            p.range()
        };
        if range.is_empty() {
            return false;
        }
        let needle = self.doc.slice(range.clone()).into_owned();
        let mut list = Vec::new();
        let mut at = 0;
        let mut primary = 0;
        while let Some(found) = crate::search::find_literal(self.doc.text(), &needle, at) {
            if found == range.start {
                primary = list.len();
            }
            list.push(Selection::new(found, found + needle.len()));
            at = found + needle.len().max(1);
        }
        self.set_cursors(Selections::new(list, primary));
        true
    }
}
