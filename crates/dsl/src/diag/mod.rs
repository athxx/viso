//! The diagnostic type every frontend phase emits into, and the registry of
//! every code ([`codes`]).
//!
//! The lexer, parser, and resolver each keep a *kind* enum — the single source of
//! the stable diagnostic-code and message string tables — and lift a kind (plus a
//! span and, where relevant, a subject) into one uniform [`Diagnostic`]. The type
//! is `viso-diag`'s, the one the shader compiler emits too, so every consumer
//! downstream (Typed HIR, UI/Binding IR, LSP, Studio, the CLI renderer) reads a
//! single type whichever compiler raised it.

pub mod codes;

pub use viso_diag::{Applicability, Diagnostic, Fix, Related, Severity, TextEdit};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::span::{TextRange, TextSize};

    fn span() -> TextRange {
        TextRange::new(TextSize::from(0), TextSize::from(1))
    }

    #[test]
    fn a_parse_kind_lifts_into_a_diagnostic() {
        use crate::syntax::ParseErrorKind;
        let d = ParseErrorKind::UnclosedDelimiter.to_diagnostic(span());
        assert_eq!(d.severity, Severity::Error);
        assert_eq!(d.code, "E1401");
        assert_eq!(d.message, ParseErrorKind::UnclosedDelimiter.message());
    }

    #[test]
    fn a_lex_error_picks_its_severity_from_is_warning() {
        use crate::syntax::{LexDiagnostic, LexError};
        // A confusable identifier is a warning; a bad number is an error.
        let warn = LexError::ConfusableIdent.to_diagnostic(span());
        assert_eq!(warn.severity, Severity::Warning);
        let err = LexError::UnterminatedString.to_diagnostic(span());
        assert_eq!(err.severity, Severity::Error);
    }

    #[test]
    fn a_resolve_kind_folds_its_subject_into_the_message() {
        use crate::resolve::ResolveErrorKind;
        let d = ResolveErrorKind::UnresolvedModule.to_diagnostic(Some(span()), "widgets::Button");
        assert_eq!(d.code, "E2001");
        assert!(
            d.message.contains("widgets::Button"),
            "the subject is folded into the message, got {:?}",
            d.message
        );
    }
}
