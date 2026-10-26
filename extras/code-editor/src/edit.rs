//! The editing commands: typing and pasting at every cursor, deleting by
//! grapheme, word and line, new lines that keep their indentation, pairs
//! that close themselves, indenting, commenting, and moving, copying and
//! deleting whole lines.
//!
//! Each command builds one change per cursor, in the text as it is, and
//! hands them to [`Session::apply`] as one edit, so many cursors are one
//! undo step and the edit costs one pass over the document.

use std::ops::Range;

use viso_ui::text_edit::Reach;

use crate::document::{Change, ChangeSet, normalize};
use crate::history::EditKind;
use crate::motion::Direction;
use crate::selection::{Selection, Selections};
use crate::session::{Preedit, Session};
use crate::text::{CharClass, char_class, indent_of, next_grapheme, prev_grapheme};

/// One cursor's change, and where its cursor goes in the text the change
/// puts in: `anchor..head` from the start of it, or mapped when `None`.
struct CursorEdit {
    change: Change,
    select: Option<(usize, usize)>,
}

impl CursorEdit {
    fn mapped(change: Change) -> CursorEdit {
        CursorEdit {
            change,
            select: None,
        }
    }

    fn caret_at(change: Change, at: usize) -> CursorEdit {
        CursorEdit {
            change,
            select: Some((at, at)),
        }
    }
}

impl Session {
    /// Applies one edit made of `edits` (any order, apart), cursors with no
    /// edit mapped through it.
    fn apply_edits(&mut self, kind: EditKind, mut edits: Vec<CursorEdit>, breaks: bool) {
        if self.config.read_only {
            return;
        }
        edits.retain(|e| !e.change.is_noop() || e.select.is_some());
        edits.sort_by_key(|e| (e.change.range.start, e.change.range.end));
        // Changes that reach into each other become one.
        let mut merged: Vec<CursorEdit> = Vec::with_capacity(edits.len());
        for e in edits {
            match merged.last_mut() {
                Some(last) if e.change.range.start < last.change.range.end => {
                    last.change.range.end = last.change.range.end.max(e.change.range.end);
                    last.change.text.push_str(&e.change.text);
                    last.select = None;
                }
                _ => merged.push(e),
            }
        }
        let changes = ChangeSet::new(merged.iter().map(|e| e.change.clone()).collect());
        if changes.is_empty() {
            // Only cursors to move.
            let mut list = Vec::new();
            for e in &merged {
                if let Some((a, h)) = e.select {
                    let at = e.change.range.start;
                    list.push(Selection::new(at + a, at + h));
                }
            }
            if !list.is_empty() {
                let primary = list.len() - 1;
                self.set_selections(Selections::new(list, primary.min(self.sel.primary_index())));
            }
            return;
        }
        // Where each change's text lands, for the cursors it places.
        let landed: Vec<Range<usize>> = changes.ranges_after().collect();
        let mut placed: Vec<Option<Selection>> = Vec::with_capacity(merged.len());
        let mut k = 0;
        for e in &merged {
            if e.change.is_noop() {
                placed.push(e.select.map(|(a, h)| {
                    let at = changes.map(e.change.range.start, crate::Assoc::Before);
                    Selection::new(at + a, at + h)
                }));
                continue;
            }
            let at = landed[k].start;
            k += 1;
            placed.push(e.select.map(|(a, h)| Selection::new(at + a, at + h)));
        }
        // Every cursor: the one its edit placed, or mapped.
        let mut list = Vec::with_capacity(self.sel.len());
        let mut primary = self.sel.primary_index();
        for (i, s) in self.sel.list().iter().enumerate() {
            let own = merged.iter().position(|e| {
                let r = &e.change.range;
                (r.start <= s.start() && s.end() <= r.end) || (r.is_empty() && r.start == s.head)
            });
            let next = match own.and_then(|j| placed[j]) {
                Some(p) => p,
                None => s.map(&changes),
            };
            if i == self.sel.primary_index() {
                primary = list.len();
            }
            list.push(next);
        }
        let after = Selections::new(list, primary);
        self.apply(kind, changes, Some(after), breaks);
    }

    /// Types `text` at every cursor, in place of what each selects: what a
    /// key or an input method's commit puts in.
    pub fn type_text(&mut self, text: &str) {
        if self.config.read_only || text.is_empty() {
            return;
        }
        let text = normalize(text);
        if text == "\n" {
            return self.newline();
        }
        let mut chars = text.chars();
        if let (Some(c), None) = (chars.next(), chars.next())
            && self.config.auto_pairs
            && self.type_pair(c)
        {
            return;
        }
        let breaks = self.typing_breaks(&text);
        let edits = self
            .sel
            .list()
            .iter()
            .map(|s| CursorEdit::caret_at(Change::replace(s.range(), text.as_ref()), text.len()))
            .collect();
        self.apply_edits(EditKind::Typing, edits, breaks);
    }

