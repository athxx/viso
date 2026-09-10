//! The paragraph pipeline: the world-ready join of segmentation, BiDi, shaping,
//! line breaking, and width fit into laid-out lines, with a last-good result and
//! caching.
//!
//! A paragraph owns the full logical-to-visual layout. It reshapes only when
//! text, font, features, or width change (per the text caching contract), keeps
//! the last successfully laid-out result while a reflow is pending, and runs its
//! heavy work on a worker. It is the surface caret, hit testing, and selection
//! read.
//!
//! # Logical versus visual order
//!
//! Logical order — the UTF-8 source and its [`crate::TextOffset`]s — is the
//! single source of truth; selection, copy, and undo always speak it. The
//! logical-to-visual reorder itself already exists one layer down: BiDi resolves
//! embedding levels and [`crate::bidi::BidiInfo::visual_order`] /
//! [`crate::bidi::BidiInfo::direction_runs_in`] give the reordered run sequence.
//! What a paragraph adds on top is per-line: the visual reorder is applied only
//! after line boundaries are known (never by reversing whole runs before
//! breaking), so the paragraph's own logical-to-visual caret map — the visual
//! runs and their inline geometry — is built during line formation.
//!
//! # The retained visual layout caret, hit test, and selection read
//!
//! [`LineLayout`] is the retained per-line metadata those three consumers read.
//! It is deliberately a value type produced once by layout and then queried
//! without reshaping: a steady-state pointer move or a caret keypress must never
//! trigger shaping (the text caching contract, and the hit-test contract that
//! the hit-test map is retained paragraph metadata). Each line owns its visual
//! runs in left-to-right visual order, and each [`VisualRun`] carries the
//! spec-12.12 field contract — its logical text range, its visual inline extent,
//! its direction and embedding level, the shaped-run face identity, and the
//! cluster map that ties source byte offsets to inline positions. The forbidden
//! shape is keeping only a flat visual glyph array and dropping the logical
//! mapping; a run here can always answer "which source offset is under this
//! inline x" and "where does this source offset sit inline", in both LTR and RTL
//! runs, which is what makes BiDi-correct caret placement and hit testing
//! possible without re-deriving anything.
//!
//! Full line breaking and width fit — turning a paragraph plus a width into this
//! line set, incrementally and cached — is the next slice; this layer defines
//! the retained layout the caret/selection/hit-test readers are written against
//! and the single-line construction they exercise.

use crate::bidi::{BaseDirection, BidiInfo, BidiLevel};
use crate::line_break::{BreakOpportunity, LineBreaker};
use crate::segment::Segmenter;
use crate::shaping::{Direction, ShapedRun};
use crate::text_position::TextOffset;

/// A caret stop within a run: a legal grapheme boundary paired with the inline
/// coordinate it sits at, in the run's own visual space.
///
/// Inline coordinates increase left to right in visual space regardless of run
/// direction: for an LTR run the first stop is at the run's left edge, for an
/// RTL run the first *logical* stop is at the run's right edge. Storing the
/// resolved visual x per stop is what lets hit testing and caret placement stay
/// direction-correct without re-deriving glyph order — an RTL run's clusters are
/// non-increasing in x, so a naive byte-order scan (the makepad hazard) would
/// misplace the caret.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CaretStop {
    /// The grapheme-boundary source byte offset this stop lands on.
    pub offset: TextOffset,
    /// The inline x of this stop within the line, in em units, increasing left
    /// to right in visual space.
    pub inline_x: f32,
}

/// One visual run within a line: a maximal same-level span in the line's visual
/// (left-to-right) order, carrying the full logical-to-visual mapping.
///
/// This is the spec-12.12 run contract. It never degrades to a bare glyph array:
/// `logical_range` keeps the source truth, `caret_stops` ties every legal caret
/// position to its inline x (built from grapheme boundaries and shaped-glyph
/// advances), and `direction`/`level` record how logical order maps to visual
/// order within the run.
#[derive(Debug, Clone)]
pub struct VisualRun {
    /// The run's source byte range `[start, end)`, in logical order — the source
    /// truth this run maps from. Never reversed.
    pub logical_range: (TextOffset, TextOffset),
    /// The run's inline extent within the line `[left, right)`, in em units,
    /// left-to-right in visual space. `right - left` is the run's advance width.
    pub visual_inline_range: (f32, f32),
    /// The direction the run was shaped and is drawn in.
    pub direction: Direction,
    /// The run's resolved UAX#9 embedding level.
    pub level: BidiLevel,
    /// The face every glyph in the run was shaped with — the shaped-run
    /// identity, retained so a re-raster or re-upload does not need to reshape.
    pub face: crate::FontFaceId,
    /// Legal caret stops within the run, one per grapheme boundary in
    /// `logical_range`, each paired with its resolved inline x. Ordered by
    /// logical offset (ascending); the inline x is monotonic left-to-right for
    /// an LTR run and monotonic right-to-left for an RTL run. The run's two
    /// logical endpoints are always present.
    pub caret_stops: Vec<CaretStop>,
}

impl VisualRun {
    /// The run's advance width in em units.
    pub fn width(&self) -> f32 {
        self.visual_inline_range.1 - self.visual_inline_range.0
    }

    /// Whether a source offset falls within this run's logical range, endpoints
    /// included at the appropriate side. The end is exclusive so adjacent runs
    /// do not both claim the boundary; the caller resolves a boundary offset via
    /// affinity across the two runs.
    pub fn contains_offset(&self, offset: TextOffset) -> bool {
        offset >= self.logical_range.0 && offset < self.logical_range.1
    }

    /// The inline x of a caret stop at `offset`, if `offset` is one of this
    /// run's stops. Interior (non-boundary) offsets are not stops and return
    /// `None`.
    pub fn inline_x_of(&self, offset: TextOffset) -> Option<f32> {
        self.caret_stops
            .iter()
            .find(|s| s.offset == offset)
            .map(|s| s.inline_x)
    }
}

/// One laid-out line: its visual runs left to right, and the line's own logical
/// span and inline extent.
///
/// A line never spans a BiDi paragraph. Its runs are ordered by visual position
/// (left to right); each run maps back to a logical source range that need not
/// be contiguous with its neighbours' (that is exactly what BiDi reordering
/// produces).
#[derive(Debug, Clone, Default)]
pub struct LineLayout {
    /// The line's runs in visual left-to-right order.
    pub runs: Vec<VisualRun>,
    /// The line's source byte range `[start, end)` in logical order — the union
    /// of its runs' logical ranges.
    pub logical_range: (TextOffset, TextOffset),
    /// The line's total inline width in em units.
    pub width: f32,
}

