//! Structure hooks: the seam through which a mounted view re-shapes its own
//! subtree when the states its control-flow regions read change.
//!
//! A region (a conditional, a match, a keyed repetition) mounts, switches and
//! reorders retained nodes rather than rebuilding the tree. The UI runtime does
//! not know how a region decides what to mount — that is the view's compiled
//! behavior — so a view registers one hook with the union of the state cells
//! its regions read, and the frame's flush runs it once per frame in which one
//! of them changed. A hook edits the tree in place: it allocates, moves and
//! frees nodes, and binds and prunes their state edges.
//!
//! Hooks run on the cold structural path only: a frame whose changed set does
//! not meet a hook's dependencies costs one sorted-slice probe per changed cell,
//! and a tree without regions registers none.

use crate::binding::BindingTable;
use crate::component::NodeStore;
use crate::reactive::EffectStore;
use crate::state::{StateId, StateStore};

/// The stores a structure hook edits.
pub struct StructureCx<'a> {
    /// The retained tree.
    pub store: &'a mut NodeStore,
    /// The state cells, read by the regions' selectors and by new bindings.
    pub states: &'a mut StateStore,
    /// The state edges of the nodes a hook mounts and frees.
    pub bindings: &'a mut BindingTable,
    /// The effects a freed node cancels.
    pub effects: &'a mut EffectStore,
}

/// A structure hook's body, called with the frame's changed cells.
type HookBody = Box<dyn FnMut(&mut StructureCx<'_>, &[StateId])>;

/// A registered hook: the cells it reads, ascending, and its body.
pub struct StructureHook {
    deps: Box<[StateId]>,
    run: HookBody,
}

impl NodeStore {
    /// Registers `run` to re-shape the tree whenever a cell of `deps` changes.
    /// The hook lives until the store is [`clear`](Self::clear)ed.
    pub fn add_structure_hook(
        &mut self,
        deps: impl IntoIterator<Item = StateId>,
        run: impl FnMut(&mut StructureCx<'_>, &[StateId]) + 'static,
    ) {
        let mut deps: Vec<StateId> = deps.into_iter().collect();
        deps.sort_unstable_by_key(|id| (id.index(), id.generation()));
        deps.dedup();
        self.structure_hooks_mut().push(StructureHook {
            deps: deps.into(),
            run: Box::new(run),
        });
    }

    /// The number of registered structure hooks.
    pub fn structure_hook_count(&self) -> usize {
        self.structure_hooks().len()
    }
}

/// Runs every structure hook whose dependencies meet `changed`, the frame's
/// drained write set, and returns how many ran. Call it after
/// [`NodeStore::flush_state_transactions`], so a node a hook frees has already
/// taken its dirty marks and a node it mounts starts clean of stale ones.
///
/// A hook registered while the hooks run (a region mounting a nested view)
/// joins the registry after them and first runs on a later frame.
pub fn run_structure_hooks(
    store: &mut NodeStore,
    states: &mut StateStore,
    bindings: &mut BindingTable,
    effects: &mut EffectStore,
    changed: &[StateId],
) -> u32 {
    if changed.is_empty() || store.structure_hooks().is_empty() {
        return 0;
    }
    let mut hooks = std::mem::take(store.structure_hooks_mut());
    let mut ran = 0;
    for hook in &mut hooks {
        let meets = changed.iter().any(|id| {
            hook.deps
                .binary_search_by_key(&(id.index(), id.generation()), |d| {
                    (d.index(), d.generation())
                })
                .is_ok()
        });
        if !meets {
            continue;
        }
        let mut cx = StructureCx {
            store: &mut *store,
            states: &mut *states,
            bindings: &mut *bindings,
            effects: &mut *effects,
        };
        (hook.run)(&mut cx, changed);
        ran += 1;
    }
    let added = std::mem::replace(store.structure_hooks_mut(), hooks);
    store.structure_hooks_mut().extend(added);
    ran
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use super::*;
    use crate::state::StateValue;

    #[test]
    fn a_hook_runs_only_when_a_dependency_changed() {
        let mut store = NodeStore::new();
        let mut states = StateStore::new();
        let mut bindings = BindingTable::new();
        let mut effects = EffectStore::default();
        let read = states.alloc(StateValue::Int(0));
        let other = states.alloc(StateValue::Int(0));
        let runs = Rc::new(Cell::new(0));
        let seen = Rc::clone(&runs);
        store.add_structure_hook([read], move |_, _| seen.set(seen.get() + 1));
        let mut run = |changed: &[StateId]| {
            run_structure_hooks(
                &mut store,
                &mut states,
                &mut bindings,
                &mut effects,
                changed,
            )
        };
        assert_eq!(run(&[other]), 0);
        assert_eq!(run(&[other, read]), 1);
        assert_eq!(runs.get(), 1);
    }
}
