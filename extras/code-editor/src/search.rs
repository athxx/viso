//! Finding text: a query, literal or a regular expression, matched over the
//! document, and the matches kept up to date as it is edited.
//!
//! A literal query that matches case is found by scanning the rope's chunks
//! with the standard substring search, a needle's length of overlap carried
//! between chunks, so it costs no copy of the text and finds matches across
//! lines. Anything else — ignoring case, whole words, a regular expression —
//! is a compiled [`regex::Regex`] run a line at a time: a match never crosses
//! a line break, as in most editors.
//!
//! An edit drops the matches it touched and searches again only the lines it
//! replaced; every other match moves with its text.

use std::ops::Range;

use crate::document::{ChangeSet, Document, LineSplice};
use crate::rope::Rope;

/// What to look for.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Query {
    pub text: String,
    /// Whether it is a regular expression.
    pub regex: bool,
    /// Whether case must match.
    pub case: bool,
    /// Whether a match must be a whole word.
    pub word: bool,
}

impl Query {
    /// A literal, case-matching query for `text`.
    pub fn literal(text: impl Into<String>) -> Query {
        Query {
            text: text.into(),
            regex: false,
            case: true,
            word: false,
        }
    }
}

/// A query compiled.
#[derive(Debug, Clone)]
pub enum Matcher {
    /// A literal that matches case: found across lines.
    Literal(String),
    /// Anything else, found a line at a time.
    Regex(regex::Regex),
}

impl Matcher {
    /// Compiles `query`; a bad regular expression says why.
    pub fn new(query: &Query) -> Result<Matcher, String> {
        if query.text.is_empty() {
            return Err("empty".into());
        }
        if !query.regex && query.case && !query.word {
            return Ok(Matcher::Literal(query.text.clone()));
        }
        let mut pattern = if query.regex {
            query.text.clone()
        } else {
            regex::escape(&query.text)
        };
        if query.word {
            pattern = format!(r"\b(?:{pattern})\b");
        }
        regex::RegexBuilder::new(&pattern)
            .case_insensitive(!query.case)
            .multi_line(true)
            .size_limit(1 << 22)
            .build()
            .map(Matcher::Regex)
            .map_err(|e| match e {
                regex::Error::Syntax(s) => s.lines().last().unwrap_or("bad pattern").to_owned(),
                other => other.to_string(),
            })
    }

    /// The matches in lines `lines` of `doc`, pushed to `out` in order.
    pub fn find_in_lines(&self, doc: &Document, lines: Range<usize>, out: &mut Vec<Range<usize>>) {
        let lines = lines.start.min(doc.line_count())..lines.end.min(doc.line_count());
        if lines.is_empty() {
            return;
        }
        match self {
            Matcher::Literal(needle) => {
                let from = doc.line_start(lines.start);
                // Matches starting in these lines; one may run on past them.
                let to = doc.line_end(lines.end - 1);
                let mut at = from;
                while let Some(found) = find_literal(doc.text(), needle, at) {
                    if found > to {
                        break;
                    }
                    out.push(found..found + needle.len());
                    at = found + needle.len().max(1);
                }
            }
            Matcher::Regex(re) => {
                for l in lines {
                    let start = doc.line_start(l);
                    let line = doc.line(l);
                    for m in re.find_iter(&line) {
                        if m.is_empty() {
                            continue;
                        }
                        out.push(start + m.start()..start + m.end());
                    }
                }
            }
        }
    }

    /// Every match in `doc`, in order.
    pub fn find_all(&self, doc: &Document) -> Vec<Range<usize>> {
        let mut out = Vec::new();
        self.find_in_lines(doc, 0..doc.line_count(), &mut out);
        out
    }

    /// What replaces match `range` of `doc`: `with`, its `$1` groups filled
    /// in for a regular expression.
    pub fn replacement(&self, doc: &Document, range: Range<usize>, with: &str) -> String {
        match self {
            Matcher::Literal(_) => with.to_owned(),
            Matcher::Regex(re) => {
                let text = doc.slice(range);
                match re.captures(&text) {
                    Some(caps) => {
                        let mut out = String::new();
                        caps.expand(with, &mut out);
                        out
                    }
                    None => with.to_owned(),
                }
            }
        }
    }
}