    /// Whether typing `text` starts a new undo step: a word begun after
    /// whitespace.
    fn typing_breaks(&self, text: &str) -> bool {
        let first = text.chars().next().map(char_class);
        let p = self.sel.primary();
        if !p.is_empty() {
            return true;
        }
        let before = self.doc.text().chars_before(p.head).next().map(char_class);
        first == Some(CharClass::Word) && before == Some(CharClass::Space)
    }

    /// Types a character that may open or close a pair; returns whether it
    /// was handled.
    fn type_pair(&mut self, c: char) -> bool {
        let pairs = self.language.auto_pairs().to_vec();
        // Typed over the closer typing put in.
        let closes = pairs.iter().any(|&(_, close)| close == c);
        if closes && self.sel.all_carets() {
            let all_over = self.sel.list().iter().all(|s| {
                self.closers.contains(&s.head)
                    && self.doc.text().chars_from(s.head).next() == Some(c)
            });
            if all_over {
                let sel = self
                    .sel
                    .map_each(|s| Selection::caret(s.head + c.len_utf8()));
                self.closers
                    .retain(|&p| !sel.list().iter().any(|s| s.head == p + c.len_utf8()));
                self.history.close();
                self.sel = sel;
                self.pending.cursors = true;
                self.pending.reveal = true;
                return true;
            }
        }
        let Some(&(open, close)) = pairs.iter().find(|&&(open, _)| open == c) else {
            return false;
        };
        let mut edits = Vec::with_capacity(self.sel.len());
        let mut closers = Vec::new();
        let list = self.sel.list().to_vec();
        for s in &list {
            if !s.is_empty() {
                // Wraps the selection, keeping it selected.
                let inner = self.doc.slice(s.range()).into_owned();
                let n = inner.len();
                let text = format!("{open}{inner}{close}");
                let o = open.len_utf8();
                let select = if s.is_backward() {
                    (o + n, o)
                } else {
                    (o, o + n)
                };
                edits.push(CursorEdit {
                    change: Change::replace(s.range(), text),
                    select: Some(select),
                });
                continue;
            }
            let next = self.doc.text().chars_from(s.head).next();
            let prev = self.doc.text().chars_before(s.head).next();
            let kind = self.kind_at_caret(s.head);
            let closes_here = next.is_none_or(|n| {
                n.is_whitespace()
                    || pairs.iter().any(|&(_, cl)| cl == n)
                    || matches!(n, ',' | ';' | '.')
            });
            // A quote pairs only away from a word, and not inside a string.
            let quote = open == close;
            let quote_ok = !quote
                || (prev.is_none_or(|p| char_class(p) != CharClass::Word) && !kind.is_literal());
            if closes_here && quote_ok && !kind.is_literal() {
                let text = format!("{open}{close}");
                edits.push(CursorEdit::caret_at(
                    Change::insert(s.head, text),
                    open.len_utf8(),
                ));
                closers.push(s.head);
            } else {
                edits.push(CursorEdit::caret_at(
                    Change::insert(s.head, open.to_string()),
                    open.len_utf8(),
                ));
            }
        }
        let before = self.sel.clone();
        self.apply_edits(EditKind::Typing, edits, true);
        // The closers now sit right after each caret that typed a pair.
        let sel = self.sel.clone();
        for (s, b) in sel.list().iter().zip(before.list()) {
            if closers.contains(&b.head) {
                self.closers.push(s.head);
            }
        }
        true
    }

    /// The kind of token the caret at `at` is in: the one before it.
    fn kind_at_caret(&mut self, at: usize) -> crate::highlight::TokenKind {
        let line = self.doc.line_of(at);
        let start = self.doc.line_start(line);
        if at == start {
            return crate::highlight::TokenKind::Text;
        }
        let mut tokens = std::mem::take(&mut self.scratch);
        self.tokens(line, &mut tokens);
        let kind = crate::highlight::kind_at(&tokens, at - start - 1);
        // A string's closing quote is its end, not inside it.
        let text = self.doc.line(line);
        let closed = kind == crate::highlight::TokenKind::String
            && crate::highlight::kind_at(&tokens, at - start)
                != crate::highlight::TokenKind::String
            && text[..at - start].ends_with(['"', '\'']);
        self.scratch = tokens;
        if closed {
            crate::highlight::TokenKind::Text
        } else {
            kind
        }
    }

