//! The undo history: the steps an undo takes back, each the edits that
//! restore the text and the cursors it had before.
//!
//! An edit is kept as its inverse — the ranges it produced and the text it
//! replaced — so a step costs the text it removed, not a copy of the
//! document. Edits of one kind in a row join one step (a typing run, a run of
//! Backspaces, a run of forward Deletes) when the cursors did not move in
//! between, and an inverse that continues the one before it at every cursor
//! is folded into it, so typing a word with five cursors keeps one change per
//! cursor however many keys it took. A typing run breaks where a word starts
//! after whitespace, so an undo takes back a word at a time. Anything else —
//! a paste, a cut, a line moved, an input method's composition, an edit from
//! a language server — is a step of its own.
//!
//! The history is bounded: past [`HISTORY_BYTES`] of kept text or
//! [`HISTORY_STEPS`] steps the oldest steps go.

use std::collections::VecDeque;

use crate::document::{Assoc, Change, ChangeSet};
use crate::selection::Selections;

/// The text a history keeps at most, over every step it holds.
pub const HISTORY_BYTES: usize = 8 * 1024 * 1024;

/// The steps a history keeps at most.
pub const HISTORY_STEPS: usize = 10_000;

/// How an edit joins the step before it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditKind {
    /// Typed text.
    Typing,
    /// Text deleted backward.
    Backspace,
    /// Text deleted forward.
    Delete,
    /// Anything else: a step of its own.
    Other,
}

/// One undoable step.
#[derive(Debug, Clone)]
struct Step {
    /// The edits that undo it, in the order they were made: an undo applies
    /// them last first.
    edits: Vec<ChangeSet>,
    /// The cursors before it.
    before: Selections,
    /// The cursors after it.
    after: Selections,
    kind: EditKind,
    /// Whether another edit may still join it.
    open: bool,
}

impl Step {
    fn cost(&self) -> usize {
        self.edits.iter().map(ChangeSet::cost).sum::<usize>()
            + (self.before.len() + self.after.len()) * std::mem::size_of::<crate::Selection>()
            + std::mem::size_of::<Step>()
    }
}

/// What an undo or a redo hands back to apply.
#[derive(Debug)]
pub struct Replay {
    /// The edits, in the order to apply them, each against the text the one
    /// before left.
    pub edits: Vec<ChangeSet>,
    /// The cursors to show after.
    pub selections: Selections,
}

/// An undo history.
#[derive(Debug, Default)]
pub struct History {
    done: VecDeque<Step>,
    undone: Vec<Step>,
    bytes: usize,
}

impl History {
    /// An empty history.
    pub fn new() -> History {
        History::default()
    }

    /// Records an edit of `kind` made from cursors `before` to cursors
    /// `after`, undone by `inverse`; `forward` is the edit itself. `breaks`
    /// starts a step whatever the kind: a word typed after whitespace.
    pub fn record(
        &mut self,
        kind: EditKind,
        forward: &ChangeSet,
        inverse: ChangeSet,
        before: &Selections,
        after: &Selections,
        breaks: bool,
    ) {
        if inverse.is_empty() {
            return;
        }
        self.drop_undone();
        if let Some(last) = self.done.back_mut()
            && last.open
            && kind != EditKind::Other
            && last.kind == kind
            && !breaks
            && last.after == *before
        {
            let old = last.cost();
            let prev = last.edits.last_mut().expect("a step has edits");
            match fold(prev, forward, &inverse, kind) {
                Some(folded) => *prev = folded,
                None => last.edits.push(inverse),
            }
            last.after = after.clone();
            let new = last.cost();
            self.bytes = self.bytes + new - old;
            self.trim();
            return;
        }
        self.close();
        let step = Step {
            edits: vec![inverse],
            before: before.clone(),
            after: after.clone(),
            kind,
            open: kind != EditKind::Other,
        };
        self.bytes += step.cost();
        self.done.push_back(step);
        self.trim();
    }

    /// Ends the open step: the next edit starts its own.
    pub fn close(&mut self) {
        if let Some(last) = self.done.back_mut() {
            last.open = false;
        }
    }

