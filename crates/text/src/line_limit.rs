//! How many lines a paragraph shows and how text it cuts off ends: the lines
//! past a limit are dropped, and a line that is cut, or that overflows the
//! width it is fitted to, ends in an ellipsis shaped like the rest of the
//! text, at its paragraph's end side.

use std::num::NonZeroU32;

use crate::bidi::{BidiInfo, BidiLevel};
use crate::paragraph::{
    CaretStop, LineLayout, Paragraph, ShapeFn, ShapedSpan, StyleRun, VisualRun, shaping_runs,
    style_at,
};
use crate::segment::Segmenter;
use crate::shaping::Direction;
use crate::text_position::TextOffset;

/// The mark a cut line ends in.
const ELLIPSIS: &str = "\u{2026}";

/// How many lines a paragraph shows, and whether text it cuts off ends in an
/// ellipsis.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct LineLimit {
    /// The most lines shown; `None` shows every line.
    pub max_lines: Option<NonZeroU32>,
    /// Whether a cut or overflowing line ends in an ellipsis.
    pub ellipsis: bool,
    /// The inline extent an ellipsized line fits within, in em; infinite
    /// when nothing bounds it.
    pub width: f32,
}

impl LineLimit {
    /// Whether the limit shows every line as laid out.
    pub fn is_none(&self) -> bool {
        self.max_lines.is_none() && !self.ellipsis
    }
}

impl Paragraph {
    /// The paragraph's laid-out lines as `limit` shows them; `None` when it
    /// shows them unchanged. The ellipsis and a cut line's remaining text
    /// shape through `shape`, the same faces and fallback as the paragraph.
    pub fn limited(&self, limit: LineLimit, shape: &mut ShapeFn<'_>) -> Option<Vec<LineLayout>> {
        let lines = self.lines();
        let text = self.text();
        let kept = limit
            .max_lines
            .map_or(lines.len(), |n| lines.len().min(n.get() as usize));
        // The empty line a trailing break owns holds no text to cut off.
        let cut = lines[kept..]
            .iter()
            .any(|line| line.logical_range.0.0 < text.len());
        let overflows = |line: &LineLayout| limit.ellipsis && line.width > limit.width;
        if !cut && !lines[..kept].iter().any(overflows) {
            return None;
        }
        let mut out = lines[..kept].to_vec();
        if limit.ellipsis {
            let bidi = BidiInfo::resolve(text, self.base());
            let last = out.len().saturating_sub(1);
            for (at, line) in out.iter_mut().enumerate() {
                if (cut && at == last) || overflows(line) {
                    *line = ellipsize(text, &bidi, self.styles(), line, limit.width, shape);
                }
            }
        }
        Some(out)
    }
}

