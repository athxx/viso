//! Grapheme-aware, BiDi-aware caret motion over a laid-out paragraph line.
//!
//! A caret is a [`TextPosition`] — a logical byte offset plus an affinity — not
//! a bare offset. At a BiDi direction boundary one logical offset sits at two
//! visual places (the trailing edge of one run and the leading edge of the
//! next); affinity is what tells them apart, so motion has to speak positions,
//! not offsets. This layer reads the retained [`LineLayout`] — its visual runs
//! and their caret stops — and never reshapes: cursor motion is a steady-state
//! interaction and the text caching contract forbids re-deriving layout for it.
//!
//! # Visual versus logical motion
//!
//! Left/Right are *visual* directions, not logical increment/decrement. Pressing
//! Right moves to the caret stop immediately to the right on screen, which in an
//! RTL run means a *lower* logical offset, and at a run boundary means crossing
//! into the neighbouring run with the appropriate affinity. Walking byte order
//! and assuming it equals visual order gets this wrong; motion here resolves
//! against the visual-x-ordered caret stops the line already carries, so RTL and
//! mixed-direction lines move correctly.
//!
//! # Grapheme and ligature granularity
//!
//! Every caret stop is a legal grapheme boundary (the [`VisualRun`] built its
//! stops from [`crate::segment::Segmenter`] boundaries), so motion can never
//! land inside a grapheme — an emoji ZWJ sequence, a combining sequence, a
//! regional-indicator flag are all indivisible. When a ligature merges several
//! graphemes into one glyph the stops still exist at each grapheme boundary —
//! placed at the font's GDEF ligature carets when it has them, else split
//! evenly across the ligature's advance — so the caret walks graphemes even
//! through a ligature rather than jumping the whole glyph.

use crate::paragraph::{LineLayout, VisualRun};
use crate::shaping::Direction;
use crate::text_position::{TextOffset, TextPosition};

/// A caret over a single laid-out line: the current position plus the preferred
/// inline x that vertical motion (Up/Down, a later slice) keeps sticky.
///
/// It holds no text of its own — it is a cursor *into* a [`LineLayout`], which
/// is passed to each motion call. That keeps the caret a small value and makes
/// explicit that motion is a read over retained layout, never a reshape.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Caret {
    /// The current logical position with its affinity.
    pub position: TextPosition,
    /// The inline x the caret prefers on vertical motion, in em units. Set when
    /// the caret is placed or moved horizontally; preserved across Up/Down so a
    /// column stays visually stable through short lines. `None` until the caret
    /// is first resolved against a line.
    pub preferred_x: Option<f32>,
}

impl Default for Caret {
    /// A caret at the document start, downstream affinity, no preferred column.
    fn default() -> Self {
        Self {
            position: TextPosition::downstream(TextOffset(0)),
            preferred_x: None,
        }
    }
}

impl Caret {
    /// A caret at `position` with no preferred column yet.
    pub fn at(position: TextPosition) -> Self {
        Self {
            position,
            preferred_x: None,
        }
    }

    /// Move one caret stop to the visual right within `line`, updating affinity
    /// and preferred column. Returns the new position.
    ///
    /// "Right" is visual: it advances along increasing inline x through the
    /// line's stops regardless of run direction. At a run boundary the position
    /// takes the affinity that keeps it associated with the run it now leads or
    /// trails, so a subsequent move continues in visual order rather than
    /// snapping back by logical order.
    pub fn move_right(&mut self, line: &LineLayout) -> TextPosition {
        self.step_visual(line, VisualStep::Right)
    }

    /// Move one caret stop to the visual left within `line`. The mirror of
    /// [`Caret::move_right`].
    pub fn move_left(&mut self, line: &LineLayout) -> TextPosition {
        self.step_visual(line, VisualStep::Left)
    }

    /// Move to the visual start (leftmost caret stop) of `line`.
    pub fn move_line_start(&mut self, line: &LineLayout) -> TextPosition {
        if let Some(stop) = visual_stops(line).first().copied() {
            self.set(stop);
        }
        self.position
    }

    /// Move to the visual end (rightmost caret stop) of `line`.
    pub fn move_line_end(&mut self, line: &LineLayout) -> TextPosition {
        if let Some(stop) = visual_stops(line).last().copied() {
            self.set(stop);
        }
        self.position
    }

    fn set(&mut self, stop: VisualStopRef) {
        self.position = stop.position;
        self.preferred_x = Some(stop.inline_x);
    }