impl LineLayout {
    /// Lay out a single line spanning the whole of `text` under one base
    /// direction, given the shaped runs for its logical direction runs.
    ///
    /// This is the world-ready single-line construction the caret / hit-test /
    /// selection readers are written against: it resolves BiDi levels, splits
    /// logical direction runs, orders them into visual runs left to right (UAX#9
    /// rule L2), and builds each run's caret-stop / inline-x map from grapheme
    /// boundaries and shaped-glyph advances. Multi-line breaking and width fit
    /// layer on top of this per-line primitive.
    ///
    /// `shaped` supplies, for each logical direction run in `text` (in the order
    /// [`BidiInfo::direction_runs_in`] yields them over the whole text), the
    /// [`ShapedRun`] the shaper produced for that run's substring. A run with no
    /// shaped entry (an empty or unshaped span) contributes a zero-width run.
    pub fn single_line(text: &str, bidi: &BidiInfo, shaped: &[ShapedRun]) -> Self {
        let whole = (TextOffset(0), TextOffset(text.len()));
        Self::in_range(text, bidi, whole, shaped)
    }

    /// Lay out one line covering the source sub-range `[range.0, range.1)`,
    /// slicing into a paragraph-wide `bidi` resolution.
    ///
    /// BiDi is paragraph-context-sensitive (spec 12.19), so the embedding levels
    /// must come from a resolution over the whole paragraph, not the line slice;
    /// this reuses that shared `bidi` and only restricts the direction-run
    /// enumeration and visual reorder to the line's range (UAX#9 rule L2 applies
    /// per display line). `shaped` supplies one [`ShapedRun`] per logical
    /// direction run *within the range*, in [`BidiInfo::direction_runs_in`]
    /// order; a missing entry contributes a zero-width run.
    pub fn in_range(
        text: &str,
        bidi: &BidiInfo,
        range: (TextOffset, TextOffset),
        shaped: &[ShapedRun],
    ) -> Self {
        let logical_runs = bidi.direction_runs_in(range.0, range.1);

        // Visual (left-to-right) order of the line's source offsets under rule
        // L2. A run's visual position is the visual position of its logical
        // start; ordering runs by that key reproduces the reordered run
        // sequence without re-running the reorder per run.
        let visual = bidi.visual_order(range.0, range.1);
        let mut visual_rank = std::collections::HashMap::with_capacity(visual.len());
        for (rank, off) in visual.iter().enumerate() {
            visual_rank.insert(*off, rank);
        }

        // Pair each logical direction run with its shaped run (same order as
        // `direction_runs_in`) and sort into visual order.
        let mut ordered: Vec<(usize, &crate::bidi::DirectionRun, Option<&ShapedRun>)> =
            logical_runs
                .iter()
                .enumerate()
                .map(|(i, run)| {
                    let rank = visual_rank.get(&run.start).copied().unwrap_or(usize::MAX);
                    (rank, run, shaped.get(i))
                })
                .collect();
        ordered.sort_by_key(|(rank, _, _)| *rank);

        let mut runs = Vec::with_capacity(ordered.len());
        let mut cursor_x = 0.0f32;
        for (_, run, shaped) in ordered {
            let run = build_visual_run(text, run, shaped, cursor_x);
            cursor_x = run.visual_inline_range.1;
            runs.push(run);
        }

        Self {
            runs,
            logical_range: range,
            width: cursor_x,
        }
    }
}

/// Build one [`VisualRun`] from its logical direction run and shaped glyphs,
/// placed starting at inline `left`.
///
/// Caret stops come from the grapheme boundaries within the run's logical range
/// (never from glyph count — a ligature merges graphemes, marks split one). Each
/// stop's inline x is accumulated from shaped-glyph advances, walked in the run's
/// visual direction so an RTL run's stops carry decreasing-in-logical-order but
/// left-to-right-correct visual x.
fn build_visual_run(
    text: &str,
    run: &crate::bidi::DirectionRun,
    shaped: Option<&ShapedRun>,
    left: f32,
) -> VisualRun {
    let (start, end) = (run.start, run.end);
    let width = shaped.map(|s| s.width_ems).unwrap_or(0.0);
    let right = left + width;

    // The grapheme boundaries within the run, in logical order — the legal caret
    // stops. `grapheme_boundaries` over the run substring yields offsets relative
    // to the substring; rebase to absolute source offsets.
    let sub = &text[start.0..end.0];
    let boundaries: Vec<TextOffset> = Segmenter::new(sub)
        .grapheme_boundaries()
        .map(|b| TextOffset(start.0 + b.0))
        .collect();

    // The inline x of a source offset within this run. Advances are summed per
    // shaping cluster; a caret stop's x is the accumulated advance of every
    // cluster that begins strictly before it. For an LTR run inline x increases
    // with logical offset from `left`; for an RTL run the first logical offset
    // sits at `right` and inline x decreases as logical offset increases, so the
    // caret stays left-to-right correct in visual space.
    let advance_before = |offset: TextOffset| -> f32 {
        let Some(shaped) = shaped else { return 0.0 };
        let local = (offset.0 - start.0) as u32;
        shaped
            .glyphs
            .iter()
            // One advance per cluster: sum a glyph's advance once for the run of
            // glyphs sharing its cluster. Summing every glyph would double-count
            // marks, which share their base's cluster and carry zero advance in
            // practice, but guarding on cluster keeps it correct regardless.
            .filter(|g| g.cluster < local)
            .map(|g| g.x_advance)
            .sum()
    };

    let caret_stops: Vec<CaretStop> = boundaries
        .iter()
        .map(|&offset| {
            let adv = advance_before(offset);
            let inline_x = match run.direction {
                Direction::LeftToRight => left + adv,
                Direction::RightToLeft => right - adv,
            };
            CaretStop { offset, inline_x }
        })
        .collect();

    VisualRun {
        logical_range: (start, end),
        visual_inline_range: (left, right),
        direction: run.direction,
        level: run.level,
        face: shaped.map(|s| s.face).unwrap_or(crate::FontFaceId(0)),
        caret_stops,
    }
}

/// Translate a retained line from the pre-edit coordinate space into the
/// post-edit space by shifting every offset at or after the edit point `from` by
/// `delta` bytes.
///
/// Lines wholly before the edit (`end <= from`) are unchanged: their offsets are
/// identical in both spaces, so they return a clone. A line at or after the edit
/// (`start >= from`) shifts wholesale — its logical range and every caret stop's
/// offset move by `delta`, while inline geometry (widths, inline x) is unaffected
/// because the shifted text is byte-for-byte the same glyphs. A line straddling
/// `from` is never a stable-stop match candidate (its own signature changed), so
/// it is returned unshifted; the reflow recomputes it regardless.
fn shift_line(line: &LineLayout, from: TextOffset, delta: isize) -> LineLayout {
    let shift = |o: TextOffset| TextOffset((o.0 as isize + delta) as usize);
    // Only lines fully at or after the edit translate cleanly.
    if delta == 0 || line.logical_range.0 < from {
        return line.clone();
    }
    let runs = line
        .runs
        .iter()
        .map(|run| VisualRun {
            logical_range: (shift(run.logical_range.0), shift(run.logical_range.1)),
            caret_stops: run
                .caret_stops
                .iter()
                .map(|s| CaretStop {
                    offset: shift(s.offset),
                    inline_x: s.inline_x,
                })
                .collect(),
            ..run.clone()
        })
        .collect();
    LineLayout {
        runs,
        logical_range: (shift(line.logical_range.0), shift(line.logical_range.1)),
        width: line.width,
    }
}

