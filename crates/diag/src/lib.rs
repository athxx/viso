//! Source spans and the one diagnostic shape every compiler in the workspace
//! reports in.
//!
//! The `.vs` frontend and the shader compiler both emit [`Diagnostic`], so the
//! CLI renderer and the language server read one type whichever compiler
//! raised it. The shape mirrors the JSON diagnostic object: a severity, a
//! stable code, a primary span, optional related spans with labels, and
//! free-form notes and fixes.
//!
//! This is a cold-path structure — diagnostics are assembled once per build,
//! never on a frame path — so owned `String`/`Vec` fields are appropriate.

pub mod span;

pub use span::{LineCol, LineIndex, TextRange, TextSize};

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

/// One diagnostic, from any compiler phase.
///
/// `code` is a `'static` string from the emitting phase's code table, so it is
/// borrowed, never allocated; `message` has any subject folded in, so it is
/// owned. `related` carries secondary spans (a colliding
/// definition, a cycle participant) each with its own label, `expected` and
/// `actual` name what the source should have held and what it held, `notes`
/// carries free-form guidance, and `fixes` carries suggested source edits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    /// How severe the diagnostic is.
    pub severity: Severity,
    /// The stable diagnostic code (e.g. `"E2001"`, `"E1205"`, `"E1401"`).
    pub code: &'static str,
    /// The primary source span the diagnostic points at.
    pub primary: TextRange,
    /// Secondary spans with labels (a colliding declaration, a cycle member, …).
    pub related: Vec<Related>,
    /// What would have been accepted at the primary span, each alternative as
    /// source spells it (a type, a construct); empty when the diagnostic is not a
    /// mismatch.
    pub expected: Vec<String>,
    /// What the primary span holds instead, as source spells it.
    pub actual: Option<String>,
    /// Free-form notes offering guidance beyond the one-line message.
    pub notes: Vec<String>,
    /// Suggested source edits, each applicable on its own.
    pub fixes: Vec<Fix>,
    /// The rendered one-line message.
    pub message: String,
}

/// How safely a [`Fix`] may be applied without a human looking at it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Applicability {
    /// The edit is certainly what was meant; tooling may apply it unattended.
    MachineApplicable,
    /// The edit is a plausible guess (a nearest-name suggestion, say) and needs a
    /// human to confirm it.
    MaybeIncorrect,
}

impl Applicability {
    /// The JSON spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Applicability::MachineApplicable => "machine-applicable",
            Applicability::MaybeIncorrect => "maybe-incorrect",
        }
    }
}

/// A secondary span of a [`Diagnostic`], with its label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Related {
    /// The `::`-joined path of the module whose file holds `range` (`""` for the
    /// package root module), or `None` for the diagnostic's own file.
    pub module: Option<String>,
    /// The span.
    pub range: TextRange,
    /// What the span is, e.g. ``"`Badge` is declared here"``.
    pub label: String,
}

impl Related {
    /// A span in the diagnostic's own file.
    pub fn new(range: TextRange, label: impl Into<String>) -> Self {
        Self {
            module: None,
            range,
            label: label.into(),
        }
    }

    /// A span in the file of `module`.
    pub fn in_module(
        module: impl Into<String>,
        range: TextRange,
        label: impl Into<String>,
    ) -> Self {
        Self {
            module: Some(module.into()),
            range,
            label: label.into(),
        }
    }
}

/// One replacement of a source range. A [`Fix`]'s edits may span several files
/// and apply together.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextEdit {
    /// The `::`-joined path of the module whose file is edited (`""` for the
    /// package root module), or `None` for the diagnostic's own file.
    pub module: Option<String>,
    /// The range replaced.
    pub range: TextRange,
    /// The text put in its place.
    pub replacement: String,
}

impl TextEdit {
    /// A replacement in the diagnostic's own file.
    pub fn new(range: TextRange, replacement: impl Into<String>) -> Self {
        Self {
            module: None,
            range,
            replacement: replacement.into(),
        }
    }

    /// A replacement in the file of `module`.
    pub fn in_module(
        module: impl Into<String>,
        range: TextRange,
        replacement: impl Into<String>,
    ) -> Self {
        Self {
            module: Some(module.into()),
            range,
            replacement: replacement.into(),
        }
    }
}

/// A suggested fix: a titled set of edits applied together.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fix {
    /// A one-line description, e.g. ``replace with `Badge` ``.
    pub title: String,
    /// How safely the fix may be applied unattended.
    pub applicability: Applicability,
    /// The edits, applied atomically.
    pub edits: Vec<TextEdit>,
}

impl Diagnostic {
    /// A bare error diagnostic: a code, a primary span, and a message, with no
    /// related spans or notes.
    pub fn error(code: &'static str, primary: TextRange, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Error,
            code,
            primary,
            related: Vec::new(),
            expected: Vec::new(),
            actual: None,
            notes: Vec::new(),
            fixes: Vec::new(),
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
            expected: Vec::new(),
            actual: None,
            notes: Vec::new(),
            fixes: Vec::new(),
            message: message.into(),
        }
    }

    /// A bare note diagnostic, otherwise like [`Diagnostic::error`].
    pub fn note(code: &'static str, primary: TextRange, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Note,
            ..Self::error(code, primary, message)
        }
    }

    /// This diagnostic as a mismatch: `expected` lists what would have been
    /// accepted and `actual` names what was found.
    pub fn expecting<S: Into<String>>(
        mut self,
        expected: impl IntoIterator<Item = S>,
        actual: impl Into<String>,
    ) -> Self {
        self.expected = expected.into_iter().map(Into::into).collect();
        self.actual = Some(actual.into());
        self
    }

    /// This diagnostic with the secondary span `related`.
    pub fn with_related(mut self, related: Related) -> Self {
        self.related.push(related);
        self
    }

    /// This diagnostic with the free-form note `note`.
    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.notes.push(note.into());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn builders_fill_the_shape() {
        let d = Diagnostic::warning("E8105", span(), "outside the subset")
            .with_note("rewrite it")
            .with_related(Related::in_module("a::b", span(), "declared here"));
        assert_eq!(d.severity, Severity::Warning);
        assert_eq!(d.code, "E8105");
        assert_eq!(d.notes, ["rewrite it"]);
        assert_eq!(d.related[0].module.as_deref(), Some("a::b"));
        assert_eq!(Diagnostic::note("E1", span(), "m").severity, Severity::Note);
    }
}