    /// Breaks the line at every cursor, the new line indented as the one
    /// it broke and further after an opener; between a pair, the closer
    /// goes on a line of its own.
    pub fn newline(&mut self) {
        let unit = self.indent_unit();
        let mut edits = Vec::with_capacity(self.sel.len());
        for s in self.sel.list() {
            let line = self.doc.line_of(s.start());
            let start = self.doc.line_start(line);
            let text = self.doc.line(line);
            let before = &text[..s.start() - start];
            let mut indent = if self.config.auto_indent {
                indent_of(before).to_owned()
            } else {
                String::new()
            };
            let end_line = self.doc.line_of(s.end());
            let after_text = self.doc.line(end_line);
            let after = &after_text[s.end() - self.doc.line_start(end_line)..];
            let opens = self.config.auto_indent && self.language.indents_after(before);
            if opens {
                let closer = before
                    .trim_end()
                    .chars()
                    .last()
                    .and_then(crate::text::bracket_pair);
                let between = closer.is_some_and(|(cl, _)| after.trim_start().starts_with(cl));
                let outer = indent.clone();
                indent.push_str(&unit);
                if between {
                    // The closer onto its own line, under the opener's.
                    let ws = after.len() - after.trim_start().len();
                    let text = format!("\n{indent}\n{outer}");
                    let caret = 1 + indent.len();
                    edits.push(CursorEdit::caret_at(
                        Change::replace(s.start()..s.end() + ws, text),
                        caret,
                    ));
                    continue;
                }
            }
            // Spaces before the caret on its line go: a line ends with none.
            let trail = before.len() - before.trim_end_matches([' ', '\t']).len();
            let trail = if before.trim().is_empty() { 0 } else { trail };
            // Nor does the new line start with the spaces after the caret.
            let lead = after.len() - after.trim_start_matches([' ', '\t']).len();
            let text = format!("\n{indent}");
            let caret = text.len();
            edits.push(CursorEdit::caret_at(
                Change::replace(s.start() - trail..s.end() + lead, text),
                caret,
            ));
        }
        self.apply_edits(EditKind::Other, edits, true);
    }

    /// Opens a line below every cursor's line, indented as it, and puts the
    /// cursors there; above with `above`.
    pub fn open_line(&mut self, above: bool) {
        let unit = self.indent_unit();
        let mut edits = Vec::with_capacity(self.sel.len());
        for s in self.sel.list() {
            let line = self.doc.line_of(s.head);
            let text = self.doc.line(line);
            let mut indent = indent_of(&text).to_owned();
            if above {
                let at = self.doc.line_start(line);
                edits.push(CursorEdit::caret_at(
                    Change::insert(at, format!("{indent}\n")),
                    indent.len(),
                ));
            } else {
                if self.config.auto_indent && self.language.indents_after(&text) {
                    indent.push_str(&unit);
                }
                let at = self.doc.line_end(line);
                let text = format!("\n{indent}");
                let caret = text.len();
                edits.push(CursorEdit::caret_at(Change::insert(at, text), caret));
            }
        }
        self.apply_edits(EditKind::Other, edits, true);
    }

    /// Pastes `text`: at every cursor; a line each when there are as many
    /// lines as cursors; above the cursors' lines when it is whole lines a
    /// copy took.
    pub fn paste(&mut self, text: &str) {
        if self.config.read_only || text.is_empty() {
            return;
        }
        let text = normalize(text).into_owned();
        let n = self.sel.len();
        let whole_lines = self.line_clip.as_deref() == Some(text.as_str()) && self.sel.all_carets();
        let edits: Vec<CursorEdit> = if whole_lines {
            let mut seen = Vec::new();
            self.sel
                .list()
                .iter()
                .filter_map(|s| {
                    let line = self.doc.line_of(s.head);
                    if seen.contains(&line) {
                        return None;
                    }
                    seen.push(line);
                    Some(CursorEdit::mapped(Change::insert(
                        self.doc.line_start(line),
                        text.clone(),
                    )))
                })
                .collect()
        } else {
            let pieces: Vec<&str> = text
                .strip_suffix('\n')
                .unwrap_or(&text)
                .split('\n')
                .collect();
            if n > 1 && pieces.len() == n {
                self.sel
                    .list()
                    .iter()
                    .zip(pieces)
                    .map(|(s, p)| CursorEdit::caret_at(Change::replace(s.range(), p), p.len()))
                    .collect()
            } else {
                self.sel
                    .list()
                    .iter()
                    .map(|s| {
                        CursorEdit::caret_at(Change::replace(s.range(), text.as_str()), text.len())
                    })
                    .collect()
            }
        };
        self.apply_edits(EditKind::Other, edits, true);
    }

