//! Single-line text editing: the retained edit buffer, the deferred edit
//! intents an input handler records, and the frame pass that folds queued
//! intents into each buffer and re-declares the node's text.
//!
//! ## Why a driver-owned registry, not node columns
//!
//! An editable node's buffer (its text, selection, and IME composition range) is
//! heap-heavy and touched only when that one node is edited — never in the hot
//! per-node traversal. So it lives in a driver-owned registry keyed by the
//! node's [`NodeId::index`], a dense `Vec<Option<Box<Buffer>>>`, exactly as
//! [`crate::virtual_list::VirtualLists`] holds list state beside the store rather
//! than woven into its SoA columns. The hot-path contract's "0 global HashMap
//! lookup per node" and "no cold Strings in per-frame traversal" both hold.
//!
//! ## Why intents are self-contained
//!
//! An [`crate::context::EventCx`] handler holds `states` and `bindings` but no
//! node store and no registry — a key/IME handler cannot reach the buffer to
//! mutate it in place (and must not: the buffer is retained state the reconcile
//! pass owns). So a handler records a self-contained [`EditIntent`] — insert
//! this string, delete backward, move the caret left — that needs no buffer read
//! at record time. The router drains the recorded intent (exactly as it drains a
//! deferred focus request) into the driver-owned [`TextEdits`] registry, keyed by
//! the node whose handler ran. [`reconcile`] then applies the queued intents to
//! that node's buffer in the layout phase.
//!
//! ## Why reconcile runs in the layout phase
//!
//! [`reconcile`] runs once per frame *before* text shaping. Applying an intent
//! that changes the buffer text re-declares the node's [`TextRequest`] via
//! [`NodeStore::set_text_request`]; the same frame's shaping step drains that
//! request, shapes it, and writes the [`Content`](crate::content::Content) back,
//! so an edit typed this frame is measured, laid out, and painted this frame. A
//! buffer whose queue is empty is skipped — the steady path touches nothing.
//!
//! ## Grapheme awareness
//!
//! Caret motion and deletion step by whole `char`s through [`prev_boundary`] and
//! [`next_boundary`]. `viso-text` is single-face with no cluster shaping, so a
//! multi-`char` grapheme has no single glyph to land a caret between anyway;
//! char boundaries are a subset of grapheme boundaries, so the [`Selection`] and
//! edit-op contracts stay unchanged when those two functions are later upgraded
//! to grapheme stepping.

use crate::content::TextRequest;
use crate::node::NodeId;

/// A caret or range selection over a buffer, as byte offsets into its text.
///
/// The caret is `cursor`; `anchor` is where a range selection was started. They
/// are equal for a bare caret. [`start`](Selection::start) and
/// [`end`](Selection::end) return them in text order regardless of drag
/// direction, which is what the edit ops act on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Selection {
    /// The moving end — where the caret is and where typing inserts.
    pub cursor: usize,
    /// The fixed end — equal to `cursor` for a bare caret, the drag origin for a
    /// range.
    pub anchor: usize,
}

impl Selection {
    /// A collapsed caret at byte offset `at`.
    #[inline]
    pub fn caret(at: usize) -> Self {
        Self {
            cursor: at,
            anchor: at,
        }
    }

    /// The earlier of the two ends, in text order.
    #[inline]
    pub fn start(&self) -> usize {
        self.cursor.min(self.anchor)
    }

    /// The later of the two ends, in text order.
    #[inline]
    pub fn end(&self) -> usize {
        self.cursor.max(self.anchor)
    }

    /// Whether the selection is a bare caret (no characters selected).
    #[inline]
    pub fn is_caret(&self) -> bool {
        self.cursor == self.anchor
    }
}

/// Which end of the text a caret motion targets, for arrow / Home / End.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Motion {
    /// One `char` toward the start of the text.
    Left,
    /// One `char` toward the end of the text.
    Right,
    /// The start of the line (single line: byte 0).
    Home,
    /// The end of the line (single line: the text length in bytes).
    End,
}

