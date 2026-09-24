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
//! ## Positions come from the text runtime
//!
//! The selection is a [`Selection`] of typed [`TextPosition`]s (byte offset plus
//! caret affinity) and the composition an [`ImeComposition`] — the text
//! runtime's own model, not a second byte-offset one kept beside it. Deletion
//! and logical caret steps move by grapheme cluster, so a combining sequence, a
//! ZWJ emoji or a flag is removed and crossed whole. Left/Right move *visually*
//! through the caret stops of the lines the text was last drawn with, and a
//! click resolves through the same stops, when the runtime supplies that
//! geometry through [`EditGeometry`]; without it (text not shaped yet, or edited
//! earlier in the same batch) Left/Right fall back to a logical grapheme step
//! and a click waits for the next shaped frame.
//!
//! Every change to the text advances the buffer's [`Revision`], which the IME
//! composition is stamped with, so a platform or worker result computed against
//! older text can be recognized as stale.

use crate::content::TextRequest;
use crate::node::NodeId;
use viso_render::Rgba;
use viso_text::caret::Caret;
use viso_text::hit_test::HitTester;
use viso_text::ime::{ImeComposition, Revision};
use viso_text::paragraph::{LineLayout, line_index_at};
use viso_text::selection::Selection;
use viso_text::{Segmenter, TextOffset, TextPosition};

/// Which way a caret motion goes, for arrow / Home / End.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Motion {
    /// One caret stop to the visual left (one grapheme back when the text has
    /// no drawn geometry yet).
    Left,
    /// One caret stop to the visual right (one grapheme forward when the text
    /// has no drawn geometry yet).
    Right,
    /// The start of the text.
    Home,
    /// The end of the text.
    End,
}

/// A self-contained editing command an input handler records without reading the
/// buffer. [`reconcile`] applies it to the node's [`Buffer`] in the layout phase.
///
/// Every variant is expressed against "the current selection" so it needs no
/// buffer snapshot at record time — the buffer state is only read when the
/// intent is applied, keeping the record path (in a handler that has no store)
/// pure.
#[derive(Debug, Clone, PartialEq)]
pub enum EditIntent {
    /// Replace the current selection with this text (an ordinary keystroke or an
    /// IME commit). Collapses to a caret after the inserted text.
    Insert(String),
    /// Delete backward: the selection if any, else the grapheme before the caret.
    Backspace,
    /// Delete forward: the selection if any, else the grapheme after the caret.
    Delete,
    /// Move the caret. `extend` keeps the anchor fixed (shift-select) instead of
    /// collapsing to a caret.
    Move { motion: Motion, extend: bool },
    /// Place the caret at the pointer, in the same window space as node bounds.
    /// `extend` keeps the anchor fixed (shift-click, drag-select).
    PlaceAt { x: f32, y: f32, extend: bool },
    /// Set or update the IME composition: replace the composition range (or the
    /// selection, if no composition is active yet) with `text`, keeping it marked
    /// as composing with the caret at `caret` bytes into `text`. An empty `text`
    /// cancels the composition.
    Compose { text: String, caret: TextOffset },
    /// Finalize the IME composition: keep the composed text as committed and end
    /// the composing state. `text` is the final segment (may differ from the last
    /// preedit).
    CommitCompose(String),
}

/// The drawn geometry of one editable node's text, as the runtime last laid it
/// out: what visual caret motion and click placement resolve against.
#[derive(Debug, Clone, Copy)]
pub struct EditLayout<'a> {
    /// The text the lines were laid out for. Geometry is only used while it
    /// equals the buffer's text.
    pub text: &'a str,
    /// The laid-out lines, inline positions in em.
    pub lines: &'a [LineLayout],
    /// The font size the lines were placed at, in the node's space units per em.
    pub font_size: f32,
    /// The distance between successive lines, in the node's space units.
    pub line_height: f32,
}

/// Where the runtime answers "how is this node's text drawn right now". The
/// text shaper implements it; [`reconcile`] asks once per edited node.
pub trait EditGeometry {
    /// The drawn layout of `node`'s text, or `None` when it has not been drawn.
    fn layout(&self, node: NodeId) -> Option<EditLayout<'_>>;
}

/// An [`EditLayout`] together with the node's origin in window space.
#[derive(Clone, Copy)]
struct Placed<'a> {
    layout: EditLayout<'a>,
    origin: (f32, f32),
}