/// The version key a paragraph's cached layout was computed against.
///
/// Layout is reused only when this key is unchanged (spec section 20: unchanged
/// text/font/features/width must not reshape or reflow). It deliberately excludes
/// nothing that changes glyph geometry or fit: the source text, the base
/// direction, a caller-opaque style epoch standing for font + shaping features,
/// and the target width. Two keys comparing equal guarantees the same laid-out
/// result, so a cache hit is a true no-op.
#[derive(Debug, Clone, Copy, PartialEq)]
struct LayoutKey {
    /// Hash of the source text — cheap to compare, and combined with `text_len`
    /// makes an accidental collision changing the result astronomically
    /// unlikely.
    text_hash: u64,
    /// Source length in bytes, guarding the hash.
    text_len: usize,
    /// The paragraph base direction.
    base: BaseDirection,
    /// A caller-opaque epoch standing for the font faces and shaping features in
    /// effect. The caller bumps it when either changes; equal epoch means equal
    /// geometry.
    style_epoch: u64,
    /// The target wrap width in em units, bit-compared so `NaN`/`-0.0` never
    /// alias a real width.
    width_bits: u32,
}

impl LayoutKey {
    fn new(text: &str, base: BaseDirection, style_epoch: u64, width: f32) -> Self {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        text.hash(&mut hasher);
        Self {
            text_hash: hasher.finish(),
            text_len: text.len(),
            base,
            style_epoch,
            width_bits: width.to_bits(),
        }
    }
}

/// A per-line signature used by incremental reflow's stable-stop condition.
///
/// When an edit reflows lines forward, propagation stops at the first line whose
/// signature matches the pre-edit layout's line at the same logical start (spec
/// 12.19): a matching end offset and inline width means the line, and every line
/// after it, is unaffected and its old layout can be spliced back unchanged. The
/// signature is exactly what makes "reflow only what changed" equal a full
/// recompute — it never stops early on a line that would in fact differ.
#[derive(Debug, Clone, Copy, PartialEq)]
struct LineSignature {
    /// The line's logical start offset.
    start: TextOffset,
    /// The line's logical end offset.
    end: TextOffset,
    /// The line's inline width in em units, bit-compared.
    width_bits: u32,
}

impl LineSignature {
    fn of(line: &LineLayout) -> Self {
        Self {
            start: line.logical_range.0,
            end: line.logical_range.1,
            width_bits: line.width.to_bits(),
        }
    }
}

/// A laid-out paragraph: the retained, world-ready multi-line layout, with a
/// version-keyed reuse gate and edit-scoped incremental reflow.
///
/// The paragraph owns its source text and base direction and produces a set of
/// display lines fitted to a target width by breaking at UAX#14 opportunities. It
/// reshapes/reflows only when its [`LayoutKey`] changes (spec section 20), and an
/// edit reflows forward only until a line signature restabilizes (spec 12.19) —
/// but the incremental result is defined to equal a full recompute, and the
/// [`Self::layout_full`] path exists precisely so the two can be checked against
/// each other.
///
/// Shaping is injected: [`Self::layout`] takes a shaping callback rather than
/// owning font bytes, so the crate stays headless and a single shaper drives both
/// the incremental and full paths identically.
#[derive(Debug)]
pub struct Paragraph {
    text: String,
    base: BaseDirection,
    style_epoch: u64,
    /// The last laid-out lines, or empty before the first layout.
    lines: Vec<LineLayout>,
    /// The key `lines` were computed against, when valid.
    key: Option<LayoutKey>,
    /// The lowest byte offset touched since the last layout, if any — the
    /// incremental reflow entry point. `None` means no edit since the last
    /// layout (a width-only change still reflows from the top, gated by the key).
    dirty_from: Option<TextOffset>,
    /// The net byte-length change of all edits since the last layout. Offsets in
    /// the cached lines are in the pre-edit coordinate space up to `dirty_from`
    /// and shifted by this delta from there on; the incremental reflow uses it to
    /// translate the retained tail into the new coordinate space before matching
    /// signatures and splicing.
    dirty_delta: isize,
    /// The highest byte offset touched by any edit since the last layout, in the
    /// post-edit coordinate space — the stable-stop floor. A stop may only fire at
    /// or after this offset: with several disjoint edits, a line between two of
    /// them can match its old signature yet sit before a later edit, so stopping
    /// there would splice a stale edited line back. `None` means no edit pending.
    dirty_to: Option<TextOffset>,
    /// Cumulative count of genuine shape invocations — every cache miss that
    /// reached the caller's shaper — since this paragraph was created. A pure
    /// cache-hit [`Self::layout`] returns before any shaping, so this stays put
    /// across a steady-state frame; the delta over a `layout` call is exactly the
    /// number of runs actually (re)shaped, which the steady-state contract test
    /// asserts is zero.
    shape_calls: u64,
}

/// A shaping callback: shape a source substring in a resolved direction into a
/// [`ShapedRun`]. The paragraph calls it once per logical direction run per line;
/// the caller resolves the face and holds the font bytes.
pub type ShapeFn<'a> = dyn FnMut(&str, Direction) -> ShapedRun + 'a;

/// A layout-scoped memo over the caller's [`ShapeFn`] that shapes each distinct
/// `(byte range, direction)` at most once per layout pass.
///
/// Within one layout the same substring is asked for repeatedly: `fit_line`
/// measures every break candidate — re-shaping each direction run as the line
/// grows — and then `build_line` shapes the winning line's runs again. Left to
/// the raw callback that is quadratic re-shaping of identical inputs. This memo
/// keys on the source byte range (a direction run's `[start, end)` uniquely
/// identifies its substring within a fixed BiDi resolution, so `direction` is
/// carried only to disambiguate defensively) and returns a retained
/// [`ShapedRun`] on a hit. The cache lives for one `compute_lines` call and is
/// dropped after, so nothing is retained across frames here — the steady-state
/// no-reshape guarantee comes from the [`LayoutKey`] gate one level up, which
/// returns before a cache is ever built.
///
/// Every genuine miss (a real call into the caller's shaper) bumps a counter the
/// paragraph exposes, which is what proves a steady-state frame shapes nothing.
struct ShapeCache<'f, 's> {
    shape: &'f mut ShapeFn<'s>,
    runs: std::collections::HashMap<(u32, u32, Direction), ShapedRun>,
    misses: u64,
}

impl<'f, 's> ShapeCache<'f, 's> {
    fn new(shape: &'f mut ShapeFn<'s>) -> Self {
        Self {
            shape,
            runs: std::collections::HashMap::new(),
            misses: 0,
        }
    }

