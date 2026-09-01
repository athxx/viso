//! Paragraph layout: place shaped glyphs into pixel space across lines.
//!
//! Scope for Phase 2: hard line breaks on `\n`, one face, left-to-right. Each
//! line is shaped independently; the pen advances left-to-right and the
//! baseline steps down by the font's line height. No automatic word wrapping,
//! BiDi, or justification.

use crate::FontId;
use crate::font::FontStore;
use crate::shape::shape;

/// A glyph placed in pixel space, ready to rasterize/position in the atlas.
///
/// `origin_px` is the glyph's pen origin (baseline point) in top-left pixel
/// coordinates: `x` grows right, `y` grows down. The rasterizer adds the
/// glyph's own bearing to reach the top-left of its bitmap.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PositionedGlyph {
    pub id: u16,
    pub origin_px: [f32; 2],
}

/// Lay out `text` with `font` at `font_size_px`, returning per-glyph pen
/// origins in pixel space.
pub fn layout(
    store: &FontStore,
    font: FontId,
    text: &str,
    font_size_px: f32,
) -> Vec<PositionedGlyph> {
    let face = store.face(font);
    let line_pitch = face.line_height_em() * font_size_px;
    // First baseline sits one ascender below the top of the layout box.
    let first_baseline = face.ascender_em * font_size_px;

    let mut out = Vec::new();
    for (line_idx, line) in text.split('\n').enumerate() {
        let baseline_y = first_baseline + line_idx as f32 * line_pitch;
        let mut pen_x = 0.0f32;
        for g in shape(face, line) {
            let origin_x = pen_x + g.offset_x_em * font_size_px;
            let origin_y = baseline_y - g.offset_y_em * font_size_px;
            out.push(PositionedGlyph {
                id: g.id,
                origin_px: [origin_x, origin_y],
            });
            pen_x += g.advance_em * font_size_px;
        }
    }
    out
}
