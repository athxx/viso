//! The hot reload transaction (architecture section 42; AGENTS 21.7).
//!
//! Hot reload in Viso is a transaction, not a rebuild. A new fragment source is
//! turned into a live UI change through an ordered pipeline whose stages before
//! the commit are all pure functions of the new source and the prior compiled
//! state:
//!
//! ```text
//! compile candidate  (plan)     — recompile + validate, pure
//!   → structural diff (diff)    — align old/new templates by NodeKey, pure
//!   → migration plan  (migrate) — match state/focus/scroll by identity, pure
//!   → atomic commit   (commit)  — the only stage that touches the live tree
//! ```
//!
//! Because every fallible stage runs before the commit and produces only plain
//! data, a failure short-circuits before anything mutates: the live tree is left
//! at its last-good state with no snapshot to restore (the keep-last-good
//! invariant — see ADR 0015). This mirrors the file-side validate-then-commit of
//! the migration reference while going further: a full atomic transaction with
//! explicit identity-keyed migration of state, focus, and scroll.

pub mod commit;
pub mod diff;
pub mod migrate;
pub mod plan;

pub use commit::{HotReloadReport, LiveRuntime, commit};
pub use diff::{InsertedNode, KeptNode, RemovedNode, ReplacedNode, StructuralPatch, diff};
pub use migrate::{
    LiveAnchors, MigrationPlan, ScrollMigration, StateAction, StateMigration, migrate,
};
pub use plan::{CandidatePlan, plan};

use crate::diag::Diagnostic;

/// The result of a successful hot reload transaction: what the commit did to the
/// live runtime, plus the compiled candidate that is now the last-good template.
///
/// The caller adopts `candidate` as the new baseline for the next reload — it holds
/// the template the live tree now matches and the reactive-source identities the
/// next diff and migration compare against.
#[derive(Debug, Clone)]
pub struct HotReload {
    /// What the commit migrated / reset / lost.
    pub report: HotReloadReport,
    /// The compiled candidate now live; adopt it as the next last-good.
    pub candidate: CandidatePlan,
}

/// Run one hot reload transaction: recompile `source`, diff it against the
/// last-good template, plan the state/focus/scroll migration, and atomically commit
/// it to the live runtime (architecture section 42; AGENTS 21.7).
///
/// The pipeline is `plan → diff → migrate → commit`, and every stage before the
/// commit is a pure function that mutates nothing. A compile or validation error in
/// `plan` short-circuits at the `?` with `Err(diagnostics)` **before** `commit` is
/// reached, so the live tree stays at its last-good state with no snapshot to
/// restore — the keep-last-good invariant (see the module docs and ADR 0015). On
/// success the live tree, bindings, state, focus, and scroll have been transitioned
/// to the candidate, and the returned [`HotReload`] carries both the report and the
/// candidate to adopt as the next baseline.
///
/// `last_good` is the template the live tree currently matches (its `tree` and
/// reactive-source `sources`); `anchors` are the live focus / scroll facts to
/// preserve where the structure allows; `rt` is the live runtime to commit into.
pub fn hot_reload(
    rt: &mut LiveRuntime<'_>,
    last_good: &CandidatePlan,
    source: &str,
    anchors: &LiveAnchors,
) -> Result<HotReload, Vec<Diagnostic>> {
    // Stage 1 — compile + validate the candidate. Any fatal diagnostic returns here,
    // before anything mutates.
    let candidate = plan(source)?;

    // Stages 2-3 — pure planning over the two templates and the live anchors.
    let patch = diff(&last_good.tree, &candidate.tree);
    let migration = migrate(&last_good.sources, &candidate.sources, &patch, anchors);

    // Stage 4 — the only mutating stage. Infallible by construction.
    let report = commit(rt, &candidate, &patch, &migration, anchors);

    Ok(HotReload { report, candidate })
}
