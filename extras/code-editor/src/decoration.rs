//! Decorations: looks laid over ranges of the text — a diagnostic's
//! squiggle, a symbol's other uses, a reviewer's mark — kept in named sets
//! so each source replaces its own without touching the others'.
//!
//! A range follows the text it covers as it is edited; one an edit removes
//! whole goes. Each set is sorted by start, so the decorations on a line are
//! found by a search and a short walk, not a pass over all of them.

use std::ops::Range;

use viso_ui::Rgba;

use crate::document::{Assoc, ChangeSet};

/// How serious a diagnostic is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    Hint,
    Info,
    Warning,
    Error,
}

/// How a decoration looks.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DecorationStyle {
    /// A diagnostic: a squiggle under it in its severity's color, and a mark
    /// in the gutter.
    Diagnostic(Severity),
    /// Another use of the symbol at the caret.
    Highlight,
    /// A use that writes the symbol at the caret.
    WriteHighlight,
    /// Colors of the caller's own.
    Custom {
        background: Option<Rgba>,
        underline: Option<Rgba>,
        /// A wavy underline rather than a straight one.
        squiggle: bool,
        /// A mark in the gutter on its first line.
        gutter: Option<Rgba>,
        /// Its background fills its lines whole.
        whole_line: bool,
    },
}

/// A look over bytes `range`, and what a pointer over it shows.
#[derive(Debug, Clone, PartialEq)]
pub struct Decoration {
    pub range: Range<usize>,
    pub style: DecorationStyle,
    pub message: Option<String>,
}

impl Decoration {
    /// A diagnostic of `severity` over `range` saying `message`.
    pub fn diagnostic(range: Range<usize>, severity: Severity, message: impl Into<String>) -> Self {
        Decoration {
            range,
            style: DecorationStyle::Diagnostic(severity),
            message: Some(message.into()),
        }
    }
}

/// The set language diagnostics go in.
pub const DIAGNOSTICS: &str = "diagnostics";

/// The set the symbol-at-the-caret highlights go in.
pub const HIGHLIGHTS: &str = "highlights";

/// One named set: sorted by start, with the longest range's length, which
/// bounds how far before a line one reaching into it can start.
#[derive(Debug, Clone)]
struct Set {
    name: String,
    list: Vec<Decoration>,
    longest: usize,
}

impl Set {
    fn sort(&mut self) {
        self.list.sort_by_key(|d| (d.range.start, d.range.end));
        self.longest = self.list.iter().map(|d| d.range.len()).max().unwrap_or(0);
    }
}

/// Decorations in named sets.
#[derive(Debug, Clone, Default)]
pub struct Decorations {
    sets: Vec<Set>,
    /// Bumped by every change.
    revision: u64,
}

impl Decorations {
    /// Show `decorations` as set `name`, in place of what it showed.
    pub fn set(&mut self, name: &str, decorations: Vec<Decoration>) {
        let mut new = Set {
            name: name.to_owned(),
            list: decorations,
            longest: 0,
        };
        new.sort();
        match self.sets.iter_mut().find(|s| s.name == name) {
            Some(set) => {
                if set.list == new.list {
                    return;
                }
                *set = new;
            }
            None if new.list.is_empty() => return,
            None => self.sets.push(new),
        }
        self.revision += 1;
    }

    /// Clears set `name`.
    pub fn clear(&mut self, name: &str) {
        self.set(name, Vec::new());
    }

    /// Set `name`'s decorations.
    pub fn get(&self, name: &str) -> &[Decoration] {
        self.sets
            .iter()
            .find(|s| s.name == name)
            .map_or(&[], |s| &s.list)
    }

    /// Bumped by every change.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Whether there are none.
    pub fn is_empty(&self) -> bool {
        self.sets.iter().all(|s| s.list.is_empty())
    }

    /// The decorations over any of bytes `range`, or empty ones in it, set
    /// by set.
    pub fn in_range(&self, range: Range<usize>) -> impl Iterator<Item = &Decoration> + '_ {
        self.sets.iter().flat_map(move |set| {
            let from = range.start.saturating_sub(set.longest);
            let first = set.list.partition_point(|d| d.range.start < from);
            set.list[first..]
                .iter()
                .take_while(move |d| d.range.start <= range.end)
                .filter(move |d| {
                    if d.range.is_empty() {
                        range.start <= d.range.start
                    } else {
                        d.range.end > range.start && d.range.start < range.end.max(range.start + 1)
                    }
                })
        })
    }

    /// The most serious diagnostic over any of bytes `range`.
    pub fn worst_in(&self, range: Range<usize>) -> Option<Severity> {
        self.in_range(range)
            .filter_map(|d| match d.style {
                DecorationStyle::Diagnostic(s) => Some(s),
                _ => None,
            })
            .max()
    }

    /// Follows an edit: ranges move with their text, one removed whole
    /// goes.
    pub fn map(&mut self, changes: &ChangeSet) {
        let mut any = false;
        for set in &mut self.sets {
            if set.list.is_empty() {
                continue;
            }
            any = true;
            set.list.retain_mut(|d| {
                let was_empty = d.range.is_empty();
                let start = changes.map(d.range.start, Assoc::After);
                let end = changes.map(d.range.end, Assoc::Before).max(start);
                d.range = start..end;
                was_empty || !d.range.is_empty()
            });
            set.sort();
        }
        if any {
            self.revision += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Change;

    #[test]
    fn sets_replace_their_own_and_ranges_follow_edits() {
        let mut d = Decorations::default();
        d.set(
            DIAGNOSTICS,
            vec![
                Decoration::diagnostic(10..14, Severity::Error, "bad"),
                Decoration::diagnostic(2..4, Severity::Warning, "meh"),
            ],
        );
        d.set(
            HIGHLIGHTS,
            vec![Decoration {
                range: 0..30,
                style: DecorationStyle::Highlight,
                message: None,
            }],
        );
        assert_eq!(d.in_range(3..5).count(), 2);
        assert_eq!(d.worst_in(0..20), Some(Severity::Error));
        assert_eq!(d.worst_in(5..9), None);
        d.map(&ChangeSet::new(vec![
            Change::insert(0, "xx"),
            Change::delete(10..14),
        ]));
        assert_eq!(d.get(DIAGNOSTICS).len(), 1);
        assert_eq!(d.get(DIAGNOSTICS)[0].range, 4..6);
        assert_eq!(d.get(HIGHLIGHTS)[0].range, 2..28);
        d.clear(HIGHLIGHTS);
        assert!(d.get(HIGHLIGHTS).is_empty());
    }
}
