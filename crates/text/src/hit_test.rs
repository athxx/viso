//! Hit testing: a screen point resolved to a logical [`TextPosition`] with
//! affinity, over a laid-out line's retained visual geometry.
//!
//! The resolution is exact, never a guess: screen point -> visual line -> visual
//! run -> the caret stop nearest the point's inline x within that run ->
//! [`TextPosition`] with the affinity that side implies. It reads the run's
//! retained caret stops and their real inline positions; it never divides the
//! point's x by an average glyph width to index a character. That average-width
//! shortcut is the forbidden shape — it breaks on proportional fonts, ligatures,
//! and any RTL or mixed-direction run — and it is exactly what a real per-glyph
//! stop map exists to avoid.
//!
//! # Retained, not recomputed
//!
//! The hit-test geometry is the same [`LineLayout`] the caret and selection
//! read. A pointer move in steady state resolves against it with a couple of
//! comparisons and never triggers shaping or reflow (the hit-test contract:
//! the map is retained paragraph metadata). Because the stops carry real
//! left-to-right inline x per run, an RTL run resolves correctly — the point's x
//! is compared against descending-in-logical-order stops, and the nearest is
//! chosen by visual distance, not by assuming byte order runs left to right.

use crate::paragraph::{LineLayout, VisualRun};
use crate::text_position::{TextOffset, TextPosition};

/// Resolves screen points to text positions over a line's retained geometry.
///
/// It is a thin reader bound to a borrowed [`LineLayout`]: constructing it costs
/// nothing and holds no owned copy of the layout, making explicit that hit
/// testing is a query over retained metadata, not a reshape.
#[derive(Debug, Clone, Copy)]
pub struct HitTester<'a> {
    line: &'a LineLayout,
}

impl<'a> HitTester<'a> {
    /// A hit tester over `line`'s retained visual geometry.
    pub fn new(line: &'a LineLayout) -> Self {
        Self { line }
    }

    /// Resolve an inline x coordinate (in the line's em units, left to right) to
    /// the logical position it selects.
    ///
    /// The vertical coordinate selects the line before this call; within a line
    /// only the inline x matters. The point is assigned to the visual run whose
    /// inline extent contains it (or the nearest run when it falls in a gap or
    /// past an edge), then to the caret stop in that run nearest the point,
    /// carrying the affinity the chosen side implies. An empty line resolves to
    /// the origin.
    pub fn position_at_inline(&self, inline_x: f32) -> TextPosition {
        let runs = &self.line.runs;
        if runs.is_empty() {
            return TextPosition::downstream(TextOffset(0));
        }
        let run = self.run_at(inline_x);
        nearest_stop(run, inline_x)
    }

    /// The visual run whose inline extent contains `inline_x`, or the nearest run
    /// when the point falls between runs or beyond the line's edges. Runs are in
    /// visual left-to-right order, so the first run whose right edge is past the
    /// point contains it; past the last run's right edge the last run wins.
    fn run_at(&self, inline_x: f32) -> &VisualRun {
        let runs = &self.line.runs;
        for run in runs {
            if inline_x < run.visual_inline_range.1 {
                return run;
            }
        }
        // Past the rightmost run's right edge: clamp to the last run.
        runs.last().expect("non-empty checked by caller")
    }
}

