//! The hot reload transaction (architecture section 42; AGENTS 21.7;
//! `Viso_Hot_Reload.md` §9–§13).
//!
//! Hot reload in Viso is a transaction, not a rebuild. A new source is turned
//! into a live UI change through an ordered pipeline whose stages before the
//! commit are all pure functions of the new source and the prior compiled
//! state, and run on the host:
//!
//! ```text
//! compile candidate  (plan)     — recompile + validate, pure
//!   → structural diff (diff)    — align old/new templates per parent, pure
//!   → migration plan  (migrate) — match state by identity, node state by keep, pure
//!   → typed patch     (patch)   — the candidate's release form and the plan, by runtime names
//!   → atomic commit             — the runtime's, the only stage that touches the live tree
//! ```
//!
//! Because every fallible stage runs before the commit and produces only plain
//! data, a failure short-circuits before anything mutates: the live tree is left
//! at its last-good state with no snapshot to restore (the keep-last-good
//! invariant — see ADR 0015). The commit is the runtime's (`viso-view`'s dev
//! module): a running app applies the patch [`view_patch`] lowers, and an
//! in-process host commits the same package and plan.

pub mod compat;
pub mod diff;
pub mod game;
pub mod migrate;
pub mod patch;
pub mod plan;

pub use compat::{Conversion, IntType, Retyping, retype};
pub use diff::{InsertedNode, KeptNode, RemovedNode, ReplacedNode, StructuralPatch, diff};
pub use migrate::{
    MigrationPlan, NodeMigration, Retype, SlotMigration, StateAction, StateMigration, migrate,
};
pub use patch::{game_patch, reload_plan, view_patch};
pub use plan::{
    CandidatePlan, MigrateFn, plan, plan_mount_for, plan_view, plan_view_for, plan_view_parsed,
};
pub use viso_dev_protocol::wire::source_hash;

/// The layout of what a candidate plan carries to the runtime that commits it;
/// bumped whenever a plan of this compiler would not commit as an older one's
/// does.
const PLAN_FORMAT: u32 = 2;

/// The fingerprint of the schema a candidate is compiled against: this
/// compiler's version and plan layout, and the content of every standard native
/// library (their functions, types, traits and widgets). A `view!` expansion
/// embeds it in the mount record, so a dev session compiles patches only for a
/// runtime built against the schema it compiles with (`Viso_Hot_Reload.md`
/// §4.1). Cold: computed once per process.
pub fn schema_fingerprint() -> u128 {
    static FINGERPRINT: std::sync::OnceLock<u128> = std::sync::OnceLock::new();
    *FINGERPRINT.get_or_init(|| {
        let natives = crate::schema::Natives::standard();
        let libraries: Vec<String> = natives
            .libraries()
            .iter()
            .map(|library| format!("{library:?}"))
            .collect();
        let format = PLAN_FORMAT.to_le_bytes();
        let head = [env!("CARGO_PKG_VERSION").as_bytes(), &format[..]];
        crate::resolve::digest(
            head.into_iter()
                .chain(libraries.iter().map(String::as_bytes)),
        )
    })
}
