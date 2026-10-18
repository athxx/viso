//! The record of one candidate revision of a `.vs` file, which `viso run`
//! reports as a `dev` event: built on the host from its own compile and the runtime's ACK or
//! NACK, the diagnostics reported before it from the source the host
//! compiled.

use std::sync::{Mutex, OnceLock};

use viso_dev_protocol::wire::{CommitCounts, GameCommit, Stage};
use viso_dsl::{Diagnostic, Severity};

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
    /// Compiled, and not sent: the session runs with `--no-hot-reload`.
    Held,
    /// The app restarted on the new build and took its snapshot back.
    WarmRestarted,
}

impl Outcome {
    /// The `dev` event spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Outcome::Applied => "applied",
            Outcome::ScopedReset => "scoped_reset",
            Outcome::Rejected => "rejected",
            Outcome::Held => "held",
            Outcome::WarmRestarted => "warm_restarted",
        }
    }

    /// The patch class of a committed candidate; a rejected one has none.
    pub fn patch_class(self) -> Option<&'static str> {
        match self {
            Outcome::Applied => Some("PATCH"),
            Outcome::ScopedReset => Some("PATCH_WITH_SCOPED_RESET"),
            Outcome::WarmRestarted => Some("WARM_RESTART_REQUIRED"),
            Outcome::Rejected | Outcome::Held => None,
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
    /// Where a rejected candidate failed; `runtime-commit` for a committed
    /// one, `transport` for a held one.
    pub stage: Stage,
    /// From the batch's compile to the runtime's answer.
    pub elapsed_us: u64,
    /// What the commit kept and lost; zero for a rejected candidate.
    pub counts: CommitCounts,
    /// The codes of its diagnostics and notices, or of the runtime's NACK.
    pub codes: Vec<String>,
    /// What swapping the file's game kept, when the commit swapped it.
    pub game: Option<GameCommit>,
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

/// One build of the Rust sources a batch of Rust edits asked for: a
/// candidate whose class is a warm restart, reported as a `dev` event naming
/// the files it carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RebuildEvent {
    /// The changed Rust files, as the project names them.
    pub files: Vec<String>,
    /// The revision the runtime matched when the build ended.
    pub base_revision: u64,
    pub candidate_revision: u64,
    pub last_good_revision: u64,
    /// `Rejected` for a failed build, `Held` for one built and not run.
    pub outcome: Outcome,
    /// The codes of the compiler's errors, each once.
    pub codes: Vec<String>,
    /// Whether the restarted app took its snapshot back.
    pub restored: bool,
    /// The new executable's prewarm run, which the old app runs through.
    pub prewarm_us: Option<u64>,
    /// The phases of the restart, for a build the app restarted on: asking
    /// the old app for its snapshot until it arrived, from then until the
    /// new app launched (the old one closing), from its launch until it
    /// reported its mounts, and its restore; `None` for a phase that did not
    /// run.
    pub snapshot_us: Option<u64>,
    pub close_us: Option<u64>,
    pub launch_us: Option<u64>,
    pub restore_us: Option<u64>,
    /// From asking for the snapshot until the restart landed.
    pub restart_us: Option<u64>,
    /// How long Cargo took.
    pub build_us: u64,
}

/// Where the session's Rust edits are: none waiting, a build running or
/// due, or the app restarting on a new build.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RustState {
    Idle,
    Building,
    Restarting,
}

impl RustState {
    /// The `runtime` event spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            RustState::Idle => "idle",
            RustState::Building => "building",
            RustState::Restarting => "restarting",
        }
    }
}

/// How the latest patch a runtime answered went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatchStatus {
    pub candidate_revision: u64,
    /// `Applied`, `ScopedReset` or `Rejected`, over every file it carried.
    pub outcome: Outcome,
    /// Where it was refused; `runtime-commit` for one committed.
    pub stage: Stage,
    /// The states reset, and the focus, scroll offsets and handlers lost.
    pub scoped_resets: u32,
    /// The host's compile and plan of it for this runtime.
    pub compile_us: u64,
    /// The runtime's commit at its frame boundary; `None` when refused.
    pub commit_us: Option<u64>,
    /// From sending it until the answer arrived.
    pub round_trip_us: u64,
    /// What the session wrote to the runtime for it.
    pub bytes: u64,
}

/// One runtime of the session as Studio shows it: a `runtime` event, each
/// time something of it changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeStatus<'a> {
    /// `false` once its connection closed or the session let it go.
    pub connected: bool,
    /// The revision it runs, which is its last good.
    pub revision: u64,
    /// The revision of the patch sent to it and not yet answered.
    pub candidate_revision: Option<u64>,
    /// The files whose latest edit it was shown failing, which run their
    /// last good.
    pub failing: Vec<String>,
    pub rust: RustState,
    /// What its connection carried each way.
    pub bytes_sent: u64,
    pub bytes_received: u64,
    pub patch: Option<&'a PatchStatus>,
}

/// The stage a candidate rejected with `diagnostics` failed at: that of
/// its first error, by the language's code ranges.
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
            game: None,
        };
        assert_eq!(event.codes(), ["E1405", "E2103"]);
    }
}
