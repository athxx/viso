//! Paragraph layout: place shaped glyphs into pixel space across lines.
//!
//! Scope: hard line breaks on `\n`, optional width-aware soft wrapping, and
//! left-to-right paragraph flow (bidi reordering is resolved per row by
//! [`shape`], which shapes each row's byte range in visual order). Each row is
//! shaped by [`shape`] over the font store's fallback chain, so a single row may
//! mix faces (Latin + CJK + emoji); the pen advances in each glyph's own em, and
//! every placed glyph carries the [`FontId`] it resolved to so the atlas
//! addresses the right face.
//!
//! Soft wrapping (when a `max_width` is given): each hard line is pre-shaped
//! once to measure per-segment advances, then split at [`linebreak`]
//! opportunities into rows no wider than the limit — primary breaks at word
//! boundaries, falling back to grapheme boundaries when one word alone overflows
//! a row, and force-placing a single grapheme that overflows an empty row (so
//! wrapping always terminates). A row's own byte range is then shaped for
//! placement, so each glyph is shaped exactly once across the whole paragraph
//! and the shaping context / bidi order within a row stays correct. With no
//! `max_width` a line is one row (the earlier hard-break-only behavior,
//! unchanged).
//!
//! Vertical metrics are per row, not per paragraph. A row starts from the
//! requested primary face's ascent / descent / line-gap and then **expands** to
//! the largest ascent, deepest descent, and largest line-gap across the faces
//! the row actually resolved to — a fallback face (a tall emoji or CJK glyph)
//! can rise higher than the primary text font, and reserving only the primary's
//! ascent would clip its top. Successive baselines advance by the previous
//! row's descent plus the next row's ascent plus the larger line-gap, so a row
//! that pulls in a taller face pushes the following row down accordingly.

use crate::FontId;
use crate::font::FontStore;
use crate::linebreak::{grapheme_break_offsets, word_break_offsets};
use crate::shape::{ShapedGlyph, shape};

/// A glyph placed in pixel space, ready to rasterize/position in the atlas.
///
/// `origin_px` is the glyph's pen origin (baseline point) in top-left pixel
/// coordinates: `x` grows right, `y` grows down. The rasterizer adds the
/// glyph's own bearing to reach the top-left of its bitmap. `font` is the chain
/// face the glyph resolved to — the atlas keys on it so fallback glyphs raster
/// from the face that actually rendered them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PositionedGlyph {
    pub font: FontId,
    pub id: u16,
    pub origin_px: [f32; 2],
}

/// A line's vertical metrics in pixels, expanded over every face the line uses.
struct LineMetrics {
    /// Baseline-to-top distance (positive).
    ascent: f32,
    /// Baseline-to-bottom distance (negative, below the baseline).
    descent: f32,
    /// Extra leading below this line before the next.
    line_gap: f32,
}

impl LineMetrics {
    /// Seed from the primary face; a line with no glyphs still reserves the
    /// paragraph font's height so blank lines advance sensibly.
    fn seed(store: &FontStore, primary: FontId, font_size_px: f32) -> Self {
        let f = store.face(primary);
        Self {
            ascent: f.ascender_em * font_size_px,
            descent: f.descender_em * font_size_px,
            line_gap: f.line_gap_em * font_size_px,
        }
    }

    /// Expand to include a glyph's resolving face — max ascent, min (deepest)
    /// descent, max line-gap.
    fn expand(&mut self, store: &FontStore, g: &ShapedGlyph, font_size_px: f32) {
        let f = store.face(g.font);
        self.ascent = self.ascent.max(f.ascender_em * font_size_px);
        self.descent = self.descent.min(f.descender_em * font_size_px);
        self.line_gap = self.line_gap.max(f.line_gap_em * font_size_px);
    }
}

