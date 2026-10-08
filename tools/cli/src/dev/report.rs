//! The record of one candidate revision of a `.vs` file, which `viso run`
//! reports as a `dev` event (`Viso_CLI.md` §36.4; `Viso_Hot_Reload.md` §37,
//! §51): built on the host from its own compile and the runtime's ACK or
//! NACK, the diagnostics reported before it from the source the host
//! compiled.

use std::sync::{Mutex, OnceLock};

use viso_dsl::{Diagnostic, Severity};
use viso_view::dev::wire::{CommitCounts, Stage};

/// How a candidate ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Committed with every state, focus and scroll offset kept.
    Applied,
    /// Committed, but a kept state was reset to its initializer, or a node lost
    /// its focus, scroll offset or handlers.
    ScopedReset,
    /// Not committed; the runtime kept its last-good revision.
    Rejected,
}

impl Outcome {
    /// The `dev` event spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Outcome::Applied => "applied",
            Outcome::ScopedReset => "scoped_reset",
            Outcome::Rejected => "rejected",
        }
    }

    /// The patch class of a committed candidate; a rejected one has none.
    pub fn patch_class(self) -> Option<&'static str> {
        match self {
            Outcome::Applied => Some("PATCH"),
            Outcome::ScopedReset => Some("PATCH_WITH_SCOPED_RESET"),
            Outcome::Rejected => None,
        }
    }

    /// The outcome of a commit that kept and lost `counts`.
    pub fn of_commit(counts: &CommitCounts) -> Self {
        if counts.scoped_resets() > 0 {
            Outcome::ScopedReset
        } else {
            Outcome::Applied
        }
    }
}

/// One candidate revision of a `.vs` file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReloadEvent {
    /// The revision the runtime matched before the candidate.
    pub base_revision: u64,
    /// The candidate's own revision: each batch of edits numbers the next,
    /// committed or not.
    pub candidate_revision: u64,
    /// The revision the runtime matches after the candidate.
    pub last_good_revision: u64,
    pub outcome: Outcome,
    /// Where a rejected candidate failed; `runtime-commit` for a committed one.
    pub stage: Stage,
    /// From the batch's compile to the runtime's answer.
    pub elapsed_us: u64,
    /// What the commit kept and lost; zero for a rejected candidate.
    pub counts: CommitCounts,
    /// The codes of its diagnostics and notices, or of the runtime's NACK.
    pub codes: Vec<String>,
}

impl ReloadEvent {
    /// The codes, in order, each once.
    pub fn codes(&self) -> Vec<&str> {
        let mut codes: Vec<&str> = Vec::new();
        for code in &self.codes {
            if !codes.contains(&code.as_str()) {
                codes.push(code);
            }
        }
        codes
    }
}

/// The §51 stage a candidate rejected with `diagnostics` failed at: that of
/// its first error, by the code ranges of Viso_DSL_1.0.md appendix C.
pub fn rejection_stage(diagnostics: &[Diagnostic]) -> Stage {
    let Some(code) = diagnostics
        .iter()
        .find(|d| d.severity == Severity::Error)
        .map(|d| d.code)
    else {
        return Stage::Typecheck;
    };
    match code.as_bytes() {
        [b'E', b'1', ..] => Stage::Parse,
        [b'E', b'2', b'0', ..] => Stage::Resolve,
        [b'E', b'2', b'6', ..] | [b'E', b'6', ..] => Stage::Capability,
        [b'E', b'5', ..] => Stage::StateCompat,
        [b'E', b'8', ..] => Stage::ShaderCompile,
        _ => Stage::Typecheck,
    }
}

/// The diagnostic code `code` as the compiler's `&'static str`, when it has
/// the appendix C shape (`E` and four digits). A runtime names only codes the
/// compiler has, so the few distinct ones are leaked once each.
pub fn intern_code(code: &str) -> Option<&'static str> {
    let bytes = code.as_bytes();
    if bytes.len() != 5 || bytes[0] != b'E' || !bytes[1..].iter().all(u8::is_ascii_digit) {
        return None;
    }
    static CODES: OnceLock<Mutex<Vec<&'static str>>> = OnceLock::new();
    let mut codes = CODES
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(&interned) = codes.iter().find(|&&c| c == code) {
        return Some(interned);
    }
    let interned: &'static str = Box::leak(code.to_owned().into_boxed_str());
    codes.push(interned);
    Some(interned)
}

#[cfg(test)]
mod tests {
    use viso_dsl::{TextRange, TextSize};

    use super::*;

    #[test]
    fn the_stage_is_that_of_the_first_error() {
        let range = TextRange::empty(TextSize::ZERO);
        let stage = |code| rejection_stage(&[Diagnostic::error(code, range, "")]);
        assert_eq!(stage("E1404"), Stage::Parse);
        assert_eq!(stage("E2001"), Stage::Resolve);
        assert_eq!(stage("E2103"), Stage::Typecheck);
        assert_eq!(stage("E2601"), Stage::Capability);
        assert_eq!(stage("E5102"), Stage::StateCompat);
        assert_eq!(stage("E8001"), Stage::ShaderCompile);
        let warned = [
            Diagnostic::warning("E1404", range, ""),
            Diagnostic::error("E2001", range, ""),
        ];
        assert_eq!(rejection_stage(&warned), Stage::Resolve);
    }

    #[test]
    fn codes_are_interned_once_and_shaped() {
        let a = intern_code("E5101").unwrap();
        assert!(std::ptr::eq(a, intern_code("E5101").unwrap()));
        assert_eq!(intern_code("HOT_RELOAD"), None);
        assert_eq!(intern_code("E51010"), None);
        let event = ReloadEvent {
            base_revision: 1,
            candidate_revision: 2,
            last_good_revision: 1,
            outcome: Outcome::Rejected,
            stage: Stage::Parse,
            elapsed_us: 0,
            counts: CommitCounts::default(),
            codes: vec!["E1405".into(), "E2103".into(), "E1405".into()],
        };
        assert_eq!(event.codes(), ["E1405", "E2103"]);
    }
}
