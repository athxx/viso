//! A text field's undo history: the steps an undo takes back, each the
//! replacements that restore the text and the selection it had before.
//!
//! An edit is kept as its inverse — the range it produced and the text it
//! replaced — so a step costs the text it removed, not a copy of the field.
//! Edits of one kind in a row join one step (a typing run, a run of
//! Backspaces, a run of forward Deletes), and an inverse that continues the
//! one before it is folded into it, so typing a word keeps one replacement
//! however many keys it took. A typing run breaks where a word starts after
//! whitespace, so an undo takes back a word at a time. Anything else — a
//! paste, a cut, a word or line deletion, an IME composition, a change from
//! outside — is a step of its own.
//!
//! The history is bounded: past [`HISTORY_BYTES`] of kept text or
//! [`HISTORY_STEPS`] steps the oldest steps go.

use std::collections::VecDeque;

use viso_text::selection::Selection;

/// The text an undo history keeps at most, over every step it holds.
pub const HISTORY_BYTES: usize = 256 * 1024;

/// The steps an undo history keeps at most.
pub const HISTORY_STEPS: usize = 1000;

/// What a replacement costs beyond its text.
const SPLICE_COST: usize = std::mem::size_of::<Splice>();

/// How an edit joins the step before it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EditKind {
    /// Typed text.
    Typing,
    /// A grapheme deleted backward.
    Backspace,
    /// A grapheme deleted forward.
    Delete,
    /// A number stepped or scrubbed.
    Step,
    /// Anything else: a step of its own.
    Other,
}

/// `[start, end)` of a text replaced by `text`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Splice {
    pub(crate) start: usize,
    pub(crate) end: usize,
    pub(crate) text: String,
}

impl Splice {
    /// Applies the replacement to `text` and returns the one that undoes it.
    pub(crate) fn apply(&self, text: &mut String) -> Splice {
        let removed = text[self.start..self.end].to_owned();
        text.replace_range(self.start..self.end, &self.text);
        Splice {
            start: self.start,
            end: self.start + self.text.len(),
            text: removed,
        }
    }

    /// Whether the replacement leaves any text as it was.
    fn is_noop(&self) -> bool {
        self.start == self.end && self.text.is_empty()
    }

    /// Folds `next` — the inverse of the edit made right after the one
    /// `self` undoes — into `self`, when one replacement undoes both.
    fn absorb(&mut self, next: &Splice) -> bool {
        if next.start == self.end && next.text.is_empty() {
            // Text inserted right after what the edit inserted.
            self.end = next.end;
        } else if next.start == self.start && next.text.len() == self.end - self.start {
            // Exactly what the edit inserted, replaced again.
            self.end = next.end;
        } else if next.start == next.end
            && self.start == self.end
            && next.start + next.text.len() == self.start
        {
            // Text deleted right before what the edit deleted.
            self.start = next.start;
            self.end = next.start;
            self.text.insert_str(0, &next.text);
        } else if next.start == next.end && self.start == self.end && next.start == self.start {
            // Text deleted right after what the edit deleted.
            self.text.push_str(&next.text);
        } else {
            return false;
        }
        true
    }
}

/// What an undo takes back: the replacements that restore the text, applied
/// last first, and the selection the text had before.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Step {
    pub(crate) before: Selection,
    pub(crate) undo: Vec<Splice>,
}

impl Step {
    /// A step that starts at `before`, its replacements still to come.
    pub(crate) fn new(before: Selection) -> Step {
        Step {
            before,
            undo: Vec::new(),
        }
    }

    /// Adds the inverse of an edit made after the step's others.
    pub(crate) fn push(&mut self, inverse: Splice) {
        if !self
            .undo
            .last_mut()
            .is_some_and(|last| last.absorb(&inverse))
        {
            self.undo.push(inverse);
        }
    }

    /// Whether undoing the step changes nothing.
    pub(crate) fn is_noop(&self) -> bool {
        self.undo.iter().all(Splice::is_noop)
    }

