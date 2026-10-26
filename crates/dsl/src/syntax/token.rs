//! The tokenizer's tokens and lexical errors, and the diagnostic a lexical
//! error becomes.

pub use viso_lexer::token::*;

use super::span::TextRange;
use crate::diag::Diagnostic;

/// A lexical error lifted into the shared [`Diagnostic`].
pub trait LexDiagnostic {
    /// This error at `range`, its severity from [`LexError::is_warning`]. Lex
    /// errors ride on the green tokens themselves; this is the uniform way a
    /// caller assembling a unit's full diagnostic list folds them in
    /// alongside parse and resolve diagnostics.
    fn to_diagnostic(self, range: TextRange) -> Diagnostic;
}

impl LexDiagnostic for LexError {
    fn to_diagnostic(self, range: TextRange) -> Diagnostic {
        if self.is_warning() {
            Diagnostic::warning(self.code(), range, self.message())
        } else {
            Diagnostic::error(self.code(), range, self.message())
        }
    }
}