    /// What copying puts on the clipboard: the selections, a line each; a
    /// caret's whole line when every cursor is one.
    pub fn copy_text(&mut self) -> Option<String> {
        if self.sel.all_carets() {
            let mut lines: Vec<usize> = self
                .sel
                .list()
                .iter()
                .map(|s| self.doc.line_of(s.head))
                .collect();
            lines.dedup();
            let mut out = String::new();
            for l in lines {
                out.push_str(&self.doc.line(l));
                out.push('\n');
            }
            self.line_clip = Some(out.clone());
            return Some(out);
        }
        self.line_clip = None;
        let parts: Vec<String> = self
            .sel
            .list()
            .iter()
            .filter(|s| !s.is_empty())
            .map(|s| self.doc.slice(s.range()).into_owned())
            .collect();
        Some(parts.join("\n"))
    }

    /// Cuts: copies, then deletes what it copied. A second cut asked for
    /// before the view next draws is the platform's echo of the first and
    /// cuts nothing.
    pub fn cut(&mut self) -> Option<String> {
        if self.config.read_only {
            return self.copy_text();
        }
        if self.cut_guard {
            return None;
        }
        let text = self.copy_text()?;
        self.cut_guard = true;
        if self.sel.all_carets() {
            self.delete_lines();
        } else {
            let edits = self
                .sel
                .list()
                .iter()
                .map(|s| CursorEdit::caret_at(Change::delete(s.range()), 0))
                .collect();
            self.apply_edits(EditKind::Other, edits, true);
        }
        Some(text)
    }

    /// Deletes backward by `reach` at every caret, or what each selects. In
    /// a line's indentation of spaces a grapheme step goes back to the tab
    /// stop; between an empty pair both go.
    pub fn backspace(&mut self, reach: Reach) {
        let tab = self.rules().tab as usize;
        let pairs = self.language.auto_pairs().to_vec();
        let mut edits = Vec::with_capacity(self.sel.len());
        for s in self.sel.list() {
            if !s.is_empty() {
                edits.push(CursorEdit::caret_at(Change::delete(s.range()), 0));
                continue;
            }
            let at = s.head;
            let line = self.doc.line_of(at);
            let start = self.doc.line_start(line);
            let text = self.doc.line(line);
            let col = at - start;
            let from = match reach {
                Reach::Line => {
                    let indent = indent_of(&text).len();
                    if col == 0 {
                        at.saturating_sub(1)
                    } else if col > indent {
                        start + indent
                    } else {
                        start
                    }
                }
                Reach::Word => self.word(at, Direction::Back),
                Reach::Grapheme => {
                    let before = &text[..col];
                    if col > 0 && self.config.insert_spaces && before.bytes().all(|b| b == b' ') {
                        // To the tab stop before.
                        let back = (col - 1) % tab + 1;
                        at - back.min(col)
                    } else if col == 0 {
                        at.saturating_sub(1)
                    } else {
                        start + prev_grapheme(&text, col)
                    }
                }
            };
            let mut to = at;
            if reach == Reach::Grapheme && from + 1 == at && self.config.auto_pairs {
                let prev = text[..col].chars().next_back();
                let next = text[col..].chars().next();
                if let (Some(p), Some(n)) = (prev, next)
                    && pairs.iter().any(|&(o, c)| o == p && c == n)
                {
                    to = at + n.len_utf8();
                }
            }
            edits.push(CursorEdit::caret_at(Change::delete(from..to), 0));
        }
        self.apply_edits(EditKind::Backspace, edits, false);
    }

    /// Deletes forward by `reach` at every caret, or what each selects.
    pub fn delete(&mut self, reach: Reach) {
        let mut edits = Vec::with_capacity(self.sel.len());
        for s in self.sel.list() {
            if !s.is_empty() {
                edits.push(CursorEdit::caret_at(Change::delete(s.range()), 0));
                continue;
            }
            let at = s.head;
            let line = self.doc.line_of(at);
            let start = self.doc.line_start(line);
            let text = self.doc.line(line);
            let col = at - start;
            let to = match reach {
                Reach::Line if col < text.len() => start + text.len(),
                Reach::Line => (at + 1).min(self.doc.len()),
                Reach::Word => {
                    // Within the line, not over the fold this line heads.
                    if col >= text.len() {
                        (at + 1).min(self.doc.len())
                    } else {
                        self.word(at, Direction::Forward)
                    }
                }
                Reach::Grapheme if col >= text.len() => (at + 1).min(self.doc.len()),
                Reach::Grapheme => start + next_grapheme(&text, col),
            };
            edits.push(CursorEdit::caret_at(Change::delete(at..to), 0));
        }
        self.apply_edits(EditKind::Delete, edits, false);
    }

    /// What one level of indentation is.
    pub fn indent_unit(&self) -> String {
        if self.config.insert_spaces {
            " ".repeat(self.rules().tab as usize)
        } else {
            "\t".to_owned()
        }
    }