/// The retained state of one editable node: its text, the selection over it, the
/// in-progress IME composition, and the text style
/// ([`font_size`](Buffer::font_size), [`color`](Buffer::color)) the node was
/// declared with.
///
/// The style is resident here on purpose. [`NodeStore::take_text_requests`] drains
/// a node's pending [`TextRequest`] once it is shaped, and the shaped
/// [`Content::Text`](crate::content::Content) carries no font size — so after the
/// first frame neither source can re-derive the style for a re-declared request.
/// Keeping the style on the buffer (which already lives beside the store, keyed by
/// node, and is touched only when that node is edited) lets [`reconcile`]
/// re-declare a complete request from the buffer alone, with no lookup into the
/// store and no extra allocation (both fields are `Copy`).
///
/// Single-line: the text holds no `'\n'` (Enter submits, it does not insert).
/// Every offset the edit ops leave behind is a grapheme boundary of `text`,
/// except a composition caret, which the platform places and which is kept on a
/// `char` boundary.
///
/// `PartialEq` (not `Eq`) because [`color`](Buffer::color) holds `f32` channels;
/// `Default` is hand-written because [`Rgba`] has no `Default` impl (its zero is
/// [`Rgba::TRANSPARENT`]).
#[derive(Debug, Clone, PartialEq)]
pub struct Buffer {
    /// The current text content.
    pub text: String,
    /// The caret / range selection over `text`.
    pub sel: Selection,
    /// The font size the node was declared with, in logical pixels. Resident so
    /// [`reconcile`] can re-declare the node's [`TextRequest`] after the original
    /// request was drained by shaping. See the type-level note.
    pub font_size: f32,
    /// The run color the node was declared with. Resident for the same reason as
    /// [`font_size`](Buffer::font_size).
    pub color: Rgba,
    /// The content locale the node was declared with. Resident for the same
    /// reason as [`font_size`](Buffer::font_size).
    pub locale: Option<String>,
    /// The active IME composition, inactive (empty range) when not composing.
    composition: ImeComposition,
    /// Advances on every change to `text`.
    revision: Revision,
    /// Queued intents recorded since the last reconcile, applied in order. A
    /// reused buffer (drained, not reallocated) so the steady path allocates
    /// nothing.
    pending: Vec<EditIntent>,
}

impl Default for Buffer {
    fn default() -> Self {
        Self::with_text(String::new())
    }
}

impl Buffer {
    /// An empty buffer with the caret at the start and a zeroed style. Prefer
    /// [`with_request`](Buffer::with_request) so the buffer carries the node's
    /// real font size and color for later re-declaration.
    pub fn new() -> Self {
        Self::default()
    }

    /// A buffer holding `text` with the caret at its end and a zeroed style.
    pub fn with_text(text: impl Into<String>) -> Self {
        let text = text.into();
        let at = TextOffset(text.len());
        Self {
            text,
            sel: Selection::caret(TextPosition::upstream(at)),
            font_size: 0.0,
            color: Rgba::TRANSPARENT,
            locale: None,
            composition: ImeComposition::default(),
            revision: Revision::default(),
            pending: Vec::new(),
        }
    }

    /// A buffer seeded from a [`TextRequest`]: its text with the caret at the end,
    /// and its font size and color kept resident so [`reconcile`] can re-declare
    /// the request after an edit. This is how the text-input widget registers a
    /// buffer at build time.
    pub fn with_request(request: &TextRequest) -> Self {
        Self {
            font_size: request.font_size,
            color: request.color,
            locale: request.locale.clone(),
            ..Self::with_text(request.text.clone())
        }
    }

    /// Whether an IME composition is currently active.
    #[inline]
    pub fn has_composition(&self) -> bool {
        self.composition.is_active()
    }

    /// The IME composition state: its range, anchor and revision.
    #[inline]
    pub fn composition(&self) -> &ImeComposition {
        &self.composition
    }