/// The first `needle` in `rope` at or after byte `from`.
pub fn find_literal(rope: &Rope, needle: &str, from: usize) -> Option<usize> {
    if needle.is_empty() || from >= rope.len() {
        return None;
    }
    let keep = needle.len() - 1;
    // The tail of the chunks before, so a match across a chunk edge is seen.
    let mut carry = String::new();
    let mut carry_start = from;
    for chunk in rope.chunks(from..rope.len()) {
        let chunk_start = carry_start + carry.len();
        if !carry.is_empty() {
            let mut window = String::with_capacity(carry.len() + keep);
            window.push_str(&carry);
            window.push_str(&chunk[..floor_char(chunk, keep.min(chunk.len()))]);
            if let Some(i) = window.find(needle) {
                return Some(carry_start + i);
            }
        }
        if let Some(i) = chunk.find(needle) {
            return Some(chunk_start + i);
        }
        if keep == 0 {
            continue;
        }
        // Keep the last `keep` bytes seen, from a char boundary.
        if chunk.len() >= keep {
            let tail = ceil_char(chunk, chunk.len() - keep);
            carry.clear();
            carry.push_str(&chunk[tail..]);
            carry_start = chunk_start + tail;
        } else {
            carry.push_str(chunk);
            if carry.len() > keep {
                let cut = ceil_char(&carry, carry.len() - keep);
                carry.drain(..cut);
                carry_start += cut;
            }
        }
    }
    None
}

/// The last `needle` in `rope` ending at or before byte `before`.
pub fn rfind_literal(rope: &Rope, needle: &str, before: usize) -> Option<usize> {
    // Rare enough (a search backward) to look line by line from the end.
    let before = before.min(rope.len());
    let mut line = rope.line_of(before);
    loop {
        let start = rope.line_start(line);
        let text = rope.slice(start..before.min(rope.line_end(line)).max(start));
        if !needle.contains('\n')
            && let Some(i) = text.rfind(needle)
        {
            return Some(start + i);
        }
        if needle.contains('\n') {
            // A needle over lines: scan forward from the top for the last.
            let mut at = 0;
            let mut last = None;
            while let Some(found) = find_literal(rope, needle, at) {
                if found + needle.len() > before {
                    break;
                }
                last = Some(found);
                at = found + 1;
            }
            return last;
        }
        line = line.checked_sub(1)?;
    }
}

fn floor_char(s: &str, mut at: usize) -> usize {
    while at > 0 && !s.is_char_boundary(at) {
        at -= 1;
    }
    at
}

fn ceil_char(s: &str, mut at: usize) -> usize {
    while at < s.len() && !s.is_char_boundary(at) {
        at += 1;
    }
    at
}

/// Moves `matches`, sorted, through an edit: a match the edit touches goes,
/// the rest follow their text.
pub fn map_ranges(matches: &mut Vec<Range<usize>>, changes: &ChangeSet) {
    if changes.is_empty() || matches.is_empty() {
        return;
    }
    let cs = changes.changes();
    let mut c = 0;
    let mut delta = 0isize;
    matches.retain_mut(|m| {
        while c < cs.len() && cs[c].range.end < m.start {
            delta += cs[c].text.len() as isize - cs[c].range.len() as isize;
            c += 1;
        }
        // Touched: the change overlaps it, or meets it at an edge.
        if c < cs.len() && cs[c].range.start <= m.end {
            return false;
        }
        m.start = (m.start as isize + delta) as usize;
        m.end = (m.end as isize + delta) as usize;
        true
    });
}

/// The matches of `matcher` kept up to date over edits of a document.
#[derive(Debug, Clone)]
pub struct Search {
    pub query: Query,
    matcher: Option<Matcher>,
    /// Why the query does not compile, if it does not.
    pub error: Option<String>,
    matches: Vec<Range<usize>>,
}

impl Search {
    /// Searches `doc` for `query`.
    pub fn new(query: Query, doc: &Document) -> Search {
        let (matcher, error) = match Matcher::new(&query) {
            Ok(m) => (Some(m), None),
            // An empty query finds nothing, and is not wrong.
            Err(_) if query.text.is_empty() => (None, None),
            Err(e) => (None, Some(e)),
        };
        let matches = matcher.as_ref().map_or_else(Vec::new, |m| m.find_all(doc));
        Search {
            query,
            matcher,
            error,
            matches,
        }
    }

    /// The matches, in order.
    pub fn matches(&self) -> &[Range<usize>] {
        &self.matches
    }

    /// The compiled query.
    pub fn matcher(&self) -> Option<&Matcher> {
        self.matcher.as_ref()
    }

    /// The matches that start in, or reach into, bytes `range`.
    pub fn in_range(&self, range: Range<usize>) -> &[Range<usize>] {
        let a = self.matches.partition_point(|m| m.end <= range.start);
        let b = self
            .matches
            .partition_point(|m| m.start < range.end.max(range.start + 1));
        &self.matches[a..b.max(a)]
    }

