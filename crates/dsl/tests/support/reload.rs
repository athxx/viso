//! The in-process hot reload a test drives: the compiler's plan, diff and
//! migration lowered to the patch a running app receives, and the runtime's
//! commit of it into the live tree the test holds.

#![allow(dead_code)]

use std::rc::Rc;

use viso_dev_protocol::wire::RESET_NOTICE;
use viso_dsl::aot::{ShaderLang, emit_view_package, package_shaders, static_shape};
use viso_dsl::diag::Diagnostic;
use viso_dsl::frontend::Origin;
use viso_dsl::hotreload::{CandidatePlan, plan, plan_view, reload_plan};
use viso_dsl::resolve::SymbolId;
use viso_dsl::syntax::{TextRange, TextSize};
use viso_dsl::view_regions::has_regions;
pub use viso_view::dev::commit::LiveRuntime;
use viso_view::dev::commit::{Candidate, CommitReport, commit, static_nodes};

/// What a commit did to a mount: the runtime's [`CommitReport`] with each
/// reset as the `E5101` warning the compiler reports it as.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HotReloadReport {
    /// State cells that kept their live value, verbatim or converted.
    pub migrated: u32,
    /// State cells that started from their initializer.
    pub reset: u32,
    /// Every [`reset`](Self::reset) state's own identity — the narrowest
    /// scope `PATCH_WITH_SCOPED_RESET` names, the
    /// compiler's bridge of the runtime `StateKey` back to the `SymbolId` it
    /// already reasons in.
    pub state_resets: Vec<SymbolId>,
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
            state_resets: report
                .state_resets
                .iter()
                .map(|reset| SymbolId::from_parts(reset.owner.hi, reset.owner.lo))
                .collect(),
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
/// it to the live runtime.
///
/// The pipeline is `plan → diff → migrate → commit`, and every stage before the
/// commit is a pure function that mutates nothing. A compile or validation error in
/// `plan` short-circuits at the `?` with `Err(diagnostics)` **before** `commit` is
/// reached, so the live tree stays at its last-good state with no snapshot to
/// restore — the keep-last-good invariant (see the module docs). On
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
/// turn, threading the returned candidate through. The shaders the view paints
/// with are packed for the reference interpreter, which a headless runtime
/// draws them with.
///
/// # Panics
///
/// If a shader that checked as a brush does not build for it.
pub fn transact(
    rt: &mut LiveRuntime<'_>,
    last_good: &CandidatePlan,
    candidate: CandidatePlan,
) -> HotReload {
    let mut package = emit_view_package(&candidate);
    package.shaders = package_shaders(&candidate, &[ShaderLang::None])
        .unwrap_or_else(|error| panic!("a checked brush does not build: {error}"));
    let plan = reload_plan(last_good, &candidate, &package);
    // A view without regions mounted outside the commit names its static
    // nodes by walking the last-good shape over the live tree.
    if rt.nodes.is_empty()
        && let Some(root) = rt.root
        && !has_regions(&last_good.tree)
    {
        *rt.nodes = static_nodes(rt.store, root, &static_shape(&last_good.tree));
    }
    let module = candidate.view.as_ref().map(|view| Rc::clone(&view.module));
    let loaded = Candidate::verified(package, module).expect("a packed brush loads");
    let report = commit(rt, &loaded, &plan).into();
    HotReload { report, candidate }
}