    /// The text revision: advances on every change to [`text`](Buffer::text).
    #[inline]
    pub fn revision(&self) -> Revision {
        self.revision
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
    fn apply_pending(&mut self, placed: Option<Placed<'_>>) -> bool {
        let before = self.revision;
        let intents = std::mem::take(&mut self.pending);
        for intent in intents.iter() {
            self.apply_one(intent, placed);
        }
        // Return the drained buffer for reuse next frame (it is now empty).
        self.pending = intents;
        self.pending.clear();
        self.revision != before
    }

    /// Apply a single intent to the buffer. `placed` is the node's drawn
    /// geometry, used only while it still describes the buffer's text.
    fn apply_one(&mut self, intent: &EditIntent, placed: Option<Placed<'_>>) {
        let placed = placed.filter(|p| p.layout.text == self.text);
        match intent {
            EditIntent::Insert(s) => self.replace_selection(s),
            EditIntent::Backspace => self.delete(true),
            EditIntent::Delete => self.delete(false),
            EditIntent::Move { motion, extend } => {
                self.move_caret(*motion, *extend, placed.map(|p| p.layout))
            }
            EditIntent::PlaceAt { x, y, extend } => {
                if let Some(placed) = placed {
                    self.place_at(placed, *x, *y, *extend);
                }
            }
            EditIntent::Compose { text, caret } => self.compose(text, *caret),
            EditIntent::CommitCompose(text) => self.commit_compose(text),
        }
    }

    /// Replace `[start, end)` of the text with `s`, advancing the revision when
    /// the text actually changes.
    fn splice(&mut self, start: TextOffset, end: TextOffset, s: &str) {
        if &self.text[start.0..end.0] == s {
            return;
        }
        self.text.replace_range(start.0..end.0, s);
        self.revision = self.revision.next();
    }

    /// Replace the current selection with `s`, collapsing to a caret after it.
    /// The buffer is single-line, so a pasted or committed line break lands as
    /// a space.
    fn replace_selection(&mut self, s: &str) {
        let s = single_line(s);
        let (start, end) = self.sel.logical_range();
        self.splice(start, end, &s);
        self.composition.clear();
        self.sel = Selection::caret(TextPosition::upstream(TextOffset(start.0 + s.len())));
    }

    /// Delete the selection, or one grapheme backward/forward from a bare caret.
    /// The caret ends at the start of the removed range.
    fn delete(&mut self, backward: bool) {
        let (start, end) = if self.sel.is_caret() {
            let at = self.sel.focus.offset;
            let graphemes = Segmenter::new(&self.text);
            if backward {
                (graphemes.prev_grapheme(at), at)
            } else {
                (at, graphemes.next_grapheme(at))
            }
        } else {
            self.sel.logical_range()
        };
        if start == end {
            return;
        }
        self.splice(start, end, "");
        self.composition.clear();
        self.sel = Selection::caret(TextPosition::downstream(start));
    }

    /// Move the caret per `motion`. With `extend`, the anchor stays put
    /// (shift-select); without it, an existing range collapses to its logical
    /// start (Left) or end (Right) and a bare caret steps.
    fn move_caret(&mut self, motion: Motion, extend: bool, layout: Option<EditLayout<'_>>) {
        let (start, end) = self.sel.logical_range();
        let target = match motion {
            Motion::Left if !extend && !self.sel.is_caret() => TextPosition::downstream(start),
            Motion::Right if !extend && !self.sel.is_caret() => TextPosition::upstream(end),
            Motion::Left | Motion::Right => {
                let right = motion == Motion::Right;
                match layout {
                    Some(layout) => visual_step(layout.lines, self.sel.focus, right),
                    None => {
                        let graphemes = Segmenter::new(&self.text);
                        let at = self.sel.focus.offset;
                        if right {
                            TextPosition::upstream(graphemes.next_grapheme(at))
                        } else {
                            TextPosition::downstream(graphemes.prev_grapheme(at))
                        }
                    }
                }
            }
            Motion::Home => TextPosition::downstream(TextOffset(0)),
            Motion::End => TextPosition::upstream(TextOffset(self.text.len())),
        };
        self.set_focus(target, extend);
    }

    /// Place the caret at window point `(x, y)` through the drawn lines.
    fn place_at(&mut self, placed: Placed<'_>, x: f32, y: f32, extend: bool) {
        let layout = placed.layout;
        if layout.lines.is_empty() || layout.font_size <= 0.0 {
            return;
        }
        let (local_x, local_y) = (x - placed.origin.0, y - placed.origin.1);
        let row = if layout.line_height > 0.0 {
            (local_y / layout.line_height).floor().max(0.0) as usize
        } else {
            0
        };
        let line = &layout.lines[row.min(layout.lines.len() - 1)];
        let position = HitTester::new(line).position_at_inline(local_x / layout.font_size);
        self.set_focus(position, extend);
    }

    /// Move the selection's focus to `position`, keeping the anchor when
    /// `extend`, else collapsing to a caret there.
    fn set_focus(&mut self, position: TextPosition, extend: bool) {
        if extend {
            self.sel.focus = position;
        } else {
            self.sel = Selection::caret(position);
        }
    }

    /// Update the IME composition: replace the composition range (or the
    /// selection, if not composing yet) with `text`, keep it marked composing,
    /// and place the caret `caret` bytes into it. Empty `text` cancels.
    fn compose(&mut self, text: &str, caret: TextOffset) {
        let (start, end) = if self.composition.is_active() {
            self.composition.range()
        } else {
            self.sel.logical_range()
        };
        self.splice(start, end, text);
        if text.is_empty() {
            self.composition.clear();
            self.sel = Selection::caret(TextPosition::downstream(start));
            return;
        }
        let composed_end = TextOffset(start.0 + text.len());
        self.composition
            .set_range(start, composed_end, self.revision);
        let mut caret = caret.0.min(text.len());
        while !text.is_char_boundary(caret) {
            caret -= 1;
        }
        self.sel = Selection::caret(TextPosition::upstream(TextOffset(start.0 + caret)));
    }

    /// Finalize the composition: replace the range it stands for with the final
    /// `text` as committed content and end the composing state.
    fn commit_compose(&mut self, text: &str) {
        let text = single_line(text);
        let (start, end) = if self.composition.is_active() {
            self.composition.replacement()
        } else {
            self.sel.logical_range()
        };
        self.splice(start, end, &text);
        self.composition.clear();
        self.sel = Selection::caret(TextPosition::upstream(TextOffset(start.0 + text.len())));
    }
}

/// One caret stop from `from` in the visual direction, on the line that draws
/// `from`.
fn visual_step(lines: &[LineLayout], from: TextPosition, right: bool) -> TextPosition {
    let Some(line) = lines.get(line_index_at(lines, from)) else {
        return from;
    };
    let mut caret = Caret::at(from);
    if right {
        caret.move_right(line)
    } else {
        caret.move_left(line)
    }
}

/// `s` with every line break (`\r\n`, `\n`, `\r`) folded to one space. Borrows
/// when there is none, which is the common case for typed text.
fn single_line(s: &str) -> std::borrow::Cow<'_, str> {
    if !s.contains(['\n', '\r']) {
        return std::borrow::Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push(' ');
            }
            '\n' => out.push(' '),
            c => out.push(c),
        }
    }
    std::borrow::Cow::Owned(out)
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
    /// Nodes whose text [`reconcile`] changed since the driver last drained them
    /// with [`TextEdits::take_changed`], in edit order, each at most once.
    changed: Vec<NodeId>,
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
        self.changed.clear();
    }

    /// Move the nodes whose text changed since the last drain into `out`
    /// (cleared first), so the driver can tell each control its new text.
    pub fn take_changed(&mut self, out: &mut Vec<NodeId>) {
        out.clear();
        out.append(&mut self.changed);
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
/// Intents a pointer handler recorded are first moved from the store's queue
/// onto their node's buffer. `geometry` supplies each edited node's drawn
/// lines for visual caret motion and click placement; `None` (no text runtime)
/// leaves Left/Right logical and clicks unresolved.
///
/// The steady path is a no-op: a buffer with no queued intent is skipped, and a
/// buffer whose intents were all caret moves re-declares nothing. Returns the
/// number of nodes whose text was re-declared this frame — a steady-state
/// counter, `0` when nothing typed.
///
/// The re-declared [`TextRequest`] is built from the buffer's own resident style
/// ([`Buffer::font_size`] / [`Buffer::color`]), so an edit does not disturb
/// styling and needs no lookup into the store — the request the node was declared
/// with was already drained by a prior frame's shaping. See [`Buffer`].
pub fn reconcile(
    store: &mut crate::component::NodeStore,
    edits: &mut TextEdits,
    geometry: Option<&dyn EditGeometry>,
) -> u32 {
    if store.has_edit_requests() {
        let mut requests = Vec::new();
        store.take_edit_requests(&mut requests);
        for (node, intent) in requests {
            if let Some(buffer) = edits.get_mut(node) {
                buffer.queue(intent);
            }
        }
    }
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
        let placed = geometry.and_then(|g| g.layout(node)).map(|layout| {
            let world = store.world(node);
            Placed {
                layout,
                origin: (world.x, world.y),
            }
        });
        let text_changed = buffer.apply_pending(placed);
        if !text_changed {
            continue;
        }
        // Re-declare the text self-contained from the buffer's resident style;
        // the original request was consumed by shaping and cannot be read back.
        let request = TextRequest {
            text: buffer.text.clone(),
            font_size: buffer.font_size,
            color: buffer.color,
            // An edit buffer is single-line here: it clips/scrolls, never wraps.
            soft_wrap: false,
            locale: buffer.locale.clone(),
        };
        store.set_text_request(node, request);
        if !edits.changed.contains(&node) {
            edits.changed.push(node);
        }
        redeclared += 1;
    }
    redeclared
}