/// Lay out `text` at `font_size_px`, shaping over `store`'s fallback chain and
/// sizing each row's box over the faces it resolved to. Returns per-glyph pen
/// origins in pixel space, each tagged with its resolving face.
///
/// `max_width_px` is the available content width for soft wrapping: `Some(w)`
/// breaks each hard line into rows no wider than `w`; `None` disables soft
/// wrapping, so only hard `\n` breaks split the text (the width-unaware
/// behavior). A width `<= 0` is treated as no wrap.
pub fn layout(
    store: &FontStore,
    font: FontId,
    text: &str,
    font_size_px: f32,
    max_width_px: Option<f32>,
) -> Vec<PositionedGlyph> {
    let wrap = max_width_px.filter(|w| *w > 0.0);
    let mut out = Vec::new();
    // Running baseline: below the top of the box by the first row's ascent,
    // then advanced per row by (prev descent depth + gap + next ascent).
    let mut baseline_y = 0.0f32;
    let mut prev: Option<LineMetrics> = None;

    for line in text.split('\n') {
        match wrap {
            // Width-aware: split this hard line into rows at break opportunities.
            Some(w) => {
                for row in wrap_line(store, line, font_size_px, w) {
                    place_row(
                        store,
                        font,
                        &row,
                        font_size_px,
                        &mut baseline_y,
                        &mut prev,
                        &mut out,
                    );
                }
                // A hard line that produced no rows (empty line) still advances
                // one row's height so blank lines take vertical space.
                if line.is_empty() {
                    place_row(
                        store,
                        font,
                        &[],
                        font_size_px,
                        &mut baseline_y,
                        &mut prev,
                        &mut out,
                    );
                }
            }
            // Width-unaware: the whole hard line is one row.
            None => {
                let glyphs = shape(store, line);
                place_row(
                    store,
                    font,
                    &glyphs,
                    font_size_px,
                    &mut baseline_y,
                    &mut prev,
                    &mut out,
                );
            }
        }
    }
    out
}

/// Place one row's already-shaped `glyphs` at the running baseline, advancing
/// `baseline_y` past this row and recording its metrics in `prev`. Emits one
/// [`PositionedGlyph`] per glyph into `out`.
fn place_row(
    store: &FontStore,
    font: FontId,
    glyphs: &[ShapedGlyph],
    font_size_px: f32,
    baseline_y: &mut f32,
    prev: &mut Option<LineMetrics>,
    out: &mut Vec<PositionedGlyph>,
) {
    let mut metrics = LineMetrics::seed(store, font, font_size_px);
    for g in glyphs {
        metrics.expand(store, g, font_size_px);
    }

    // Advance the baseline: the first row drops by its own ascent; each later
    // row drops by the previous row's descent depth, the larger of the two
    // line-gaps, and this row's ascent.
    *baseline_y += match prev.as_ref() {
        None => metrics.ascent,
        Some(p) => -p.descent + p.line_gap.max(metrics.line_gap) + metrics.ascent,
    };

    let mut pen_x = 0.0f32;
    for g in glyphs {
        let origin_x = pen_x + g.offset_x_em * font_size_px;
        let origin_y = *baseline_y - g.offset_y_em * font_size_px;
        out.push(PositionedGlyph {
            font: g.font,
            id: g.id,
            origin_px: [origin_x, origin_y],
        });
        pen_x += g.advance_em * font_size_px;
    }

    *prev = Some(metrics);
}

