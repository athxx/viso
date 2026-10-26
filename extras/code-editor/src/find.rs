//! Finding in a session: the open search, stepping the primary cursor
//! through its matches, replacing one or all of them, and the bracket that
//! pairs with the one at the caret.

use std::ops::Range;

use crate::document::{Change, ChangeSet};
use crate::highlight::{Token, TokenKind};
use crate::history::EditKind;
use crate::search::{Query, Search};
use crate::selection::{Selection, Selections};
use crate::session::Session;
use crate::text::{CharClass, bracket_pair, char_class};

/// How far a bracket's pair is looked for, in lines either way.
const BRACKET_LINES: usize = 2000;

impl Session {
    /// Searches for `query`, in place of the open search; the cursors stay.
    pub fn find(&mut self, query: Query) {
        if self.search.as_ref().is_some_and(|s| s.query == query) {
            return;
        }
        self.search = Some(Search::new(query, &self.doc));
        self.pending.search = true;
    }

    /// Ends the search; returns whether one was open.
    pub fn close_search(&mut self) -> bool {
        let open = self.search.take().is_some();
        self.pending.search |= open;
        open
    }

    /// The match the primary selection is on.
    pub fn current_match(&self) -> Option<usize> {
        let search = self.search.as_ref()?;
        let p = self.sel.primary().range();
        let matches = search.matches();
        let i = matches.partition_point(|m| m.start < p.start);
        (matches.get(i) == Some(&p)).then_some(i)
    }

    /// Selects the next match after the primary selection, or the one
    /// before it with `back`, wrapping; returns whether there is one.
    pub fn find_next(&mut self, back: bool) -> bool {
        let p = self.sel.primary();
        let at = if back { p.start() } else { p.end() };
        self.select_match(at, back)
    }

    /// Selects the first match at or after the primary selection's start:
    /// where a search typed a letter at a time stays.
    pub fn find_from_cursor(&mut self) -> bool {
        let at = self.sel.primary().start();
        self.select_match(at, false)
    }

    fn select_match(&mut self, at: usize, back: bool) -> bool {
        let Some(search) = &self.search else {
            return false;
        };
        let i = if back {
            search.prev_from(at)
        } else {
            search.next_from(at)
        };
        let Some(m) = i.map(|i| search.matches()[i].clone()) else {
            return false;
        };
        self.set_selections(Selections::one(Selection::new(m.start, m.end)));
        self.pending.reveal = true;
        self.pending.search = true;
        true
    }

    /// Replaces the match the primary selection is on with `with` (its
    /// groups filled in for a regular expression) and selects the next; off
    /// a match, only selects the next. Returns whether it replaced.
    pub fn replace_current(&mut self, with: &str) -> bool {
        if self.config.read_only {
            return false;
        }
        let Some(i) = self.current_match() else {
            self.find_next(false);
            return false;
        };
        let Some(search) = &self.search else {
            return false;
        };
        let range = search.matches()[i].clone();
        let Some(text) = search
            .matcher()
            .map(|m| m.replacement(&self.doc, range.clone(), with))
        else {
            return false;
        };
        let end = range.start + text.len();
        self.apply(
            EditKind::Other,
            ChangeSet::one(Change::replace(range, text)),
            Some(Selections::one(Selection::caret(end))),
            true,
        );
        self.find_next(false);
        true
    }

    /// Replaces every match with `with`, as one undo step; returns how
    /// many it replaced.
    pub fn replace_all(&mut self, with: &str) -> usize {
        if self.config.read_only {
            return 0;
        }
        let Some(search) = &self.search else {
            return 0;
        };
        let Some(matcher) = search.matcher() else {
            return 0;
        };
        let changes: Vec<Change> = search
            .matches()
            .iter()
            .map(|m| Change::replace(m.clone(), matcher.replacement(&self.doc, m.clone(), with)))
            .collect();
        let n = changes.len();
        self.apply(EditKind::Other, ChangeSet::new(changes), None, true);
        n
    }

    /// What a search opened now looks for: the primary selection on one
    /// line, or the word at the caret.
    pub fn seed_query(&self) -> Option<String> {
        let p = self.sel.primary();
        if !p.is_empty() {
            let text = self.doc.slice(p.range());
            return (!text.contains('\n')).then(|| text.into_owned());
        }
        let word = self.word_at(p.head);
        let text = self.doc.slice(word);
        let is_word = text.chars().next().map(char_class) == Some(CharClass::Word);
        is_word.then(|| text.into_owned())
    }