/// `line` cut at the last grapheme boundary its text fits `width` from with
/// the ellipsis after it, trailing white space dropped; the ellipsis at the
/// paragraph's end side, in the style of the line's last character.
fn ellipsize(
    text: &str,
    bidi: &BidiInfo,
    styles: &[StyleRun],
    line: &LineLayout,
    width: f32,
    shape: &mut ShapeFn<'_>,
) -> LineLayout {
    let start = line.logical_range.0;
    let end = crate::paragraph::content_end(text, start, line.logical_range.1);
    let direction = if line.rtl {
        Direction::RightToLeft
    } else {
        Direction::LeftToRight
    };
    let last = text[start.0..end.0]
        .char_indices()
        .next_back()
        .map_or(start, |(at, _)| TextOffset(start.0 + at));
    let style = style_at(styles, last);
    let mark = shape(ELLIPSIS, direction, style);
    let room = width - mark.width_ems();
    let bounds: Vec<TextOffset> = Segmenter::new(&text[start.0..end.0])
        .grapheme_boundaries()
        .map(|b| TextOffset(start.0 + b.0))
        .collect();
    // The widest prefix that fits: the first boundary always does, as an
    // empty prefix, so the search keeps at least the ellipsis.
    let (mut lo, mut hi) = (0, bounds.len() - 1);
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        let spans = spans(text, bidi, styles, (start, bounds[mid]), shape);
        if spans.iter().map(ShapedSpan::width_ems).sum::<f32>() <= room {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    let to = bounds[lo];
    let to = TextOffset(start.0 + text[start.0..to.0].trim_end().len());
    let shaped = spans(text, bidi, styles, (start, to), shape);
    let mut out = LineLayout::in_styled_range(text, bidi, styles, (start, to), &shaped);
    let level = BidiLevel(u8::from(line.rtl));
    let mark_width = mark.width_ems();
    let mut marks = Vec::with_capacity(mark.segments.len());
    let mut left = if line.rtl { 0.0 } else { out.width };
    let mut place = |segment: &crate::paragraph::ShapedSegment| {
        let right = left + segment.run.width_ems;
        marks.push(VisualRun {
            logical_range: (to, to),
            visual_inline_range: (left, right),
            direction,
            level,
            face: segment.run.face,
            style,
            caret_stops: vec![CaretStop {
                offset: to,
                inline_x: if line.rtl { right } else { left },
            }],
            glyphs: segment.run.glyphs.clone(),
        });
        left = right;
    };
    if line.rtl {
        mark.segments.iter().rev().for_each(&mut place);
        for run in &mut out.runs {
            run.visual_inline_range.0 += mark_width;
            run.visual_inline_range.1 += mark_width;
            for stop in &mut run.caret_stops {
                stop.inline_x += mark_width;
            }
        }
        marks.append(&mut out.runs);
        out.runs = marks;
    } else {
        mark.segments.iter().for_each(&mut place);
        out.runs.append(&mut marks);
    }
    out.width += mark_width;
    out
}

/// The shaped pieces of `text[range]`, in logical order.
fn spans(
    text: &str,
    bidi: &BidiInfo,
    styles: &[StyleRun],
    range: (TextOffset, TextOffset),
    shape: &mut ShapeFn<'_>,
) -> Vec<ShapedSpan> {
    shaping_runs(bidi, styles, range.0, range.1)
        .iter()
        .map(|piece| {
            let run = &piece.run;
            shape(&text[run.start.0..run.end.0], run.direction, piece.style)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bidi::BaseDirection;
    use crate::paragraph::ShapedSegment;
    use crate::shaping::{ShapedGlyph, ShapedRun};

    /// Every char one em wide, its glyph its code point.
    fn monospace(text: &str, _: Direction, _: u16) -> ShapedSpan {
        let glyphs: Vec<ShapedGlyph> = text
            .char_indices()
            .map(|(at, ch)| ShapedGlyph {
                glyph_id: ch as u16,
                cluster: at as u32,
                x_advance: 1.0,
                x_offset: 0.0,
                y_offset: 0.0,
                unsafe_to_break: false,
            })
            .collect();
        ShapedSpan {
            segments: vec![ShapedSegment {
                start: 0,
                run: ShapedRun {
                    face: crate::FontFaceId(1),
                    width_ems: glyphs.len() as f32,
                    glyphs,
                    text_len: text.len() as u32,
                    ligature_carets: Vec::new(),
                },
            }],
        }
    }

    fn laid(text: &str, width: f32) -> Paragraph {
        let mut paragraph = Paragraph::new(text, BaseDirection::Auto, 0);
        paragraph.layout(width, &mut monospace);
        paragraph
    }

    /// The chars `line` draws, left to right.
    fn drawn(line: &LineLayout) -> String {
        line.runs
            .iter()
            .flat_map(|run| &run.glyphs)
            .map(|g| char::from_u32(u32::from(g.glyph_id)).unwrap_or('?'))
            .collect()
    }

    fn limit(max_lines: u32, ellipsis: bool, width: f32) -> LineLimit {
        LineLimit {
            max_lines: NonZeroU32::new(max_lines),
            ellipsis,
            width,
        }
    }

    #[test]
    fn a_paragraph_within_its_limit_is_shown_unchanged() {
        let paragraph = laid("one two", 10.0);
        assert!(
            paragraph
                .limited(limit(2, true, 10.0), &mut monospace)
                .is_none()
        );
        let trailing = laid("one\n", 10.0);
        assert!(
            trailing
                .limited(limit(1, true, 10.0), &mut monospace)
                .is_none(),
            "a trailing break cuts nothing off"
        );
    }

    #[test]
    fn lines_past_the_limit_are_dropped_and_the_last_kept_one_ends_in_an_ellipsis() {
        let text = "one two three";
        let paragraph = laid(text, 4.0);
        assert_eq!(paragraph.lines().len(), 3);
        let clipped = paragraph
            .limited(limit(2, false, 4.0), &mut monospace)
            .expect("cut");
        assert_eq!(clipped.len(), 2);
        assert_eq!(drawn(&clipped[1]), "two ");
        let ended = paragraph
            .limited(limit(2, true, 4.0), &mut monospace)
            .expect("cut");
        assert_eq!(drawn(&ended[1]), "two\u{2026}", "trailing space dropped");
        assert_eq!(ended[1].width, 4.0);
    }

    #[test]
    fn an_overflowing_line_is_cut_to_fit_with_its_ellipsis() {
        let text = "abcdefgh";
        let paragraph = laid(text, 0.0);
        let cut = paragraph
            .limited(limit(0, true, 5.0), &mut monospace)
            .expect("overflows");
        assert_eq!(drawn(&cut[0]), "abcd\u{2026}");
        assert_eq!(cut[0].width, 5.0);
        let none = paragraph
            .limited(limit(0, true, 0.5), &mut monospace)
            .expect("overflows");
        assert_eq!(drawn(&none[0]), "\u{2026}", "at least the ellipsis");
    }

    #[test]
    fn a_right_to_left_line_ends_in_its_ellipsis_on_the_left() {
        let text = "\u{5d0}\u{5d1}\u{5d2}\u{5d3}\u{5d4}\u{5d5}";
        let paragraph = laid(text, 0.0);
        let cut = paragraph
            .limited(limit(0, true, 4.0), &mut monospace)
            .expect("overflows");
        let line = &cut[0];
        assert!(line.rtl);
        assert_eq!(line.runs[0].glyphs[0].glyph_id, 0x2026);
        assert_eq!(line.runs[0].visual_inline_range, (0.0, 1.0));
        assert_eq!(line.runs[1].visual_inline_range, (1.0, 4.0));
        assert_eq!(line.width, 4.0);
    }
}
