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
//! invariant — see ADR 0015). The commit is `viso_view::dev::commit`, which a
//! running app applies a patch with and an in-process host such as a test
//! drives through [`transact`].

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
pub use patch::{reload_plan, view_patch};
pub use plan::{CandidatePlan, MigrateFn, plan, plan_view, plan_view_for, plan_view_parsed};
pub use viso_view::dev::commit::{Candidate, LiveRuntime, static_nodes};
pub use viso_view::dev::wire::source_hash;

use std::rc::Rc;

use crate::aot::{emit_view_package, static_shape};
use crate::diag::Diagnostic;
use crate::frontend::Origin;
use crate::syntax::{TextRange, TextSize};
use crate::view_regions::has_regions;
use viso_view::dev::commit::{CommitReport, commit};
use viso_view::dev::wire::RESET_NOTICE;

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

/// What a commit did to a mount: the runtime's [`CommitReport`] with each
/// reset as the `E5101` warning the compiler reports it as.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HotReloadReport {
    /// State cells that kept their live value, verbatim or converted.
    pub migrated: u32,
    /// State cells that started from their initializer.
    pub reset: u32,
    /// An `E5101` warning for each state whose live value was reset because
    /// it does not convert into the state's new type.
    pub notices: Vec<Diagnostic>,
    /// Whether a focused node lost focus because it did not survive.
    pub focus_lost: bool,
    /// Scroll offsets whose node did not survive.
    pub scroll_lost: u32,
    /// Whether the view's handlers were dropped because its recompiled
    /// behavior did not mount.
    pub handlers_lost: bool,
}

impl From<CommitReport> for HotReloadReport {
    fn from(report: CommitReport) -> Self {
        HotReloadReport {
            migrated: report.migrated,
            reset: report.reset,
            notices: report
                .notices
                .into_iter()
                .map(|notice| {
                    let at =
                        TextRange::new(TextSize::from(notice.start), TextSize::from(notice.end));
                    Diagnostic::warning(RESET_NOTICE, at, notice.message)
                })
                .collect(),
            focus_lost: report.focus_lost,
            scroll_lost: report.scroll_lost,
            handlers_lost: report.handlers_lost,
        }
    }
}

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
/// reactive-source `sources`); `rt` is the live runtime to commit into.
pub fn hot_reload(
    rt: &mut LiveRuntime<'_>,
    last_good: &CandidatePlan,
    source: &str,
) -> Result<HotReload, Vec<Diagnostic>> {
    // Stage 1 — compile + validate the candidate. Any fatal diagnostic returns here,
    // before anything mutates.
    let candidate = plan(source)?;
    Ok(transact(rt, last_good, candidate))
}

/// [`hot_reload`] for the component of a `.vs` file: the same transaction, over a
/// candidate compiled by the file frontend, so the view's handlers, state
/// initializers and behavior reload with its tree.
pub fn hot_reload_view(
    rt: &mut LiveRuntime<'_>,
    last_good: &CandidatePlan,
    source: &str,
    origin: &Origin,
) -> Result<HotReload, Vec<Diagnostic>> {
    let candidate = plan_view(source, origin)?;
    Ok(transact(rt, last_good, candidate))
}

/// The stages after compile over a candidate already compiled by [`plan`] or
/// [`plan_view`], committed in process: the plan lowered to the typed patch a
/// running app receives, then the runtime's commit. A session that mounts one
/// file several times compiles the edit once and commits it to each mount in
/// turn, threading the returned candidate through.
pub fn transact(
    rt: &mut LiveRuntime<'_>,
    last_good: &CandidatePlan,
    candidate: CandidatePlan,
) -> HotReload {
    let plan = reload_plan(last_good, &candidate);
    // A view without regions mounted outside the commit names its static
    // nodes by walking the last-good shape over the live tree.
    if rt.nodes.is_empty()
        && let Some(root) = rt.root
        && !has_regions(&last_good.tree)
    {
        *rt.nodes = static_nodes(rt.store, root, &static_shape(&last_good.tree));
    }
    let module = candidate.view.as_ref().map(|view| Rc::clone(&view.module));
    let loaded = Candidate::verified(emit_view_package(&candidate), module);
    let report = commit(rt, &loaded, &plan).into();
    HotReload { report, candidate }
}