    /// The bracket next to the primary caret and the one it pairs with, in
    /// code rather than in a string or a comment.
    pub fn matching_bracket(&mut self) -> Option<(Range<usize>, Range<usize>)> {
        let p = self.sel.primary();
        if !p.is_empty() {
            return None;
        }
        let at = p.head;
        let line = self.doc.line_of(at);
        let start = self.doc.line_start(line);
        let text = self.doc.line(line).into_owned();
        let mut tokens = Vec::new();
        self.tokens(line, &mut tokens);
        // The bracket after the caret, or else the one before it.
        let col = at - start;
        let before = text[..col].chars().next_back().map(|c| col - c.len_utf8());
        let here = [Some(col), before].into_iter().flatten().find_map(|i| {
            let c = text[i..].chars().next()?;
            let pair = bracket_pair(c)?;
            in_code(&tokens, i).then_some((i, c, pair))
        })?;
        let (i, c, (other, opens)) = here;
        let mut depth = 0usize;
        let mut scan = |l: usize, s: &mut Session, from: Option<usize>| -> Option<usize> {
            let text = s.doc.line(l).into_owned();
            let mut tokens = Vec::new();
            s.tokens(l, &mut tokens);
            let mut found = None;
            let mut visit = |j: usize, ch: char| {
                if !in_code(&tokens, j) {
                    return false;
                }
                if ch == c {
                    depth += 1;
                } else if ch == other {
                    if depth == 0 {
                        found = Some(j);
                        return true;
                    }
                    depth -= 1;
                }
                false
            };
            if opens {
                let from = from.unwrap_or(0);
                for (j, ch) in text[from..].char_indices() {
                    if visit(from + j, ch) {
                        break;
                    }
                }
            } else {
                let to = from.unwrap_or(text.len());
                for (j, ch) in text[..to].char_indices().rev() {
                    if visit(j, ch) {
                        break;
                    }
                }
            }
            found.map(|j| s.doc.line_start(l) + j)
        };
        let lines = self.doc.line_count();
        let first = if opens { i + c.len_utf8() } else { i };
        if let Some(j) = scan(line, self, Some(first)) {
            return Some((start + i..start + i + c.len_utf8(), j..j + other.len_utf8()));
        }
        let range: Box<dyn Iterator<Item = usize>> = if opens {
            Box::new(line + 1..lines.min(line + 1 + BRACKET_LINES))
        } else {
            Box::new((line.saturating_sub(BRACKET_LINES)..line).rev())
        };
        for l in range {
            if let Some(j) = scan(l, self, None) {
                return Some((start + i..start + i + c.len_utf8(), j..j + other.len_utf8()));
            }
        }
        None
    }
}

/// Whether byte `at` of a line is in code: not in a string, a character or
/// a comment.
fn in_code(tokens: &[Token], at: usize) -> bool {
    !matches!(
        crate::highlight::kind_at(tokens, at),
        TokenKind::String
            | TokenKind::Escape
            | TokenKind::Char
            | TokenKind::Comment
            | TokenKind::DocComment
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(text: &str, caret: usize) -> Session {
        let mut s = Session::new(text);
        s.set_selections(Selections::one(Selection::caret(caret)));
        s
    }

    #[test]
    fn stepping_through_matches_wraps_and_replacing_moves_on() {
        let mut s = session("one two one\ntwo one", 0);
        s.find(Query::literal("one"));
        assert_eq!(s.search().unwrap().matches().len(), 3);
        assert!(s.find_from_cursor());
        assert_eq!(s.selections().primary().range(), 0..3);
        assert_eq!(s.current_match(), Some(0));
        assert!(s.find_next(false));
        assert_eq!(s.selections().primary().range(), 8..11);
        assert!(s.find_next(false));
        assert!(s.find_next(false));
        // Past the last, the first again; back from it, the last.
        assert_eq!(s.current_match(), Some(0));
        assert!(s.find_next(true));
        assert_eq!(s.current_match(), Some(2));
        // Replacing the current one selects the next.
        assert!(s.replace_current("1"));
        assert_eq!(s.text(), "one two one\ntwo 1");
        assert_eq!(s.selections().primary().range(), 0..3);
        assert_eq!(s.replace_all("uno"), 2);
        assert_eq!(s.text(), "uno two uno\ntwo 1");
        assert!(s.undo());
        assert_eq!(s.text(), "one two one\ntwo 1");
    }

    #[test]
    fn a_regular_expression_fills_in_its_groups() {
        let mut s = session("let a = 1;\nlet b = 2;", 0);
        s.find(Query {
            text: r"let (\w) = (\d)".into(),
            regex: true,
            case: true,
            word: false,
        });
        assert_eq!(s.replace_all("const $1: i32 = $2"), 2);
        assert_eq!(s.text(), "const a: i32 = 1;\nconst b: i32 = 2;");
    }

    #[test]
    fn a_query_is_seeded_from_the_selection_or_the_word() {
        let mut s = session("alpha beta", 7);
        assert_eq!(s.seed_query().as_deref(), Some("beta"));
        s.set_selections(Selections::one(Selection::new(0, 3)));
        assert_eq!(s.seed_query().as_deref(), Some("alp"));
    }

    #[cfg(feature = "vs")]
    #[test]
    fn brackets_pair_across_lines_past_strings() {
        let text = "f(a, \")\", [b]) {\n  x\n}";
        let mut s = session(text, 1);
        s.set_language(std::rc::Rc::new(crate::lang::vs::Vs));
        assert_eq!(s.matching_bracket(), Some((1..2, 13..14)));
        s.set_selections(Selections::one(Selection::caret(text.len())));
        assert_eq!(
            s.matching_bracket(),
            Some((text.len() - 1..text.len(), 15..16))
        );
    }
}