    /// The shaped run for `text[range.0..range.1]` in `direction`, shaping through
    /// the caller's callback only on a miss. `text` is the paragraph source; the
    /// key is the byte range so identical substrings collapse to one shape.
    fn shape(
        &mut self,
        text: &str,
        range: (TextOffset, TextOffset),
        direction: Direction,
    ) -> &ShapedRun {
        let key = (range.0.0 as u32, range.1.0 as u32, direction);
        // `entry` would borrow `self.runs` across the miss closure that also needs
        // `self.shape`/`self.misses`; split the lookup so the miss path is a plain
        // insert with no overlapping borrow.
        if !self.runs.contains_key(&key) {
            let sub = &text[range.0.0..range.1.0];
            let run = (self.shape)(sub, direction);
            self.misses += 1;
            self.runs.insert(key, run);
        }
        &self.runs[&key]
    }
}

impl Default for Paragraph {
    /// An empty paragraph resolving under first-strong base direction: base
    /// direction is a stated input, and `Auto` defers to the text's own first
    /// strong character rather than an ambient locale.
    fn default() -> Self {
        Self::new(String::new(), BaseDirection::Auto, 0)
    }
}

impl Paragraph {
    /// A paragraph over `text` with the given base direction and style epoch.
    pub fn new(text: impl Into<String>, base: BaseDirection, style_epoch: u64) -> Self {
        Self {
            text: text.into(),
            base,
            style_epoch,
            lines: Vec::new(),
            key: None,
            dirty_from: None,
            dirty_delta: 0,
            dirty_to: None,
            shape_calls: 0,
        }
    }

    /// The paragraph's source text.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The laid-out lines from the last [`Self::layout`], top to bottom.
    pub fn lines(&self) -> &[LineLayout] {
        &self.lines
    }

    /// Total shape invocations over this paragraph's life — the count of runs
    /// actually handed to the shaper, cache misses only.
    ///
    /// The steady-state text contract is that an unchanged paragraph laid out
    /// again shapes nothing; observing this counter before and after a
    /// [`Self::layout`] and finding it unchanged is the frame-counter proof of
    /// that (the retained runs and the [`LayoutKey`] gate mean a steady frame
    /// never reaches the shaper).
    pub fn shape_call_count(&self) -> u64 {
        self.shape_calls
    }

    /// Replace the whole source text, marking every line dirty. The style epoch
    /// is unchanged; use [`Self::set_style_epoch`] when font/features change.
    pub fn set_text(&mut self, text: impl Into<String>) {
        self.text = text.into();
        self.dirty_from = Some(TextOffset(0));
        // A whole-text replacement reflows from the top; no tail is retained, so
        // the delta is irrelevant and reset, and the whole text is dirty.
        self.dirty_delta = 0;
        self.dirty_to = Some(TextOffset(self.text.len()));
    }

    /// Record an edit that replaced `range` with `replacement`, updating the
    /// source and marking the affected region dirty from `range.0`.
    ///
    /// The dirty mark is the incremental reflow's entry: the next [`Self::layout`]
    /// reflows from the line containing `range.0` and stops when a line signature
    /// restabilizes. Offsets must be char boundaries; the replacement is spliced
    /// verbatim (no normalization, per spec 12.3).
    pub fn edit(&mut self, range: (TextOffset, TextOffset), replacement: &str) {
        let (start, end) = (
            range.0.0.min(self.text.len()),
            range.1.0.min(self.text.len()),
        );
        let (start, end) = (start.min(end), start.max(end));
        let removed = end - start;
        let inserted = replacement.len();
        self.text.replace_range(start..end, replacement);
        let delta = inserted as isize - removed as isize;
        let from = TextOffset(start);
        self.dirty_from = Some(match self.dirty_from {
            Some(existing) => existing.min(from),
            None => from,
        });
        // The stop floor is the highest touched offset in current coordinates.
        // This edit reaches `start + inserted`; a floor recorded by an earlier
        // edit at a higher offset shifts by this edit's delta so it stays in the
        // live buffer's coordinate space.
        let this_end = TextOffset(start + inserted);
        self.dirty_to = Some(match self.dirty_to {
            Some(prev) if prev.0 > start => {
                let shifted = TextOffset((prev.0 as isize + delta) as usize);
                shifted.max(this_end)
            }
            Some(prev) => prev.max(this_end),
            None => this_end,
        });
        // Track the net byte shift of the untouched tail so a later reflow can
        // translate the retained lines into the new coordinate space.
        self.dirty_delta += delta;
    }

    /// Bump the style epoch (font faces / shaping features changed), invalidating
    /// all cached geometry.
    pub fn set_style_epoch(&mut self, epoch: u64) {
        if epoch != self.style_epoch {
            self.style_epoch = epoch;
            self.dirty_from = Some(TextOffset(0));
        }
    }

    /// Lay out the paragraph to `width`, reusing the cached lines when nothing
    /// that affects layout changed, and otherwise reflowing incrementally from
    /// the dirtied region forward until a line signature restabilizes.
    ///
    /// Returns `true` when a (re)layout ran, `false` on a pure cache hit (spec
    /// section 20: unchanged text/font/features/width does no work). The result
    /// is guaranteed identical to [`Self::layout_full`] with the same inputs.
    pub fn layout(&mut self, width: f32, shape: &mut ShapeFn<'_>) -> bool {
        let key = LayoutKey::new(&self.text, self.base, self.style_epoch, width);

        // Cache gate: same key and no pending edit means the retained lines are
        // still valid — a true no-op, no BiDi, no shaping, no reflow.
        if self.key == Some(key) && self.dirty_from.is_none() {
            return false;
        }

        // A width or style change (key differs) forces reflow from the top; a
        // pure edit reflows from the dirtied line. When both, the earliest wins.
        let reflow_from = if self.key.map(|k| k.width_bits) != Some(key.width_bits)
            || self.key.map(|k| k.style_epoch) != Some(key.style_epoch)
            || self.key.is_none()
        {
            Some(TextOffset(0))
        } else {
            self.dirty_from
        };

        match reflow_from {
            Some(TextOffset(0)) | None => {
                let (lines, misses) =
                    self.compute_lines(width, TextOffset(0), TextOffset(0), &[], shape);
                self.lines = lines;
                self.shape_calls += misses;
            }
            Some(from) => {
                // Incremental reflow. `from` is the edit's low offset in the
                // pre-edit coordinate space; everything before it is byte-for-byte
                // unchanged.
                //
                // The line *containing* the edit must reflow — but so must the line
                // *before* it: shrinking or growing the edited word can pull the
                // following word back onto the previous line or push its last word
                // down. So the retained prefix stops one line earlier than the
                // dirty line, and reflow restarts at that earlier line's start.
                let dirty_idx = self
                    .lines
                    .iter()
                    .position(|l| l.logical_range.1 > from)
                    .unwrap_or(self.lines.len());
                let reflow_idx = dirty_idx.saturating_sub(1);
                let prefix: Vec<LineLayout> = self.lines[..reflow_idx].to_vec();
                // The retained tail (reflow line onward) is in old coordinates;
                // translate lines at or after the edit into the new coordinate
                // space so the stable-stop and splice compare like for like.
                let old_tail: Vec<LineLayout> = self.lines[reflow_idx..]
                    .iter()
                    .map(|l| shift_line(l, from, self.dirty_delta))
                    .collect();
                let start = prefix
                    .last()
                    .map(|l| l.logical_range.1)
                    .unwrap_or(TextOffset(0));
                // A stable-stop may only fire once reflow has passed every edited
                // region: a line before the last edit can match its old signature
                // yet sit ahead of a later edit, and splicing there would restore a
                // stale edited line. `dirty_to` is that floor, in new coordinates.
                let floor = self.dirty_to.unwrap_or(from);
                let (mut relaid, misses) =
                    self.compute_lines(width, start, floor, &old_tail, shape);
                self.shape_calls += misses;
                let mut spliced = prefix;
                spliced.append(&mut relaid);
                self.lines = spliced;
            }
        }

        self.key = Some(key);
        self.dirty_from = None;
        self.dirty_to = None;
        self.dirty_delta = 0;
        true
    }

