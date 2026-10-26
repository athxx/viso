//! Cursors: each a selection from an anchor to a head, the head where the
//! caret is. Kept sorted and apart, one of them primary: the one the view
//! follows and a single-cursor command acts on.

use std::ops::Range;

use crate::document::{Assoc, ChangeSet};

/// A selection from `anchor` to `head`, a caret when they meet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    pub anchor: usize,
    pub head: usize,
    /// The cell a run of up and down moves keeps to, past shorter lines.
    pub goal: Option<u32>,
}

impl Selection {
    /// A caret at `at`.
    pub fn caret(at: usize) -> Selection {
        Selection {
            anchor: at,
            head: at,
            goal: None,
        }
    }

    /// A selection from `anchor` to `head`.
    pub fn new(anchor: usize, head: usize) -> Selection {
        Selection {
            anchor,
            head,
            goal: None,
        }
    }

    /// Where it starts.
    #[inline]
    pub fn start(&self) -> usize {
        self.anchor.min(self.head)
    }

    /// Where it ends.
    #[inline]
    pub fn end(&self) -> usize {
        self.anchor.max(self.head)
    }

    /// The bytes it selects.
    #[inline]
    pub fn range(&self) -> Range<usize> {
        self.start()..self.end()
    }

    /// Whether it selects nothing: a caret.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.anchor == self.head
    }

    /// Whether its head is before its anchor.
    #[inline]
    pub fn is_backward(&self) -> bool {
        self.head < self.anchor
    }

    /// It with its head at `head`, its anchor too unless `extend`.
    pub fn moved(self, head: usize, extend: bool) -> Selection {
        Selection {
            anchor: if extend { self.anchor } else { head },
            head,
            goal: None,
        }
    }

    /// Where it is after `changes`. A caret stays after text inserted at
    /// it; a selection keeps text inserted at either edge out.
    pub fn map(self, changes: &ChangeSet) -> Selection {
        if self.is_empty() {
            let at = changes.map(self.head, Assoc::After);
            return Selection::caret(at);
        }
        let (start, end) = (
            changes.map(self.start(), Assoc::After),
            changes.map(self.end(), Assoc::Before),
        );
        let end = end.max(start);
        if self.is_backward() {
            Selection::new(end, start)
        } else {
            Selection::new(start, end)
        }
    }
}

/// The cursors: sorted, apart, never none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selections {
    list: Vec<Selection>,
    primary: usize,
}

impl Default for Selections {
    fn default() -> Self {
        Selections::one(Selection::caret(0))
    }
}

impl Selections {
    /// One cursor.
    pub fn one(selection: Selection) -> Selections {
        Selections {
            list: vec![selection],
            primary: 0,
        }
    }

    /// The cursors `list`, the one at `primary` primary, in any order and
    /// overlapping as they may: sorted and merged. An empty list is one
    /// caret at the start.
    pub fn new(list: Vec<Selection>, primary: usize) -> Selections {
        if list.is_empty() {
            return Selections::default();
        }
        let primary = primary.min(list.len() - 1);
        let mut s = Selections { list, primary };
        s.normalize();
        s
    }

    /// The cursors, in order.
    #[inline]
    pub fn list(&self) -> &[Selection] {
        &self.list
    }

    /// How many there are.
    #[inline]
    pub fn len(&self) -> usize {
        self.list.len()
    }

    /// Never: there is always one.
    #[inline]
    pub fn is_empty(&self) -> bool {
        false
    }

    /// The primary cursor's index.
    #[inline]
    pub fn primary_index(&self) -> usize {
        self.primary
    }

    /// The primary cursor.
    #[inline]
    pub fn primary(&self) -> Selection {
        self.list[self.primary]
    }

    /// Just the primary cursor.
    pub fn only_primary(&self) -> Selections {
        Selections::one(self.primary())
    }

    /// Each cursor made by `f`, merged again.
    pub fn map_each(&self, mut f: impl FnMut(Selection) -> Selection) -> Selections {
        let list = self.list.iter().map(|&s| f(s)).collect();
        Selections::new(list, self.primary)
    }