/// The caret stop in `run` nearest `inline_x`, resolved to a [`TextPosition`].
///
/// Stops carry real inline x, so "nearest" is a genuine visual-distance choice,
/// not an average-width index. The affinity ties the position to this run
/// ([`VisualRun::stop_affinity`]): at a BiDi seam the same offset also belongs
/// to the neighbouring run, possibly at the other end of the line, and the
/// caret has to draw where the point was, not where the other run puts it.
fn nearest_stop(run: &VisualRun, inline_x: f32) -> TextPosition {
    // `caret_stops` are in logical order; their inline x is monotonic (ascending
    // for LTR, descending for RTL). Pick the stop minimizing |x - inline_x|.
    let mut best = &run.caret_stops[0];
    let mut best_dist = (best.inline_x - inline_x).abs();
    for stop in &run.caret_stops[1..] {
        let dist = (stop.inline_x - inline_x).abs();
        if dist < best_dist {
            best = stop;
            best_dist = dist;
        }
    }
    TextPosition {
        offset: best.offset,
        affinity: run.stop_affinity(best.offset),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FontFaceId;
    use crate::bidi::{BaseDirection, BidiInfo};
    use crate::shaping::{ShapedRun, Shaper};

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

    #[test]
    fn empty_line_hits_origin() {
        let line = layout_line("", BaseDirection::Auto);
        let ht = HitTester::new(&line);
        assert_eq!(ht.position_at_inline(0.0).offset, TextOffset(0));
        assert_eq!(ht.position_at_inline(5.0).offset, TextOffset(0));
    }

    #[test]
    fn ltr_click_before_first_glyph_hits_start() {
        let line = layout_line("AVA", BaseDirection::LeftToRight);
        let ht = HitTester::new(&line);
        // Far left resolves to offset 0.
        assert_eq!(ht.position_at_inline(-1.0).offset, TextOffset(0));
    }

    #[test]
    fn ltr_click_past_end_hits_end() {
        let line = layout_line("AVA", BaseDirection::LeftToRight);
        let ht = HitTester::new(&line);
        // Far right resolves to the last offset.
        assert_eq!(
            ht.position_at_inline(line.width + 10.0).offset,
            TextOffset(3)
        );
    }

    #[test]
    fn ltr_click_near_a_stop_resolves_to_that_offset() {
        // Clicking near stop 1's real inline x resolves to offset 1, not an
        // average-width guess, from either side of it.
        let line = layout_line("AVA", BaseDirection::LeftToRight);
        let run = &line.runs[0];
        let x1 = run.inline_x_of(TextOffset(1)).unwrap();
        let ht = HitTester::new(&line);
        assert_eq!(ht.position_at_inline(x1).offset, TextOffset(1));
        assert_eq!(ht.position_at_inline(x1 - 0.001).offset, TextOffset(1));
        assert_eq!(ht.position_at_inline(x1 + 0.001).offset, TextOffset(1));
    }

    #[test]
    fn hit_position_draws_where_the_point_was() {
        // "A" + Hebrew under LTR base: offset 1 is both the Latin run's right
        // edge and the Hebrew run's right edge. A click at either place resolves
        // to a position whose caret draws there, not at the other end.
        let text = "A\u{05D0}\u{05D1}";
        let line = layout_line(text, BaseDirection::LeftToRight);
        let ht = HitTester::new(&line);
        for run in &line.runs {
            for stop in &run.caret_stops {
                let hit = ht.position_at_inline(stop.inline_x);
                let drawn = line.caret_x(hit).expect("a hit is a caret stop");
                assert!(
                    (drawn - stop.inline_x).abs() < 1e-4,
                    "stop {:?} at {} drew at {}",
                    stop.offset,
                    stop.inline_x,
                    drawn
                );
            }
        }
    }

    #[test]
    fn proportional_rtl_hit_matches_shaped_geometry() {
        // A proportional RTL run (advances 0.3, 0.9, 0.5 em in logical order):
        // the shaped advances, not an average width, decide which offset each
        // point selects.
        let text = "\u{05D0}\u{05D5}\u{05DD}";
        let glyph = |cluster, x_advance| crate::shaping::ShapedGlyph {
            glyph_id: 1,
            cluster,
            x_advance,
            x_offset: 0.0,
            y_offset: 0.0,
            unsafe_to_break: false,
        };
        let shaped = ShapedRun {
            face: FontFaceId(0),
            glyphs: vec![glyph(4, 0.5), glyph(2, 0.9), glyph(0, 0.3)],
            width_ems: 1.7,
            text_len: 6,
            ligature_carets: Vec::new(),
        };
        let bidi = BidiInfo::resolve(text, BaseDirection::RightToLeft);
        let line = LineLayout::single_line(text, &bidi, &[shaped]);
        let run = &line.runs[0];
        let xs: Vec<f32> = run.caret_stops.iter().map(|s| s.inline_x).collect();
        let expected = [1.7, 1.4, 0.5, 0.0];
        assert!(
            xs.iter().zip(expected).all(|(x, e)| (x - e).abs() < 1e-4),
            "{xs:?}"
        );
        let ht = HitTester::new(&line);
        for pair in run.caret_stops.windows(2) {
            // A point just past the midpoint toward the later stop selects it.
            let toward = pair[0].inline_x + (pair[1].inline_x - pair[0].inline_x) * 0.6;
            assert_eq!(ht.position_at_inline(toward).offset, pair[1].offset);
            let back = pair[0].inline_x + (pair[1].inline_x - pair[0].inline_x) * 0.4;
            assert_eq!(ht.position_at_inline(back).offset, pair[0].offset);
        }
    }

    #[test]
    fn ltr_midpoint_snaps_to_nearest_real_stop() {
        // A point closer to stop 2 than stop 1 resolves to offset 2 — decided by
        // real stop positions, so it works regardless of glyph width variation.
        let line = layout_line("AVA", BaseDirection::LeftToRight);
        let run = &line.runs[0];
        let x1 = run.inline_x_of(TextOffset(1)).unwrap();
        let x2 = run.inline_x_of(TextOffset(2)).unwrap();
        let ht = HitTester::new(&line);
        // 60% of the way from stop 1 to stop 2 is nearer stop 2.
        let p = x1 + (x2 - x1) * 0.6;
        assert_eq!(ht.position_at_inline(p).offset, TextOffset(2));
    }

    #[test]
    fn rtl_click_left_edge_hits_highest_logical_offset() {
        // In a pure RTL line the visual-left edge is the highest logical offset.
        // Clicking at the far left resolves there, not to logical 0 — assuming
        // byte order is visual order would return the wrong end.
        let text = "\u{05D0}\u{05D1}\u{05D2}";
        let line = layout_line(text, BaseDirection::RightToLeft);
        let ht = HitTester::new(&line);
        let left = ht.position_at_inline(0.0);
        let right = ht.position_at_inline(line.width);
        // Visual-left offset is greater than visual-right offset in an RTL run.
        assert!(
            left.offset.0 > right.offset.0,
            "RTL: visual-left offset {} should exceed visual-right {}",
            left.offset.0,
            right.offset.0
        );
    }

    #[test]
    fn hit_test_round_trips_with_caret_stops() {
        // Hitting exactly at each stop's inline x returns that stop's offset:
        // geometry and the caret map agree.
        let line = layout_line("AVA", BaseDirection::LeftToRight);
        let run = &line.runs[0];
        let ht = HitTester::new(&line);
        for stop in &run.caret_stops {
            assert_eq!(ht.position_at_inline(stop.inline_x).offset, stop.offset);
        }
    }

    #[test]
    fn mixed_line_hit_resolves_into_correct_run() {
        // "A" + Hebrew: clicking in the leftmost region lands in the Latin run
        // (offset 0 or 1), clicking at the far right lands in the RTL run.
        let text = "A\u{05D0}\u{05D1}";
        let line = layout_line(text, BaseDirection::LeftToRight);
        let ht = HitTester::new(&line);
        let leftmost = ht.position_at_inline(-1.0);
        assert_eq!(leftmost.offset, TextOffset(0));
        // Far right is inside the RTL run's logical range (bytes 1..5).
        let rightmost = ht.position_at_inline(line.width + 5.0);
        assert!(rightmost.offset.0 >= 1);
    }
}