    /// Lay out the whole paragraph from scratch, ignoring any cache. This is the
    /// reference the incremental path is defined to equal; the equivalence is the
    /// section's acceptance test.
    pub fn layout_full(&mut self, width: f32, shape: &mut ShapeFn<'_>) -> Vec<LineLayout> {
        let (lines, misses) = self.compute_lines(width, TextOffset(0), TextOffset(0), &[], shape);
        self.shape_calls += misses;
        lines
    }

    /// Break and lay out lines covering `[from, text.len())` to `width`, greedily
    /// fitting at UAX#14 opportunities.
    ///
    /// `old_tail` lets an incremental reflow stop early: once a freshly-computed
    /// line's signature matches an old line at the same start, the remaining old
    /// lines are unaffected and are spliced back verbatim (spec 12.19 stable
    /// stop). For a full layout `old_tail` is empty and every line is computed.
    fn compute_lines(
        &self,
        width: f32,
        from: TextOffset,
        stop_floor: TextOffset,
        old_tail: &[LineLayout],
        shape: &mut ShapeFn<'_>,
    ) -> (Vec<LineLayout>, u64) {
        let text = &self.text;
        if from.0 >= text.len() {
            return (Vec::new(), 0);
        }

        let bidi = BidiInfo::resolve(text, self.base);
        let breaker = LineBreaker::new();
        let breaks: Vec<(TextOffset, BreakOpportunity)> =
            breaker.break_opportunities(text).collect();

        // One memo for the whole pass: `fit_line` measures each break candidate and
        // `build_line` builds the winner from the same substrings, so a run is
        // shaped once and reused across every fit probe and the final build.
        let mut cache = ShapeCache::new(shape);

        let mut out: Vec<LineLayout> = Vec::new();
        let mut line_start = from;

        while line_start.0 < text.len() {
            let (line_end, _mandatory) =
                self.fit_line(&bidi, line_start, width, &breaks, &mut cache);
            let line = self.build_line(&bidi, (line_start, line_end), &mut cache);

            // Stable-stop: if this line matches the old layout's line at the same
            // start, the rest of the old tail is unaffected — splice it back and
            // stop. This is what keeps incremental reflow O(edited lines), and it
            // can only fire when the recomputed line is byte-identical to the old
            // one, so the spliced result equals a full recompute.
            if !old_tail.is_empty()
                && line_start >= stop_floor
                && let Some(idx) = old_tail
                    .iter()
                    .position(|l| l.logical_range.0 == line_start)
                && LineSignature::of(&old_tail[idx]) == LineSignature::of(&line)
            {
                out.push(line);
                out.extend(old_tail[idx + 1..].iter().cloned());
                return (out, cache.misses);
            }

            line_start = line_end;
            out.push(line);
        }

        (out, cache.misses)
    }

    /// Find the end offset of the line starting at `line_start` when fitting to
    /// `width`: the furthest break opportunity whose text fits, or the first
    /// break past the width when even one segment overflows (never zero-advance),
    /// and always ending at a mandatory break.
    fn fit_line(
        &self,
        bidi: &BidiInfo,
        line_start: TextOffset,
        width: f32,
        breaks: &[(TextOffset, BreakOpportunity)],
        cache: &mut ShapeCache<'_, '_>,
    ) -> (TextOffset, bool) {
        let text = &self.text;
        let mut last_fit: Option<TextOffset> = None;
        for &(at, class) in breaks.iter().filter(|(at, _)| *at > line_start) {
            let candidate_w = self.measure(bidi, line_start, at, cache);
            let fits = candidate_w <= width || width <= 0.0;
            if class == BreakOpportunity::Mandatory {
                // A mandatory break ends the line. If nothing fit before it and it
                // overflows, the line still takes the whole segment (a single
                // unbreakable run is never split to zero width).
                if fits || last_fit.is_none() {
                    return (at, true);
                }
                return (last_fit.unwrap(), false);
            }
            if fits {
                last_fit = Some(at);
            } else {
                // This opportunity overflows: end at the last one that fit, or —
                // if none did — take this first segment anyway to guarantee
                // forward progress.
                return match last_fit {
                    Some(end) => (end, false),
                    None => (at, false),
                };
            }
        }
        // No break past the start: the rest of the text is one line.
        (TextOffset(text.len()), true)
    }

    /// The inline width of `[start, end)` under the current style, summed over
    /// its logical direction runs, using the shared paragraph BiDi resolution.
    fn measure(
        &self,
        bidi: &BidiInfo,
        start: TextOffset,
        end: TextOffset,
        cache: &mut ShapeCache<'_, '_>,
    ) -> f32 {
        let text = &self.text;
        bidi.direction_runs_in(start, end)
            .iter()
            .map(|run| {
                cache
                    .shape(text, (run.start, run.end), run.direction)
                    .width_ems
            })
            .sum()
    }