    /// Where the cursors are after `changes`.
    pub fn map(&self, changes: &ChangeSet) -> Selections {
        self.map_each(|s| s.map(changes))
    }

    /// The cursors with `selection` added, primary.
    pub fn with(&self, selection: Selection) -> Selections {
        let mut list = self.list.clone();
        list.push(selection);
        let primary = list.len() - 1;
        Selections::new(list, primary)
    }

    /// The cursors without the one at `index`, unless it is the only one.
    pub fn without(&self, index: usize) -> Selections {
        if self.list.len() == 1 || index >= self.list.len() {
            return self.clone();
        }
        let mut list = self.list.clone();
        list.remove(index);
        let primary = if self.primary > index {
            self.primary - 1
        } else {
            self.primary.min(list.len() - 1)
        };
        Selections { list, primary }
    }

    /// Whether every cursor is a caret.
    pub fn all_carets(&self) -> bool {
        self.list.iter().all(Selection::is_empty)
    }

    /// Clamps every cursor into a text `len` bytes long.
    pub fn clamp(&self, len: usize) -> Selections {
        self.map_each(|s| Selection {
            anchor: s.anchor.min(len),
            head: s.head.min(len),
            goal: s.goal,
        })
    }

    /// Sorts and merges: cursors that overlap, or carets at one place or at
    /// a selection's edge, become one, primary if any of them was.
    fn normalize(&mut self) {
        if self.list.len() == 1 {
            self.primary = 0;
            return;
        }
        let mut marked: Vec<(Selection, bool)> = self
            .list
            .iter()
            .enumerate()
            .map(|(i, &s)| (s, i == self.primary))
            .collect();
        marked.sort_by_key(|(s, _)| (s.start(), s.end()));
        self.list.clear();
        self.primary = 0;
        for (s, is_primary) in marked {
            match self.list.last_mut() {
                Some(last)
                    if s.start() < last.end()
                        || (s.start() == last.end() && (s.is_empty() || last.is_empty())) =>
                {
                    let (start, end) = (last.start(), last.end().max(s.end()));
                    let backward = if is_primary {
                        s.is_backward()
                    } else {
                        last.is_backward()
                    };
                    let goal = if is_primary { s.goal } else { last.goal };
                    *last = if backward {
                        Selection::new(end, start)
                    } else {
                        Selection::new(start, end)
                    };
                    if last.is_empty() {
                        last.goal = goal;
                    }
                }
                _ => self.list.push(s),
            }
            if is_primary {
                self.primary = self.list.len() - 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Change;

    #[test]
    fn overlapping_cursors_merge_and_keep_the_primary() {
        let s = Selections::new(
            vec![
                Selection::new(8, 10),
                Selection::caret(3),
                Selection::new(2, 5),
                Selection::caret(10),
                Selection::caret(20),
            ],
            1,
        );
        assert_eq!(
            s.list(),
            [
                Selection::new(2, 5),
                Selection::new(8, 10),
                Selection::caret(20)
            ]
        );
        assert_eq!(s.primary_index(), 0);
        let s = Selections::new(vec![Selection::caret(4), Selection::caret(9)], 1);
        assert_eq!(s.primary(), Selection::caret(9));
        let s = s.with(Selection::caret(1));
        assert_eq!(s.primary(), Selection::caret(1));
        assert_eq!(s.primary_index(), 0);
        let s = s.without(0);
        assert_eq!(s.list(), [Selection::caret(4), Selection::caret(9)]);
    }

    #[test]
    fn carets_move_past_their_own_inserts() {
        let s = Selections::new(vec![Selection::caret(1), Selection::caret(4)], 0);
        let cs = ChangeSet::new(vec![Change::insert(1, "ab"), Change::insert(4, "ab")]);
        assert_eq!(
            s.map(&cs).list(),
            [Selection::caret(3), Selection::caret(8)]
        );
        let sel = Selection::new(6, 2);
        let cs = ChangeSet::new(vec![Change::insert(2, "x"), Change::insert(6, "y")]);
        assert_eq!(sel.map(&cs), Selection::new(7, 3));
    }
}
