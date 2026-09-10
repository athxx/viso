//! Text selection: a logical range as source truth, resolved to visual highlight
//! fragments per line.
//!
//! A selection is two logical [`TextPosition`]s — an anchor (fixed) and a focus
//! (the moving end). That logical range is the single source of truth: copy,
//! cut, and delete read it in logical order, and BiDi reordering never swaps
//! anchor and focus. What differs between logical and visual is only the
//! highlight: one contiguous logical range can paint as several disjoint
//! rectangles on a line when the line mixes directions, because a logical span
//! that is contiguous in memory is split across the line's visual runs.
//!
//! # One logical range, many visual fragments
//!
//! The resolution is: logical range -> per-line intersection -> per-visual-run
//! intersection -> a fragment rect for each run the selection touches. A pure
//! LTR or pure RTL line yields one fragment; a mixed line yields one fragment
//! per run the selection overlaps, each spanning that run's visual extent for
//! the covered sub-range. This is where Viso exceeds makepad, which paints one
//! contiguous span per row assuming byte order equals visual order — wrong the
//! moment a selection crosses a direction boundary. Highlighting reads visual
//! geometry; the selection itself stays logical.

use crate::paragraph::{LineLayout, VisualRun};
use crate::text_position::{TextOffset, TextPosition};

/// A text selection: an anchor and a focus, both logical positions. The anchor
/// is where selection began; the focus is the moving end. Their order in the
/// text is not implied — [`Selection::logical_range`] normalizes them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Selection {
    /// The fixed end, where the selection was anchored.
    pub anchor: TextPosition,
    /// The moving end, which follows the caret.
    pub focus: TextPosition,
}

/// One visual highlight rectangle on a line: the inline extent to paint for the
/// part of the selection that falls in one visual run.
///
/// The rectangle is inline-only (a `[left, right)` span in em units); the cross
/// axis (the line's vertical band) is the line's business, added when fragments
/// are placed. Fragments on a line are independent — a mixed-direction line
/// produces several, and they need not be contiguous.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SelectionFragment {
    /// Inline left edge in em units (visual space, left-to-right).
    pub left: f32,
    /// Inline right edge in em units. Always `>= left`.
    pub right: f32,
}

impl SelectionFragment {
    /// The fragment's inline width in em units.
    pub fn width(&self) -> f32 {
        self.right - self.left
    }
}

impl Selection {
    /// A caret-only selection (empty range) at `position`.
    pub fn caret(position: TextPosition) -> Self {
        Self {
            anchor: position,
            focus: position,
        }
    }

    /// Whether the selection is a bare caret (anchor and focus coincide). A
    /// caret selects nothing and produces no highlight fragments.
    pub fn is_caret(&self) -> bool {
        self.anchor.offset == self.focus.offset
    }

    /// The selection's logical byte range `[start, end)`, normalized so `start <=
    /// end` regardless of drag direction. This is the source-truth order copy,
    /// cut, and delete operate in; anchor/focus roles are preserved separately
    /// on the [`Selection`] and never swapped by this normalization.
    pub fn logical_range(&self) -> (TextOffset, TextOffset) {
        let a = self.anchor.offset;
        let b = self.focus.offset;
        if a <= b { (a, b) } else { (b, a) }
    }

    /// The visual highlight fragments for this selection on `line`, one per
    /// visual run the selection overlaps, in the line's visual left-to-right run
    /// order.
    ///
    /// Each run's logical range is intersected with the selection's logical
    /// range; a non-empty intersection becomes a fragment whose inline extent is
    /// the run's inline span for exactly that sub-range — the two covered caret
    /// stops' inline x, min/max'd so the rect is direction-agnostic (an RTL run's
    /// covered stops descend in x, so the fragment's left is the higher offset's
    /// x). A caret selection yields no fragments.
    pub fn fragments(&self, line: &LineLayout) -> Vec<SelectionFragment> {
        if self.is_caret() {
            return Vec::new();
        }
        let (sel_start, sel_end) = self.logical_range();
        let mut fragments = Vec::new();
        for run in &line.runs {
            if let Some(fragment) = run_fragment(run, sel_start, sel_end) {
                fragments.push(fragment);
            }
        }
        fragments
    }
}

/// The highlight fragment for the part of `[sel_start, sel_end)` that falls in
/// `run`, or `None` when the selection does not overlap the run.
///
/// The covered sub-range is `[max(run.start, sel_start), min(run.end, sel_end)]`
/// clamped to the run's grapheme-boundary caret stops (a selection endpoint
/// always sits on one). The fragment's inline extent is the min and max of the
/// two covered endpoints' inline x, which is correct in both directions: for an
/// LTR run the lower offset is the smaller x, for an RTL run the higher offset
/// is the smaller x, and the min/max collapses both to a left-to-right rect.
fn run_fragment(
    run: &VisualRun,
    sel_start: TextOffset,
    sel_end: TextOffset,
) -> Option<SelectionFragment> {
    let (run_start, run_end) = run.logical_range;
    let lo = run_start.max(sel_start);
    let hi = run_end.min(sel_end);
    if lo >= hi {
        // No overlap, or a zero-width touch at a shared boundary.
        return None;
    }
    // The covered endpoints resolve to caret-stop inline x within the run. An
    // endpoint interior to the run (a selection landing mid-run) is a grapheme
    // boundary and therefore a stop; the run's own endpoints are always stops.
    let lo_x = stop_x(run, lo)?;
    let hi_x = stop_x(run, hi)?;
    let left = lo_x.min(hi_x);
    let right = lo_x.max(hi_x);
    Some(SelectionFragment { left, right })
}