    /// The lines the cursors touch, each once, in order: a selection ending
    /// at a line's start leaves that line out.
    fn touched_lines(&self) -> Vec<usize> {
        let mut out: Vec<usize> = Vec::new();
        for s in self.sel.list() {
            let first = self.doc.line_of(s.start());
            let mut last = self.doc.line_of(s.end());
            if last > first && s.end() == self.doc.line_start(last) {
                last -= 1;
            }
            for l in first..=last {
                if out.last() != Some(&l) && !out.contains(&l) {
                    out.push(l);
                }
            }
        }
        out.sort_unstable();
        out
    }

    /// Tab: indents the lines of a selection over lines; else puts in
    /// indentation to the next tab stop at every cursor.
    pub fn tab(&mut self) {
        let over_lines = self
            .sel
            .list()
            .iter()
            .any(|s| self.doc.line_of(s.start()) != self.doc.line_of(s.end()));
        if over_lines {
            return self.indent_lines();
        }
        let tab = self.rules().tab;
        let edits = self
            .sel
            .list()
            .iter()
            .map(|s| {
                let text = if self.config.insert_spaces {
                    let (_, _, col) = self.point_of(s.start());
                    " ".repeat(crate::text::tab_cells(col, tab) as usize)
                } else {
                    "\t".to_owned()
                };
                let n = text.len();
                CursorEdit::caret_at(Change::replace(s.range(), text), n)
            })
            .collect();
        self.apply_edits(EditKind::Typing, edits, true);
    }

    /// Indents every line the cursors touch by one level; a blank line
    /// stays blank.
    pub fn indent_lines(&mut self) {
        let unit = self.indent_unit();
        let edits = self
            .touched_lines()
            .into_iter()
            .filter(|&l| {
                !self.doc.line(l).trim().is_empty() || self.sel.len() == 1 && self.sel.all_carets()
            })
            .map(|l| CursorEdit::mapped(Change::insert(self.doc.line_start(l), unit.clone())))
            .collect();
        self.apply_edits(EditKind::Other, edits, true);
    }

    /// Takes one level of indentation off every line the cursors touch.
    pub fn outdent_lines(&mut self) {
        let tab = self.rules().tab as usize;
        let edits = self
            .touched_lines()
            .into_iter()
            .filter_map(|l| {
                let text = self.doc.line(l);
                let indent = indent_of(&text);
                let n = if indent.starts_with('\t') {
                    1
                } else {
                    indent
                        .bytes()
                        .take(tab)
                        .take_while(|&b| b == b' ')
                        .count()
                        .max(usize::from(indent.as_bytes().get(..1) == Some(b"\t")))
                };
                let start = self.doc.line_start(l);
                (n > 0).then(|| CursorEdit::mapped(Change::delete(start..start + n)))
            })
            .collect();
        self.apply_edits(EditKind::Other, edits, true);
    }

    /// Comments out every line the cursors touch with the language's line
    /// comment, or, when every one with text already is, uncomments them.
    pub fn toggle_comment(&mut self) -> bool {
        let Some(token) = self.language.line_comment().map(str::to_owned) else {
            return false;
        };
        let lines = self.touched_lines();
        let texts: Vec<(usize, String)> = lines
            .iter()
            .map(|&l| (l, self.doc.line(l).into_owned()))
            .filter(|(_, t)| !t.trim().is_empty())
            .collect();
        if texts.is_empty() {
            return false;
        }
        let all = texts
            .iter()
            .all(|(_, t)| t.trim_start().starts_with(&token));
        let column = texts
            .iter()
            .map(|(_, t)| indent_of(t).len())
            .min()
            .unwrap_or(0);
        let edits = texts
            .iter()
            .map(|(l, t)| {
                let start = self.doc.line_start(*l);
                if all {
                    let at = indent_of(t).len();
                    let mut end = at + token.len();
                    if t[end..].starts_with(' ') {
                        end += 1;
                    }
                    CursorEdit::mapped(Change::delete(start + at..start + end))
                } else {
                    CursorEdit::mapped(Change::insert(start + column, format!("{token} ")))
                }
            })
            .collect();
        self.apply_edits(EditKind::Other, edits, true);
        true
    }

    /// The runs of touched lines, each `first..=last`.
    fn line_blocks(&self) -> Vec<(usize, usize)> {
        let mut blocks: Vec<(usize, usize)> = Vec::new();
        for l in self.touched_lines() {
            match blocks.last_mut() {
                Some((_, last)) if *last + 1 >= l => *last = l,
                _ => blocks.push((l, l)),
            }
        }
        blocks
    }