    fn cost(&self) -> usize {
        self.undo.iter().map(|s| s.text.len() + SPLICE_COST).sum()
    }

    /// Applies the step to `text` and returns the one that takes it back,
    /// starting at `now`.
    fn apply(&self, text: &mut String, now: Selection) -> Step {
        let mut back = Step {
            before: now,
            undo: Vec::with_capacity(self.undo.len()),
        };
        for splice in self.undo.iter().rev() {
            back.undo.push(splice.apply(text));
        }
        back
    }
}

/// A field's undo and redo steps.
#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct History {
    done: VecDeque<Step>,
    undone: Vec<Step>,
    /// The kind of edit the last step takes more of; `None` when the next
    /// edit starts a step.
    open: Option<EditKind>,
    /// Whether the last typed text was whitespace.
    after_space: bool,
    /// The text and replacements held, over both stacks.
    bytes: usize,
}

impl History {
    /// Records an edit of `kind` made from selection `before`, as the
    /// replacement that undoes it. `space` is whether typed text was all
    /// whitespace.
    pub(crate) fn record(
        &mut self,
        kind: EditKind,
        before: Selection,
        inverse: Splice,
        space: bool,
    ) {
        self.drop_undone();
        let joins = kind != EditKind::Other
            && self.open == Some(kind)
            && !(kind == EditKind::Typing && self.after_space && !space);
        match self.done.back_mut() {
            Some(step) if joins => {
                self.bytes -= step.cost();
                step.push(inverse);
                self.bytes += step.cost();
            }
            _ => {
                let mut step = Step::new(before);
                step.push(inverse);
                self.push_done(step);
            }
        }
        self.open = (kind != EditKind::Other).then_some(kind);
        self.after_space = space;
    }

    /// Records `step` whole, as one undo of its own.
    pub(crate) fn record_step(&mut self, step: Step) {
        self.close();
        if step.is_noop() {
            return;
        }
        self.drop_undone();
        self.push_done(step);
    }

    /// Ends the open step: the next edit starts one.
    pub(crate) fn close(&mut self) {
        self.open = None;
    }

    /// Forgets every step.
    pub(crate) fn clear(&mut self) {
        *self = History::default();
    }

    /// Takes back the last step from `text` at selection `now`, returning
    /// the selection it had before; `None` when there is nothing to undo.
    pub(crate) fn undo(&mut self, text: &mut String, now: Selection) -> Option<Selection> {
        let step = self.done.pop_back()?;
        self.bytes -= step.cost();
        let redo = step.apply(text, now);
        self.bytes += redo.cost();
        self.undone.push(redo);
        self.close();
        Some(step.before)
    }

    /// Makes the last undone step again, returning the selection it left.
    pub(crate) fn redo(&mut self, text: &mut String, now: Selection) -> Option<Selection> {
        let step = self.undone.pop()?;
        self.bytes -= step.cost();
        let undo = step.apply(text, now);
        self.push_done(undo);
        self.close();
        Some(step.before)
    }

    fn push_done(&mut self, step: Step) {
        self.bytes += step.cost();
        self.done.push_back(step);
        while self.done.len() > 1 && (self.bytes > HISTORY_BYTES || self.done.len() > HISTORY_STEPS)
        {
            if let Some(oldest) = self.done.pop_front() {
                self.bytes -= oldest.cost();
            }
        }
    }

    fn drop_undone(&mut self) {
        for step in self.undone.drain(..) {
            self.bytes -= step.cost();
        }
    }

    #[cfg(test)]
    pub(crate) fn bytes(&self) -> usize {
        self.bytes
    }

