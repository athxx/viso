//! Lowering a planned reload into the typed UI patch a running app commits
//! with no compiler present (`Viso_Hot_Reload.md` §9–§13).
//!
//! The diff and the migration plan align the last-good and candidate
//! templates by [`NodeKey`] and their states by [`SymbolId`]; the patch names
//! a static node by its static index and a node a region mounts by the
//! region, arm and arm item of its template, and a state by its durable
//! `StateKey`, the runtime twin of the identity the plan matched it by. The
//! candidate travels in its release form.

use viso_ui::aot::AotNode;
use viso_ui::state::StateKey;
use viso_view::ViewPackage;
use viso_view::dev::wire::{
    FileId, NodeCarry, NodeRef, ReloadPlan, RetypePlan, StateAction as PatchAction, StatePlan,
    StructuralOp, ViewPatch,
};

use crate::aot::emit_view_package;
use crate::hotreload::compat::retype;
use crate::hotreload::diff::{StructuralPatch, diff};
use crate::hotreload::migrate::{MigrationPlan, StateAction, migrate};
use crate::hotreload::plan::CandidatePlan;
use crate::ir::binding_ir::NodeKey;
use crate::resolve::SymbolId;
use crate::view_regions::{RegionKeys, StaticNodes, has_regions};

/// The patch that moves the mounts of `file` from `last_good` to
/// `candidate`.
pub fn view_patch(file: FileId, last_good: &CandidatePlan, candidate: &CandidatePlan) -> ViewPatch {
    let package = emit_view_package(candidate);
    let plan = reload_plan(last_good, candidate, &package);
    ViewPatch {
        file,
        package,
        plan,
    }
}

/// How a mount moves from `last_good` to `candidate`: the structural diff,
/// the state and node migration, and each kept state's retyping, planned by
/// identity and lowered to the runtime's names. `package` is the candidate's
/// already-emitted release form — its node table is where a structural op
/// slices the subtree it builds, so the caller computes it once and shares
/// it between the plan and the patch it sends alongside it.
pub fn reload_plan(
    last_good: &CandidatePlan,
    candidate: &CandidatePlan,
    package: &ViewPackage,
) -> ReloadPlan {
    let patch = diff(&last_good.tree, &candidate.tree);
    let mut migration = migrate(
        &last_good.sources,
        &candidate.sources,
        slots(last_good),
        slots(candidate),
        &patch,
    );
    retype(last_good, candidate, &mut migration);
    lower(last_good, candidate, package, &patch, &migration)
}

fn lower(
    last_good: &CandidatePlan,
    candidate: &CandidatePlan,
    package: &ViewPackage,
    patch: &StructuralPatch,
    migration: &MigrationPlan,
) -> ReloadPlan {
    let names = |plan: &CandidatePlan| (StaticNodes::of(&plan.tree), RegionKeys::of(&plan.tree));
    let (old, new) = (names(last_good), names(candidate));
    let structural = structural_ops(last_good, candidate, package, &old, patch).unwrap_or_default();
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
        preserving: patch.is_structure_preserving(),
        structural,
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

/// The structural ops that move a region-free mount from `last_good` to
/// `candidate`: `patch`'s insert/remove/replace entries, addressed in the
/// last-good tree and each insert/replace's fresh subtree sliced straight
/// from `package`'s pre-order node table — `commit.rs::apply_structural`
/// then runs them against the live tree instead of freeing and rebuilding
/// its whole root, so a node no op names keeps its live identity.
///
/// `None` falls back to the whole-root rebuild `apply_structural` already
/// has: when either template has a region (a structural op's `before`/
/// `start` addressing is only proven correct across static nodes) or any
/// entry's address does not resolve to one — the all-or-nothing guard a
/// malformed addressing needs, so a partially-applied op batch never runs.
fn structural_ops(
    last_good: &CandidatePlan,
    candidate: &CandidatePlan,
    package: &ViewPackage,
    old: &(StaticNodes, RegionKeys),
    patch: &StructuralPatch,
) -> Option<Vec<StructuralOp>> {
    if has_regions(&last_good.tree) || has_regions(&candidate.tree) {
        return Some(Vec::new());
    }
    let new_statics = StaticNodes::of(&candidate.tree);
    let nodes = &package.ui.nodes;
    // An insert's `parent`/`before` are the candidate keys of kept nodes
    // (diff.rs only ever anchors an insert to one) — their last-good key is
    // the one the structural op addresses, by the pairing `patch.keep`
    // already proved.
    let old_of = |new_key: NodeKey| patch.keep.iter().find(|k| k.new == new_key).map(|k| k.old);
    let old_ref = |key: NodeKey| node_ref(old, key);
    let subtree = |key: NodeKey| -> Option<(u32, Vec<AotNode>)> {
        let start = new_statics.ordinal(key)?;
        let len = subtree_len(nodes, start as usize);
        Some((start, nodes[start as usize..start as usize + len].to_vec()))
    };

    let mut ops = Vec::with_capacity(patch.remove.len() + patch.replace.len() + patch.insert.len());
    for removed in &patch.remove {
        ops.push(StructuralOp::Remove {
            node: old_ref(removed.key)?,
        });
    }
    for replaced in &patch.replace {
        let node = old_ref(replaced.old)?;
        let (start, subtree) = subtree(replaced.new)?;
        ops.push(StructuralOp::Replace {
            node,
            start,
            subtree,
        });
    }
    for inserted in &patch.insert {
        let parent = old_ref(old_of(inserted.parent)?)?;
        let before = match inserted.before {
            Some(before) => Some(old_ref(old_of(before)?)?),
            None => None,
        };
        let (start, subtree) = subtree(inserted.key)?;
        ops.push(StructuralOp::Insert {
            parent,
            before,
            start,
            subtree,
        });
    }
    Some(ops)
}

/// The number of nodes in `nodes`' pre-order table that make up the subtree
/// rooted at `start`: `start` itself plus every node its `child_count` chain
/// transitively owns — the same forward-pass accounting `build_nodes` uses to
/// reconstruct a tree from the flat table, run just far enough to find where
/// this one subtree ends.
fn subtree_len(nodes: &[AotNode], start: usize) -> usize {
    let mut remaining = 1usize;
    let mut i = start;
    while remaining > 0 {
        remaining -= 1;
        remaining += nodes[i].child_count as usize;
        i += 1;
    }
    i - start
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
