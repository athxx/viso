//! Self-contained shader diagnostics (architecture section 30 / 36, AGENTS 19).
//!
//! `viso-shader` sits below `viso-dsl` in the crate DAG (render → shader → gpu;
//! dsl is above), so it cannot import the DSL's `Diagnostic` even though
//! architecture section 36 permits the two compilers to *share* diagnostic
//! infrastructure — the dependency direction forbids that specific edge. This
//! module therefore carries a parallel-but-independent diagnostic type: the same
//! severity ordering, a stable string code, notes, and a rendered message, so a
//! future shared renderer could treat both uniformly, without `viso-shader`
//! depending on `viso-dsl`.
//!
//! Shader diagnostics locate a problem by [`CompileStage`](crate::CompileStage)
//! rather than a text span: the built-in primitives are described by a Rust-side
//! structured IR builder, not parsed `.vs` text, so there is no source range to
//! point at yet (the shader text frontend is deferred — see `todo.md`). When a
//! text frontend lands, a primary span can be added alongside the stage, matching
//! the DSL diagnostic shape.
//!
//! This is a cold-path structure — diagnostics are assembled once per compile,
//! never on a frame path — so owned `String`/`Vec` fields are appropriate
//! (AGENTS section 7.2).

use crate::CompileStage;

/// How severe a shader diagnostic is. Ordered least-to-most severe so a pass can
/// take the maximum severity of a set with `Ord`, matching the DSL `Severity`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    /// An informational annotation that does not by itself fail a compile.
    Note,
    /// A problem that should be surfaced but does not stop compilation.
    Warning,
    /// A hard error: the compile cannot proceed past this.
    Error,
}

/// One shader diagnostic, from any [`CompileStage`].
///
/// `code` is a `'static` string (e.g. `"Shader0001"`), so it is borrowed, never
/// allocated; `message` is owned. `stage` records which pipeline stage produced
/// the diagnostic (source → parsed syntax → typed IR → validation → codegen),
/// standing in for the primary text span the DSL diagnostic carries until a
/// shader text frontend exists. `notes` carries free-form guidance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    /// How severe the diagnostic is.
    pub severity: Severity,
    /// The stable diagnostic code (e.g. `"Shader0001"`).
    pub code: &'static str,
    /// The pipeline stage that produced the diagnostic.
    pub stage: CompileStage,
    /// Free-form notes offering guidance beyond the one-line message.
    pub notes: Vec<String>,
    /// The rendered one-line message.
    pub message: String,
}

impl Diagnostic {
    /// A bare error diagnostic: a code, the stage it came from, and a message,
    /// with no notes. The common shape validation failures start from.
    pub fn error(code: &'static str, stage: CompileStage, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Error,
            code,
            stage,
            notes: Vec::new(),
            message: message.into(),
        }
    }

    /// A bare warning diagnostic, otherwise like [`Diagnostic::error`].
    pub fn warning(code: &'static str, stage: CompileStage, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Warning,
            code,
            stage,
            notes: Vec::new(),
            message: message.into(),
        }
    }

    /// Attach a free-form note, builder-style.
    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.notes.push(note.into());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn severity_orders_note_below_warning_below_error() {
        assert!(Severity::Note < Severity::Warning);
        assert!(Severity::Warning < Severity::Error);
        // The worst of a set is the max, matching how a pass rolls up severity.
        let worst = [Severity::Note, Severity::Error, Severity::Warning]
            .into_iter()
            .max()
            .unwrap();
        assert_eq!(worst, Severity::Error);
    }

    #[test]
    fn error_and_warning_carry_stage_and_code() {
        let e = Diagnostic::error("Shader0001", CompileStage::Validation, "bad layout");
        assert_eq!(e.severity, Severity::Error);
        assert_eq!(e.code, "Shader0001");
        assert_eq!(e.stage, CompileStage::Validation);
        assert!(e.notes.is_empty());

        let w = Diagnostic::warning("Shader0002", CompileStage::TypedIr, "unused varying")
            .with_note("consider removing it");
        assert_eq!(w.severity, Severity::Warning);
        assert_eq!(w.notes, ["consider removing it"]);
    }
}