    /// Deletes every line the cursors touch, breaks and all.
    pub fn delete_lines(&mut self) {
        let lines = self.doc.line_count();
        let edits = self
            .line_blocks()
            .into_iter()
            .map(|(a, b)| {
                let range = if b + 1 < lines {
                    self.doc.line_start(a)..self.doc.line_start(b + 1)
                } else if a > 0 {
                    self.doc.line_end(a - 1)..self.doc.len()
                } else {
                    0..self.doc.len()
                };
                CursorEdit::mapped(Change::delete(range))
            })
            .collect();
        self.apply_edits(EditKind::Other, edits, true);
    }

    /// Copies every run of touched lines below itself (above with `up`),
    /// the cursors going with the copy below.
    pub fn duplicate_lines(&mut self, up: bool) {
        let blocks = self.line_blocks();
        let mut changes = Vec::with_capacity(blocks.len());
        let mut shift = Vec::with_capacity(blocks.len());
        for (a, b) in blocks {
            let start = self.doc.line_start(a);
            let end = self.doc.line_end(b);
            let text = self.doc.slice(start..end).into_owned();
            let len = text.len() + 1;
            if up {
                changes.push(Change::insert(start, format!("{text}\n")));
            } else {
                changes.push(Change::insert(end, format!("\n{text}")));
            }
            shift.push((start, end, len));
        }
        let cs = ChangeSet::new(changes);
        // Cursors in a copied run go with the copy below; mapping keeps
        // them on the run above.
        let sel = self.sel.map_each(|s| {
            let moved = |p: usize| {
                let before = cs.map(p, crate::Assoc::Before);
                if up {
                    return before;
                }
                match shift.iter().find(|&&(a, b, _)| a <= p && p <= b) {
                    Some(&(a, _, len)) => cs.map(a, crate::Assoc::Before) + (p - a) + len,
                    None => before,
                }
            };
            Selection::new(moved(s.anchor), moved(s.head))
        });
        // Duplicating up: the cursors stay on the copy above, where the text
        // they were on now is.
        self.apply(EditKind::Other, cs, Some(sel), true);
    }

    /// Moves every run of touched lines one line up, or down, past the line
    /// there; the cursors go with them.
    pub fn move_lines(&mut self, dir: Direction) {
        let lines = self.doc.line_count();
        let blocks = self.line_blocks();
        let mut changes = Vec::with_capacity(blocks.len());
        let mut moves: Vec<(Range<usize>, isize)> = Vec::new();
        for (a, b) in blocks {
            match dir {
                Direction::Back if a > 0 => {
                    let other = self.doc.line(a - 1).into_owned();
                    let start = self.doc.line_start(a - 1);
                    let end = self.doc.line_end(b);
                    let block = self.doc.slice(self.doc.line_start(a)..end).into_owned();
                    changes.push(Change::replace(start..end, format!("{block}\n{other}")));
                    moves.push((self.doc.line_start(a)..end, -(other.len() as isize + 1)));
                    moves.push((start..self.doc.line_end(a - 1), block.len() as isize + 1));
                }
                Direction::Forward if b + 1 < lines => {
                    let other = self.doc.line(b + 1).into_owned();
                    let start = self.doc.line_start(a);
                    let end = self.doc.line_end(b + 1);
                    let block = self.doc.slice(start..self.doc.line_end(b)).into_owned();
                    changes.push(Change::replace(start..end, format!("{other}\n{block}")));
                    moves.push((start..self.doc.line_end(b), other.len() as isize + 1));
                    moves.push((self.doc.line_start(b + 1)..end, -(block.len() as isize + 1)));
                }
                _ => return,
            }
        }
        let cs = ChangeSet::new(changes);
        let sel = self.sel.map_each(|s| {
            let moved = |p: usize| match moves.iter().find(|(r, _)| r.start <= p && p <= r.end) {
                Some((_, by)) => (p as isize + by) as usize,
                None => p,
            };
            let mut out = Selection::new(moved(s.anchor), moved(s.head));
            out.goal = s.goal;
            out
        });
        self.apply(EditKind::Other, cs, Some(sel), true);
    }

    /// Replaces bytes `range` with `text` as an edit of its own, the cursors
    /// following: a language service's edit, a replace.
    pub fn replace_range(&mut self, range: Range<usize>, text: &str) {
        let text = normalize(text);
        self.apply(
            EditKind::Other,
            ChangeSet::one(Change::replace(range, text.as_ref())),
            None,
            true,
        );
    }

    /// Applies `changes` (in the text as it is, any order) as one edit, the
    /// cursors following: a language service's edits.
    pub fn apply_changes(&mut self, changes: Vec<Change>) {
        if self.config.read_only {
            return;
        }
        let changes = changes
            .into_iter()
            .map(|c| Change::replace(c.range, normalize(&c.text)))
            .collect();
        self.apply(EditKind::Other, ChangeSet::new(changes), None, true);
    }