/// A self-contained editing command an input handler records without reading the
/// buffer. [`reconcile`] applies it to the node's [`Buffer`] in the layout phase.
///
/// Every variant is expressed against "the current selection" so it needs no
/// buffer snapshot at record time — the buffer state is only read when the
/// intent is applied, keeping the record path (in a handler that has no store)
/// pure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditIntent {
    /// Replace the current selection with this text (an ordinary keystroke or an
    /// IME commit). Collapses to a caret after the inserted text.
    Insert(String),
    /// Delete backward: the selection if any, else the `char` before the caret.
    Backspace,
    /// Delete forward: the selection if any, else the `char` after the caret.
    Delete,
    /// Move the caret. `extend` keeps the anchor fixed (shift-select) instead of
    /// collapsing to a caret.
    Move { motion: Motion, extend: bool },
    /// Set or update the IME composition: replace the composition range (or the
    /// selection, if no composition is active yet) with `text`, keeping it marked
    /// as composing with the caret at `caret` bytes into `text`. An empty `text`
    /// cancels the composition.
    Compose { text: String, caret: usize },
    /// Finalize the IME composition: keep the composed text as committed and end
    /// the composing state. `text` is the final segment (may differ from the last
    /// preedit).
    CommitCompose(String),
}

/// The retained state of one editable node: its text, the selection over it, and
/// the byte range currently held by an in-progress IME composition.
///
/// Single-line: the text holds no `'\n'` (Enter submits, it does not insert), and
/// there is no wrap or vertical motion. Byte offsets index into `text`; all edit
/// ops keep the selection and composition range on `char` boundaries.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Buffer {
    /// The current text content.
    pub text: String,
    /// The caret / range selection over `text`.
    pub sel: Selection,
    /// Start byte of the active IME composition, or `composition_end` when none.
    composition_start: usize,
    /// End byte of the active IME composition; `> composition_start` while
    /// composing.
    composition_end: usize,
    /// Queued intents recorded since the last reconcile, applied in order. A
    /// reused buffer (drained, not reallocated) so the steady path allocates
    /// nothing.
    pending: Vec<EditIntent>,
}

impl Buffer {
    /// An empty buffer with the caret at the start.
    pub fn new() -> Self {
        Self::default()
    }

    /// A buffer holding `text` with the caret at its end.
    pub fn with_text(text: impl Into<String>) -> Self {
        let text = text.into();
        let at = text.len();
        Self {
            text,
            sel: Selection::caret(at),
            composition_start: at,
            composition_end: at,
            pending: Vec::new(),
        }
    }

    /// Whether an IME composition is currently active.
    #[inline]
    pub fn has_composition(&self) -> bool {
        self.composition_end > self.composition_start
    }

    /// Record an intent to apply on the next reconcile. Called by the router with
    /// the intent a handler deferred; cheap and allocation-free once warmed.
    #[inline]
    pub fn queue(&mut self, intent: EditIntent) {
        self.pending.push(intent);
    }

    /// Whether any intent is queued (lets reconcile skip an untouched buffer).
    #[inline]
    pub fn is_dirty(&self) -> bool {
        !self.pending.is_empty()
    }

    /// Apply every queued intent in order, clearing the queue. Returns whether
    /// the text changed (so the caller re-declares the node's text) — a
    /// selection-only move returns `false` and needs no reshape.
    fn apply_pending(&mut self) -> bool {
        let before = self.text.len();
        // A cheap change detector that also catches same-length edits: compare a
        // fingerprint of the text before and after. Length alone misses a
        // replace-in-place, so hash the bytes.
        let hash_before = fnv1a(self.text.as_bytes());
        let intents = std::mem::take(&mut self.pending);
        for intent in intents.iter() {
            self.apply_one(intent);
        }
        // Return the drained buffer for reuse next frame (it is now empty).
        self.pending = intents;
        self.pending.clear();
        self.text.len() != before || fnv1a(self.text.as_bytes()) != hash_before
    }