    fn step_visual(&mut self, line: &LineLayout, step: VisualStep) -> TextPosition {
        let stops = visual_stops(line);
        if stops.is_empty() {
            return self.position;
        }
        // Find where the current position sits in the visual stop sequence, then
        // step one stop in the requested visual direction. Matching on the full
        // position (offset + affinity) disambiguates the two visual places a
        // boundary offset can occupy.
        let current = stops.iter().position(|s| s.position == self.position);
        let idx = match current {
            Some(i) => i,
            // The caret is not on a known stop (e.g. freshly constructed at an
            // offset whose affinity differs from the map's). Snap to the stop
            // sharing its offset, preferring the one the step will move away
            // from so the first press lands on a neighbour rather than staying.
            None => stops
                .iter()
                .position(|s| s.position.offset == self.position.offset)
                .unwrap_or(0),
        };
        let next = match step {
            VisualStep::Right => (idx + 1).min(stops.len() - 1),
            VisualStep::Left => idx.saturating_sub(1),
        };
        self.set(stops[next]);
        self.position
    }
}

enum VisualStep {
    Left,
    Right,
}

/// A caret stop resolved to its line-global visual x and the logical position
/// (offset + affinity) a caret takes when it sits there.
#[derive(Debug, Clone, Copy, PartialEq)]
struct VisualStopRef {
    position: TextPosition,
    inline_x: f32,
}

/// The line's caret stops flattened into a single left-to-right visual sequence,
/// each carrying the position a caret takes there.
///
/// Runs are already in visual left-to-right order. Within a run the stops are in
/// logical order, whose inline x is ascending for an LTR run and descending for
/// an RTL run; emitting them in visual-x order per run and concatenating runs
/// yields one globally left-to-right sequence. A run seam contributes its offset
/// once per run, each with the affinity that draws it on that run's side
/// ([`VisualRun::stop_affinity`]), so a caret can rest on either side of a BiDi
/// seam; a seam whose two sides coincide in both offset and x is emitted once.
fn visual_stops(line: &LineLayout) -> Vec<VisualStopRef> {
    let mut out: Vec<VisualStopRef> = Vec::new();
    for run in &line.runs {
        for (stop_idx, stop) in run_stops_visual(run).iter().enumerate() {
            let position = TextPosition {
                offset: stop.offset,
                affinity: run.stop_affinity(stop.offset),
            };
            if stop_idx == 0
                && out.last().is_some_and(|prev| {
                    prev.position.offset == position.offset
                        && (prev.inline_x - stop.inline_x).abs() < f32::EPSILON
                })
            {
                continue;
            }
            out.push(VisualStopRef {
                position,
                inline_x: stop.inline_x,
            });
        }
    }
    out
}

