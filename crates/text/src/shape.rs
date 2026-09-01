//! Text shaping: map a string to positioned glyph ids via rustybuzz.
//!
//! Scope for Phase 2: single face, left-to-right, one run per call (no BiDi,
//! no script itemization, no font fallback). Advances and offsets are returned
//! in **em units** so the caller applies pixel size later.

use crate::font::FontFace;

/// One shaped glyph: its id, source cluster (byte offset into the input), and
/// pen advance / positioning offset in em units.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ShapedGlyph {
    pub id: u16,
    pub cluster: u32,
    pub advance_em: f32,
    pub offset_x_em: f32,
    pub offset_y_em: f32,
}

/// Shape a single left-to-right run of `text` with `face`.
pub fn shape(face: &FontFace, text: &str) -> Vec<ShapedGlyph> {
    let rb = face.rb();
    let upem = face.units_per_em;

    let mut buffer = rustybuzz::UnicodeBuffer::new();
    buffer.push_str(text);
    buffer.set_direction(rustybuzz::Direction::LeftToRight);

    let output = rustybuzz::shape(&rb, &[], buffer);
    let infos = output.glyph_infos();
    let positions = output.glyph_positions();

    infos
        .iter()
        .zip(positions.iter())
        .map(|(info, pos)| ShapedGlyph {
            id: info.glyph_id as u16,
            cluster: info.cluster,
            advance_em: pos.x_advance as f32 / upem,
            offset_x_em: pos.x_offset as f32 / upem,
            offset_y_em: pos.y_offset as f32 / upem,
        })
        .collect()
}