    /// The index of the first match at or after byte `at`, wrapping.
    pub fn next_from(&self, at: usize) -> Option<usize> {
        if self.matches.is_empty() {
            return None;
        }
        let i = self.matches.partition_point(|m| m.start < at);
        Some(if i == self.matches.len() { 0 } else { i })
    }

    /// The index of the last match before byte `at`, wrapping.
    pub fn prev_from(&self, at: usize) -> Option<usize> {
        if self.matches.is_empty() {
            return None;
        }
        let i = self.matches.partition_point(|m| m.start < at);
        Some(if i == 0 {
            self.matches.len() - 1
        } else {
            i - 1
        })
    }

    /// Follows an edit of `doc`, which `changes` made and `splices` say the
    /// lines of.
    pub fn update(&mut self, doc: &Document, changes: &ChangeSet, splices: &[LineSplice]) {
        let Some(matcher) = &self.matcher else {
            return;
        };
        map_ranges(&mut self.matches, changes);
        let mut found = Vec::new();
        for s in splices {
            // A literal over lines may start a line or two above the edit.
            let reach = match matcher {
                Matcher::Literal(n) => crate::rope::count_newlines(n.as_bytes()),
                Matcher::Regex(_) => 0,
            };
            let from = s.new.saturating_sub(reach);
            matcher.find_in_lines(doc, from..s.new + s.added, &mut found);
        }
        if found.is_empty() {
            return;
        }
        self.matches.extend(found);
        self.matches.sort_by_key(|m| m.start);
        self.matches.dedup_by(|b, a| b.start < a.end);
    }

    /// Searches `doc` from scratch.
    pub fn refresh(&mut self, doc: &Document) {
        self.matches = self
            .matcher
            .as_ref()
            .map_or_else(Vec::new, |m| m.find_all(doc));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Change;

    #[test]
    fn a_literal_is_found_across_chunks_and_lines() {
        let mut text = "x".repeat(5000);
        text.push_str("needle\nin\nhay");
        text.push_str(&"y".repeat(3000));
        text.push_str("needle");
        let rope = Rope::from(text.as_str());
        assert!(rope.depth() > 0);
        assert_eq!(find_literal(&rope, "needle", 0), Some(5000));
        assert_eq!(find_literal(&rope, "needle", 5001), Some(text.len() - 6));
        assert_eq!(find_literal(&rope, "le\nin\nh", 0), Some(5004));
        // Every chunk edge: a needle placed across each.
        for at in (1000..1100).step_by(7) {
            let mut t = "a".repeat(at);
            t.push_str("XYZW");
            t.push_str(&"b".repeat(2000));
            let rope = Rope::from(t.as_str());
            assert_eq!(find_literal(&rope, "XYZW", 0), Some(at), "at {at}");
        }
        assert_eq!(
            rfind_literal(&rope, "XYZW", rope.len()),
            find_literal(&rope, "XYZW", 0)
        );
    }

    #[test]
    fn queries_compile_to_what_they_ask() {
        let doc = Document::new("Foo foo food\nfoo_bar FOO");
        let all = |q: Query| Matcher::new(&q).unwrap().find_all(&doc);
        assert_eq!(all(Query::literal("foo")).len(), 3);
        let anycase = Query {
            case: false,
            ..Query::literal("foo")
        };
        assert_eq!(all(anycase.clone()).len(), 5);
        let words = Query {
            word: true,
            ..anycase
        };
        assert_eq!(all(words), [0..3, 4..7, 21..24]);
        let re = Query {
            regex: true,
            ..Query::literal(r"fo(o)\w*")
        };
        let m = Matcher::new(&re).unwrap();
        assert_eq!(m.find_all(&doc), [4..7, 8..12, 13..20]);
        assert_eq!(m.replacement(&doc, 8..12, "<$1>"), "<o>");
        assert!(
            Matcher::new(&Query {
                regex: true,
                ..Query::literal("(")
            })
            .is_err()
        );
    }

    #[test]
    fn matches_follow_edits_and_the_edited_lines_are_searched_again() {
        let mut doc = Document::new("ab ab\ncd ab\nab");
        let mut search = Search::new(Query::literal("ab"), &doc);
        assert_eq!(search.matches(), [0..2, 3..5, 9..11, 12..14]);
        let cs = ChangeSet::new(vec![Change::insert(7, "ab"), Change::delete(12..13)]);
        let applied = doc.apply(&cs);
        search.update(&doc, &cs, &applied.splices);
        let fresh = Search::new(Query::literal("ab"), &doc);
        assert_eq!(search.matches(), fresh.matches());
        assert_eq!(search.next_from(6), Some(2));
        assert_eq!(search.prev_from(0), Some(search.matches().len() - 1));
    }
}