    /// Build the [`LineLayout`] for `range`, shaping each of its logical direction
    /// runs and reusing the shared paragraph BiDi resolution.
    fn build_line(
        &self,
        bidi: &BidiInfo,
        range: (TextOffset, TextOffset),
        cache: &mut ShapeCache<'_, '_>,
    ) -> LineLayout {
        let text = &self.text;
        let shaped: Vec<ShapedRun> = bidi
            .direction_runs_in(range.0, range.1)
            .iter()
            .map(|run| {
                cache
                    .shape(text, (run.start, run.end), run.direction)
                    .clone()
            })
            .collect();
        LineLayout::in_range(text, bidi, range, &shaped)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FontFaceId;
    use crate::bidi::BaseDirection;
    use crate::shaping::Shaper;

    const DEJAVU: &[u8] = include_bytes!("../tests/fixtures/DejaVuSans-subset.ttf");

    /// Shape one logical direction run's substring with the fixture face.
    fn shape_run(sub: &str, dir: Direction) -> ShapedRun {
        Shaper::new()
            .shape_run(FontFaceId(0), DEJAVU, 0, sub, dir)
            .expect("fixture parses")
    }

    /// Lay out a single line over `text`, shaping each of its logical direction
    /// runs with the fixture face. This mirrors what the paragraph layer will do
    /// once line breaking exists: split direction runs, shape each, and build the
    /// visual layout.
    fn layout_line(text: &str, base: BaseDirection) -> (BidiInfo, LineLayout) {
        let bidi = BidiInfo::resolve(text, base);
        let logical_runs = bidi.direction_runs_in(TextOffset(0), TextOffset(text.len()));
        let shaped: Vec<ShapedRun> = logical_runs
            .iter()
            .map(|run| {
                let sub = &text[run.start.0..run.end.0];
                shape_run(sub, run.direction)
            })
            .collect();
        let line = LineLayout::single_line(text, &bidi, &shaped);
        (bidi, line)
    }

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    #[test]
    fn ltr_line_has_one_run_with_ascending_caret_stops() {
        // Pure LTR: one visual run, stops at every grapheme boundary, inline x
        // strictly increasing left to right.
        let (_, line) = layout_line("AVA", BaseDirection::LeftToRight);
        assert_eq!(line.runs.len(), 1);
        let run = &line.runs[0];
        assert_eq!(run.direction, Direction::LeftToRight);
        assert_eq!(run.logical_range, (TextOffset(0), TextOffset(3)));
        // Four stops for three one-byte graphemes: 0,1,2,3.
        let offsets: Vec<usize> = run.caret_stops.iter().map(|s| s.offset.0).collect();
        assert_eq!(offsets, vec![0, 1, 2, 3]);
        // Inline x ascends from the run's left edge (0.0) to its width.
        assert!(approx(run.caret_stops[0].inline_x, 0.0));
        for w in run.caret_stops.windows(2) {
            assert!(w[1].inline_x > w[0].inline_x, "LTR inline x ascends");
        }
        assert!(approx(
            run.caret_stops.last().unwrap().inline_x,
            run.visual_inline_range.1
        ));
        assert!(approx(line.width, run.width()));
    }

    #[test]
    fn rtl_run_caret_stops_descend_in_visual_x() {
        // A pure Hebrew run resolves RTL. Its first logical stop sits at the run's
        // right edge and inline x decreases as the logical offset advances — the
        // BiDi-correct placement makepad's byte-order scan gets wrong.
        let (_, line) = layout_line("\u{05D0}\u{05D1}\u{05D2}", BaseDirection::RightToLeft);
        assert_eq!(line.runs.len(), 1);
        let run = &line.runs[0];
        assert_eq!(run.direction, Direction::RightToLeft);
        // First logical stop at the right edge, last at the left edge.
        assert!(approx(
            run.caret_stops.first().unwrap().inline_x,
            run.visual_inline_range.1
        ));
        assert!(approx(
            run.caret_stops.last().unwrap().inline_x,
            run.visual_inline_range.0
        ));
        for w in run.caret_stops.windows(2) {
            assert!(
                w[1].inline_x < w[0].inline_x,
                "RTL inline x descends as logical offset advances"
            );
        }
    }

    #[test]
    fn mixed_line_orders_runs_left_to_right() {
        // "A" + Hebrew under LTR base: an LTR run then an RTL run, both placed
        // left to right, contiguous inline, logical ranges preserved unreversed.
        let text = "A\u{05D0}\u{05D1}";
        let (_, line) = layout_line(text, BaseDirection::LeftToRight);
        assert_eq!(line.runs.len(), 2);
        // Visual order: the Latin "A" is leftmost under LTR base.
        assert_eq!(line.runs[0].direction, Direction::LeftToRight);
        assert_eq!(line.runs[0].logical_range, (TextOffset(0), TextOffset(1)));
        assert_eq!(line.runs[1].direction, Direction::RightToLeft);
        // Runs are contiguous inline: the second starts where the first ends.
        assert!(approx(
            line.runs[0].visual_inline_range.1,
            line.runs[1].visual_inline_range.0
        ));
        // The line covers the whole source.
        assert_eq!(line.logical_range, (TextOffset(0), TextOffset(text.len())));
    }

    #[test]
    fn empty_line_has_no_runs_and_zero_width() {
        let (_, line) = layout_line("", BaseDirection::Auto);
        assert!(line.runs.is_empty());
        assert!(approx(line.width, 0.0));
        assert_eq!(line.logical_range, (TextOffset(0), TextOffset(0)));
    }

    #[test]
    fn inline_x_of_returns_stop_x_and_none_for_interior() {
        // "你好" is two 3-byte clusters; boundaries at 0,3,6. Byte 1 is interior
        // (inside the first cluster) and is not a stop.
        let (_, line) = layout_line("\u{4F60}\u{597D}", BaseDirection::LeftToRight);
        let run = &line.runs[0];
        assert!(run.inline_x_of(TextOffset(0)).is_some());
        assert!(run.inline_x_of(TextOffset(3)).is_some());
        assert!(run.inline_x_of(TextOffset(6)).is_some());
        assert!(run.inline_x_of(TextOffset(1)).is_none());
    }

    // --- Paragraph layout: incremental == full recompute ---

    /// A shaping callback over the fixture face, capturing a fresh [`Shaper`] per
    /// paragraph. Both the incremental and full paths are driven by an identical
    /// closure so any difference is layout, not shaping.
    fn fixture_shaper() -> impl FnMut(&str, Direction) -> ShapedRun {
        move |sub: &str, dir: Direction| shape_run(sub, dir)
    }

    /// Structural equality of two laid-out line sets: same line count, and each
    /// line matches in logical range, width, and every visual run (logical range,
    /// inline extent, direction, level, face, and caret stops). This is the DoD
    /// comparison — bit-for-bit layout identity, not a coarse signature.
    fn lines_eq(a: &[LineLayout], b: &[LineLayout]) -> bool {
        if a.len() != b.len() {
            return false;
        }
        a.iter().zip(b).all(|(x, y)| {
            x.logical_range == y.logical_range
                && x.width.to_bits() == y.width.to_bits()
                && x.runs.len() == y.runs.len()
                && x.runs.iter().zip(&y.runs).all(|(rx, ry)| {
                    rx.logical_range == ry.logical_range
                        && rx.visual_inline_range.0.to_bits() == ry.visual_inline_range.0.to_bits()
                        && rx.visual_inline_range.1.to_bits() == ry.visual_inline_range.1.to_bits()
                        && rx.direction == ry.direction
                        && rx.level == ry.level
                        && rx.face == ry.face
                        && rx.caret_stops.len() == ry.caret_stops.len()
                        && rx.caret_stops.iter().zip(&ry.caret_stops).all(|(cx, cy)| {
                            cx.offset == cy.offset && cx.inline_x.to_bits() == cy.inline_x.to_bits()
                        })
                })
        })
    }

    /// The width one fixture-shaped LTR run of `text` occupies, for choosing a
    /// wrap width that forces a known number of lines without a magic constant.
    fn measured_width(text: &str) -> f32 {
        shape_run(text, Direction::LeftToRight).width_ems
    }

    #[test]
    fn single_line_paragraph_lays_out_and_caches() {
        let mut p = Paragraph::new("hello world", BaseDirection::LeftToRight, 0);
        let mut shape = fixture_shaper();
        // First layout runs; a second with the same inputs is a pure cache hit.
        assert!(p.layout(f32::INFINITY, &mut shape));
        assert!(!p.layout(f32::INFINITY, &mut shape));
        // Unwrapped, the whole text is one line.
        assert_eq!(p.lines().len(), 1);
        assert_eq!(
            p.lines()[0].logical_range,
            (TextOffset(0), TextOffset("hello world".len()))
        );
    }

    #[test]
    fn wrapping_breaks_at_opportunities() {
        // A width that fits "hello " but not "hello world" forces two lines,
        // breaking at the space (a UAX#14 opportunity), not mid-word.
        let text = "hello world";
        let width = measured_width("hello ") + 0.01;
        let mut p = Paragraph::new(text, BaseDirection::LeftToRight, 0);
        let mut shape = fixture_shaper();
        p.layout(width, &mut shape);
        assert_eq!(p.lines().len(), 2, "wraps into two lines");
        assert_eq!(p.lines()[0].logical_range.0, TextOffset(0));
        assert_eq!(
            p.lines().last().unwrap().logical_range.1,
            TextOffset(text.len())
        );
    }

    #[test]
    fn incremental_edit_equals_full_recompute() {
        // The DoD: after an edit, an incremental relayout must produce a line set
        // identical to laying the edited text out from scratch.
        let text = "the quick brown fox jumps over the lazy dog";
        let width = measured_width("the quick brown ") + 0.01;

        let mut incremental = Paragraph::new(text, BaseDirection::LeftToRight, 0);
        let mut shape = fixture_shaper();
        incremental.layout(width, &mut shape);

        // Edit a word deep in the paragraph and relayout incrementally.
        // Replace "lazy" (bytes 35..39) with "sleeping".
        incremental.edit((TextOffset(35), TextOffset(39)), "sleeping");
        incremental.layout(width, &mut shape);

        // Lay the same edited text out from scratch.
        let edited = "the quick brown fox jumps over the sleeping dog";
        let mut full = Paragraph::new(edited, BaseDirection::LeftToRight, 0);
        let mut shape2 = fixture_shaper();
        let full_lines = full.layout_full(width, &mut shape2);

        assert_eq!(incremental.text(), edited);
        assert!(
            lines_eq(incremental.lines(), &full_lines),
            "incremental relayout must equal full recompute\nincremental: {:#?}\nfull: {:#?}",
            incremental.lines(),
            full_lines
        );
    }

    #[test]
    fn edit_in_first_line_equals_full_recompute() {
        // An edit in the very first line reflows from the top; still must equal a
        // full recompute.
        let text = "alpha beta gamma delta epsilon zeta eta theta";
        let width = measured_width("alpha beta ") + 0.01;

        let mut incremental = Paragraph::new(text, BaseDirection::LeftToRight, 0);
        let mut shape = fixture_shaper();
        incremental.layout(width, &mut shape);

        // Replace "alpha" (0..5) with "AL".
        incremental.edit((TextOffset(0), TextOffset(5)), "AL");
        incremental.layout(width, &mut shape);

        let edited = "AL beta gamma delta epsilon zeta eta theta";
        let mut full = Paragraph::new(edited, BaseDirection::LeftToRight, 0);
        let mut shape2 = fixture_shaper();
        let full_lines = full.layout_full(width, &mut shape2);

        assert!(lines_eq(incremental.lines(), &full_lines));
    }

    #[test]
    fn width_change_reflows_and_equals_full() {
        // Changing the wrap width invalidates the cache key and reflows from the
        // top; the result must equal a from-scratch layout at the new width.
        let text = "one two three four five six seven eight nine ten";
        let mut incremental = Paragraph::new(text, BaseDirection::LeftToRight, 0);
        let mut shape = fixture_shaper();

        incremental.layout(measured_width("one two three ") + 0.01, &mut shape);
        let narrow = measured_width("one two ") + 0.01;
        assert!(
            incremental.layout(narrow, &mut shape),
            "width change relays out"
        );

        let mut full = Paragraph::new(text, BaseDirection::LeftToRight, 0);
        let mut shape2 = fixture_shaper();
        let full_lines = full.layout_full(narrow, &mut shape2);
        assert!(lines_eq(incremental.lines(), &full_lines));
    }

    #[test]
    fn multi_edit_before_layout_equals_full_recompute() {
        // Two edits between layouts: the dirty mark tracks the earliest touched
        // offset, and the single relayout must still equal a full recompute of the
        // final text.
        let text = "red orange yellow green blue indigo violet";
        let width = measured_width("red orange ") + 0.01;

        let mut incremental = Paragraph::new(text, BaseDirection::LeftToRight, 0);
        let mut shape = fixture_shaper();
        incremental.layout(width, &mut shape);

        // Edit a later word, then an earlier word, before relaying out.
        // "violet" is bytes 36..42; "green" is bytes 18..23.
        incremental.edit((TextOffset(36), TextOffset(42)), "purple");
        incremental.edit((TextOffset(18), TextOffset(23)), "GREEN");
        incremental.layout(width, &mut shape);

        let edited = "red orange yellow GREEN blue indigo purple";
        let mut full = Paragraph::new(edited, BaseDirection::LeftToRight, 0);
        let mut shape2 = fixture_shaper();
        let full_lines = full.layout_full(width, &mut shape2);

        assert_eq!(incremental.text(), edited);
        assert!(lines_eq(incremental.lines(), &full_lines));
    }

    #[test]
    fn rtl_paragraph_incremental_equals_full() {
        // An RTL paragraph edited mid-text: incremental reflow must equal a full
        // recompute, so BiDi's paragraph-context sensitivity is honored.
        let text = "\u{05D0}\u{05D1} \u{05D2}\u{05D3} \u{05D4}\u{05D5} \u{05D6}\u{05D7}";
        let width = measured_width("\u{05D0}\u{05D1} ") + 0.01;

        let mut incremental = Paragraph::new(text, BaseDirection::RightToLeft, 0);
        let mut shape = fixture_shaper();
        incremental.layout(width, &mut shape);

        // Replace the third word (bytes 5..9, "\u{05D2}\u{05D3}") with one letter.
        incremental.edit((TextOffset(5), TextOffset(9)), "\u{05DA}");
        incremental.layout(width, &mut shape);

        let mut full = Paragraph::new(incremental.text(), BaseDirection::RightToLeft, 0);
        let mut shape2 = fixture_shaper();
        let full_lines = full.layout_full(width, &mut shape2);
        assert!(lines_eq(incremental.lines(), &full_lines));
    }

    #[test]
    fn style_epoch_change_invalidates_and_equals_full() {
        // Bumping the style epoch (font/features changed) forces a full reflow;
        // the result must equal a from-scratch layout under the new epoch.
        let text = "sample paragraph text for layout";
        let width = measured_width("sample ") + 0.01;

        let mut incremental = Paragraph::new(text, BaseDirection::LeftToRight, 0);
        let mut shape = fixture_shaper();
        incremental.layout(width, &mut shape);
        incremental.set_style_epoch(1);
        assert!(
            incremental.layout(width, &mut shape),
            "epoch bump relays out"
        );

        let mut full = Paragraph::new(text, BaseDirection::LeftToRight, 1);
        let mut shape2 = fixture_shaper();
        let full_lines = full.layout_full(width, &mut shape2);
        assert!(lines_eq(incremental.lines(), &full_lines));
    }

    #[test]
    fn mandatory_break_forces_line_end() {
        // A hard newline ends a line regardless of width; each side is its own
        // line and the whole thing equals a full recompute.
        let text = "first\nsecond";
        let mut incremental = Paragraph::new(text, BaseDirection::LeftToRight, 0);
        let mut shape = fixture_shaper();
        incremental.layout(f32::INFINITY, &mut shape);
        assert_eq!(incremental.lines().len(), 2, "hard newline splits lines");

        let mut full = Paragraph::new(text, BaseDirection::LeftToRight, 0);
        let mut shape2 = fixture_shaper();
        let full_lines = full.layout_full(f32::INFINITY, &mut shape2);
        assert!(lines_eq(incremental.lines(), &full_lines));
    }

    // --- TF-P3.1: retained shaped runs / steady-state no reshape ---

    /// A shaping callback that also tallies how many times it was actually
    /// invoked, so a test can distinguish "the paragraph asked to shape" from "the
    /// paragraph reused a retained run". Returns the closure and a shared counter.
    fn counting_shaper() -> (
        impl FnMut(&str, Direction) -> ShapedRun,
        std::rc::Rc<std::cell::Cell<u64>>,
    ) {
        let calls = std::rc::Rc::new(std::cell::Cell::new(0u64));
        let seen = calls.clone();
        let shape = move |sub: &str, dir: Direction| {
            seen.set(seen.get() + 1);
            shape_run(sub, dir)
        };
        (shape, calls)
    }

    #[test]
    fn steady_state_frame_shapes_nothing() {
        // The DoD frame-counter proof: a static paragraph laid out once and then
        // laid out again with identical inputs must issue zero shape calls on the
        // second (steady-state) frame. The gate returns before any shaping, so the
        // paragraph's own counter and the callback's own counter both stay put.
        let (mut shape, calls) = counting_shaper();
        let mut p = Paragraph::new("hello steady world", BaseDirection::LeftToRight, 0);

        assert!(p.layout(f32::INFINITY, &mut shape), "first frame lays out");
        let after_first = p.shape_call_count();
        let callback_after_first = calls.get();
        assert!(after_first > 0, "first layout shapes the runs");
        assert_eq!(
            after_first, callback_after_first,
            "paragraph's shape counter matches genuine callback invocations"
        );

        // Steady frame: same text, style, width. No layout, no shaping.
        assert!(
            !p.layout(f32::INFINITY, &mut shape),
            "steady frame is a cache hit"
        );
        assert_eq!(
            p.shape_call_count(),
            after_first,
            "steady-state frame issues zero shape calls (paragraph counter)"
        );
        assert_eq!(
            calls.get(),
            callback_after_first,
            "steady-state frame issues zero shape calls (callback counter)"
        );

        // Repeated steady frames stay at zero-shape.
        for _ in 0..8 {
            assert!(!p.layout(f32::INFINITY, &mut shape));
        }
        assert_eq!(p.shape_call_count(), after_first);
        assert_eq!(calls.get(), callback_after_first);
    }

    #[test]
    fn wrapping_layout_shapes_each_run_once_not_per_break_probe() {
        // Retained shaped runs: within one wrapping layout, `fit_line` probes many
        // break candidates and `build_line` builds the winners, all over the same
        // substrings. The per-pass memo must collapse those to one shape per
        // distinct run. Without it, fit probing re-shapes growing prefixes and the
        // count explodes with the break-candidate count.
        let text = "the quick brown fox jumps over the lazy dog";
        let width = measured_width("the quick ") + 0.01;
        let (mut shape, calls) = counting_shaper();
        let mut p = Paragraph::new(text, BaseDirection::LeftToRight, 0);

        p.layout(width, &mut shape);

        // The distinct direction-run substrings shaped across the whole layout are
        // bounded by the break segments, not the O(segments^2) fit probes. Every
        // fit probe over `[line_start, at)` for the same `at` is one cached run;
        // the pure-LTR text has one direction run per measured span. The exact
        // number depends on break structure, but it must be far below the naive
        // re-shape count and must equal the paragraph's own miss counter.
        assert_eq!(
            calls.get(),
            p.shape_call_count(),
            "callback invocations equal the paragraph's counted misses"
        );

        // Upper bound: distinct measured spans are at most the number of break
        // opportunities squared in the naive scheme; the memo caps genuine shapes
        // at the count of distinct (line_start, at) spans, which for this text and
        // width is well under 30. Assert a generous ceiling that the naive
        // quadratic re-shape would blow past.
        let breaker = crate::line_break::LineBreaker::new();
        let break_count = breaker.break_opportunities(text).count();
        assert!(
            calls.get() <= (break_count as u64) * 3,
            "shapes ({}) stay near the break count ({}), not its square",
            calls.get(),
            break_count
        );
    }

    #[test]
    fn edit_reshapes_only_touched_region_not_whole_paragraph() {
        // An incremental edit shapes only the reflowed span, not the whole
        // paragraph: the retained tail is spliced without reshaping. The shape
        // count for the incremental relayout must be far below a full recompute's.
        let text = "the quick brown fox jumps over the lazy dog by the river bank today";
        let width = measured_width("the quick brown ") + 0.01;

        let (mut shape, calls) = counting_shaper();
        let mut p = Paragraph::new(text, BaseDirection::LeftToRight, 0);
        p.layout(width, &mut shape);
        let after_initial = calls.get();

        // Full recompute of the same text, for a reshape-count baseline.
        let (mut shape_full, calls_full) = counting_shaper();
        let mut full = Paragraph::new(text, BaseDirection::LeftToRight, 0);
        full.layout_full(width, &mut shape_full);
        let full_count = calls_full.get();

        // Edit a word early in the paragraph and relayout incrementally.
        // Replace "lazy" (bytes 35..39) with "sleepy".
        p.edit((TextOffset(35), TextOffset(39)), "sleepy");
        p.layout(width, &mut shape);
        let incremental_reshape = calls.get() - after_initial;

        assert!(
            incremental_reshape < full_count,
            "incremental edit reshapes ({}) fewer runs than a full recompute ({})",
            incremental_reshape,
            full_count
        );
    }
}