    /// Apply a single intent to the buffer.
    fn apply_one(&mut self, intent: &EditIntent) {
        match intent {
            EditIntent::Insert(s) => self.replace_selection(s),
            EditIntent::Backspace => self.delete(true),
            EditIntent::Delete => self.delete(false),
            EditIntent::Move { motion, extend } => self.move_caret(*motion, *extend),
            EditIntent::Compose { text, caret } => self.compose(text, *caret),
            EditIntent::CommitCompose(text) => self.commit_compose(text),
        }
    }

    /// Replace the current selection with `s`, collapsing to a caret after it.
    fn replace_selection(&mut self, s: &str) {
        let (start, end) = (self.sel.start(), self.sel.end());
        self.text.replace_range(start..end, s);
        let at = start + s.len();
        self.sel = Selection::caret(at);
        self.collapse_composition(at);
    }

    /// Delete the selection, or one `char` toward `backward`/forward if it is a
    /// bare caret. The caret ends at the start of the removed range.
    fn delete(&mut self, backward: bool) {
        let (start, end) = if self.sel.is_caret() {
            let c = self.sel.cursor;
            if backward {
                (prev_boundary(&self.text, c), c)
            } else {
                (c, next_boundary(&self.text, c))
            }
        } else {
            (self.sel.start(), self.sel.end())
        };
        if start == end {
            return;
        }
        self.text.replace_range(start..end, "");
        self.sel = Selection::caret(start);
        self.collapse_composition(start);
    }

    /// Move the caret per `motion`. With `extend`, the anchor stays put
    /// (shift-select); without it, an existing range collapses to the motion's
    /// natural end and a bare caret steps.
    fn move_caret(&mut self, motion: Motion, extend: bool) {
        let target = match motion {
            Motion::Left => {
                if !extend && !self.sel.is_caret() {
                    self.sel.start()
                } else {
                    prev_boundary(&self.text, self.sel.cursor)
                }
            }
            Motion::Right => {
                if !extend && !self.sel.is_caret() {
                    self.sel.end()
                } else {
                    next_boundary(&self.text, self.sel.cursor)
                }
            }
            Motion::Home => 0,
            Motion::End => self.text.len(),
        };
        self.sel.cursor = target;
        if !extend {
            self.sel.anchor = target;
        }
    }

    /// Update the IME composition: replace the composition range (or the
    /// selection, if not composing yet) with `text`, keep it marked composing,
    /// and place the caret `caret` bytes into it. Empty `text` cancels.
    fn compose(&mut self, text: &str, caret: usize) {
        let (start, end) = if self.has_composition() {
            (self.composition_start, self.composition_end)
        } else {
            (self.sel.start(), self.sel.end())
        };
        self.text.replace_range(start..end, text);
        self.composition_start = start;
        self.composition_end = start + text.len();
        let caret = caret.min(text.len());
        self.sel = Selection::caret(start + caret);
        if text.is_empty() {
            // Cancelled: no active composition.
            self.composition_end = self.composition_start;
        }
    }

    /// Finalize the composition: replace the composition range with the final
    /// `text` as committed content and end the composing state.
    fn commit_compose(&mut self, text: &str) {
        let (start, end) = if self.has_composition() {
            (self.composition_start, self.composition_end)
        } else {
            (self.sel.start(), self.sel.end())
        };
        self.text.replace_range(start..end, text);
        let at = start + text.len();
        self.sel = Selection::caret(at);
        self.composition_start = at;
        self.composition_end = at;
    }

    /// After a non-IME edit at `at`, drop any stale composition range so it does
    /// not point into shifted text.
    #[inline]
    fn collapse_composition(&mut self, at: usize) {
        self.composition_start = at;
        self.composition_end = at;
    }
}