    /// Forgets every step.
    pub fn clear(&mut self) {
        self.done.clear();
        self.undone.clear();
        self.bytes = 0;
    }

    /// Whether there is a step to undo.
    pub fn can_undo(&self) -> bool {
        !self.done.is_empty()
    }

    /// Whether there is a step to redo.
    pub fn can_redo(&self) -> bool {
        !self.undone.is_empty()
    }

    /// Takes the last step to undo; hand [`History::undone`] each applied
    /// edit's inverse, in order, to make it redoable.
    pub fn undo(&mut self) -> Option<Replay> {
        let mut step = self.done.pop_back()?;
        self.bytes -= step.cost();
        let edits: Vec<ChangeSet> = std::mem::take(&mut step.edits).into_iter().rev().collect();
        let selections = step.before.clone();
        step.open = false;
        self.undone.push(step);
        Some(Replay { edits, selections })
    }

    /// Takes the last undone step to redo; hand [`History::redone`] each
    /// applied edit's inverse, in order, to make it undoable again.
    pub fn redo(&mut self) -> Option<Replay> {
        let mut step = self.undone.pop()?;
        let edits: Vec<ChangeSet> = std::mem::take(&mut step.edits).into_iter().rev().collect();
        let selections = step.after.clone();
        self.done.push_back(step);
        Some(Replay { edits, selections })
    }

    /// The inverses of the edits an undo applied, in the order applied:
    /// what redoes it.
    pub fn undone(&mut self, inverses: Vec<ChangeSet>) {
        if let Some(step) = self.undone.last_mut() {
            step.edits = inverses;
        }
    }

    /// The inverses of the edits a redo applied, in the order applied: what
    /// undoes it again.
    pub fn redone(&mut self, inverses: Vec<ChangeSet>) {
        if let Some(step) = self.done.back_mut() {
            step.edits = inverses;
            self.bytes += step.cost();
        }
        self.trim();
    }

    /// The text and bookkeeping it keeps, in bytes.
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// The steps it can undo.
    pub fn steps(&self) -> usize {
        self.done.len()
    }

    fn drop_undone(&mut self) {
        self.undone.clear();
    }

    fn trim(&mut self) {
        while self.done.len() > 1 && (self.bytes > HISTORY_BYTES || self.done.len() > HISTORY_STEPS)
        {
            if let Some(step) = self.done.pop_front() {
                self.bytes -= step.cost();
            }
        }
    }
}