#[cfg(test)]
mod tests {
    use super::*;
    use viso_text::bidi::{BaseDirection, BidiInfo};
    use viso_text::{CaretAffinity, FontFaceId, ShapedGlyph, ShapedRun};

    fn buf(text: &str, cursor: usize) -> Buffer {
        let mut b = Buffer::with_text(text);
        b.sel = Selection::caret(TextPosition::downstream(TextOffset(cursor)));
        b
    }

    fn range(anchor: usize, focus: usize) -> Selection {
        Selection {
            anchor: TextPosition::downstream(TextOffset(anchor)),
            focus: TextPosition::downstream(TextOffset(focus)),
        }
    }

    fn apply(b: &mut Buffer, intent: EditIntent) {
        b.apply_one(&intent, None);
    }

    fn step(b: &mut Buffer, motion: Motion) {
        apply(
            b,
            EditIntent::Move {
                motion,
                extend: false,
            },
        );
    }

    fn at(b: &Buffer) -> usize {
        assert!(b.sel.is_caret());
        b.sel.focus.offset.0
    }

    /// One laid-out line for `text`, every char one em wide, resolved with the
    /// real bidi pass.
    fn line_for(text: &str) -> Vec<LineLayout> {
        let bidi = BidiInfo::resolve(text, BaseDirection::LeftToRight);
        let shaped: Vec<ShapedRun> = bidi
            .direction_runs()
            .iter()
            .map(|run| {
                let slice = &text[run.start.0..run.end.0];
                let mut glyphs: Vec<ShapedGlyph> = slice
                    .char_indices()
                    .map(|(i, _)| ShapedGlyph {
                        glyph_id: 1,
                        cluster: i as u32,
                        x_advance: 1.0,
                        x_offset: 0.0,
                        y_offset: 0.0,
                        unsafe_to_break: false,
                    })
                    .collect();
                if run.level.0 % 2 == 1 {
                    glyphs.reverse();
                }
                ShapedRun {
                    face: FontFaceId(0),
                    width_ems: glyphs.len() as f32,
                    glyphs,
                    text_len: slice.len() as u32,
                    ligature_carets: Vec::new(),
                }
            })
            .collect();
        vec![LineLayout::single_line(text, &bidi, &shaped)]
    }

