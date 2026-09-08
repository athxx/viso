//! Paragraph layout: place shaped glyphs into pixel space across lines.
//!
//! Scope: hard line breaks on `\n`, left-to-right paragraph flow, no automatic
//! word wrapping or justification. Each line is shaped by [`shape`] over the
//! font store's fallback chain, so a single line may mix faces (Latin + CJK +
//! emoji); the pen advances in each glyph's own em, and every placed glyph
//! carries the [`FontId`] it resolved to so the atlas addresses the right face.
//!
//! Vertical metrics are per line, not per paragraph. A line starts from the
//! requested primary face's ascent / descent / line-gap and then **expands** to
//! the largest ascent, deepest descent, and largest line-gap across the faces
//! the line actually resolved to — a fallback face (a tall emoji or CJK glyph)
//! can rise higher than the primary text font, and reserving only the primary's
//! ascent would clip its top. Successive baselines advance by the previous
//! row's descent plus the next row's ascent plus the larger line-gap, so a line
//! that pulls in a taller face pushes the following line down accordingly.

use crate::FontId;
use crate::font::FontStore;
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

/// Lay out `text` at `font_size_px`, shaping each line over `store`'s fallback
/// chain and sizing each line's box over the faces it resolved to. Returns
/// per-glyph pen origins in pixel space, each tagged with its resolving face.
pub fn layout(
    store: &FontStore,
    font: FontId,
    text: &str,
    font_size_px: f32,
) -> Vec<PositionedGlyph> {
    let mut out = Vec::new();
    // Running baseline: below the top of the box by the first line's ascent,
    // then advanced per line by (prev descent depth + gap + next ascent).
    let mut baseline_y = 0.0f32;
    let mut prev: Option<LineMetrics> = None;

    for line in text.split('\n') {
        // Shape the line first so metrics can expand over the faces it uses.
        let glyphs = shape(store, line);
        let mut metrics = LineMetrics::seed(store, font, font_size_px);
        for g in &glyphs {
            metrics.expand(store, g, font_size_px);
        }

        // Advance the baseline: the first line drops by its own ascent; each
        // later line drops by the previous line's descent depth, the larger of
        // the two line-gaps, and this line's ascent.
        baseline_y += match &prev {
            None => metrics.ascent,
            Some(p) => -p.descent + p.line_gap.max(metrics.line_gap) + metrics.ascent,
        };

        let mut pen_x = 0.0f32;
        for g in &glyphs {
            let origin_x = pen_x + g.offset_x_em * font_size_px;
            let origin_y = baseline_y - g.offset_y_em * font_size_px;
            out.push(PositionedGlyph {
                font: g.font,
                id: g.id,
                origin_px: [origin_x, origin_y],
            });
            pen_x += g.advance_em * font_size_px;
        }

        prev = Some(metrics);
    }
    out
}