    // ----- input method -----

    /// Shows the input method's composition `text` at the primary caret,
    /// its own caret at byte `caret`; an empty one ends it.
    pub fn set_preedit(&mut self, text: &str, caret: usize) {
        if text.is_empty() {
            if self.preedit.take().is_some() {
                self.pending.text = true;
                self.pending.cursors = true;
            }
            return;
        }
        let at = self.sel.primary().start();
        let caret = caret.min(text.len());
        let next = Preedit {
            at,
            text: text.to_owned(),
            caret,
        };
        if self.preedit.as_ref() != Some(&next) {
            let wraps_before = self
                .config
                .wrap
                .then(|| self.row_count(self.doc.line_of(at)));
            self.preedit = Some(next);
            self.pending.text = true;
            self.pending.cursors = true;
            self.pending.reveal = true;
            if wraps_before.is_some_and(|n| n != self.row_count(self.doc.line_of(at))) {
                self.pending.rows = true;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::highlight::{Language, Token, TokenKind};
    use std::rc::Rc;

    /// Braces and `//` comments, strings in quotes.
    struct Code;

    impl Language for Code {
        fn name(&self) -> &str {
            "code"
        }

        fn line(&self, _: u32, line: &str, out: &mut Vec<Token>) -> u32 {
            let mut in_str = false;
            let mut start = 0;
            for (i, c) in line.char_indices() {
                if c == '"' {
                    if in_str {
                        out.push(Token::new(i + 1 - start, TokenKind::String));
                        start = i + 1;
                    } else {
                        out.push(Token::new(i - start, TokenKind::Text));
                        start = i;
                    }
                    in_str = !in_str;
                }
            }
            let kind = if in_str {
                TokenKind::String
            } else {
                TokenKind::Text
            };
            out.push(Token::new(line.len() - start, kind));
            0
        }

        fn line_comment(&self) -> Option<&str> {
            Some("//")
        }
    }

    fn session(text: &str, carets: &[usize]) -> Session {
        let mut s = Session::new(text);
        s.set_language(Rc::new(Code));
        s.set_selections(Selections::new(
            carets.iter().map(|&c| Selection::caret(c)).collect(),
            0,
        ));
        s
    }

    fn carets(s: &Session) -> Vec<usize> {
        s.selections().list().iter().map(|s| s.head).collect()
    }

    #[test]
    fn typing_at_many_carets_is_one_undo_step() {
        let mut s = session("one\ntwo\nthree", &[3, 7, 13]);
        for c in ["!", "?"] {
            s.type_text(c);
        }
        assert_eq!(s.text(), "one!?\ntwo!?\nthree!?");
        assert_eq!(carets(&s), [5, 11, 19]);
        assert!(s.undo());
        assert_eq!(s.text(), "one\ntwo\nthree");
        assert_eq!(carets(&s), [3, 7, 13]);
        assert!(s.redo());
        assert_eq!(s.text(), "one!?\ntwo!?\nthree!?");
    }

    #[test]
    fn a_new_line_keeps_the_indent_and_opens_a_block() {
        let mut s = session("    fn a() {}", &[12]);
        s.newline();
        assert_eq!(s.text(), "    fn a() {\n        \n    }");
        assert_eq!(carets(&s), [21]);
        let mut s = session("    let x = 1;", &[14]);
        s.newline();
        assert_eq!(s.text(), "    let x = 1;\n    ");
    }

    #[test]
    fn pairs_close_themselves_and_are_typed_over() {
        let mut s = session("", &[0]);
        s.type_text("(");
        assert_eq!(s.text(), "()");
        s.type_text("x");
        s.type_text(")");
        assert_eq!(s.text(), "(x)");
        assert_eq!(carets(&s), [3]);
        // Not before a word.
        let mut s = session("word", &[0]);
        s.type_text("(");
        assert_eq!(s.text(), "(word");
        // A selection is wrapped.
        let mut s = session("abc", &[0]);
        s.set_selections(Selections::one(Selection::new(0, 3)));
        s.type_text("[");
        assert_eq!(s.text(), "[abc]");
        assert_eq!(s.selections().primary().range(), 1..4);
        // Backspace in an empty pair takes both.
        let mut s = session("", &[0]);
        s.type_text("{");
        s.backspace(Reach::Grapheme);
        assert_eq!(s.text(), "");
        // A quote inside a string does not pair.
        let mut s = session("\"ab\"", &[2]);
        s.type_text("\"");
        assert_eq!(s.text(), "\"a\"b\"");
    }

    #[test]
    fn backspace_in_indentation_goes_to_the_tab_stop() {
        let mut s = session("      x", &[6]);
        s.backspace(Reach::Grapheme);
        assert_eq!(s.text(), "    x");
        s.backspace(Reach::Grapheme);
        assert_eq!(s.text(), "x");
        let mut s = session("ab cd", &[5]);
        s.backspace(Reach::Word);
        assert_eq!(s.text(), "ab ");
    }

    #[test]
    fn lines_indent_comment_move_and_duplicate() {
        let mut s = session("a\nb\nc", &[0]);
        s.set_selections(Selections::one(Selection::new(0, 3)));
        s.indent_lines();
        assert_eq!(s.text(), "    a\n    b\nc");
        s.outdent_lines();
        assert_eq!(s.text(), "a\nb\nc");
        assert!(s.toggle_comment());
        assert_eq!(s.text(), "// a\n// b\nc");
        assert!(s.toggle_comment());
        assert_eq!(s.text(), "a\nb\nc");
        let mut s = session("a\nb\nc", &[2]);
        s.move_lines(Direction::Back);
        assert_eq!(s.text(), "b\na\nc");
        assert_eq!(carets(&s), [0]);
        s.move_lines(Direction::Forward);
        s.move_lines(Direction::Forward);
        assert_eq!(s.text(), "a\nc\nb");
        assert_eq!(carets(&s), [4]);
        s.duplicate_lines(false);
        assert_eq!(s.text(), "a\nc\nb\nb");
        assert_eq!(carets(&s), [6]);
        s.delete_lines();
        assert_eq!(s.text(), "a\nc\nb");
    }

    #[test]
    fn paste_spreads_lines_over_cursors_and_whole_lines_go_above() {
        let mut s = session("x\ny", &[1, 3]);
        s.paste("1\n2\n");
        assert_eq!(s.text(), "x1\ny2");
        let mut s = session("abc\ndef", &[1]);
        let copied = s.copy_text().unwrap();
        assert_eq!(copied, "abc\n");
        s.set_selections(Selections::one(Selection::caret(5)));
        s.paste(&copied);
        assert_eq!(s.text(), "abc\nabc\ndef");
        // A cut and its echo cut once.
        let mut s = session("a\nb\nc", &[2]);
        assert_eq!(s.cut().as_deref(), Some("b\n"));
        assert_eq!(s.cut(), None);
        assert_eq!(s.text(), "a\nc");
        s.take_pending();
        assert_eq!(s.cut().as_deref(), Some("c\n"));
        assert_eq!(s.text(), "a");
    }

    #[test]
    fn motions_go_by_word_line_and_row() {
        let mut s = session("let value = 1;\n    x", &[0]);
        s.move_cursors(viso_ui::text_edit::Motion::WordRight, false);
        assert_eq!(carets(&s), [3]);
        s.move_cursors(viso_ui::text_edit::Motion::WordRight, false);
        assert_eq!(carets(&s), [9]);
        s.move_cursors(viso_ui::text_edit::Motion::Down, false);
        assert_eq!(carets(&s), [20]);
        s.move_cursors(viso_ui::text_edit::Motion::LineStart, false);
        assert_eq!(carets(&s), [19]);
        s.move_cursors(viso_ui::text_edit::Motion::LineStart, false);
        assert_eq!(carets(&s), [15]);
        s.move_cursors(viso_ui::text_edit::Motion::Up, true);
        assert_eq!(s.selections().primary().range(), 0..15);
        // The goal column holds over a short line.
        let mut s = session("abcdef\nab\nabcdef", &[5]);
        s.move_cursors(viso_ui::text_edit::Motion::Down, false);
        assert_eq!(carets(&s), [9]);
        s.move_cursors(viso_ui::text_edit::Motion::Down, false);
        assert_eq!(carets(&s), [15]);
    }

    #[test]
    fn next_match_adds_cursors_and_all_matches_selects_each() {
        let mut s = session("foo bar foo baz foo", &[1]);
        assert!(s.add_next_match());
        assert_eq!(s.selections().primary().range(), 0..3);
        assert!(s.add_next_match());
        assert!(s.add_next_match());
        assert_eq!(s.selections().len(), 3);
        assert!(!s.add_next_match());
        s.type_text("x");
        assert_eq!(s.text(), "x bar x baz x");
        let mut s = session("ab ab ab", &[0]);
        assert!(s.select_all_matches());
        assert_eq!(s.selections().len(), 3);
        s.add_cursor_vertically(Direction::Forward);
    }

    #[test]
    fn cursors_above_and_below_keep_their_column() {
        let mut s = session("abcd\nab\nabcd", &[3]);
        s.add_cursor_vertically(Direction::Forward);
        s.add_cursor_vertically(Direction::Forward);
        assert_eq!(carets(&s), [3, 7, 11]);
        s.type_text("-");
        assert_eq!(s.text(), "abc-d\nab-\nabc-d");
    }
}
