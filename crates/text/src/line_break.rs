//! Line breaking (UAX#14): find the mandatory and permitted break opportunities
//! in a paragraph, before width-driven line filling.
//!
//! This produces the opportunity set; the width fit and CJK forbidden-position
//! adjustment happen in [`crate::paragraph`], tailored by
//! [`crate::line_break_tailoring`].

use crate::text_position::TextOffset;

/// The class of a line-break opportunity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BreakOpportunity {
    /// A mandatory break (for example after a hard line feed).
    Mandatory,
    /// A permitted break the width fit may choose.
    Allowed,
}

/// The break-opportunity analyzer over a paragraph.
#[derive(Debug, Default)]
pub struct LineBreaker {
    // TODO(TF-P2): UAX#14 pair-table state.
}

impl LineBreaker {
    /// The next break opportunity at or after an offset, with its class.
    pub fn next_break(&self, _from: TextOffset) -> Option<(TextOffset, BreakOpportunity)> {
        todo!("TF-P2: UAX#14 break opportunities")
    }
}