/// The previous `char` boundary strictly before byte `at`, or `at` if already at
/// the start. A single point where char stepping becomes grapheme stepping later.
#[inline]
pub fn prev_boundary(text: &str, at: usize) -> usize {
    if at == 0 {
        return 0;
    }
    let mut i = at - 1;
    while i > 0 && !text.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// The next `char` boundary strictly after byte `at`, or `at` if already at the
/// end. The grapheme-upgrade twin of [`prev_boundary`].
#[inline]
pub fn next_boundary(text: &str, at: usize) -> usize {
    let len = text.len();
    if at >= len {
        return len;
    }
    let mut i = at + 1;
    while i < len && !text.is_char_boundary(i) {
        i += 1;
    }
    i
}

/// A tiny FNV-1a over the buffer text, used only to detect a same-length edit so
/// reconcile knows whether to re-declare the text. Not a hot path (runs once per
/// dirty buffer per frame), not security-sensitive.
#[inline]
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// The driver-owned registry of edit buffers, indexed by the editable node's
/// [`NodeId::index`]. A dense `Vec` (not a per-node HashMap and not a NodeStore
/// column): the heap-heavy, rarely-touched buffer stays off the hot SoA columns,
/// and reconcile looks a buffer up by one index. Mirrors
/// [`crate::virtual_list::VirtualLists`].
#[derive(Default)]
pub struct TextEdits {
    /// `buffers[node_index]` is the buffer for that editable node, or `None` for
    /// a non-editable node (the common case).
    buffers: Vec<Option<Box<Buffer>>>,
}

impl TextEdits {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Drop every buffer, resetting to empty. Called beside `NodeStore::clear`
    /// when the tree is rebuilt wholesale.
    pub fn clear(&mut self) {
        self.buffers.clear();
    }

    /// Register `buffer` for `node`, growing the dense index as needed. Replaces
    /// any prior registration at that slot.
    pub fn register(&mut self, node: NodeId, buffer: Box<Buffer>) {
        let i = node.index() as usize;
        if i >= self.buffers.len() {
            self.buffers.resize_with(i + 1, || None);
        }
        self.buffers[i] = Some(buffer);
    }

    /// The buffer registered for `node`, if any.
    #[inline]
    pub fn get(&self, node: NodeId) -> Option<&Buffer> {
        self.buffers
            .get(node.index() as usize)
            .and_then(|b| b.as_deref())
    }

    /// Mutable access to the buffer registered for `node`, if any. The router
    /// uses this to queue a deferred intent onto the editing node.
    #[inline]
    pub fn get_mut(&mut self, node: NodeId) -> Option<&mut Buffer> {
        self.buffers
            .get_mut(node.index() as usize)
            .and_then(|b| b.as_deref_mut())
    }

    /// Whether any buffer is registered (lets a frame skip reconcile entirely).
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.buffers.iter().all(|b| b.is_none())
    }
}

/// Fold every dirty buffer's queued intents into its text and re-declare the
/// node's [`TextRequest`] when the text changed. Runs once per frame in the
/// layout phase, before text shaping, so an edit recorded during input is shaped
/// and painted the same frame.
///
/// The steady path is a no-op: a buffer with no queued intent is skipped, and a
/// buffer whose intents were all caret moves re-declares nothing. Returns the
/// number of nodes whose text was re-declared this frame — a steady-state
/// counter, `0` when nothing typed.
///
/// The re-declared [`TextRequest`] reuses the node's existing font size and
/// color from its current request, so an edit does not disturb styling; a node
/// with no prior request (never declared as text) is skipped defensively.
pub fn reconcile(store: &mut crate::component::NodeStore, edits: &mut TextEdits) -> u32 {
    let mut redeclared = 0;
    for i in 0..edits.buffers.len() {
        let Some(buffer) = edits.buffers[i].as_deref_mut() else {
            continue;
        };
        if !buffer.is_dirty() {
            continue;
        }
        let Some(node) = store.arena().live_id(i as u32) else {
            // The node was freed (whole-tree rebuild raced the registry clear);
            // drop the stale buffer defensively.
            edits.buffers[i] = None;
            continue;
        };
        let text_changed = buffer.apply_pending();
        if !text_changed {
            continue;
        }
        // Re-declare the text, carrying the node's existing style. A node that
        // was never declared as text has no style to carry — skip it.
        let Some(req) = store
            .text_request(node)
            .or_else(|| current_request(store, node))
        else {
            continue;
        };
        let request = TextRequest {
            text: buffer.text.clone(),
            font_size: req.font_size,
            color: req.color,
        };
        store.set_text_request(node, request);
        redeclared += 1;
    }
    redeclared
}