/// Split one hard line (`text`, no `\n`) into rows no wider than `max_width_px`,
/// returning each row already shaped for placement.
///
/// The line is pre-shaped once to measure per-break-segment advances; those
/// widths choose row byte boundaries at [`word_break_offsets`], falling back to
/// [`grapheme_break_offsets`] when one word overflows a row and to force-placing
/// a single grapheme when it overflows an empty row. Each chosen row byte range
/// is then reshaped so its glyphs carry correct within-row shaping context and
/// bidi order — and because rows partition the line, every byte is shaped once
/// for measurement and once for placement, never per candidate.
fn wrap_line(
    store: &FontStore,
    text: &str,
    font_size_px: f32,
    max_width_px: f32,
) -> Vec<Vec<ShapedGlyph>> {
    if text.is_empty() {
        return Vec::new();
    }

    // Per-byte advance in pixels, indexed by cluster start, summed so the width
    // of any byte range `[lo, hi)` is a difference of prefix sums. A glyph's
    // advance is attributed to its cluster's start byte; bytes with no glyph
    // (interior of a multi-byte cluster) carry zero, so the prefix sum is flat
    // across them and a break offset (always a segment boundary) reads the right
    // cumulative width regardless of bidi run order.
    let prefix = advance_prefix_px(store, text, font_size_px);

    let words = word_break_offsets(text);
    let mut rows = Vec::new();
    let mut row_start = 0usize; // byte offset where the current row begins

    while row_start < text.len() {
        // The widest word break offset that keeps `[row_start, offset)` within
        // the width limit. `None` means even the first word past `row_start`
        // overflows the (empty) row.
        let fitted = widest_fit(&prefix, &words, row_start, max_width_px);
        let row_end = match fitted {
            Some(end) => end,
            None => {
                // One word overflows an empty row: fall back to grapheme breaks
                // within that word so it can split mid-word.
                let next_word = next_offset(&words, row_start);
                let graphemes = grapheme_break_offsets(&text[row_start..next_word]);
                // Offsets are relative to the word slice; rebase to the line.
                let graphemes: Vec<usize> = graphemes.iter().map(|g| row_start + g).collect();
                match widest_fit(&prefix, &graphemes, row_start, max_width_px) {
                    Some(end) => end,
                    // A single grapheme overflows the empty row: force-place it
                    // so wrapping terminates rather than looping.
                    None => next_offset(&graphemes, row_start),
                }
            }
        };
        rows.push(shape(store, &text[row_start..row_end]));
        row_start = row_end;
    }

    rows
}

/// The largest offset in `offsets` (all `> from`) such that the byte range
/// `[from, offset)` is no wider than `max_width_px`, or `None` if even the
/// smallest such offset overflows.
fn widest_fit(prefix: &[f32], offsets: &[usize], from: usize, max_width_px: f32) -> Option<usize> {
    let base = prefix[from];
    let mut best = None;
    for &off in offsets {
        if off <= from {
            continue;
        }
        if prefix[off] - base <= max_width_px {
            best = Some(off);
        } else {
            // Offsets are ascending, so once one overflows all later do too.
            break;
        }
    }
    best
}

/// The first offset in `offsets` strictly greater than `from`, or `prefix`-less
/// callers pass the line length; here it is the next segment boundary after
/// `from`. `offsets` always ends at the slice length, so a boundary exists.
fn next_offset(offsets: &[usize], from: usize) -> usize {
    offsets
        .iter()
        .copied()
        .find(|&o| o > from)
        .expect("break offsets end at the text length, so one exists past `from`")
}

/// Prefix sums of per-cluster pixel advance over `text`, length `text.len() + 1`
/// so `prefix[i]` is the total advance of all glyphs whose cluster starts before
/// byte `i`. Built from one whole-line shape: advance magnitude per glyph is
/// independent of bidi run order, so a byte-range width is `prefix[hi] -
/// prefix[lo]`.
fn advance_prefix_px(store: &FontStore, text: &str, font_size_px: f32) -> Vec<f32> {
    let mut per_byte = vec![0.0f32; text.len() + 1];
    for g in shape(store, text) {
        // Attribute the advance to the cluster's start byte; the prefix sum
        // spreads it to every offset at or past the next byte.
        per_byte[g.cluster as usize] += g.advance_em * font_size_px;
    }
    let mut prefix = vec![0.0f32; text.len() + 1];
    let mut acc = 0.0f32;
    for i in 0..text.len() {
        acc += per_byte[i];
        prefix[i + 1] = acc;
    }
    prefix
}