    #[cfg(test)]
    pub(crate) fn steps(&self) -> usize {
        self.done.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use viso_text::{TextOffset, TextPosition};

    fn caret(at: usize) -> Selection {
        Selection::caret(TextPosition::downstream(TextOffset(at)))
    }

    /// Replaces `[start, end)` of `text` with `s` and records it.
    fn edit(h: &mut History, text: &mut String, kind: EditKind, start: usize, end: usize, s: &str) {
        let inverse = Splice {
            start,
            end,
            text: s.to_owned(),
        }
        .apply(text);
        let space = !s.is_empty() && s.chars().all(char::is_whitespace);
        h.record(kind, caret(end), inverse, space);
    }

    #[test]
    fn a_typed_word_is_one_replacement_and_a_word_after_a_space_is_a_step_of_its_own() {
        let (mut h, mut text) = (History::default(), String::new());
        for (i, c) in "ab cd".char_indices() {
            edit(&mut h, &mut text, EditKind::Typing, i, i, &c.to_string());
        }
        assert_eq!(h.steps(), 2);
        assert_eq!(h.done[0].undo.len(), 1, "`ab ` folds into one replacement");
        assert_eq!(h.undo(&mut text, caret(5)), Some(caret(3)));
        assert_eq!(text, "ab ");
        h.undo(&mut text, caret(3));
        assert_eq!(text, "");
        assert_eq!(h.redo(&mut text, caret(0)), Some(caret(3)));
        assert_eq!(text, "ab ");
        h.redo(&mut text, caret(3));
        assert_eq!(text, "ab cd");
    }

    #[test]
    fn backspaces_and_deletes_in_a_row_fold_and_kinds_do_not_mix() {
        let (mut h, mut text) = (History::default(), "abcdef".to_owned());
        edit(&mut h, &mut text, EditKind::Backspace, 3, 4, "");
        edit(&mut h, &mut text, EditKind::Backspace, 2, 3, "");
        assert_eq!(text, "abef");
        edit(&mut h, &mut text, EditKind::Delete, 2, 3, "");
        edit(&mut h, &mut text, EditKind::Delete, 2, 3, "");
        assert_eq!(text, "ab");
        assert_eq!(h.steps(), 2);
        assert!(h.done.iter().all(|s| s.undo.len() == 1));
        h.undo(&mut text, caret(2));
        assert_eq!(text, "abef");
        h.undo(&mut text, caret(2));
        assert_eq!(text, "abcdef");
    }

    #[test]
    fn an_edit_after_an_undo_drops_the_redo() {
        let (mut h, mut text) = (History::default(), String::new());
        edit(&mut h, &mut text, EditKind::Other, 0, 0, "x");
        h.undo(&mut text, caret(1));
        edit(&mut h, &mut text, EditKind::Other, 0, 0, "y");
        assert_eq!(h.redo(&mut text, caret(1)), None);
        assert_eq!(text, "y");
    }

    #[test]
    fn a_replacement_typed_over_again_folds_into_the_first() {
        // A composition: its marked text replaced on every update.
        let mut text = "ab".to_owned();
        let mut step = Step::new(caret(0));
        for (old, s) in [(2, "ㅎ"), (3, "하"), (3, "한")] {
            step.push(
                Splice {
                    start: 0,
                    end: old,
                    text: s.to_owned(),
                }
                .apply(&mut text),
            );
        }
        assert_eq!(text, "한");
        assert_eq!(step.undo.len(), 1);
        let mut h = History::default();
        h.record_step(step);
        h.undo(&mut text, caret(3));
        assert_eq!(text, "ab");
    }

    #[test]
    fn the_oldest_steps_go_past_the_budget() {
        let (mut h, mut text) = (History::default(), String::new());
        let chunk = "x".repeat(HISTORY_BYTES / 4);
        for _ in 0..8 {
            let at = text.len();
            edit(&mut h, &mut text, EditKind::Other, 0, at, &chunk);
        }
        assert!(h.bytes() <= HISTORY_BYTES);
        assert!(h.steps() < 8);
        for _ in 0..HISTORY_STEPS + 10 {
            edit(&mut h, &mut text, EditKind::Other, 0, 0, "");
        }
        assert!(h.steps() <= HISTORY_STEPS);
    }
}
