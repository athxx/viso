//! Paragraph layout: place shaped glyphs into pixel space across lines.
//!
//! Scope: hard line breaks on `\n`, left-to-right paragraph flow, no automatic
//! word wrapping or justification. Each line is shaped by [`shape`] over the
//! font store's fallback chain, so a single line may mix faces (Latin + CJK +
//! emoji); the pen advances in the glyph's own em, and every placed glyph
//! carries the [`FontId`] it resolved to so the atlas addresses the right face.
//!
//! Line metrics (baseline pitch, first-baseline drop) come from the requested
//! primary `font`: the layout box height is anchored on the paragraph's nominal
//! face rather than the tallest fallback glyph. Per-line max-ascent metrics are
//! a later refinement; for a single mixed line the primary's metrics keep the
//! baseline stable regardless of which fallback faces the run resolves to.

use crate::FontId;
use crate::font::FontStore;
use crate::shape::shape;

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

/// Lay out `text` at `font_size_px`, shaping each line over `store`'s fallback
/// chain and anchoring line metrics on the requested primary `font`. Returns
/// per-glyph pen origins in pixel space, each tagged with its resolving face.
pub fn layout(
    store: &FontStore,
    font: FontId,
    text: &str,
    font_size_px: f32,
) -> Vec<PositionedGlyph> {
    let metrics = store.face(font);
    let line_pitch = metrics.line_height_em() * font_size_px;
    // First baseline sits one ascender below the top of the layout box.
    let first_baseline = metrics.ascender_em * font_size_px;

    let mut out = Vec::new();
    for (line_idx, line) in text.split('\n').enumerate() {
        let baseline_y = first_baseline + line_idx as f32 * line_pitch;
        let mut pen_x = 0.0f32;
        for g in shape(store, line) {
            let origin_x = pen_x + g.offset_x_em * font_size_px;
            let origin_y = baseline_y - g.offset_y_em * font_size_px;
            out.push(PositionedGlyph {
                font: g.font,
                id: g.id,
                origin_px: [origin_x, origin_y],
            });
            pen_x += g.advance_em * font_size_px;
        }
    }
    out
}
