//! Bidirectional text (UAX#9): resolve embedding levels for a paragraph and
//! reorder runs from logical to visual order.
//!
//! BiDi resolution produces the per-character levels shaping and line layout
//! use to split direction runs and to reorder each line for display. It uses
//! `unicode-bidi`. Caret motion across a direction boundary is disambiguated by
//! [`crate::text_position::CaretAffinity`].

/// Resolved bidirectional levels for a paragraph.
#[derive(Debug, Default)]
pub struct BidiInfo {
    // TODO(TF-P2): per-char embedding levels + paragraph base direction.
}

impl BidiInfo {
    /// Resolve embedding levels for paragraph source text.
    pub fn resolve(_text: &str) -> Self {
        todo!("TF-P2: UAX#9 level resolution")
    }
}
