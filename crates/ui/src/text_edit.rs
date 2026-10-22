//! Text editing: the retained edit buffer, the deferred edit
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
//!
//! ## What a field draws is not always its text
//!
//! A field holding no text draws its [`FieldLook::placeholder`], and a secure
//! one a bullet for each grapheme of its text. The buffer declares what it
//! draws ([`Buffer::request`]) and maps between offsets of that and of its
//! text ([`Buffer::drawn_position`]), so caret motion, clicks and the IME
//! caret resolve against the drawn lines while every edit applies to the text.

use crate::content::TextRequest;
use crate::edit_history::{EditKind, History, Splice, Step};
use crate::node::NodeId;
use crate::text_style::{FontSize, TextOverflow, TextStyle};
use viso_render::Rgba;
use viso_text::caret::Caret;
use viso_text::hit_test::HitTester;
use viso_text::ime::{ImeComposition, Revision};
use viso_text::paragraph::{LineLayout, line_index_at};
use viso_text::selection::Selection;
use viso_text::{Segmenter, TextOffset, TextPosition};

/// The mark a secure field draws for each grapheme of its text.
const MASK: char = '\u{2022}';

/// The share of its text color's opacity a field's placeholder draws with.
const PLACEHOLDER_OPACITY: f32 = 0.5;

/// How a text field shows what it holds beyond the style its text draws in.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FieldLook {
    /// What it draws while it holds no text, in its text color at half the
    /// opacity; empty for nothing.
    pub placeholder: String,
    /// Whether it masks its text: it draws a bullet for each grapheme, gives
    /// no text to copy or cut, and an assistive technology never reads it.
    pub secure: bool,
    /// Whether it holds lines: its text wraps to its width, Enter breaks a
    /// line, and pasted breaks are kept. A single-line field folds every
    /// break to a space. A secure field holds one line.
    pub multiline: bool,
}

/// What a text field's node declares, queued on the store until
/// [`reconcile`] seeds the node's buffer with it.
#[derive(Debug, Clone, PartialEq)]
pub struct FieldSeed {
    /// The style its text draws in, and, when [`valued`](Self::valued), its
    /// text.
    pub request: TextRequest,
    /// Whether the request's text is the field's: a field no value drives
    /// keeps the text it holds.
    pub valued: bool,
    /// How it shows what it holds.
    pub look: FieldLook,
}

/// What a field draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Drawn {
    /// Its text.
    Text,
    /// Its placeholder, as it holds no text.
    Placeholder,
    /// A bullet for each grapheme of its text.
    Mask,
}

/// Where a caret motion goes.
///
/// Left and Right step visually through the drawn caret stops; the word
/// motions go to the start of the word before the caret or the end of the
/// word after it in the paragraph's reading direction; the line motions go
/// to the ends of the drawn line, Up and Down to the line above or below at
/// the column the caret keeps, the page motions as many lines as the field
/// shows, and the paragraph and document motions to the ends of the hard line
/// and of the text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Motion {
    /// One caret stop to the visual left (one grapheme back when the text has
    /// no drawn geometry yet).
    Left,
    /// One caret stop to the visual right (one grapheme forward when the text
    /// has no drawn geometry yet).
    Right,
    /// To the word boundary on the visual left.
    WordLeft,
    /// To the word boundary on the visual right.
    WordRight,
    /// The start of the drawn line (of the paragraph without geometry).
    LineStart,
    /// The end of the drawn line (of the paragraph without geometry).
    LineEnd,
    /// The line above, at the caret's column; the text's start from the
    /// first line.
    Up,
    /// The line below, at the caret's column; the text's end from the last
    /// line.
    Down,
    /// As many lines up as the field shows.
    PageUp,
    /// As many lines down as the field shows.
    PageDown,
    /// The start of the paragraph, or of the one before from its start.
    ParagraphStart,
    /// The end of the paragraph, or of the one after from its end.
    ParagraphEnd,
    /// The start of the text.
    DocStart,
    /// The end of the text.
    DocEnd,
}

impl Motion {
    /// Whether the motion moves between lines, keeping the caret's column.
    fn is_vertical(self) -> bool {
        matches!(
            self,
            Motion::Up | Motion::Down | Motion::PageUp | Motion::PageDown
        )
    }
}

/// How far a deletion from a bare caret reaches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reach {
    /// One grapheme.
    Grapheme,
    /// To the word boundary.
    Word,
    /// To the end of the drawn line.
    Line,
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
    /// Replace the current selection with typed text. Collapses to a caret
    /// after it; typing in a row is one undo step a word at a time.
    Insert(String),
    /// Replace the current selection with pasted text, one undo step.
    Paste(String),
    /// Delete backward: the selection if any, else from the caret back as
    /// far as the reach.
    Backspace(Reach),
    /// Delete forward: the selection if any, else from the caret on as far
    /// as the reach.
    Delete(Reach),
    /// Delete the selection as one undo step, after its text was copied.
    Cut,
    /// Move the caret. `extend` keeps the anchor fixed (shift-select) instead of
    /// collapsing to a caret.
    Move { motion: Motion, extend: bool },
    /// Select the whole text.
    SelectAll,
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
    /// preedit). With no composition it is typed text.
    CommitCompose(String),
    /// Take back the last undo step.
    Undo,
    /// Make the last undone step again.
    Redo,
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
    /// Each line's inline offset within the node, its alignment, in the
    /// node's space units; a line past them is not offset.
    pub offsets: &'a [f32],
}

impl EditLayout<'_> {
    /// The row drawing `at` and its line.
    fn line_at(&self, at: TextPosition) -> Option<(usize, &LineLayout)> {
        let row = line_index_at(self.lines, at);
        self.lines.get(row).map(|line| (row, line))
    }

    /// The x of the caret at `at` on `row`, in the node's space.
    fn caret_x(&self, row: usize, at: TextPosition) -> f32 {
        let x = self.lines[row].caret_x(at).unwrap_or(0.0);
        x * self.font_size + self.offsets.get(row).copied().unwrap_or(0.0)
    }

    /// The position on `row` nearest `x` in the node's space.
    fn position_at(&self, row: usize, x: f32) -> TextPosition {
        let offset = self.offsets.get(row).copied().unwrap_or(0.0);
        HitTester::new(&self.lines[row]).position_at_inline((x - offset) / self.font_size)
    }
}

