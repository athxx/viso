//! Lowering a planned reload into the typed UI patch a running app commits
//! with no compiler present (`Viso_Hot_Reload.md` §9–§13).
//!
//! The diff and the migration plan align the last-good and candidate
//! templates by [`NodeKey`] and their states by [`SymbolId`]; the patch names
//! a static node by its static index and a node a region mounts by the
//! region, arm and arm item of its template, and a state by its durable
//! `StateKey`, the runtime twin of the identity the plan matched it by. The
//! candidate travels in its release form.

use viso_ui::state::StateKey;
use viso_view::dev::wire::{
    FileId, NodeCarry, NodeRef, ReloadPlan, RetypePlan, StateAction as PatchAction, StatePlan,
    ViewPatch,
};

use crate::aot::emit_view_package;
use crate::hotreload::compat::retype;
use crate::hotreload::diff::diff;
use crate::hotreload::migrate::{MigrationPlan, StateAction, migrate};
use crate::hotreload::plan::CandidatePlan;
use crate::ir::binding_ir::NodeKey;
use crate::resolve::SymbolId;
use crate::view_regions::{RegionKeys, StaticNodes};

/// The patch that moves the mounts of `file` from `last_good` to
/// `candidate`.
pub fn view_patch(file: FileId, last_good: &CandidatePlan, candidate: &CandidatePlan) -> ViewPatch {
    ViewPatch {
        file,
        package: emit_view_package(candidate),
        plan: reload_plan(last_good, candidate),
    }
}

/// How a mount moves from `last_good` to `candidate`: the structural diff,
/// the state and node migration, and each kept state's retyping, planned by
/// identity and lowered to the runtime's names.
pub fn reload_plan(last_good: &CandidatePlan, candidate: &CandidatePlan) -> ReloadPlan {
    let patch = diff(&last_good.tree, &candidate.tree);
    let mut migration = migrate(
        &last_good.sources,
        &candidate.sources,
        slots(last_good),
        slots(candidate),
        &patch,
    );
    retype(last_good, candidate, &mut migration);
    lower(
        last_good,
        candidate,
        patch.is_structure_preserving(),
        &migration,
    )
}

fn lower(
    last_good: &CandidatePlan,
    candidate: &CandidatePlan,
    preserving: bool,
    migration: &MigrationPlan,
) -> ReloadPlan {
    let names = |plan: &CandidatePlan| (StaticNodes::of(&plan.tree), RegionKeys::of(&plan.tree));
    let (old, new) = (names(last_good), names(candidate));
    let nodes = migration
        .nodes
        .iter()
        .filter_map(|carry| {
            Some(NodeCarry {
                from: node_ref(&old, carry.from)?,
                to: node_ref(&new, carry.to)?,
                carries: carry.carries,
            })
        })
        .collect();
    let states = migration
        .states
        .iter()
        .filter_map(|state| {
            let symbol = state.symbol;
            let action = match state.action {
                StateAction::Keep => PatchAction::Keep,
                StateAction::Convert => PatchAction::Convert,
                StateAction::Reset => PatchAction::Reset,
                StateAction::New => PatchAction::New,
                // A dropped state's cell is left inert: nothing binds it after
                // the rebind.
                StateAction::Dropped => return None,
            };
            let retype = migration.retype(symbol).map(|retype| {
                Box::new(RetypePlan {
                    conversion: retype.conversion.clone(),
                    migrator: retype.migrator.clone(),
                    held: retype.held,
                    name: retype.name.clone(),
                    from: retype.from.clone(),
                    to: retype.to.clone(),
                    at: (retype.at.start().to_u32(), retype.at.end().to_u32()),
                })
            });
            Some(StatePlan {
                key: StateKey::from_parts(symbol.hi, symbol.lo),
                action,
                initial: candidate.initial(symbol),
                from_slot: (action != PatchAction::New)
                    .then(|| slot(last_good, symbol))
                    .flatten(),
                slot: slot(candidate, symbol),
                retype,
            })
        })
        .collect();
    ReloadPlan {
        preserving,
        nodes,
        states,
    }
}

/// The runtime's name for the node `key` of a template, `None` for one no
/// target authors (a node under a leaf).
fn node_ref((statics, regions): &(StaticNodes, RegionKeys), key: NodeKey) -> Option<NodeRef> {
    if let Some(index) = statics.ordinal(key) {
        return Some(NodeRef::Static(index));
    }
    let (region, arm, item) = regions.item(key)?;
    Some(NodeRef::Region { region, arm, item })
}

/// The behavior state slots of `plan`, by identity; empty without a behavior.
fn slots(plan: &CandidatePlan) -> &[(SymbolId, u32)] {
    plan.view.as_ref().map_or(&[], |view| &view.slots)
}

/// The behavior slot of state `symbol` in `plan`.
fn slot(plan: &CandidatePlan, symbol: SymbolId) -> Option<u32> {
    slots(plan)
        .iter()
        .find(|&&(s, _)| s == symbol)
        .map(|&(_, slot)| slot)
}
