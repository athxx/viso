//! Locale and strictness tailoring for line breaking: CJK forbidden-position
//! rules (kinsoku) and per-locale adjustments to the UAX#14 defaults.
//!
//! The default UAX#14 opportunities from [`crate::line_break`] are tailored here
//! by a strictness level and locale so that, for example, a line does not begin
//! with a closing bracket or end with an opening one.

/// How strictly CJK forbidden-position rules are applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineBreakStrictness {
    /// Loosest permitted set.
    Loose,
    /// Common default.
    Normal,
    /// Strictest forbidden-position set.
    Strict,
}

/// Locale-and-strictness tailoring applied over UAX#14 opportunities.
#[derive(Debug, Default)]
pub struct LineBreakTailoring {
    // TODO(TF-P2): locale table + kinsoku forbidden leading/trailing sets.
}

impl LineBreakTailoring {
    /// Adjust a candidate break for forbidden-position rules under a strictness.
    pub fn tailor(&self, _strictness: LineBreakStrictness) {
        todo!("TF-P2: kinsoku forbidden-position tailoring")
    }
}