/// Where the runtime answers "how is this node's text drawn right now". The
/// text shaper implements it; [`reconcile`] asks once per edited node.
pub trait EditGeometry {
    /// The drawn layout of `node`'s text, or `None` when it has not been drawn.
    fn layout(&self, node: NodeId) -> Option<EditLayout<'_>>;
}

/// An [`EditLayout`] together with the node's box in window space.
#[derive(Clone, Copy)]
struct Placed<'a> {
    layout: EditLayout<'a>,
    origin: (f32, f32),
    /// The box's width and height.
    extent: (f32, f32),
}

/// The retained state of one editable node: its text, the selection over it, the
/// in-progress IME composition, its undo history, and the text style
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
/// store.
///
/// A single-line buffer's text holds no `'\n'` (Enter submits, it does not
/// insert); a multi-line one's breaks are all `'\n'`.
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
    /// The font size the node was declared with. Resident so [`reconcile`]
    /// can re-declare the node's [`TextRequest`] after the original request
    /// was drained by shaping. See the type-level note.
    pub font_size: FontSize,
    /// The run color the node was declared with. Resident for the same reason as
    /// [`font_size`](Buffer::font_size).
    pub color: Rgba,
    /// The content locale the node was declared with. Resident for the same
    /// reason as [`font_size`](Buffer::font_size).
    pub locale: Option<String>,
    /// The face, line height and alignment the node was declared with.
    /// Resident for the same reason as [`font_size`](Buffer::font_size).
    pub style: TextStyle,
    /// How the field shows its text: its placeholder and masking.
    pub look: FieldLook,
    /// The active IME composition, inactive (empty range) when not composing.
    composition: ImeComposition,
    /// What undoing the composition under way takes back, while one is.
    composing: Option<Step>,
    /// The column, in the node's space, Up and Down keep while they move the
    /// caret from line to line.
    goal_x: Option<f32>,
    /// The steps undo and redo walk.
    history: History,
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
            font_size: FontSize::Fixed(0.0),
            color: Rgba::TRANSPARENT,
            locale: None,
            style: TextStyle::default(),
            look: FieldLook::default(),
            composition: ImeComposition::default(),
            composing: None,
            goal_x: None,
            history: History::default(),
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
            style: request.style.clone(),
            ..Self::with_text(request.text.clone())
        }
    }

    /// A buffer seeded from a [`FieldSeed`]: its style and look, and its text
    /// with the caret at the end when a value drives it, else none.
    pub fn with_seed(seed: &FieldSeed) -> Self {
        let mut buffer = Self {
            look: seed.look.clone(),
            ..Self::with_request(&seed.request)
        };
        if !seed.valued {
            buffer.text.clear();
            buffer.sel = Selection::caret(TextPosition::upstream(TextOffset(0)));
        }
        buffer
    }

    /// Seed the buffer from its node's declared `seed`: take its style and
    /// look, and its text when a value drives it and the buffer does not
    /// already hold it, then put the caret at the end and end any
    /// composition. A buffer that already holds the text keeps its caret,
    /// selection and composition, so the state a field writes back seeds
    /// nothing. A new text is one undo step. Returns whether what the field
    /// draws changed.
    pub fn seed(&mut self, seed: &FieldSeed) -> bool {
        let request = &seed.request;
        let styled = self.font_size != request.font_size
            || self.color != request.color
            || self.locale != request.locale
            || self.style != request.style
            || self.look != seed.look;
        self.font_size = request.font_size;
        self.color = request.color;
        self.locale.clone_from(&request.locale);
        self.style.clone_from(&request.style);
        self.look.clone_from(&seed.look);
        if !seed.valued || self.text == request.text {
            return styled;
        }
        self.end_composition();
        let (start, old_end, new_end) = changed_range(&self.text, &request.text);
        self.splice(
            TextOffset(start),
            TextOffset(old_end),
            &request.text[start..new_end],
            Some(EditKind::Other),
        );
        self.sel = Selection::caret(TextPosition::upstream(TextOffset(self.text.len())));
        true
    }

    /// Replace the text and selection with ones recorded of this field
    /// elsewhere (a snapshot of the process before), ending any composition
    /// and forgetting the undo history. A selection end past the text or
    /// inside a character moves to its end.
    pub fn restore(&mut self, text: String, sel: Selection) {
        let end = TextPosition::upstream(TextOffset(text.len()));
        let clamp = |position: TextPosition| {
            let at = position.offset.0;
            if at <= text.len() && text.is_char_boundary(at) {
                position
            } else {
                end
            }
        };
        self.sel = Selection {
            anchor: clamp(sel.anchor),
            focus: clamp(sel.focus),
        };
        self.text = text;
        self.composition = ImeComposition::default();
        self.composing = None;
        self.goal_x = None;
        self.history.clear();
        self.revision = self.revision.next();
    }

    /// The text request that declares what the field draws in its resident
    /// style: its text, its placeholder while it holds none, or a bullet for
    /// each grapheme of a secure one's. A single-line field clips and
    /// scrolls, a multi-line one wraps to its width; neither cuts its text
    /// off.
    pub fn request(&self) -> TextRequest {
        let (text, color) = match self.drawn() {
            Drawn::Text => (self.text.clone(), self.color),
            Drawn::Placeholder => (
                self.look.placeholder.clone(),
                Rgba {
                    a: self.color.a * PLACEHOLDER_OPACITY,
                    ..self.color
                },
            ),
            Drawn::Mask => (
                std::iter::repeat_n(MASK, graphemes(&self.text)).collect(),
                self.color,
            ),
        };
        TextRequest {
            text,
            font_size: self.font_size,
            color,
            soft_wrap: self.holds_lines(),
            locale: self.locale.clone(),
            style: TextStyle {
                max_lines: None,
                overflow: TextOverflow::Clip,
                ..self.style.clone()
            },
        }
    }

    /// What the field draws now.
    fn drawn(&self) -> Drawn {
        if self.text.is_empty() && !self.look.placeholder.is_empty() {
            Drawn::Placeholder
        } else if self.look.secure {
            Drawn::Mask
        } else {
            Drawn::Text
        }
    }

    /// Whether `text` is what the field draws now (the text of
    /// [`request`](Self::request)), so geometry laid out for it describes the
    /// field. Allocates nothing.
    pub fn draws(&self, text: &str) -> bool {
        match self.drawn() {
            Drawn::Text => text == self.text,
            Drawn::Placeholder => text == self.look.placeholder,
            Drawn::Mask => {
                text.len() == graphemes(&self.text) * MASK.len_utf8()
                    && text.chars().all(|c| c == MASK)
            }
        }
    }

    /// Position `at` of the text as a position of what the field draws.
    pub fn drawn_position(&self, at: TextPosition) -> TextPosition {
        let offset = match self.drawn() {
            Drawn::Text => return at,
            Drawn::Placeholder => 0,
            Drawn::Mask => graphemes(&self.text[..at.offset.0]) * MASK.len_utf8(),
        };
        TextPosition {
            offset: TextOffset(offset),
            ..at
        }
    }

    /// Position `at` of what the field draws as a position of its text.
    fn text_position(&self, at: TextPosition) -> TextPosition {
        let offset = match self.drawn() {
            Drawn::Text => return at,
            Drawn::Placeholder => TextOffset(0),
            Drawn::Mask => Segmenter::new(&self.text)
                .grapheme_boundaries()
                .nth(at.offset.0 / MASK.len_utf8())
                .unwrap_or(TextOffset(self.text.len())),
        };
        TextPosition { offset, ..at }
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
        let placed = placed.filter(|p| self.draws(p.layout.text));
        if !matches!(intent, EditIntent::Move { motion, .. } if motion.is_vertical()) {
            self.goal_x = None;
        }
        match intent {
            EditIntent::Insert(s) => self.type_text(s),
            EditIntent::Paste(s) => {
                let s = self.fold(s);
                self.replace_selection(&s, EditKind::Other);
            }
            EditIntent::Backspace(reach) => self.erase(false, *reach, placed),
            EditIntent::Delete(reach) => self.erase(true, *reach, placed),
            EditIntent::Cut => {
                let (start, end) = self.sel.logical_range();
                self.remove(start, end, EditKind::Other);
            }
            EditIntent::Move { motion, extend } => {
                self.history.close();
                self.move_caret(*motion, *extend, placed);
            }
            EditIntent::SelectAll => {
                self.history.close();
                self.sel = Selection {
                    anchor: TextPosition::downstream(TextOffset(0)),
                    focus: TextPosition::upstream(TextOffset(self.text.len())),
                };
            }
            EditIntent::PlaceAt { x, y, extend } => {
                if let Some(placed) = placed {
                    self.history.close();
                    self.place_at(placed, *x, *y, *extend);
                }
            }
            EditIntent::Compose { text, caret } => self.compose(text, *caret),
            EditIntent::CommitCompose(text) => self.commit_compose(text),
            EditIntent::Undo => self.walk_history(true),
            EditIntent::Redo => self.walk_history(false),
        }
    }

    /// Replace `[start, end)` of the text with `s`, advancing the revision when
    /// the text actually changes. The change is an undo step of `kind`, or
    /// part of the composition under way when there is no kind.
    fn splice(&mut self, start: TextOffset, end: TextOffset, s: &str, kind: Option<EditKind>) {
        if &self.text[start.0..end.0] == s {
            return;
        }
        let inverse = Splice {
            start: start.0,
            end: end.0,
            text: s.to_owned(),
        }
        .apply(&mut self.text);
        self.revision = self.revision.next();
        if self.look.secure {
            return;
        }
        match (kind, &mut self.composing) {
            (Some(kind), _) => {
                let space = !s.is_empty() && s.chars().all(char::is_whitespace);
                self.history.record(kind, self.sel, inverse, space);
            }
            (None, Some(step)) => step.push(inverse),
            (None, None) => {}
        }
    }

    /// Whether the field holds lines: a multi-line one that does not mask.
    pub fn holds_lines(&self) -> bool {
        self.look.multiline && !self.look.secure
    }

    /// `s` with its line breaks as the field holds them: each `\n` in a
    /// multi-line field, a space in a single-line one.
    fn fold<'s>(&self, s: &'s str) -> std::borrow::Cow<'s, str> {
        if self.holds_lines() {
            unify_breaks(s)
        } else {
            single_line(s)
        }
    }

    /// Type `s` over the selection: a typing run joins one undo step.
    fn type_text(&mut self, s: &str) {
        if !self.sel.is_caret() {
            self.history.close();
        }
        let s = self.fold(s);
        self.replace_selection(&s, EditKind::Typing);
    }

    /// Replace the current selection with `s`, collapsing to a caret after it.
    fn replace_selection(&mut self, s: &str, kind: EditKind) {
        self.end_composition();
        let (start, end) = self.sel.logical_range();
        self.splice(start, end, s, Some(kind));
        self.sel = Selection::caret(TextPosition::upstream(TextOffset(start.0 + s.len())));
    }

    /// Remove `[start, end)` as an edit of `kind`, leaving the caret there.
    fn remove(&mut self, start: TextOffset, end: TextOffset, kind: EditKind) {
        if start == end {
            return;
        }
        self.end_composition();
        self.splice(start, end, "", Some(kind));
        self.sel = Selection::caret(TextPosition::downstream(start));
    }

    /// Delete the selection, or from a bare caret as far as `reach` goes
    /// backward or `forward`. The caret ends at the start of the removed
    /// range.
    fn erase(&mut self, forward: bool, reach: Reach, placed: Option<Placed<'_>>) {
        if !self.sel.is_caret() {
            let (start, end) = self.sel.logical_range();
            self.history.close();
            self.remove(start, end, EditKind::Other);
            return;
        }
        let at = self.sel.focus.offset;
        let graphemes = Segmenter::new(&self.text);
        let grapheme = if forward {
            graphemes.next_grapheme(at)
        } else {
            graphemes.prev_grapheme(at)
        };
        let to = match reach {
            Reach::Grapheme => grapheme,
            Reach::Word if self.look.secure => self.edge(forward),
            Reach::Word if forward => TextOffset(word_end_after(&self.text, at.0)),
            Reach::Word => TextOffset(word_start_before(&self.text, at.0)),
            Reach::Line => {
                let motion = if forward {
                    Motion::LineEnd
                } else {
                    Motion::LineStart
                };
                match self.line_edge(motion, placed).offset {
                    // At a line's edge a line deletion takes the break.
                    edge if edge == at => grapheme,
                    edge => edge,
                }
            }
        };
        let (start, end) = if forward { (at, to) } else { (to, at) };
        let kind = match (reach, forward) {
            (Reach::Grapheme, false) => EditKind::Backspace,
            (Reach::Grapheme, true) => EditKind::Delete,
            _ => {
                self.history.close();
                EditKind::Other
            }
        };
        self.remove(start, end, kind);
    }

    /// The start of the text, or with `forward` its end.
    fn edge(&self, forward: bool) -> TextOffset {
        TextOffset(if forward { self.text.len() } else { 0 })
    }

    /// Move the caret per `motion`. With `extend`, the anchor stays put
    /// (shift-select); without it, an existing range collapses to its logical
    /// start (Left) or end (Right) and a bare caret steps.
    fn move_caret(&mut self, motion: Motion, extend: bool, placed: Option<Placed<'_>>) {
        let (start, end) = self.sel.logical_range();
        let layout = placed.map(|p| p.layout);
        let target = match motion {
            Motion::Left if !extend && !self.sel.is_caret() => TextPosition::downstream(start),
            Motion::Right if !extend && !self.sel.is_caret() => TextPosition::upstream(end),
            Motion::Left | Motion::Right => {
                let right = motion == Motion::Right;
                match layout {
                    Some(layout) => self.text_position(visual_step(
                        layout.lines,
                        self.drawn_position(self.sel.focus),
                        right,
                    )),
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
            Motion::WordLeft | Motion::WordRight => {
                let drawn = self.drawn_position(self.sel.focus);
                let rtl =
                    layout.is_some_and(|l| l.line_at(drawn).is_some_and(|(_, line)| line.rtl));
                let forward = (motion == Motion::WordRight) != rtl;
                let at = self.sel.focus.offset.0;
                if self.look.secure {
                    TextPosition::downstream(self.edge(forward))
                } else if forward {
                    TextPosition::upstream(TextOffset(word_end_after(&self.text, at)))
                } else {
                    TextPosition::downstream(TextOffset(word_start_before(&self.text, at)))
                }
            }
            Motion::LineStart | Motion::LineEnd => self.line_edge(motion, placed),
            Motion::Up | Motion::Down | Motion::PageUp | Motion::PageDown => {
                match self.vertical(motion, placed) {
                    Some(target) => target,
                    None => return,
                }
            }
            Motion::ParagraphStart => {
                let at = self.sel.focus.offset.0;
                let here = paragraph_start(&self.text, at);
                let to = if here == at && at > 0 {
                    paragraph_start(&self.text, at - 1)
                } else {
                    here
                };
                TextPosition::downstream(TextOffset(to))
            }
            Motion::ParagraphEnd => {
                let at = self.sel.focus.offset.0;
                let here = paragraph_end(&self.text, at);
                let to = if here == at && at < self.text.len() {
                    paragraph_end(&self.text, at + 1)
                } else {
                    here
                };
                TextPosition::upstream(TextOffset(to))
            }
            Motion::DocStart => TextPosition::downstream(TextOffset(0)),
            Motion::DocEnd => TextPosition::upstream(TextOffset(self.text.len())),
        };
        self.set_focus(target, extend);
    }

    /// The start or end of the line drawing the caret, of its paragraph when
    /// the text has no drawn geometry.
    fn line_edge(&self, motion: Motion, placed: Option<Placed<'_>>) -> TextPosition {
        let start = motion == Motion::LineStart;
        let drawn = self.drawn_position(self.sel.focus);
        if let Some(layout) = placed.map(|p| p.layout)
            && let Some((_, line)) = layout.line_at(drawn)
        {
            let (from, to) = line.logical_range;
            if start {
                return self.text_position(TextPosition::downstream(from));
            }
            // A hard break ends the line but is not on it.
            let to = if layout.text[from.0..to.0].ends_with('\n') {
                TextOffset(to.0 - 1)
            } else {
                to
            };
            return self.text_position(TextPosition::upstream(to));
        }
        let at = self.sel.focus.offset.0;
        if start {
            TextPosition::downstream(TextOffset(paragraph_start(&self.text, at)))
        } else {
            TextPosition::upstream(TextOffset(paragraph_end(&self.text, at)))
        }
    }

    /// Where a vertical `motion` takes the caret, keeping its column; `None`
    /// when it cannot tell without geometry.
    fn vertical(&mut self, motion: Motion, placed: Option<Placed<'_>>) -> Option<TextPosition> {
        let down = matches!(motion, Motion::Down | Motion::PageDown);
        let Some(placed) = placed else {
            // One paragraph moves to its ends; between paragraphs a line
            // takes geometry.
            return (!self.text.contains('\n')).then(|| TextPosition {
                offset: self.edge(down),
                affinity: self.sel.focus.affinity,
            });
        };
        let layout = placed.layout;
        let drawn = self.drawn_position(self.sel.focus);
        let (row, _) = layout.line_at(drawn)?;
        let x = *self.goal_x.get_or_insert(layout.caret_x(row, drawn));
        let rows = match motion {
            Motion::PageUp | Motion::PageDown if layout.line_height > 0.0 => {
                ((placed.extent.1 / layout.line_height).floor() as usize).max(1)
            }
            _ => 1,
        };
        let target = if down {
            row + rows
        } else {
            match row.checked_sub(rows) {
                Some(target) => target,
                None => return Some(TextPosition::downstream(TextOffset(0))),
            }
        };
        if target >= layout.lines.len() {
            return Some(TextPosition::upstream(TextOffset(self.text.len())));
        }
        Some(self.text_position(layout.position_at(target, x)))
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
        let row = row.min(layout.lines.len() - 1);
        let position = layout.position_at(row, local_x);
        self.set_focus(self.text_position(position), extend);
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

    /// Take back the last undo step, or with `undo` false make the last
    /// undone one again.
    fn walk_history(&mut self, undo: bool) {
        self.end_composition();
        let mut text = std::mem::take(&mut self.text);
        let sel = if undo {
            self.history.undo(&mut text, self.sel)
        } else {
            self.history.redo(&mut text, self.sel)
        };
        self.text = text;
        if let Some(sel) = sel {
            self.sel = sel;
            self.revision = self.revision.next();
        }
    }

    /// End any composition, keeping what it marked as typed.
    fn end_composition(&mut self) {
        self.composition.clear();
        if let Some(step) = self.composing.take() {
            self.history.record_step(step);
        }
    }

    /// Update the IME composition: replace the composition range (or the
    /// selection, if not composing yet) with `text`, keep it marked composing,
    /// and place the caret `caret` bytes into it. Empty `text` cancels.
    fn compose(&mut self, text: &str, caret: TextOffset) {
        let (start, end) = if self.composition.is_active() {
            self.composition.range()
        } else {
            self.history.close();
            self.composing = Some(Step::new(self.sel));
            self.sel.logical_range()
        };
        self.splice(start, end, text, None);
        if text.is_empty() {
            self.sel = Selection::caret(TextPosition::downstream(start));
            self.end_composition();
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
    /// `text` as committed content and end the composing state. With no
    /// composition the text is typed.
    fn commit_compose(&mut self, text: &str) {
        if !self.composition.is_active() {
            self.type_text(text);
            return;
        }
        let text = self.fold(text);
        let (start, end) = self.composition.replacement();
        self.splice(start, end, &text, None);
        self.sel = Selection::caret(TextPosition::upstream(TextOffset(start.0 + text.len())));
        self.end_composition();
    }
}

/// How many grapheme clusters `text` holds.
fn graphemes(text: &str) -> usize {
    Segmenter::new(text).grapheme_boundaries().count() - 1
}

/// Whether a word segment is a word, not a run of spaces or punctuation.
fn is_word(segment: &str) -> bool {
    segment.chars().any(|c| c.is_alphanumeric() || c == '_')
}

/// The start of the word before `at`, or the text's start.
fn word_start_before(text: &str, at: usize) -> usize {
    let bounds: Vec<TextOffset> = Segmenter::new(text).word_boundaries().collect();
    bounds
        .windows(2)
        .rev()
        .find(|w| w[0].0 < at && is_word(&text[w[0].0..w[1].0]))
        .map_or(0, |w| w[0].0)
}

/// The end of the word after `at`, or the text's end.
fn word_end_after(text: &str, at: usize) -> usize {
    let bounds: Vec<TextOffset> = Segmenter::new(text).word_boundaries().collect();
    bounds
        .windows(2)
        .find(|w| w[1].0 > at && is_word(&text[w[0].0..w[1].0]))
        .map_or(text.len(), |w| w[1].0)
}

/// The start of the paragraph holding `at`.
fn paragraph_start(text: &str, at: usize) -> usize {
    text[..at].rfind('\n').map_or(0, |i| i + 1)
}

/// The end of the paragraph holding `at`, before its break.
fn paragraph_end(text: &str, at: usize) -> usize {
    text[at..].find('\n').map_or(text.len(), |i| at + i)
}

/// The range that differs between `old` and `new`: its start and its end in
/// each, on character boundaries.
fn changed_range(old: &str, new: &str) -> (usize, usize, usize) {
    let prefix: usize = old
        .chars()
        .zip(new.chars())
        .take_while(|(a, b)| a == b)
        .map(|(a, _)| a.len_utf8())
        .sum();
    let suffix: usize = old[prefix..]
        .chars()
        .rev()
        .zip(new[prefix..].chars().rev())
        .take_while(|(a, b)| a == b)
        .map(|(a, _)| a.len_utf8())
        .sum();
    (prefix, old.len() - suffix, new.len() - suffix)
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

/// `s` with every line break (`\r\n`, `\n`, `\r`) as `\n`. Borrows when it
/// has no `\r`.
fn unify_breaks(s: &str) -> std::borrow::Cow<'_, str> {
    if !s.contains('\r') {
        return std::borrow::Cow::Borrowed(s);
    }
    std::borrow::Cow::Owned(s.replace("\r\n", "\n").replace('\r', "\n"))
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
/// [`NodeId::index`]. A node is editable once a buffer is registered for it or
/// once a handler records an edit intent on it, which starts it an empty
/// buffer. A dense `Vec` (not a per-node HashMap and not a NodeStore
/// column): the heap-heavy, rarely-touched buffer stays off the hot SoA columns,
/// and reconcile looks a buffer up by one index. Mirrors
/// [`crate::virtual_list::VirtualLists`].
#[derive(Default)]
pub struct TextEdits {
    /// `buffers[node_index]` is the buffer for that editable node with the node
    /// that owns it, or `None` for a non-editable node (the common case). A
    /// buffer whose owner was freed is never lent to the node that reuses its
    /// index.
    buffers: Vec<Option<(NodeId, Box<Buffer>)>>,
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
        self.buffers[i] = Some((node, buffer));
    }

    /// The buffer registered for `node`, if any.
    #[inline]
    pub fn get(&self, node: NodeId) -> Option<&Buffer> {
        match self.buffers.get(node.index() as usize) {
            Some(Some((owner, buffer))) if *owner == node => Some(buffer),
            _ => None,
        }
    }

    /// Mutable access to the buffer registered for `node`, if any.
    #[inline]
    pub fn get_mut(&mut self, node: NodeId) -> Option<&mut Buffer> {
        match self.buffers.get_mut(node.index() as usize) {
            Some(Some((owner, buffer))) if *owner == node => Some(buffer),
            _ => None,
        }
    }

    /// Unregister `node`'s buffer and hand it back, so a hot reload can carry
    /// it to the node that rebuilds `node`.
    pub fn take(&mut self, node: NodeId) -> Option<Box<Buffer>> {
        let slot = self.buffers.get_mut(node.index() as usize)?;
        match slot {
            Some((owner, _)) if *owner == node => slot.take().map(|(_, buffer)| buffer),
            _ => None,
        }
    }

    /// `node`'s buffer, starting it an empty one if it has none. The router
    /// queues a recorded intent here, so a node a handler edits needs no
    /// registration up front.
    pub fn editing(&mut self, node: NodeId) -> &mut Buffer {
        if self.get(node).is_none() {
            self.register(node, Box::default());
        }
        self.get_mut(node).expect("registered above")
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
/// Text field seeds the store queued ([`NodeStore::seed_text`]) apply first,
/// registering a buffer for a field that has none; a seed re-declares the text
/// only when it changed it, and never counts as an edit the driver reports.
///
/// [`NodeStore::seed_text`]: crate::component::NodeStore::seed_text
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
    let mut redeclared = 0;
    if store.has_text_seeds() {
        let mut seeds = Vec::new();
        store.take_text_seeds(&mut seeds);
        for (node, seed) in seeds {
            if !store.arena().is_live(node) {
                continue;
            }
            let buffer = match edits.get_mut(node) {
                Some(buffer) => {
                    if !buffer.seed(&seed) {
                        continue;
                    }
                    &*buffer
                }
                None => {
                    edits.register(node, Box::new(Buffer::with_seed(&seed)));
                    edits.get(node).expect("registered above")
                }
            };
            store.set_text_request(node, buffer.request());
            redeclared += 1;
        }
    }
    if store.has_edit_requests() {
        let mut requests = Vec::new();
        store.take_edit_requests(&mut requests);
        for (node, intent) in requests {
            if store.arena().is_live(node) {
                edits.editing(node).queue(intent);
            }
        }
    }
    for i in 0..edits.buffers.len() {
        let Some((node, buffer)) = edits.buffers[i].as_mut() else {
            continue;
        };
        if !buffer.is_dirty() {
            continue;
        }
        let node = *node;
        if !store.arena().is_live(node) {
            // The node was freed (whole-tree rebuild raced the registry clear);
            // drop the stale buffer defensively.
            edits.buffers[i] = None;
            continue;
        }
        let placed = geometry.and_then(|g| g.layout(node)).map(|layout| {
            let world = store.world(node);
            Placed {
                layout,
                origin: (world.x, world.y),
                extent: (world.w, world.h),
            }
        });
        let text_changed = buffer.apply_pending(placed);
        if !text_changed {
            continue;
        }
        // Re-declare the text self-contained from the buffer's resident style;
        // the original request was consumed by shaping and cannot be read back.
        store.set_text_request(node, buffer.request());
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
                offsets: &[],
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
                offsets: &[],
            },
            origin: (100.0, 50.0),
            extent: (200.0, 24.0),
        }
    }

    /// One laid-out line per paragraph of LTR `text`, every char one em wide;
    /// each line's range takes its hard break.
    fn lines_for(text: &str) -> Vec<LineLayout> {
        let bidi = BidiInfo::resolve(text, BaseDirection::LeftToRight);
        let mut lines = Vec::new();
        let mut start = 0;
        for paragraph in text.split_inclusive('\n') {
            let end = start + paragraph.trim_end_matches('\n').len();
            let range = (TextOffset(start), TextOffset(end));
            let spans: Vec<viso_text::paragraph::ShapedSpan> = bidi
                .direction_runs_in(range.0, range.1)
                .iter()
                .map(|run| {
                    let slice = &text[run.start.0..run.end.0];
                    let glyphs: Vec<ShapedGlyph> = slice
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
                    ShapedRun {
                        face: FontFaceId(0),
                        width_ems: glyphs.len() as f32,
                        glyphs,
                        text_len: slice.len() as u32,
                        ligature_carets: Vec::new(),
                    }
                    .into()
                })
                .collect();
            let mut line = LineLayout::in_range(text, &bidi, range, &spans);
            line.logical_range.1 = TextOffset(start + paragraph.len());
            lines.push(line);
            start += paragraph.len();
        }
        lines
    }

    fn moved(b: &mut Buffer, motion: Motion, placed: Option<Placed<'_>>) -> usize {
        b.apply_one(
            &EditIntent::Move {
                motion,
                extend: false,
            },
            placed,
        );
        at(b)
    }

    #[test]
    fn word_motions_skip_spaces_and_punctuation_to_word_edges() {
        let mut b = buf("hello, wide world", 17);
        assert_eq!(moved(&mut b, Motion::WordLeft, None), 12);
        assert_eq!(moved(&mut b, Motion::WordLeft, None), 7);
        assert_eq!(moved(&mut b, Motion::WordLeft, None), 0);
        assert_eq!(moved(&mut b, Motion::WordLeft, None), 0);
        assert_eq!(moved(&mut b, Motion::WordRight, None), 5);
        assert_eq!(moved(&mut b, Motion::WordRight, None), 11);
        assert_eq!(moved(&mut b, Motion::WordRight, None), 17);
    }

    #[test]
    fn word_and_line_deletions_reach_their_edges() {
        let mut b = buf("hello, wide world", 17);
        apply(&mut b, EditIntent::Backspace(Reach::Word));
        assert_eq!((b.text.as_str(), at(&b)), ("hello, wide ", 12));
        b.sel = Selection::caret(TextPosition::downstream(TextOffset(0)));
        apply(&mut b, EditIntent::Delete(Reach::Word));
        assert_eq!(b.text, ", wide ");
        b.sel = Selection::caret(TextPosition::downstream(TextOffset(2)));
        apply(&mut b, EditIntent::Delete(Reach::Line));
        assert_eq!(b.text, ", ");
        apply(&mut b, EditIntent::Backspace(Reach::Line));
        assert_eq!(b.text, "");
    }

    #[test]
    fn a_secure_field_moves_and_deletes_by_word_to_its_ends() {
        let mut b = buf("pass word", 9);
        b.look.secure = true;
        assert_eq!(moved(&mut b, Motion::WordLeft, None), 0);
        b.sel = Selection::caret(TextPosition::upstream(TextOffset(9)));
        apply(&mut b, EditIntent::Backspace(Reach::Word));
        assert_eq!(b.text, "");
    }

    #[test]
    fn paragraph_and_line_motions_without_geometry_follow_hard_breaks() {
        let mut b = buf("ab\ncd\nef", 4);
        assert_eq!(moved(&mut b, Motion::LineStart, None), 3);
        assert_eq!(moved(&mut b, Motion::LineEnd, None), 5);
        assert_eq!(moved(&mut b, Motion::ParagraphStart, None), 3);
        assert_eq!(moved(&mut b, Motion::ParagraphStart, None), 0);
        assert_eq!(moved(&mut b, Motion::ParagraphEnd, None), 2);
        assert_eq!(moved(&mut b, Motion::ParagraphEnd, None), 5);
        assert_eq!(moved(&mut b, Motion::DocEnd, None), 8);
        assert_eq!(moved(&mut b, Motion::DocStart, None), 0);
        let before = at(&b);
        assert_eq!(
            moved(&mut b, Motion::Down, None),
            before,
            "lines take geometry"
        );
        let mut one = buf("abc", 1);
        assert_eq!(moved(&mut one, Motion::Down, None), 3);
        assert_eq!(moved(&mut one, Motion::Up, None), 0);
    }

    #[test]
    fn up_and_down_keep_the_column_through_a_short_line() {
        let text = "abcdef\nab\nabcdef";
        let lines = lines_for(text);
        let mut b = buf(text, 5);
        let p = Some(placed(text, &lines));
        assert_eq!(moved(&mut b, Motion::Down, p), 9, "the short line's end");
        assert_eq!(moved(&mut b, Motion::Down, p), 15, "back at the column");
        assert_eq!(
            moved(&mut b, Motion::Down, p),
            16,
            "past the last line: the end"
        );
        b.sel = Selection::caret(TextPosition::downstream(TextOffset(15)));
        assert_eq!(
            moved(&mut b, Motion::PageUp, p),
            5,
            "a page is two lines here"
        );
        assert_eq!(
            moved(&mut b, Motion::Up, p),
            0,
            "above the first line: the start"
        );
        b.sel = Selection::caret(TextPosition::downstream(TextOffset(2)));
        assert_eq!(
            moved(&mut b, Motion::LineEnd, p),
            6,
            "the break is not on the line"
        );
        assert_eq!(moved(&mut b, Motion::LineStart, p), 0);
    }

    #[test]
    fn select_all_selects_the_whole_text() {
        let mut b = buf("abc", 1);
        apply(&mut b, EditIntent::SelectAll);
        assert_eq!(b.sel.logical_range(), (TextOffset(0), TextOffset(3)));
    }

    fn typed(b: &mut Buffer, s: &str) {
        for c in s.chars() {
            apply(b, EditIntent::CommitCompose(c.to_string()));
        }
    }

    #[test]
    fn typing_undoes_a_word_at_a_time_and_redoes() {
        let mut b = buf("", 0);
        typed(&mut b, "ab cd");
        apply(&mut b, EditIntent::Undo);
        assert_eq!((b.text.as_str(), at(&b)), ("ab ", 3));
        apply(&mut b, EditIntent::Undo);
        assert_eq!(b.text, "");
        apply(&mut b, EditIntent::Undo);
        assert_eq!(b.text, "", "nothing more to undo");
        apply(&mut b, EditIntent::Redo);
        apply(&mut b, EditIntent::Redo);
        assert_eq!((b.text.as_str(), at(&b)), ("ab cd", 5));
    }

    #[test]
    fn a_caret_move_paste_and_cut_each_start_a_step() {
        let mut b = buf("", 0);
        typed(&mut b, "ab");
        step(&mut b, Motion::Left);
        typed(&mut b, "x");
        apply(&mut b, EditIntent::Paste("12\n3".into()));
        assert_eq!(b.text, "ax12 3b", "a single-line paste folds its breaks");
        apply(&mut b, EditIntent::SelectAll);
        apply(&mut b, EditIntent::Cut);
        assert_eq!(b.text, "");
        for expect in ["ax12 3b", "axb", "ab", ""] {
            apply(&mut b, EditIntent::Undo);
            assert_eq!(b.text, expect);
        }
    }

    #[test]
    fn backspaces_in_a_row_are_one_step_and_restore_the_selection() {
        let mut b = buf("abcd", 4);
        apply(&mut b, EditIntent::Backspace(Reach::Grapheme));
        apply(&mut b, EditIntent::Backspace(Reach::Grapheme));
        assert_eq!(b.text, "ab");
        apply(&mut b, EditIntent::Undo);
        assert_eq!((b.text.as_str(), at(&b)), ("abcd", 4));
    }

    #[test]
    fn a_composition_is_one_step() {
        let mut b = buf("ab", 2);
        for preedit in ["n", "ni"] {
            apply(
                &mut b,
                EditIntent::Compose {
                    text: preedit.into(),
                    caret: TextOffset(preedit.len()),
                },
            );
        }
        apply(&mut b, EditIntent::CommitCompose("你".into()));
        typed(&mut b, "x");
        assert_eq!(b.text, "ab你x");
        apply(&mut b, EditIntent::Undo);
        assert_eq!(b.text, "ab你");
        apply(&mut b, EditIntent::Undo);
        assert_eq!((b.text.as_str(), at(&b)), ("ab", 2));
    }

    #[test]
    fn a_secure_field_keeps_no_history() {
        let mut b = buf("", 0);
        b.look.secure = true;
        typed(&mut b, "pw");
        apply(&mut b, EditIntent::Undo);
        assert_eq!(b.text, "pw");
    }

    #[test]
    fn a_new_value_from_outside_is_one_step() {
        let mut b = Buffer::with_seed(&seed("draft"));
        assert!(b.seed(&seed("")));
        assert_eq!(b.text, "");
        apply(&mut b, EditIntent::Undo);
        assert_eq!(b.text, "draft");
    }

    #[test]
    fn a_multi_line_field_keeps_its_breaks_and_wraps() {
        let mut b = buf("ab", 2);
        b.look.multiline = true;
        apply(&mut b, EditIntent::Insert("\n".into()));
        apply(&mut b, EditIntent::Paste("c\r\nd\re".into()));
        assert_eq!(b.text, "ab\nc\nd\ne");
        assert!(b.request().soft_wrap);
        assert!(!buf("x", 0).request().soft_wrap);
    }

    #[test]
    fn the_changed_range_keeps_the_common_ends() {
        assert_eq!(
            changed_range("hello world", "hello brave world"),
            (6, 6, 12)
        );
        assert_eq!(changed_range("abc", "abc"), (3, 3, 3));
        assert_eq!(changed_range("aé", "aè"), (1, 3, 3));
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
        apply(&mut b, EditIntent::Backspace(Reach::Grapheme));
        assert_eq!(b.text, "ab");
        assert_eq!(at(&b), 2);
    }

    #[test]
    fn backspace_at_start_is_noop() {
        let mut b = buf("abc", 0);
        let revision = b.revision();
        apply(&mut b, EditIntent::Backspace(Reach::Grapheme));
        assert_eq!(b.text, "abc");
        assert_eq!(at(&b), 0);
        assert_eq!(b.revision(), revision);
    }

    #[test]
    fn delete_at_caret_removes_next_char() {
        let mut b = buf("abc", 0);
        apply(&mut b, EditIntent::Delete(Reach::Grapheme));
        assert_eq!(b.text, "bc");
        assert_eq!(at(&b), 0);
    }

    #[test]
    fn delete_removes_selection() {
        let mut b = Buffer::with_text("hello");
        b.sel = range(1, 4);
        apply(&mut b, EditIntent::Delete(Reach::Grapheme));
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
        step(&mut b, Motion::LineEnd);
        assert_eq!(at(&b), 3);
        step(&mut b, Motion::LineStart);
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
        apply(&mut b, EditIntent::Delete(Reach::Grapheme));
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
        apply(&mut b, EditIntent::Backspace(Reach::Grapheme));
        assert_eq!(b.text, family);
        apply(&mut b, EditIntent::Backspace(Reach::Grapheme));
        assert_eq!(b.text, "");
    }

    #[test]
    fn decomposed_text_is_kept_as_typed() {
        // NFD input survives an edit round-trip byte for byte: the buffer never
        // normalizes what the user or IME produced.
        let nfd = "Cafe\u{301}";
        let mut b = buf(nfd, nfd.len());
        apply(&mut b, EditIntent::Insert("!".into()));
        apply(&mut b, EditIntent::Backspace(Reach::Grapheme));
        assert_eq!(b.text, nfd);
        apply(&mut b, EditIntent::Backspace(Reach::Grapheme));
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

    fn request(text: &str) -> TextRequest {
        TextRequest {
            text: text.to_owned(),
            font_size: FontSize::Fixed(14.0),
            color: Rgba {
                r: 1.0,
                g: 1.0,
                b: 1.0,
                a: 0.8,
            },
            soft_wrap: false,
            locale: None,
            style: TextStyle::default(),
        }
    }

    fn seed(text: &str) -> FieldSeed {
        FieldSeed {
            request: request(text),
            valued: true,
            look: FieldLook::default(),
        }
    }

    fn field(text: &str, placeholder: &str, secure: bool) -> Buffer {
        Buffer::with_seed(&FieldSeed {
            look: FieldLook {
                placeholder: placeholder.to_owned(),
                secure,
                ..FieldLook::default()
            },
            ..seed(text)
        })
    }

    #[test]
    fn an_empty_field_draws_its_placeholder_dimmed_until_it_holds_text() {
        let mut b = field("", "Name", false);
        let drawn = b.request();
        assert_eq!(drawn.text, "Name");
        assert_eq!(drawn.color.a, 0.4, "half its text color's opacity");
        assert!(b.draws("Name") && !b.draws(""));
        let lines = line_for("Name");
        let click = EditIntent::PlaceAt {
            x: 130.0,
            y: 51.0,
            extend: false,
        };
        b.apply_one(&click, Some(placed("Name", &lines)));
        assert_eq!(
            b.sel.focus.offset,
            TextOffset(0),
            "a click on it starts the text"
        );
        apply(&mut b, EditIntent::Insert("A".into()));
        let drawn = b.request();
        assert_eq!((drawn.text.as_str(), drawn.color.a), ("A", 0.8));
    }

    #[test]
    fn a_secure_field_draws_a_bullet_per_grapheme_and_edits_its_text_through_them() {
        // `a`, `e` with a combining acute, `b`: three graphemes in five bytes.
        let text = "ae\u{301}b";
        let mut b = field(text, "Password", true);
        let drawn = b.request().text;
        assert_eq!(drawn, "\u{2022}\u{2022}\u{2022}");
        assert!(b.draws(&drawn) && !b.draws(text));
        assert_eq!(b.drawn_position(b.sel.focus).offset, TextOffset(9));
        let lines = line_for(&drawn);
        let click = EditIntent::PlaceAt {
            x: 120.0,
            y: 51.0,
            extend: false,
        };
        b.apply_one(&click, Some(placed(&drawn, &lines)));
        assert_eq!(
            b.sel.focus.offset,
            TextOffset(4),
            "after the second grapheme"
        );
        let left = EditIntent::Move {
            motion: Motion::Left,
            extend: false,
        };
        b.apply_one(&left, Some(placed(&drawn, &lines)));
        assert_eq!(b.sel.focus.offset, TextOffset(1), "a whole grapheme back");
        let clear = line_for(text);
        b.apply_one(&click, Some(placed(text, &clear)));
        assert_eq!(
            b.sel.focus.offset,
            TextOffset(1),
            "the clear text's lines describe nothing"
        );
        apply(&mut b, EditIntent::Delete(Reach::Grapheme));
        assert_eq!(b.text, "ab");
        assert_eq!(b.request().text, "\u{2022}\u{2022}");
        b.text.clear();
        assert_eq!(
            b.request().text,
            "Password",
            "empty, it draws its placeholder"
        );
    }

    #[test]
    fn a_field_no_value_drives_keeps_its_text_through_a_seed() {
        let mut b = Buffer::with_text("typed");
        let restyle = FieldSeed {
            valued: false,
            look: FieldLook {
                placeholder: "Hint".into(),
                ..FieldLook::default()
            },
            ..seed("")
        };
        assert!(b.seed(&restyle), "its style and look changed");
        assert_eq!(
            (b.text.as_str(), b.look.placeholder.as_str()),
            ("typed", "Hint")
        );
        assert!(!b.seed(&restyle));
        assert_eq!(Buffer::with_seed(&restyle).request().text, "Hint");
    }

    #[test]
    fn a_seed_registers_a_field_and_replaces_only_a_different_text() {
        let mut store = crate::component::NodeStore::new();
        let id = live_node(&mut store);
        let mut edits = TextEdits::new();
        let mut requests = Vec::new();
        store.seed_text(id, seed("Ada"));
        assert_eq!(reconcile(&mut store, &mut edits, None), 1);
        let buffer = edits.get(id).expect("the seed registers a buffer");
        assert_eq!(
            (buffer.text.as_str(), buffer.font_size),
            ("Ada", FontSize::Fixed(14.0))
        );
        store.take_text_requests(&mut requests);
        assert_eq!(requests.len(), 1, "the seeded text is declared");

        // The caret moved by the user survives a seed of the text it holds.
        edits.get_mut(id).unwrap().queue(EditIntent::Move {
            motion: Motion::LineStart,
            extend: false,
        });
        reconcile(&mut store, &mut edits, None);
        store.seed_text(id, seed("Ada"));
        assert_eq!(reconcile(&mut store, &mut edits, None), 0);
        assert_eq!(edits.get(id).unwrap().sel.focus.offset, TextOffset(0));
        store.take_text_requests(&mut requests);
        assert!(requests.is_empty(), "an equal seed declares nothing");

        store.seed_text(id, seed("Grace"));
        assert_eq!(reconcile(&mut store, &mut edits, None), 1);
        let buffer = edits.get(id).unwrap();
        assert_eq!(buffer.text, "Grace");
        assert_eq!(buffer.sel.focus.offset, TextOffset(5), "caret at the end");
        let mut changed = Vec::new();
        edits.take_changed(&mut changed);
        assert!(changed.is_empty(), "a seed is not an edit");
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
        edits
            .get_mut(id)
            .unwrap()
            .queue(EditIntent::Backspace(Reach::Grapheme));
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
