//! The shared diagnostic type every frontend phase emits into (doc sections 30
//! and 138).
//!
//! The lexer, parser, and resolver each keep a *kind* enum — the single source of
//! the stable diagnostic-code and message string tables — but they no longer carry
//! their own wrapper structs. Instead each family lifts a kind (plus a span and,
//! where relevant, a subject) into one uniform [`Diagnostic`], so every consumer
//! downstream (Typed HIR, UI/Binding IR, LSP, Studio, the CLI renderer) reads a
//! single type. The shape mirrors the section 138 JSON schema: a severity, a stable
//! `E####`/`Lex####`/`Parse####` code, a primary span, optional related spans with
//! labels, and free-form notes.
//!
//! This is a cold-path structure — diagnostics are assembled once per build, never
//! on a frame path — so owned `String`/`Vec` fields are appropriate (AGENTS section
//! 7.2).

use crate::syntax::span::TextRange;

/// How severe a diagnostic is. Ordered least-to-most severe so a pass can take the
/// maximum severity of a set with `Ord` and a renderer can filter by threshold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    /// An informational annotation that does not by itself fail a build.
    Note,
    /// A problem that should be surfaced but does not stop compilation.
    Warning,
    /// A hard error: the build cannot proceed past this.
    Error,
}

/// One diagnostic, from any frontend phase.
///
/// `code` is a `'static` string from the emitting kind's `code()` table, so it is
/// borrowed, never allocated; `message` is the kind's `message()` with any subject
/// folded in, so it is owned. `related` carries secondary spans (a colliding
/// definition, a cycle participant) each with its own label, and `notes` carries
/// free-form guidance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    /// How severe the diagnostic is.
    pub severity: Severity,
    /// The stable diagnostic code (e.g. `"E2001"`, `"Parse0001"`, `"Lex0007"`).
    pub code: &'static str,
    /// The primary source span the diagnostic points at.
    pub primary: TextRange,
    /// Secondary spans with labels (a colliding declaration, a cycle member, …).
    pub related: Vec<(TextRange, String)>,
    /// Free-form notes offering guidance beyond the one-line message.
    pub notes: Vec<String>,
    /// The rendered one-line message.
    pub message: String,
}

impl Diagnostic {
    /// A bare error diagnostic: a code, a primary span, and a message, with no
    /// related spans or notes. The common shape every family's `to_diagnostic`
    /// starts from.
    pub fn error(code: &'static str, primary: TextRange, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Error,
            code,
            primary,
            related: Vec::new(),
            notes: Vec::new(),
            message: message.into(),
        }
    }

    /// A bare warning diagnostic, otherwise like [`Diagnostic::error`].
    pub fn warning(code: &'static str, primary: TextRange, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Warning,
            code,
            primary,
            related: Vec::new(),
            notes: Vec::new(),
            message: message.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::span::TextSize;

    fn span() -> TextRange {
        TextRange::new(TextSize::from(0), TextSize::from(1))
    }

    #[test]
    fn severity_orders_note_below_warning_below_error() {
        assert!(Severity::Note < Severity::Warning);
        assert!(Severity::Warning < Severity::Error);
        // A pass can take the worst severity of a set by `max`.
        let worst = [Severity::Note, Severity::Error, Severity::Warning]
            .into_iter()
            .max()
            .unwrap();
        assert_eq!(worst, Severity::Error);
    }

    #[test]
    fn a_parse_kind_lifts_into_a_diagnostic() {
        use crate::syntax::ParseErrorKind;
        let d = ParseErrorKind::UnclosedDelimiter.to_diagnostic(span());
        assert_eq!(d.severity, Severity::Error);
        assert_eq!(d.code, "Parse0001");
        assert_eq!(d.message, ParseErrorKind::UnclosedDelimiter.message());
    }

    #[test]
    fn a_lex_error_picks_its_severity_from_is_warning() {
        use crate::syntax::LexError;
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
