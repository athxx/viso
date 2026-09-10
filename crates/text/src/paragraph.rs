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

use crate::bidi::{BidiInfo, BidiLevel};
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
        let logical_runs = bidi.direction_runs_in(whole.0, whole.1);

        // Visual (left-to-right) order of the line's source offsets under rule
        // L2. A run's visual position is the visual position of its logical
        // start; ordering runs by that key reproduces the reordered run
        // sequence without re-running the reorder per run.
        let visual = bidi.visual_order(whole.0, whole.1);
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
            logical_range: whole,
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

/// A laid-out paragraph: the cached, world-ready layout result.
#[derive(Debug, Default)]
pub struct Paragraph {
    // TODO(TF-P2): lines, runs, shaped glyphs, and the layout version key that
    // gates reshape (text/font/features/width).
}

impl Paragraph {
    /// Lay out the paragraph to a target width, reusing cached results when the
    /// layout version key is unchanged.
    pub fn layout(&mut self, _width: f32) {
        todo!("TF-P2: world-ready paragraph layout with cache gate")
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
}
