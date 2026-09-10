//! Hit testing: map a visual point in a laid-out paragraph to a logical
//! [`crate::text_position::TextPosition`], with correct affinity.
//!
//! Hit testing is the inverse of caret placement: a click resolves to the
//! nearest grapheme boundary and the affinity that matches which side of a
//! glyph the point fell on. It reads line layout; it does not reshape.

use crate::text_position::TextPosition;

/// Hit tester over a laid-out paragraph.
#[derive(Debug, Default)]
pub struct HitTester {
    // TODO(TF-P2): line/run geometry index for point resolution.
}

impl HitTester {
    /// Resolve a visual point to a logical caret position with affinity.
    pub fn position_at(&self, _x: f32, _y: f32) -> TextPosition {
        todo!("TF-P2: point -> TextPosition with affinity")
    }
}
