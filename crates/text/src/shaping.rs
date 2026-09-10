//! Complex-script shaping: turn a resolved, single-face, single-direction run
//! into positioned glyphs with clusters, driven by `rustybuzz`.
//!
//! Shaping is complex-text-complete: ligatures, marks, contextual forms, and
//! OpenType feature application. It runs on a worker, never the main thread, and
//! emits shaping clusters that map back to source byte offsets.

use crate::FontFaceId;

/// One positioned glyph produced by shaping, carrying the cluster (source byte
/// offset) it belongs to.
#[derive(Debug, Clone, Copy)]
pub struct ShapedGlyph {
    // TODO(TF-P0): glyph id, advance, offset, source cluster byte range.
}

/// The shaper over a resolved face.
#[derive(Debug, Default)]
pub struct Shaper {
    // TODO(TF-P0): rustybuzz face/buffer reuse keyed by FontFaceId.
}

impl Shaper {
    /// Shape one single-face, single-direction run into positioned glyphs.
    pub fn shape_run(&mut self, _face: FontFaceId, _text: &str) -> Vec<ShapedGlyph> {
        todo!("TF-P0: rustybuzz shaping of one resolved run")
    }
}
