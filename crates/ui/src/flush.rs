//! The frame's state flush: the pending writes fanned through every reactor
//! downstream of state, repeated until no reactor writes more.
//!
//! One round drains the pending set once and runs, in order, the memo-gated
//! derivations, the direct bindings, the structure hooks, the semantic-state
//! projections and the effects. A structure hook may write a cell while it
//! mounts (a region's instance state), so the flush runs another round over
//! those writes in the same frame; the frame settles when a round leaves
//! nothing pending. A round whose writes keep causing writes is a reactive
//! cycle: past [`SETTLE_ROUNDS`] the flush stops and drops what is still
//! pending, so a cycle costs a bounded amount of one frame rather than every
//! frame after it.

use crate::binding::BindingTable;
use crate::component::NodeStore;
use crate::reactive::{ComputedStore, EffectStore, SemanticProjector};
use crate::state::{StateId, StateStore};
use crate::structure::run_structure_hooks;

/// How many rounds one frame's flush runs before it stops as a reactive cycle.
pub const SETTLE_ROUNDS: u32 = 16;

/// A frame whose flush did not settle within [`SETTLE_ROUNDS`] rounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReactiveCycle {
    /// How many written cells were still pending, and were dropped, when the
    /// flush stopped. Their values stay written; only the reactions to them
    /// do not run.
    pub dropped: usize,
}

impl ReactiveCycle {
    /// The diagnostic code of a reactive cycle.
    pub const CODE: &'static str = "E4202";
}

impl std::fmt::Display for ReactiveCycle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}: state writes did not settle within {SETTLE_ROUNDS} rounds; \
             dropped the changes of {} cell(s)",
            Self::CODE,
            self.dropped
        )
    }
}

/// Flushes the pending state writes until they settle and returns how many
/// rounds ran: `0` for a frame with nothing pending, which touches nothing.
/// `changed` is reused scratch for each round's drained set, left empty.
///
/// Each round runs, over the cells written since the last:
///
/// 1. the derivations, first, so a memo-gated re-evaluation dirties its node
///    only when its value changed, before Measure/Layout;
/// 2. the direct bindings, turning each changed cell into targeted node
///    dirtying through the compiled static and dynamic-script edges;
/// 3. the structure hooks, after the bindings so a node a hook frees has taken
///    its marks;
/// 4. the semantic-state projections, writing each node's `semantic_state`;
/// 5. the effects, re-running those whose dependencies changed.
///
/// # Errors
///
/// A [`ReactiveCycle`] when round [`SETTLE_ROUNDS`] still leaves writes
/// pending; they are dropped.
pub fn settle_states(
    store: &mut NodeStore,
    states: &mut StateStore,
    bindings: &mut BindingTable,
    computeds: &mut ComputedStore,
    projectors: &mut SemanticProjector,
    effects: &mut EffectStore,
    changed: &mut Vec<StateId>,
) -> Result<u32, ReactiveCycle> {
    let mut rounds = 0;
    while states.has_pending() {
        changed.clear();
        states.take_pending(changed);
        if rounds == SETTLE_ROUNDS {
            let dropped = changed.len();
            changed.clear();
            return Err(ReactiveCycle { dropped });
        }
        rounds += 1;
        computeds.wake_computed(changed, states, store);
        store.flush_state_transactions(changed, bindings);
        run_structure_hooks(store, states, bindings, effects, changed);
        projectors.wake(changed, states, store);
        effects.wake(changed, states);
    }
    changed.clear();
    Ok(rounds)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::StateValue;

    #[derive(Default)]
    struct Stores {
        store: NodeStore,
        states: StateStore,
        bindings: BindingTable,
        computeds: ComputedStore,
        projectors: SemanticProjector,
        effects: EffectStore,
        changed: Vec<StateId>,
    }

    impl Stores {
        fn settle(&mut self) -> Result<u32, ReactiveCycle> {
            settle_states(
                &mut self.store,
                &mut self.states,
                &mut self.bindings,
                &mut self.computeds,
                &mut self.projectors,
                &mut self.effects,
                &mut self.changed,
            )
        }
    }

    fn int(states: &StateStore, id: StateId) -> i32 {
        match states.get(id) {
            Some(StateValue::Int(n)) => n,
            other => panic!("an int cell, not {other:?}"),
        }
    }

    #[test]
    fn nothing_pending_runs_no_round() {
        let mut stores = Stores::default();
        assert_eq!(stores.settle(), Ok(0));
    }

    #[test]
    fn a_write_made_while_settling_flushes_in_the_same_frame() {
        let mut stores = Stores::default();
        let source = stores.states.alloc(StateValue::Int(0));
        let derived = stores.states.alloc(StateValue::Int(0));
        let seen = stores.states.alloc(StateValue::Int(0));
        stores.store.add_structure_hook([source], move |cx, _| {
            let next = int(cx.states, source) * 10;
            cx.states.set(derived, StateValue::Int(next));
        });
        stores.store.add_structure_hook([derived], move |cx, _| {
            let next = int(cx.states, derived) + 1;
            cx.states.set(seen, StateValue::Int(next));
        });
        stores.states.set(source, StateValue::Int(4));
        assert_eq!(stores.settle(), Ok(3), "source, derived, then seen");
        assert_eq!(int(&stores.states, seen), 41);
        assert!(!stores.states.has_pending());
        assert!(stores.changed.is_empty());
    }

    #[test]
    fn a_hook_feeding_itself_stops_as_a_reactive_cycle() {
        let mut stores = Stores::default();
        let cell = stores.states.alloc(StateValue::Int(0));
        stores.store.add_structure_hook([cell], move |cx, _| {
            let next = int(cx.states, cell) + 1;
            cx.states.set(cell, StateValue::Int(next));
        });
        stores.states.set(cell, StateValue::Int(1));
        let cycle = stores.settle().expect_err("the hook never settles");
        assert_eq!(cycle, ReactiveCycle { dropped: 1 });
        assert_eq!(ReactiveCycle::CODE, "E4202");
        assert_eq!(int(&stores.states, cell), 1 + SETTLE_ROUNDS as i32);
        assert!(
            !stores.states.has_pending(),
            "the cycle's writes are dropped"
        );
        assert_eq!(stores.settle(), Ok(0), "the next frame is quiet");
    }
}