/// The inline x of the caret stop at `offset` within `run`. `offset` is expected
/// to be one of the run's grapheme-boundary stops; falls back to the nearest
/// enclosing stop's x when it is not (a selection endpoint should always land on
/// a boundary, so this is a safety net, not a normal path).
fn stop_x(run: &VisualRun, offset: TextOffset) -> Option<f32> {
    if let Some(x) = run.inline_x_of(offset) {
        return Some(x);
    }
    // Not an exact stop: snap to the nearest stop at or before `offset` in
    // logical order so the fragment stays inside the run's extent.
    run.caret_stops
        .iter()
        .take_while(|s| s.offset <= offset)
        .last()
        .map(|s| s.inline_x)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FontFaceId;
    use crate::bidi::{BaseDirection, BidiInfo};
    use crate::shaping::{ShapedRun, Shaper};
    use crate::text_position::TextPosition;

    const DEJAVU: &[u8] = include_bytes!("../tests/fixtures/DejaVuSans-subset.ttf");

    fn layout_line(text: &str, base: BaseDirection) -> LineLayout {
        let bidi = BidiInfo::resolve(text, base);
        let logical_runs = bidi.direction_runs_in(TextOffset(0), TextOffset(text.len()));
        let mut shaper = Shaper::new();
        let shaped: Vec<ShapedRun> = logical_runs
            .iter()
            .map(|run| {
                let sub = &text[run.start.0..run.end.0];
                shaper
                    .shape_run(FontFaceId(0), DEJAVU, 0, sub, run.direction)
                    .expect("fixture parses")
            })
            .collect();
        LineLayout::single_line(text, &bidi, &shaped)
    }

    fn sel(start: usize, end: usize) -> Selection {
        Selection {
            anchor: TextPosition::downstream(TextOffset(start)),
            focus: TextPosition::downstream(TextOffset(end)),
        }
    }

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    #[test]
    fn logical_range_normalizes_but_keeps_anchor_focus() {
        // A backward drag (focus before anchor) still yields an ascending
        // logical range, while anchor/focus keep their roles.
        let s = Selection {
            anchor: TextPosition::downstream(TextOffset(3)),
            focus: TextPosition::downstream(TextOffset(1)),
        };
        assert_eq!(s.logical_range(), (TextOffset(1), TextOffset(3)));
        assert_eq!(s.anchor.offset, TextOffset(3));
        assert_eq!(s.focus.offset, TextOffset(1));
    }

    #[test]
    fn caret_selection_has_no_fragments() {
        let line = layout_line("AVA", BaseDirection::LeftToRight);
        let s = Selection::caret(TextPosition::downstream(TextOffset(1)));
        assert!(s.is_caret());
        assert!(s.fragments(&line).is_empty());
    }

    #[test]
    fn ltr_selection_is_one_contiguous_fragment() {
        // Selecting "V" (bytes 1..2) in "AVA" is one fragment spanning the second
        // glyph's inline extent.
        let line = layout_line("AVA", BaseDirection::LeftToRight);
        let frags = sel(1, 2).fragments(&line);
        assert_eq!(frags.len(), 1);
        // The fragment sits between stop 1 and stop 2 of the single run.
        let run = &line.runs[0];
        let x1 = run.inline_x_of(TextOffset(1)).unwrap();
        let x2 = run.inline_x_of(TextOffset(2)).unwrap();
        assert!(approx(frags[0].left, x1.min(x2)));
        assert!(approx(frags[0].right, x1.max(x2)));
        assert!(frags[0].width() > 0.0);
    }

    #[test]
    fn full_ltr_selection_covers_whole_run_width() {
        let line = layout_line("AVA", BaseDirection::LeftToRight);
        let frags = sel(0, 3).fragments(&line);
        assert_eq!(frags.len(), 1);
        assert!(approx(frags[0].left, 0.0));
        assert!(approx(frags[0].right, line.width));
    }

    #[test]
    fn rtl_selection_fragment_is_left_to_right_rect() {
        // In a pure RTL Hebrew line, selecting the first two logical graphemes
        // yields one fragment whose left < right despite the covered stops
        // descending in visual x.
        let text = "\u{05D0}\u{05D1}\u{05D2}";
        let line = layout_line(text, BaseDirection::RightToLeft);
        // First two graphemes are bytes 0..4 (each Hebrew letter is 2 bytes).
        let frags = sel(0, 4).fragments(&line);
        assert_eq!(frags.len(), 1);
        assert!(frags[0].right > frags[0].left, "rect is normalized L-to-R");
        assert!(frags[0].width() > 0.0);
    }

    #[test]
    fn mixed_selection_splits_into_per_run_fragments() {
        // "A" + Hebrew "אב" under LTR base: a selection spanning the seam
        // (bytes 0..5, the whole text) crosses the LTR run and the RTL run and
        // must produce two fragments, one per run — makepad's single-span model
        // would wrongly merge them.
        let text = "A\u{05D0}\u{05D1}";
        let line = layout_line(text, BaseDirection::LeftToRight);
        let frags = sel(0, text.len()).fragments(&line);
        assert_eq!(frags.len(), 2, "one fragment per visual run crossed");
        for f in &frags {
            assert!(f.width() > 0.0);
        }
    }

    #[test]
    fn selection_touching_only_one_run_yields_one_fragment() {
        // A selection wholly inside the Latin run of the mixed line touches only
        // that run.
        let text = "A\u{05D0}\u{05D1}";
        let line = layout_line(text, BaseDirection::LeftToRight);
        let frags = sel(0, 1).fragments(&line);
        assert_eq!(frags.len(), 1);
    }
}
