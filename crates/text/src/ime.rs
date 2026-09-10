//! IME composition: the preedit / composition region a platform input method
//! drives, expressed in typed positions and bridged to UTF-16.
//!
//! The platform reports composition ranges and clause boundaries in UTF-16 code
//! units; this maps them through [`crate::text_position::Utf16Bridge`] to UTF-8
//! offsets and tracks the active composition so the paragraph can render preedit
//! styling without committing text early.

use crate::text_position::TextOffset;

/// The active IME composition over a paragraph.
#[derive(Debug, Default)]
pub struct ImeComposition {
    // TODO(TF-P2): composition byte range, clause segments, caret within preedit.
}

impl ImeComposition {
    /// Set the active composition range from platform-reported UTF-16 bounds.
    pub fn set_range(&mut self, _start: TextOffset, _end: TextOffset) {
        todo!("TF-P2: track preedit composition range")
    }
}