    struct FakeGeometry {
        node: NodeId,
        text: String,
        lines: Vec<LineLayout>,
    }

    impl EditGeometry for FakeGeometry {
        fn layout(&self, node: NodeId) -> Option<EditLayout<'_>> {
            (node == self.node).then(|| EditLayout {
                text: &self.text,
                lines: &self.lines,
                font_size: 10.0,
                line_height: 12.0,
            })
        }
    }

    fn placed<'a>(text: &'a str, lines: &'a [LineLayout]) -> Placed<'a> {
        Placed {
            layout: EditLayout {
                text,
                lines,
                font_size: 10.0,
                line_height: 12.0,
            },
            origin: (100.0, 50.0),
        }
    }

    #[test]
    fn insert_at_caret_appends_and_collapses() {
        let mut b = buf("ab", 2);
        apply(&mut b, EditIntent::Insert("c".into()));
        assert_eq!(b.text, "abc");
        assert_eq!(at(&b), 3);
        assert_eq!(b.sel.focus.affinity, CaretAffinity::Upstream);
    }

    #[test]
    fn insert_replaces_selection() {
        let mut b = Buffer::with_text("hello");
        b.sel = range(4, 1);
        apply(&mut b, EditIntent::Insert("X".into()));
        assert_eq!(b.text, "hXo");
        assert_eq!(at(&b), 2);
    }

    #[test]
    fn backspace_at_caret_removes_prev_char() {
        let mut b = buf("abc", 3);
        apply(&mut b, EditIntent::Backspace);
        assert_eq!(b.text, "ab");
        assert_eq!(at(&b), 2);
    }

    #[test]
    fn backspace_at_start_is_noop() {
        let mut b = buf("abc", 0);
        let revision = b.revision();
        apply(&mut b, EditIntent::Backspace);
        assert_eq!(b.text, "abc");
        assert_eq!(at(&b), 0);
        assert_eq!(b.revision(), revision);
    }

    #[test]
    fn delete_at_caret_removes_next_char() {
        let mut b = buf("abc", 0);
        apply(&mut b, EditIntent::Delete);
        assert_eq!(b.text, "bc");
        assert_eq!(at(&b), 0);
    }

    #[test]
    fn delete_removes_selection() {
        let mut b = Buffer::with_text("hello");
        b.sel = range(1, 4);
        apply(&mut b, EditIntent::Delete);
        assert_eq!(b.text, "ho");
        assert_eq!(at(&b), 1);
    }

    #[test]
    fn move_left_collapses_selection_to_start() {
        let mut b = Buffer::with_text("hello");
        b.sel = range(1, 4);
        step(&mut b, Motion::Left);
        assert_eq!(at(&b), 1);
    }

    #[test]
    fn move_right_collapses_selection_to_end() {
        let mut b = Buffer::with_text("hello");
        b.sel = range(4, 1);
        step(&mut b, Motion::Right);
        assert_eq!(at(&b), 4);
    }

    #[test]
    fn shift_move_extends_selection() {
        let mut b = buf("abc", 3);
        apply(
            &mut b,
            EditIntent::Move {
                motion: Motion::Left,
                extend: true,
            },
        );
        assert_eq!(b.sel.anchor.offset, TextOffset(3));
        assert_eq!(b.sel.focus.offset, TextOffset(2));
    }

    #[test]
    fn home_end_jump_to_line_ends() {
        let mut b = buf("abc", 1);
        step(&mut b, Motion::End);
        assert_eq!(at(&b), 3);
        step(&mut b, Motion::Home);
        assert_eq!(at(&b), 0);
    }

    #[test]
    fn combining_sequence_is_one_step_and_one_delete() {
        // "e" + U+0301 COMBINING ACUTE: one grapheme, three bytes.
        let text = "ae\u{301}b";
        let mut b = buf(text, 1);
        step(&mut b, Motion::Right);
        assert_eq!(at(&b), 4);
        step(&mut b, Motion::Left);
        assert_eq!(at(&b), 1);
        apply(&mut b, EditIntent::Delete);
        assert_eq!(b.text, "ab");
        assert_eq!(at(&b), 1);
    }

    #[test]
    fn zwj_emoji_and_flag_delete_whole() {
        // Family (man ZWJ woman ZWJ girl), then the flag of Japan.
        let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
        let flag = "\u{1F1EF}\u{1F1F5}";
        let text = format!("{family}{flag}");
        let mut b = buf(&text, text.len());
        apply(&mut b, EditIntent::Backspace);
        assert_eq!(b.text, family);
        apply(&mut b, EditIntent::Backspace);
        assert_eq!(b.text, "");
    }

    #[test]
    fn decomposed_text_is_kept_as_typed() {
        // NFD input survives an edit round-trip byte for byte: the buffer never
        // normalizes what the user or IME produced.
        let nfd = "Cafe\u{301}";
        let mut b = buf(nfd, nfd.len());
        apply(&mut b, EditIntent::Insert("!".into()));
        apply(&mut b, EditIntent::Backspace);
        assert_eq!(b.text, nfd);
        apply(&mut b, EditIntent::Backspace);
        assert_eq!(b.text, "Caf");
    }

    #[test]
    fn compose_then_commit_leaves_final_text() {
        let mut b = buf("", 0);
        apply(
            &mut b,
            EditIntent::Compose {
                text: "n".into(),
                caret: TextOffset(1),
            },
        );
        assert!(b.has_composition());
        assert_eq!(b.text, "n");
        apply(
            &mut b,
            EditIntent::Compose {
                text: "ni".into(),
                caret: TextOffset(2),
            },
        );
        assert_eq!(b.text, "ni");
        apply(&mut b, EditIntent::CommitCompose("你".into()));
        assert!(!b.has_composition());
        assert_eq!(b.text, "你");
        assert_eq!(at(&b), "你".len());
    }

    #[test]
    fn cjk_composition_tracks_revisions() {
        let mut b = buf("ab", 1);
        let start = b.revision();
        apply(
            &mut b,
            EditIntent::Compose {
                text: "zhong".into(),
                caret: TextOffset(5),
            },
        );
        let first = b.composition().revision();
        assert!(first > start);
        assert_eq!(b.composition().range(), (TextOffset(1), TextOffset(6)));
        apply(
            &mut b,
            EditIntent::Compose {
                text: "中".into(),
                caret: TextOffset(3),
            },
        );
        assert_eq!(b.text, "a中b");
        assert_eq!(b.composition().range(), (TextOffset(1), TextOffset(4)));
        assert!(b.composition().revision() > first);
        // A result computed against the first preedit is now stale.
        assert!(!b.composition().accepts(first));
        apply(&mut b, EditIntent::CommitCompose("中".into()));
        assert_eq!(b.text, "a中b");
        assert!(!b.has_composition());
        assert_eq!(at(&b), 4);
    }

    #[test]
    fn compose_caret_stays_on_a_char_boundary() {
        let mut b = buf("", 0);
        apply(
            &mut b,
            EditIntent::Compose {
                text: "中".into(),
                caret: TextOffset(2),
            },
        );
        assert_eq!(at(&b), 0);
    }

    #[test]
    fn compose_empty_cancels() {
        let mut b = buf("", 0);
        apply(
            &mut b,
            EditIntent::Compose {
                text: "n".into(),
                caret: TextOffset(1),
            },
        );
        apply(
            &mut b,
            EditIntent::Compose {
                text: "".into(),
                caret: TextOffset(0),
            },
        );
        assert!(!b.has_composition());
        assert_eq!(b.text, "");
    }

    #[test]
    fn insert_folds_line_breaks_to_spaces() {
        let mut b = buf("ab", 1);
        apply(&mut b, EditIntent::Insert("x\r\ny\nz\rw".into()));
        assert_eq!(b.text, "ax y z wb");
        assert_eq!(at(&b), 8);
    }

    #[test]
    fn commit_compose_folds_line_breaks() {
        let mut b = buf("", 0);
        apply(&mut b, EditIntent::CommitCompose("a\nb".into()));
        assert_eq!(b.text, "a b");
    }

    #[test]
    fn single_line_borrows_when_clean() {
        assert!(matches!(
            single_line("plain"),
            std::borrow::Cow::Borrowed(_)
        ));
        assert_eq!(single_line("\r\r\n"), "  ");
    }

    #[test]
    fn visual_moves_follow_drawn_order_in_rtl() {
        // "ab " then Hebrew "אב": Right walks the screen left to right, which
        // crosses the RTL run from its logical end back to its logical start.
        let text = "ab \u{5D0}\u{5D1}";
        let lines = line_for(text);
        let mut b = buf(text, 0);
        let geometry = placed(text, &lines);
        let mut trail = Vec::new();
        for _ in 0..6 {
            b.apply_one(
                &EditIntent::Move {
                    motion: Motion::Right,
                    extend: false,
                },
                Some(geometry),
            );
            trail.push((lines[0].caret_x(b.sel.focus).unwrap(), at(&b)));
        }
        assert!(
            trail.windows(2).all(|w| w[1].0 >= w[0].0),
            "Right never moves leftward on screen: {trail:?}"
        );
        // The run seam at x = 3 is two logical stops (end of "ab ", end of the
        // Hebrew run), then the caret walks the Hebrew run backwards.
        let offsets: Vec<usize> = trail.iter().map(|&(_, o)| o).collect();
        assert_eq!(offsets, [1, 2, 3, 7, 5, 3]);
        assert_eq!(trail.last().unwrap().0, 5.0);
    }

    #[test]
    fn one_offset_at_a_direction_boundary_draws_where_its_affinity_says() {
        // Offset 3 ends "ab " (x = 3) and starts the Hebrew run, whose logical
        // start is drawn at its right edge (x = 5).
        let text = "ab \u{5D0}\u{5D1}";
        let lines = line_for(text);
        let mut b = buf(text, 0);
        let mut seen = Vec::new();
        for _ in 0..6 {
            b.apply_one(
                &EditIntent::Move {
                    motion: Motion::Right,
                    extend: false,
                },
                Some(placed(text, &lines)),
            );
            if at(&b) == 3 {
                seen.push((b.sel.focus.affinity, lines[0].caret_x(b.sel.focus).unwrap()));
            }
        }
        assert_eq!(
            seen,
            [
                (CaretAffinity::Upstream, 3.0),
                (CaretAffinity::Downstream, 5.0)
            ]
        );
    }

    #[test]
    fn visual_steps_cross_a_ligature_grapheme_by_grapheme() {
        // "ffi" shaped as one 3 em ligature glyph, then "x": the caret stops
        // inside the ligature at each grapheme, never skipping it whole.
        let text = "ffix";
        let bidi = BidiInfo::resolve(text, BaseDirection::LeftToRight);
        let glyph = |cluster, x_advance| ShapedGlyph {
            glyph_id: 7,
            cluster,
            x_advance,
            x_offset: 0.0,
            y_offset: 0.0,
            unsafe_to_break: false,
        };
        let shaped = ShapedRun {
            face: FontFaceId(0),
            glyphs: vec![glyph(0, 3.0), glyph(3, 1.0)],
            width_ems: 4.0,
            text_len: 4,
            ligature_carets: Vec::new(),
        };
        let lines = vec![LineLayout::single_line(text, &bidi, &[shaped])];
        let mut b = buf(text, 0);
        let mut trail = Vec::new();
        for _ in 0..4 {
            b.apply_one(
                &EditIntent::Move {
                    motion: Motion::Right,
                    extend: false,
                },
                Some(placed(text, &lines)),
            );
            trail.push(at(&b));
        }
        assert_eq!(trail, [1, 2, 3, 4]);
    }

    #[test]
    fn drag_across_directions_selects_one_logical_range_in_two_fragments() {
        // Drag from x = 1 em ("b") to x = 4 em (between the Hebrew letters).
        let text = "ab \u{5D0}\u{5D1}";
        let lines = line_for(text);
        let mut b = buf(text, 0);
        for (x, extend) in [(110.0, false), (140.0, true)] {
            b.apply_one(
                &EditIntent::PlaceAt { x, y: 55.0, extend },
                Some(placed(text, &lines)),
            );
        }
        // Logically: from "b" through the first Hebrew letter.
        assert_eq!(b.sel.logical_range(), (TextOffset(1), TextOffset(5)));
        // Visually: "b " on the left, and the first Hebrew letter drawn at the
        // run's right edge — two disjoint rectangles.
        let fragments: Vec<(f32, f32)> = b
            .sel
            .fragments(&lines[0])
            .iter()
            .map(|f| (f.left, f.right))
            .collect();
        assert_eq!(fragments, [(1.0, 3.0), (4.0, 5.0)]);
    }

    #[test]
    fn stale_geometry_falls_back_to_logical_steps() {
        let lines = line_for("abc");
        let mut b = buf("abcd", 4);
        b.apply_one(
            &EditIntent::Move {
                motion: Motion::Left,
                extend: false,
            },
            Some(placed("abc", &lines)),
        );
        assert_eq!(at(&b), 3);
    }

    #[test]
    fn place_at_resolves_through_drawn_stops() {
        let text = "abcd";
        let lines = line_for(text);
        let mut b = buf(text, 0);
        // Origin (100, 50), 10px per em: x = 121 is 2.1 em, nearest stop 2.
        b.apply_one(
            &EditIntent::PlaceAt {
                x: 121.0,
                y: 55.0,
                extend: false,
            },
            Some(placed(text, &lines)),
        );
        assert_eq!(at(&b), 2);
        // Drag to 3.8 em, extending: the anchor stays at 2.
        b.apply_one(
            &EditIntent::PlaceAt {
                x: 138.0,
                y: 90.0,
                extend: true,
            },
            Some(placed(text, &lines)),
        );
        assert_eq!(b.sel.anchor.offset, TextOffset(2));
        assert_eq!(b.sel.focus.offset, TextOffset(4));
        assert_eq!(b.sel.logical_range(), (TextOffset(2), TextOffset(4)));
    }

    #[test]
    fn place_at_without_geometry_is_ignored() {
        let mut b = buf("abcd", 1);
        apply(
            &mut b,
            EditIntent::PlaceAt {
                x: 0.0,
                y: 0.0,
                extend: false,
            },
        );
        assert_eq!(at(&b), 1);
    }

    #[test]
    fn apply_pending_reports_text_change_and_drains() {
        let mut b = buf("ab", 2);
        b.queue(EditIntent::Insert("c".into()));
        assert!(b.is_dirty());
        assert!(b.apply_pending(None));
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
        assert!(!b.apply_pending(None));
        assert_eq!(b.text, "abc");
    }

    fn live_node(store: &mut crate::component::NodeStore) -> NodeId {
        let mut cx = crate::component::BuildCx::new(store);
        let h = cx.leaf(crate::component::LeafStyle {
            size: crate::layout::Size::fixed(1.0, 1.0),
            ..Default::default()
        });
        cx.root();
        h.id()
    }

    #[test]
    fn registry_registers_and_recovers() {
        let mut edits = TextEdits::new();
        assert!(edits.is_empty());
        let mut store = crate::component::NodeStore::new();
        let id = live_node(&mut store);
        edits.register(id, Box::new(Buffer::with_text("hi")));
        assert!(!edits.is_empty());
        assert_eq!(edits.get(id).map(|b| b.text.as_str()), Some("hi"));
        edits.get_mut(id).unwrap().queue(EditIntent::Backspace);
        assert!(edits.get(id).unwrap().is_dirty());
        edits.clear();
        assert!(edits.is_empty());
    }

    #[test]
    fn reconcile_routes_store_requests_through_geometry() {
        let mut store = crate::component::NodeStore::new();
        let id = live_node(&mut store);
        let mut edits = TextEdits::new();
        edits.register(id, Box::new(Buffer::with_text("abcd")));
        let world = store.world(id);
        let geometry = FakeGeometry {
            node: id,
            text: "abcd".into(),
            lines: line_for("abcd"),
        };
        store.queue_edit(
            id,
            EditIntent::PlaceAt {
                x: world.x + 11.0,
                y: world.y + 1.0,
                extend: false,
            },
        );
        assert!(store.has_edit_requests());
        // A placement changes only the selection: nothing re-declared.
        assert_eq!(reconcile(&mut store, &mut edits, Some(&geometry)), 0);
        assert!(!store.has_edit_requests());
        assert_eq!(at(edits.get(id).unwrap()), 1);
    }
}