/// `prev`, the inverse of an edit, and `next`, the inverse of the edit
/// `forward` made right after it, as one inverse, when every cursor's change
/// continues its last one.
fn fold(
    prev: &ChangeSet,
    forward: &ChangeSet,
    next: &ChangeSet,
    kind: EditKind,
) -> Option<ChangeSet> {
    let (a, b) = (prev.changes(), next.changes());
    if a.len() != b.len() {
        return None;
    }
    let mut out = Vec::with_capacity(a.len());
    for (a, b) in a.iter().zip(b) {
        // `a` in the offsets after `forward`.
        let start = forward.map(a.range.start, Assoc::Before);
        let end = forward.map(a.range.end, Assoc::Before).max(start);
        let change = match kind {
            // Typed right after the text typed before.
            EditKind::Typing if b.text.is_empty() && b.range.start == end => {
                Change::replace(start..b.range.end, a.text.clone())
            }
            // Deleted right before what was deleted before.
            EditKind::Backspace
                if a.range.is_empty() && b.range.is_empty() && b.range.start == start =>
            {
                Change::insert(start, format!("{}{}", b.text, a.text))
            }
            // Deleted right after it.
            EditKind::Delete
                if a.range.is_empty() && b.range.is_empty() && b.range.start == start =>
            {
                Change::insert(start, format!("{}{}", a.text, b.text))
            }
            _ => return None,
        };
        out.push(change);
    }
    Some(ChangeSet::new(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Document;
    use crate::selection::Selection;

    struct Model {
        doc: Document,
        sel: Selections,
        history: History,
    }

    impl Model {
        fn new(text: &str, carets: &[usize]) -> Model {
            Model {
                doc: Document::new(text),
                sel: Selections::new(carets.iter().map(|&c| Selection::caret(c)).collect(), 0),
                history: History::new(),
            }
        }

        fn edit(&mut self, kind: EditKind, changes: Vec<Change>) {
            let cs = ChangeSet::new(changes);
            let applied = self.doc.apply(&cs);
            let after = self.sel.map(&cs);
            self.history
                .record(kind, &cs, applied.inverse, &self.sel, &after, false);
            self.sel = after;
        }

        fn type_at_carets(&mut self, text: &str) {
            let changes = self
                .sel
                .list()
                .iter()
                .map(|s| Change::insert(s.head, text))
                .collect();
            self.edit(EditKind::Typing, changes);
        }

        fn backspace(&mut self) {
            let changes = self
                .sel
                .list()
                .iter()
                .map(|s| Change::delete(s.head - 1..s.head))
                .collect();
            self.edit(EditKind::Backspace, changes);
        }

        fn undo(&mut self) -> bool {
            let Some(replay) = self.history.undo() else {
                return false;
            };
            let inverses = replay
                .edits
                .iter()
                .map(|e| self.doc.apply(e).inverse)
                .collect();
            self.history.undone(inverses);
            self.sel = replay.selections;
            true
        }

        fn redo(&mut self) -> bool {
            let Some(replay) = self.history.redo() else {
                return false;
            };
            let inverses = replay
                .edits
                .iter()
                .map(|e| self.doc.apply(e).inverse)
                .collect();
            self.history.redone(inverses);
            self.sel = replay.selections;
            true
        }
    }

    #[test]
    fn typing_at_many_carets_folds_into_one_change_each() {
        let mut m = Model::new("a\nb\nc", &[1, 3, 5]);
        for ch in ["x", "y", "z"] {
            m.type_at_carets(ch);
        }
        assert_eq!(m.doc.to_text(), "axyz\nbxyz\ncxyz");
        assert_eq!(m.history.steps(), 1);
        assert_eq!(m.history.done[0].edits.len(), 1);
        assert_eq!(m.history.done[0].edits[0].changes().len(), 3);
        assert!(m.undo());
        assert_eq!(m.doc.to_text(), "a\nb\nc");
        assert_eq!(
            m.sel.list(),
            [
                Selection::caret(1),
                Selection::caret(3),
                Selection::caret(5)
            ]
        );
        assert!(m.redo());
        assert_eq!(m.doc.to_text(), "axyz\nbxyz\ncxyz");
        assert_eq!(m.sel.list()[2], Selection::caret(14));
        assert!(m.undo());
        assert_eq!(m.doc.to_text(), "a\nb\nc");
    }

    #[test]
    fn backspaces_fold_and_kinds_do_not_mix() {
        let mut m = Model::new("hello world", &[11]);
        for _ in 0..3 {
            m.backspace();
        }
        assert_eq!(m.doc.to_text(), "hello wo");
        assert_eq!(m.history.steps(), 1);
        m.type_at_carets("!");
        assert_eq!(m.history.steps(), 2);
        assert!(m.undo());
        assert!(m.undo());
        assert_eq!(m.doc.to_text(), "hello world");
        assert!(!m.undo());
        assert!(m.redo());
        assert!(m.redo());
        assert_eq!(m.doc.to_text(), "hello wo!");
    }

    #[test]
    fn an_edit_after_an_undo_drops_the_redo() {
        let mut m = Model::new("", &[0]);
        m.type_at_carets("a");
        m.history.close();
        m.type_at_carets("b");
        assert!(m.undo());
        m.type_at_carets("c");
        assert!(!m.redo());
        assert_eq!(m.doc.to_text(), "ac");
    }

    #[test]
    fn a_moved_cursor_starts_a_step() {
        let mut m = Model::new("ab", &[1]);
        m.type_at_carets("x");
        m.sel = Selections::one(Selection::caret(0));
        m.type_at_carets("y");
        assert_eq!(m.history.steps(), 2);
    }
}