/// Recover a node's font size and color from its already-shaped content, for the
/// common case where the pending text request was drained by a prior frame's
/// shaping and only the [`Content`](crate::content::Content) remains.
fn current_request(store: &crate::component::NodeStore, node: NodeId) -> Option<&TextRequest> {
    // The shaped Content carries color but not font size; without a live request
    // we cannot recover font size, so a node whose request was already drained
    // must keep a request alive for editing. This returns `None` here and the
    // widget layer keeps the request resident; see the text_input widget.
    let _ = (store, node);
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buf(text: &str, cursor: usize) -> Buffer {
        let mut b = Buffer::with_text(text);
        b.sel = Selection::caret(cursor);
        b.composition_start = cursor;
        b.composition_end = cursor;
        b
    }

    #[test]
    fn selection_orders_ends() {
        let s = Selection {
            cursor: 2,
            anchor: 5,
        };
        assert_eq!(s.start(), 2);
        assert_eq!(s.end(), 5);
        assert!(!s.is_caret());
        assert!(Selection::caret(3).is_caret());
    }

    #[test]
    fn insert_at_caret_appends_and_collapses() {
        let mut b = buf("ab", 2);
        b.apply_one(&EditIntent::Insert("c".into()));
        assert_eq!(b.text, "abc");
        assert_eq!(b.sel, Selection::caret(3));
    }

    #[test]
    fn insert_replaces_selection() {
        let mut b = Buffer::with_text("hello");
        b.sel = Selection {
            cursor: 1,
            anchor: 4,
        };
        b.apply_one(&EditIntent::Insert("X".into()));
        assert_eq!(b.text, "hXo");
        assert_eq!(b.sel, Selection::caret(2));
    }

    #[test]
    fn backspace_at_caret_removes_prev_char() {
        let mut b = buf("abc", 3);
        b.apply_one(&EditIntent::Backspace);
        assert_eq!(b.text, "ab");
        assert_eq!(b.sel, Selection::caret(2));
    }

    #[test]
    fn backspace_at_start_is_noop() {
        let mut b = buf("abc", 0);
        b.apply_one(&EditIntent::Backspace);
        assert_eq!(b.text, "abc");
        assert_eq!(b.sel, Selection::caret(0));
    }

    #[test]
    fn delete_at_caret_removes_next_char() {
        let mut b = buf("abc", 0);
        b.apply_one(&EditIntent::Delete);
        assert_eq!(b.text, "bc");
        assert_eq!(b.sel, Selection::caret(0));
    }

    #[test]
    fn delete_removes_selection() {
        let mut b = Buffer::with_text("hello");
        b.sel = Selection {
            cursor: 4,
            anchor: 1,
        };
        b.apply_one(&EditIntent::Delete);
        assert_eq!(b.text, "ho");
        assert_eq!(b.sel, Selection::caret(1));
    }

    #[test]
    fn move_left_steps_one_char() {
        let mut b = buf("abc", 3);
        b.apply_one(&EditIntent::Move {
            motion: Motion::Left,
            extend: false,
        });
        assert_eq!(b.sel, Selection::caret(2));
    }

    #[test]
    fn move_left_collapses_selection_to_start() {
        let mut b = Buffer::with_text("hello");
        b.sel = Selection {
            cursor: 4,
            anchor: 1,
        };
        b.apply_one(&EditIntent::Move {
            motion: Motion::Left,
            extend: false,
        });
        assert_eq!(b.sel, Selection::caret(1));
    }

    #[test]
    fn shift_move_extends_selection() {
        let mut b = buf("abc", 3);
        b.apply_one(&EditIntent::Move {
            motion: Motion::Left,
            extend: true,
        });
        assert_eq!(
            b.sel,
            Selection {
                cursor: 2,
                anchor: 3
            }
        );
    }

    #[test]
    fn home_end_jump_to_line_ends() {
        let mut b = buf("abc", 1);
        b.apply_one(&EditIntent::Move {
            motion: Motion::End,
            extend: false,
        });
        assert_eq!(b.sel, Selection::caret(3));
        b.apply_one(&EditIntent::Move {
            motion: Motion::Home,
            extend: false,
        });
        assert_eq!(b.sel, Selection::caret(0));
    }

    #[test]
    fn multibyte_char_steps_whole_codepoint() {
        // "é" is 2 bytes; the caret must skip the whole char, not split it.
        let mut b = buf("é", 2);
        b.apply_one(&EditIntent::Move {
            motion: Motion::Left,
            extend: false,
        });
        assert_eq!(b.sel, Selection::caret(0));
        b.apply_one(&EditIntent::Backspace);
        assert_eq!(b.text, "é");
        assert_eq!(b.sel, Selection::caret(0));
    }

    #[test]
    fn compose_then_commit_leaves_final_text() {
        let mut b = buf("", 0);
        b.apply_one(&EditIntent::Compose {
            text: "n".into(),
            caret: 1,
        });
        assert!(b.has_composition());
        assert_eq!(b.text, "n");
        b.apply_one(&EditIntent::Compose {
            text: "ni".into(),
            caret: 2,
        });
        assert_eq!(b.text, "ni");
        b.apply_one(&EditIntent::CommitCompose("你".into()));
        assert!(!b.has_composition());
        assert_eq!(b.text, "你");
        assert_eq!(b.sel, Selection::caret("你".len()));
    }

    #[test]
    fn compose_empty_cancels() {
        let mut b = buf("", 0);
        b.apply_one(&EditIntent::Compose {
            text: "n".into(),
            caret: 1,
        });
        b.apply_one(&EditIntent::Compose {
            text: "".into(),
            caret: 0,
        });
        assert!(!b.has_composition());
        assert_eq!(b.text, "");
    }

    #[test]
    fn apply_pending_reports_text_change_and_drains() {
        let mut b = buf("ab", 2);
        b.queue(EditIntent::Insert("c".into()));
        assert!(b.is_dirty());
        assert!(b.apply_pending());
        assert_eq!(b.text, "abc");
        assert!(!b.is_dirty());
    }

    #[test]
    fn apply_pending_caret_move_reports_no_text_change() {
        let mut b = buf("abc", 3);
        b.queue(EditIntent::Move {
            motion: Motion::Left,
            extend: false,
        });
        assert!(!b.apply_pending());
        assert_eq!(b.text, "abc");
    }

    #[test]
    fn registry_registers_and_recovers() {
        let mut edits = TextEdits::new();
        assert!(edits.is_empty());
        // Mint a live node id.
        let mut store = crate::component::NodeStore::new();
        let id = {
            let mut cx = crate::component::BuildCx::new(&mut store);
            let h = cx.leaf(crate::component::LeafStyle {
                size: crate::layout::Size::fixed(1.0, 1.0),
                ..Default::default()
            });
            cx.root();
            h.id()
        };
        edits.register(id, Box::new(Buffer::with_text("hi")));
        assert!(!edits.is_empty());
        assert_eq!(edits.get(id).map(|b| b.text.as_str()), Some("hi"));
        edits.get_mut(id).unwrap().queue(EditIntent::Backspace);
        assert!(edits.get(id).unwrap().is_dirty());
        edits.clear();
        assert!(edits.is_empty());
    }
}