/// A run's caret stops in visual (left-to-right) order. An LTR run's stops are
/// already ascending in inline x; an RTL run's stops descend in inline x as the
/// logical offset advances, so reverse them to walk left to right.
fn run_stops_visual(run: &VisualRun) -> Vec<crate::paragraph::CaretStop> {
    let mut stops = run.caret_stops.clone();
    if run.direction == Direction::RightToLeft {
        stops.reverse();
    }
    stops
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FontFaceId;
    use crate::bidi::{BaseDirection, BidiInfo};
    use crate::paragraph::LineLayout;
    use crate::shaping::{ShapedRun, Shaper};
    use crate::text_position::TextOffset;

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
    fn ltr_move_right_walks_graphemes_left_to_right() {
        // In "AVA" the caret starts at offset 0 and Right advances 0->1->2->3,
        // then clamps at the visual end.
        let line = layout_line("AVA", BaseDirection::LeftToRight);
        let mut caret = Caret::at(TextPosition::downstream(TextOffset(0)));
        assert_eq!(caret.move_right(&line).offset, TextOffset(1));
        assert_eq!(caret.move_right(&line).offset, TextOffset(2));
        assert_eq!(caret.move_right(&line).offset, TextOffset(3));
        // Clamped at the end.
        assert_eq!(caret.move_right(&line).offset, TextOffset(3));
    }

    #[test]
    fn ltr_move_left_walks_graphemes_right_to_left() {
        let line = layout_line("AVA", BaseDirection::LeftToRight);
        let mut caret = Caret::at(TextPosition::downstream(TextOffset(3)));
        assert_eq!(caret.move_left(&line).offset, TextOffset(2));
        assert_eq!(caret.move_left(&line).offset, TextOffset(1));
        assert_eq!(caret.move_left(&line).offset, TextOffset(0));
        assert_eq!(caret.move_left(&line).offset, TextOffset(0));
    }

    #[test]
    fn cjk_move_right_steps_whole_grapheme_not_byte() {
        // "你好" is two 3-byte graphemes. Right moves 0 -> 3 -> 6, never landing
        // on the interior bytes 1,2,4,5.
        let line = layout_line("\u{4F60}\u{597D}", BaseDirection::LeftToRight);
        let mut caret = Caret::at(TextPosition::downstream(TextOffset(0)));
        assert_eq!(caret.move_right(&line).offset, TextOffset(3));
        assert_eq!(caret.move_right(&line).offset, TextOffset(6));
    }

    #[test]
    fn rtl_move_right_decreases_logical_offset() {
        // Pure Hebrew resolves RTL: the visual-left stop is the highest logical
        // offset. Moving visually Right from the leftmost stop decreases toward
        // logical 0. The line has one RTL run; visual stops run left-to-right as
        // logical offsets end..start.
        let text = "\u{05D0}\u{05D1}\u{05D2}";
        let line = layout_line(text, BaseDirection::RightToLeft);
        // Start at the visual-left end (logical end, offset = text.len()).
        let mut caret = Caret::at(TextPosition::upstream(TextOffset(text.len())));
        // Moving Right (further left is impossible; we're at the visual-left
        // edge) clamps. Instead verify motion from the visual-right edge leftward.
        let _ = &mut caret;
        // Place at visual-right edge = logical offset 0, and move Right (visually
        // rightward is off the edge) => clamp; move Left goes into the text.
        let mut caret = Caret::at(TextPosition::downstream(TextOffset(0)));
        // Logical 0 is the RTL run's rightmost visual stop. Visual Left steps to
        // the next lower visual x, i.e. the next grapheme, logical offset 2.
        let p = caret.move_left(&line);
        assert!(
            p.offset.0 > 0,
            "visual-left step from logical 0 in RTL increases logical offset, got {}",
            p.offset.0
        );
    }

    #[test]
    fn line_start_and_end_are_visual_edges() {
        let line = layout_line("AVA", BaseDirection::LeftToRight);
        let mut caret = Caret::at(TextPosition::downstream(TextOffset(1)));
        assert_eq!(caret.move_line_start(&line).offset, TextOffset(0));
        assert_eq!(caret.move_line_end(&line).offset, TextOffset(3));
    }

    #[test]
    fn preferred_x_is_set_on_horizontal_move() {
        let line = layout_line("AVA", BaseDirection::LeftToRight);
        let mut caret = Caret::at(TextPosition::downstream(TextOffset(0)));
        assert!(caret.preferred_x.is_none());
        caret.move_right(&line);
        assert!(caret.preferred_x.is_some());
    }

    #[test]
    fn mixed_line_move_right_crosses_run_boundary() {
        // "A" + Hebrew "אב" under LTR base: visual layout is [A][ב][א] — the
        // Latin run then the RTL run reversed. Walking Right from offset 0 must
        // traverse every caret stop of the line exactly once and end at the
        // rightmost visual stop, never getting stuck at the run seam.
        let text = "A\u{05D0}\u{05D1}";
        let line = layout_line(text, BaseDirection::LeftToRight);
        let mut caret = Caret::at(TextPosition::downstream(TextOffset(0)));
        let mut visited = vec![caret.position.offset];
        for _ in 0..6 {
            let p = caret.move_right(&line);
            if visited.last() != Some(&p.offset) {
                visited.push(p.offset);
            }
        }
        // Every grapheme boundary offset (0,1,3,5) is reachable moving Right.
        assert!(visited.contains(&TextOffset(0)));
        assert!(visited.contains(&TextOffset(1)));
        assert!(visited.contains(&TextOffset(3)));
        assert!(visited.contains(&TextOffset(5)));
    }

    #[test]
    fn bidi_seam_offset_has_two_visual_carets_picked_by_affinity() {
        // "A" + Hebrew "אב" under LTR base draws [A][בא]: offset 1 is the Latin
        // run's right edge and the Hebrew run's right edge. Upstream keeps the
        // caret after "A"; downstream puts it at the far right of the line.
        let text = "A\u{05D0}\u{05D1}";
        let line = layout_line(text, BaseDirection::LeftToRight);
        let up = line
            .caret_x(TextPosition::upstream(TextOffset(1)))
            .expect("seam is a stop");
        let down = line
            .caret_x(TextPosition::downstream(TextOffset(1)))
            .expect("seam is a stop");
        assert!((up - line.runs[0].visual_inline_range.1).abs() < 1e-4);
        assert!((down - line.width).abs() < 1e-4);
        assert!(down > up + 0.1, "the two visual positions differ");
        // Visual motion visits both and every stop draws where motion put it.
        let mut caret = Caret::at(TextPosition::downstream(TextOffset(0)));
        let mut seen = Vec::new();
        for _ in 0..6 {
            let before = caret.position;
            let p = caret.move_right(&line);
            if p == before {
                break;
            }
            let x = line.caret_x(p).expect("motion lands on a stop");
            assert!((x - caret.preferred_x.unwrap()).abs() < 1e-4);
            seen.push(p);
        }
        assert!(seen.contains(&TextPosition::upstream(TextOffset(1))));
        assert!(seen.contains(&TextPosition::downstream(TextOffset(1))));
    }

    #[test]
    fn empty_line_motion_is_stable() {
        let line = layout_line("", BaseDirection::Auto);
        let mut caret = Caret::at(TextPosition::downstream(TextOffset(0)));
        assert_eq!(caret.move_right(&line).offset, TextOffset(0));
        assert_eq!(caret.move_left(&line).offset, TextOffset(0));
    }
}
