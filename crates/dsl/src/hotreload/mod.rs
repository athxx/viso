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

pub mod diff;
pub mod migrate;
pub mod plan;

pub use diff::{InsertedNode, KeptNode, RemovedNode, ReplacedNode, StructuralPatch, diff};
pub use migrate::{
    LiveAnchors, MigrationPlan, ScrollMigration, StateAction, StateMigration, migrate,
};
pub use plan::{CandidatePlan, plan};
