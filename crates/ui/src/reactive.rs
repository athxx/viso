//! Derived values and side effects: the read/derive end of the reactive link.
//!
//! [`StateStore`] is the write end — a write records a pending change that the
//! frame's flush turns into targeted node dirtying. This module adds the two
//! constructs that sit *downstream* of state:
//!
//! - [`Computed`] — a pure cached derivation. Its body reads state through a
//!   read-only [`ComputeCx`] (no `set`, so purity is a type-level guarantee),
//!   and every read is recorded against a [`DepCursor`] passed into the eval —
//!   an explicit cursor, not a thread-local, so dependency tracking stays
//!   local, testable, and free of hidden global state. Each eval refreshes the
//!   recorded dependency set into a reverse index so a later write to any
//!   dependency schedules a re-eval. On re-eval the new result is compared to
//!   the cached one and the downstream node is marked dirty *only if the value
//!   actually changed* — the memo boundary that keeps an unchanged derivation
//!   from rippling work outward.
//!
//! - [`Effect`] — a side effect with a lifecycle. Its body runs for effect (not
//!   value) and may return a cleanup closure. When a dependency changes the
//!   prior cleanup runs first and then the body re-runs (dependency restart);
//!   when the owning node is freed or unmounted the cleanup runs and the effect
//!   is cancelled. This slice runs effects synchronously — no timers or async
//!   yet; those layer on later without changing this contract.
//!
//! Both constructs wake off their own compact reverse index from [`StateId`] to
//! the ids that read it — not the dirty bitset — but their wakes differ in kind
//! because their outputs do. A [`Computed`]'s output *is* a node's dirty class,
//! so [`ComputedStore::wake_computed`] re-evaluates each affected derivation and
//! marks its downstream node dirty *only when the value actually changed* — the
//! memo boundary in the wake itself. An [`Effect`] has no dirty class — its
//! output is the side effect — so routing it through the dirty bitset would
//! overload a bit that means "recompute layout/paint for this node"; instead
//! [`EffectStore::wake`] re-runs each affected effect body. Keeping both off the
//! bitset leaves the dirty classes a clean eight, one meaning each; the compiled
//! [`BindingTable`](crate::binding::BindingTable) stays reserved for direct
//! state→node value bindings and the dynamic-script fallback.
//!
//! Both stores are keyed by compact generational ids so a stale handle is
//! detectable, matching [`crate::node::NodeId`] and [`StateId`].

use crate::component::NodeStore;
use crate::dirty::DirtyClass;
use crate::node::NodeId;
use crate::state::{StateId, StateStore, StateValue};

/// A compact generational handle to a stored [`Computed`] cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ComputedId {
    index: u32,
    generation: u32,
}

impl ComputedId {
    /// The dense slot index.
    #[inline]
    pub fn index(self) -> u32 {
        self.index
    }
    /// The generation this handle was minted at.
    #[inline]
    pub fn generation(self) -> u32 {
        self.generation
    }
}

/// A compact generational handle to a stored [`Effect`] cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EffectId {
    index: u32,
    generation: u32,
}

impl EffectId {
    /// The dense slot index.
    #[inline]
    pub fn index(self) -> u32 {
        self.index
    }
    /// The generation this handle was minted at.
    #[inline]
    pub fn generation(self) -> u32 {
        self.generation
    }
}

/// Records the state reads a [`Computed`] or [`Effect`] body performs during one
/// evaluation, so the caller can register (or refresh) the dynamic bindings that
/// wake it when a dependency changes.
///
/// This is the *explicit* alternative to a thread-local tracking stack: the body
/// reads state through a context that holds a `&mut DepCursor`, so every read
/// lands here with no hidden global state — the cursor is owned by the eval and
/// dropped when it ends. Dependencies are deduplicated so reading the same state
/// twice in one body records it once.
#[derive(Debug, Default)]
pub struct DepCursor {
    deps: Vec<StateId>,
}

impl DepCursor {
    /// A fresh, empty cursor.
    #[inline]
    fn new() -> Self {
        Self { deps: Vec::new() }
    }

    /// Record a dependency, deduplicating.
    #[inline]
    fn record(&mut self, id: StateId) {
        if !self.deps.contains(&id) {
            self.deps.push(id);
        }
    }

    /// The dependencies recorded this evaluation.
    #[inline]
    pub fn deps(&self) -> &[StateId] {
        &self.deps
    }
}

/// The read-only context a [`Computed`] or [`Effect`] body evaluates against.
///
/// It exposes `get` and nothing else: a body can read state but cannot write it,
/// so a derivation is pure by construction (there is no `set` to call). Each read
/// is recorded against the [`DepCursor`], building the dependency set that the
/// caller turns into dynamic bindings.
pub struct ComputeCx<'a> {
    states: &'a StateStore,
    cursor: &'a mut DepCursor,
}

impl<'a> ComputeCx<'a> {
    #[inline]
    fn new(states: &'a StateStore, cursor: &'a mut DepCursor) -> Self {
        Self { states, cursor }
    }

    /// Read a state value, recording it as a dependency of the current
    /// evaluation. `None` for a stale handle (still recorded, so the derivation
    /// re-runs if that slot is later reused and written).
    #[inline]
    pub fn get(&mut self, id: StateId) -> Option<StateValue> {
        self.cursor.record(id);
        self.states.get(id)
    }
}

/// A cleanup closure returned by an effect body, run before the next restart or
/// on cancellation. Boxed because each effect carries its own.
pub type Cleanup = Box<dyn FnOnce()>;

/// The boxed body of a [`Computed`]: reads state through the read-only context
/// and returns the derived value. `FnMut` so a body may own mutable scratch,
/// but its reads are pure with respect to the store.
type EvalFn = Box<dyn FnMut(&mut ComputeCx<'_>) -> StateValue>;

/// The boxed body of an [`Effect`]: runs for effect and returns an optional
/// [`Cleanup`] the next restart (or cancellation) runs first.
type EffectFn = Box<dyn FnMut(&mut ComputeCx<'_>) -> Option<Cleanup>>;

/// A pure cached derivation over state.
///
/// The `eval` closure reads state through the [`ComputeCx`] and returns the
/// derived value. It must be pure — no I/O, timers, or native side effects — a
/// contract the read-only context makes hard to break. The store caches the last
/// result and the recorded dependency set.
struct ComputedSlot {
    /// The cached last result. `None` before the first evaluation.
    value: Option<StateValue>,
    /// The node whose dirty classes this derivation drives when it changes.
    node: NodeId,
    /// The dirty classes to mark on `node` when the value changes.
    class: DirtyClass,
    /// The dependency set recorded at the last evaluation.
    deps: Vec<StateId>,
    /// The pure derivation body.
    eval: EvalFn,
    generation: u32,
    occupied: bool,
}

/// A side effect with cleanup and dependency-restart lifecycle.
struct EffectSlot {
    /// The node the effect is scoped to; freeing it cancels the effect.
    node: NodeId,
    /// The dependency set recorded at the last run.
    deps: Vec<StateId>,
    /// The effect body. Runs for effect and returns an optional cleanup that the
    /// next restart (or the cancellation) runs first.
    body: EffectFn,
    /// The cleanup returned by the most recent run, if any.
    cleanup: Option<Cleanup>,
    generation: u32,
    occupied: bool,
}

/// The store of pure cached derivations, keyed by generational [`ComputedId`].
///
/// Sits beside the [`StateStore`] in the app driver. Values are evaluated lazily
/// on first read and re-evaluated only when a recorded dependency changes; an
/// unchanged re-evaluation marks nothing dirty.
///
/// Waking is driven by a reverse index rather than the dirty bitset, mirroring
/// [`EffectStore`]: [`wake_computed`] looks up the derivations that read a
/// changed [`StateId`] and re-evaluates each, marking its downstream node dirty
/// only when the value changed. The index is `dep_index[state.index()] -> the
/// derivations depending on it`, refreshed for a derivation's dependencies on
/// every [`eval`] so a dependency it stops reading stops waking it.
///
/// [`wake_computed`]: ComputedStore::wake_computed
/// [`eval`]: ComputedStore::eval
#[derive(Default)]
pub struct ComputedStore {
    slots: Vec<ComputedSlot>,
    free: Vec<u32>,
    /// Reverse dependency index, aligned to [`StateStore`] dense indices:
    /// `dep_index[i]` holds every derivation that read the state at dense index
    /// `i` at its last eval.
    dep_index: Vec<Vec<ComputedId>>,
    /// Reused buffer of derivations to re-evaluate this wake, so a wake allocates
    /// nothing on the steady path.
    wake_scratch: Vec<ComputedId>,
}

impl ComputedStore {
    /// A fresh, empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a derivation that drives `node`'s `class` dirtying. The `eval`
    /// body is stored but not run until [`ComputedStore::eval`]; the returned
    /// [`ComputedId`] is the handle.
    pub fn alloc(
        &mut self,
        node: NodeId,
        class: DirtyClass,
        eval: impl FnMut(&mut ComputeCx<'_>) -> StateValue + 'static,
    ) -> ComputedId {
        let eval: EvalFn = Box::new(eval);
        if let Some(index) = self.free.pop() {
            let slot = &mut self.slots[index as usize];
            debug_assert!(!slot.occupied);
            slot.occupied = true;
            slot.value = None;
            slot.node = node;
            slot.class = class;
            slot.deps.clear();
            slot.eval = eval;
            ComputedId {
                index,
                generation: slot.generation,
            }
        } else {
            let index = self.slots.len() as u32;
            self.slots.push(ComputedSlot {
                value: None,
                node,
                class,
                deps: Vec::new(),
                eval,
                generation: 0,
                occupied: true,
            });
            ComputedId {
                index,
                generation: 0,
            }
        }
    }

    /// Free a derivation, bumping its generation so surviving handles go stale.
    /// Returns whether the id was live.
    pub fn free(&mut self, id: ComputedId) -> bool {
        match self.slots.get(id.index as usize) {
            Some(slot) if slot.occupied && slot.generation == id.generation => {
                // Drop reverse-index entries before the slot dies so a later
                // write to a former dependency cannot name a stale derivation.
                self.deindex(id);
                let slot = &mut self.slots[id.index as usize];
                slot.occupied = false;
                slot.generation = slot.generation.wrapping_add(1);
                slot.deps.clear();
                slot.value = None;
                self.free.push(id.index);
                true
            }
            _ => false,
        }
    }

    /// Whether `id` refers to a currently-live derivation.
    #[inline]
    pub fn is_live(&self, id: ComputedId) -> bool {
        matches!(
            self.slots.get(id.index as usize),
            Some(slot) if slot.occupied && slot.generation == id.generation
        )
    }

    /// The cached value of a derivation, if it has been evaluated. `None` for a
    /// stale handle or a never-evaluated cell.
    #[inline]
    pub fn value(&self, id: ComputedId) -> Option<StateValue> {
        let slot = self.slots.get(id.index as usize)?;
        (slot.occupied && slot.generation == id.generation)
            .then_some(slot.value)
            .flatten()
    }

    /// Evaluate a derivation, refreshing its recorded dependency set into the
    /// reverse index that [`wake_computed`] reads.
    ///
    /// Returns whether the result changed from the cached value: `true` on the
    /// first evaluation and on any later one that produced a different value,
    /// `false` when the re-evaluation reproduced the cached result. The caller
    /// marks the downstream node dirty exactly when this returns `true` — an
    /// unchanged derivation propagates nothing (the memo boundary).
    ///
    /// The freshly observed dependencies replace the derivation's prior entries
    /// in the reverse index (deindex → refresh → reindex, matching
    /// [`EffectStore::run`]), so a dependency it stops reading stops waking it
    /// and a newly read one starts.
    ///
    /// [`wake_computed`]: ComputedStore::wake_computed
    pub fn eval(&mut self, id: ComputedId, states: &StateStore) -> bool {
        let Some(slot) = self.slots.get_mut(id.index as usize) else {
            return false;
        };
        if !slot.occupied || slot.generation != id.generation {
            return false;
        }

        let mut cursor = DepCursor::new();
        let next = {
            let mut cx = ComputeCx::new(states, &mut cursor);
            (slot.eval)(&mut cx)
        };

        // Drop the derivation's old reverse-index entries, record the freshly
        // observed dependency set, then reindex against it.
        self.deindex(id);
        {
            let slot = &mut self.slots[id.index as usize];
            slot.deps.clear();
            slot.deps.extend_from_slice(cursor.deps());
        }
        self.reindex(id);

        let slot = &mut self.slots[id.index as usize];
        let changed = slot.value != Some(next);
        slot.value = Some(next);
        changed
    }

    /// Re-evaluate every derivation that read any of the `changed` states and
    /// mark its downstream node dirty when its value changed — the flush's
    /// computed pass. Each affected derivation re-evaluates once even if several
    /// of its dependencies changed in the same transaction. Returns how many
    /// derivations produced a changed value (and so dirtied their node).
    ///
    /// The memo boundary lives here: [`eval`] returns whether the value changed,
    /// and only then does the node's dirty class get set — an unchanged
    /// derivation touches no node. A re-eval may itself change a derivation's
    /// dependency set (recorded fresh in [`eval`]); the newly gathered set takes
    /// effect for the *next* wake, so a single wake is a fixed pass over the
    /// derivations the current index names — no cascade within one flush.
    ///
    /// [`eval`]: ComputedStore::eval
    pub fn wake_computed(
        &mut self,
        changed: &[StateId],
        states: &StateStore,
        nodes: &mut NodeStore,
    ) -> u32 {
        // Gather the affected derivations into the reused scratch, deduplicating
        // so a derivation reading two changed states re-evaluates once.
        self.wake_scratch.clear();
        for &state in changed {
            let Some(deps) = self.dep_index.get(state.index() as usize) else {
                continue;
            };
            for &c in deps {
                if !self.wake_scratch.contains(&c) {
                    self.wake_scratch.push(c);
                }
            }
        }

        // Take the scratch out so `eval` can borrow `self` mutably; put it back
        // (empty) afterward to keep its capacity for the next wake.
        let mut targets = core::mem::take(&mut self.wake_scratch);
        let mut dirtied = 0;
        for &c in &targets {
            // A stale id here means the derivation was freed since indexing;
            // `eval` rejects it, so nothing dirties on a dead derivation.
            if self.eval(c, states)
                && let Some(slot) = self.slots.get(c.index as usize)
                && slot.occupied
                && slot.generation == c.generation
            {
                let (node, class) = (slot.node, slot.class);
                nodes.mark_dirty(node, class);
                dirtied += 1;
            }
        }
        targets.clear();
        self.wake_scratch = targets;
        dirtied
    }

    /// Add `id` to the reverse index for each of its currently recorded
    /// dependencies. Call after refreshing `slot.deps`.
    fn reindex(&mut self, id: ComputedId) {
        let deps = core::mem::take(&mut self.slots[id.index as usize].deps);
        for &dep in &deps {
            let i = dep.index() as usize;
            if i >= self.dep_index.len() {
                self.dep_index.resize_with(i + 1, Vec::new);
            }
            if !self.dep_index[i].contains(&id) {
                self.dep_index[i].push(id);
            }
        }
        self.slots[id.index as usize].deps = deps;
    }

    /// Remove `id` from the reverse index for each of its recorded dependencies.
    /// Leaves `slot.deps` intact (the caller clears it if the derivation dies).
    fn deindex(&mut self, id: ComputedId) {
        let deps = core::mem::take(&mut self.slots[id.index as usize].deps);
        for &dep in &deps {
            if let Some(bucket) = self.dep_index.get_mut(dep.index() as usize) {
                bucket.retain(|&c| c != id);
            }
        }
        self.slots[id.index as usize].deps = deps;
    }
}

/// The store of side effects, keyed by generational [`EffectId`].
///
/// Effects run synchronously this slice. Each holds the node it is scoped to,
/// its recorded dependency set, its body, and the cleanup its last run returned.
/// A dependency change restarts the effect (cleanup then re-run); freeing the
/// owning node cancels it (cleanup then drop).
///
/// Waking is driven by a reverse index rather than the dirty bitset: [`wake`]
/// looks up the effects that read a changed [`StateId`] and re-runs each. The
/// index is `dep_index[state.index()] -> the effects depending on it`, rebuilt
/// for an effect's dependencies on every run so a dependency an effect stops
/// reading stops waking it.
///
/// [`wake`]: EffectStore::wake
#[derive(Default)]
pub struct EffectStore {
    slots: Vec<EffectSlot>,
    free: Vec<u32>,
    /// Reverse dependency index, aligned to [`StateStore`] dense indices:
    /// `dep_index[i]` holds every effect that read the state at dense index `i`
    /// on its last run. A scratch reuse target for [`wake`] avoids reallocating.
    dep_index: Vec<Vec<EffectId>>,
    /// Reused buffer of effects to re-run this wake, so a wake allocates nothing
    /// on the steady path.
    wake_scratch: Vec<EffectId>,
}

impl EffectStore {
    /// A fresh, empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register an effect scoped to `node`. The `body` is stored but not run
    /// until [`EffectStore::run`]; the returned [`EffectId`] is the handle.
    pub fn alloc(
        &mut self,
        node: NodeId,
        body: impl FnMut(&mut ComputeCx<'_>) -> Option<Cleanup> + 'static,
    ) -> EffectId {
        let body: EffectFn = Box::new(body);
        if let Some(index) = self.free.pop() {
            let slot = &mut self.slots[index as usize];
            debug_assert!(!slot.occupied);
            debug_assert!(slot.cleanup.is_none(), "freed effect kept a cleanup");
            slot.occupied = true;
            slot.node = node;
            slot.deps.clear();
            slot.body = body;
            slot.cleanup = None;
            EffectId {
                index,
                generation: slot.generation,
            }
        } else {
            let index = self.slots.len() as u32;
            self.slots.push(EffectSlot {
                node,
                deps: Vec::new(),
                body,
                cleanup: None,
                generation: 0,
                occupied: true,
            });
            EffectId {
                index,
                generation: 0,
            }
        }
    }

    /// Whether `id` refers to a currently-live effect.
    #[inline]
    pub fn is_live(&self, id: EffectId) -> bool {
        matches!(
            self.slots.get(id.index as usize),
            Some(slot) if slot.occupied && slot.generation == id.generation
        )
    }

    /// Run (or re-run) an effect: run any prior cleanup first, then the body,
    /// recording its dependency set into the reverse index that [`wake`] reads.
    ///
    /// This is both the first run and the dependency restart — a restart is just
    /// "cleanup then run again". The recorded dependencies replace the effect's
    /// prior entries in the reverse index, so a dependency the body stops reading
    /// stops waking it and a newly read one starts.
    ///
    /// [`wake`]: EffectStore::wake
    pub fn run(&mut self, id: EffectId, states: &StateStore) -> bool {
        let Some(slot) = self.slots.get_mut(id.index as usize) else {
            return false;
        };
        if !slot.occupied || slot.generation != id.generation {
            return false;
        }

        // Dependency restart: the prior run's cleanup runs before the re-run.
        if let Some(cleanup) = slot.cleanup.take() {
            cleanup();
        }

        let mut cursor = DepCursor::new();
        let next_cleanup = {
            let mut cx = ComputeCx::new(states, &mut cursor);
            (slot.body)(&mut cx)
        };
        self.slots[id.index as usize].cleanup = next_cleanup;

        // Drop the effect's old reverse-index entries, then record the freshly
        // observed dependency set.
        self.deindex(id);
        {
            let slot = &mut self.slots[id.index as usize];
            slot.deps.clear();
            slot.deps.extend_from_slice(cursor.deps());
        }
        self.reindex(id);
        true
    }

    /// Re-run every effect that read any of the `changed` states — the flush's
    /// effect pass. Each affected effect restarts once even if several of its
    /// dependencies changed in the same transaction. Returns how many effects
    /// were re-run.
    ///
    /// A re-run may itself change an effect's dependency set (recorded fresh in
    /// [`run`]); the newly gathered set takes effect for the *next* wake, so a
    /// single wake is a fixed pass over the effects the current index names — no
    /// cascade within one flush.
    pub fn wake(&mut self, changed: &[StateId], states: &StateStore) -> u32 {
        // Gather the affected effects into the reused scratch, deduplicating so
        // an effect reading two changed states restarts once.
        self.wake_scratch.clear();
        for &state in changed {
            let Some(deps) = self.dep_index.get(state.index() as usize) else {
                continue;
            };
            for &eff in deps {
                if !self.wake_scratch.contains(&eff) {
                    self.wake_scratch.push(eff);
                }
            }
        }

        // Take the scratch out so `run` can borrow `self` mutably; put it back
        // (empty) afterward to keep its capacity for the next wake.
        let mut targets = core::mem::take(&mut self.wake_scratch);
        let mut ran = 0;
        for &eff in &targets {
            // A stale id here means the effect was cancelled since indexing;
            // `run` rejects it, so no cleanup fires on a dead effect.
            if self.run(eff, states) {
                ran += 1;
            }
        }
        targets.clear();
        self.wake_scratch = targets;
        ran
    }

    /// Cancel an effect: run its pending cleanup, drop its reverse-index entries,
    /// and free the slot. Returns whether the id was live. Idempotent for a stale
    /// handle.
    pub fn cancel(&mut self, id: EffectId) -> bool {
        let Some(slot) = self.slots.get_mut(id.index as usize) else {
            return false;
        };
        if !slot.occupied || slot.generation != id.generation {
            return false;
        }
        if let Some(cleanup) = slot.cleanup.take() {
            cleanup();
        }
        self.deindex(id);
        let slot = &mut self.slots[id.index as usize];
        slot.occupied = false;
        slot.generation = slot.generation.wrapping_add(1);
        slot.deps.clear();
        self.free.push(id.index);
        true
    }

    /// Cancel every effect scoped to `node`, running each pending cleanup — the
    /// unmount path. Called when a node is freed so its effects release their
    /// resources deterministically. Returns how many effects were cancelled.
    pub fn cancel_for_node(&mut self, node: NodeId) -> u32 {
        let mut cancelled = 0;
        for index in 0..self.slots.len() {
            if !self.slots[index].occupied || self.slots[index].node != node {
                continue;
            }
            let id = EffectId {
                index: index as u32,
                generation: self.slots[index].generation,
            };
            if let Some(cleanup) = self.slots[index].cleanup.take() {
                cleanup();
            }
            self.deindex(id);
            let slot = &mut self.slots[index];
            slot.occupied = false;
            slot.generation = slot.generation.wrapping_add(1);
            slot.deps.clear();
            self.free.push(index as u32);
            cancelled += 1;
        }
        cancelled
    }

    /// Add `id` to the reverse index for each of its currently recorded
    /// dependencies. Call after refreshing `slot.deps`.
    fn reindex(&mut self, id: EffectId) {
        let deps = core::mem::take(&mut self.slots[id.index as usize].deps);
        for &dep in &deps {
            let i = dep.index() as usize;
            if i >= self.dep_index.len() {
                self.dep_index.resize_with(i + 1, Vec::new);
            }
            if !self.dep_index[i].contains(&id) {
                self.dep_index[i].push(id);
            }
        }
        self.slots[id.index as usize].deps = deps;
    }

    /// Remove `id` from the reverse index for each of its recorded dependencies.
    /// Leaves `slot.deps` intact (the caller clears it if the effect is dying).
    fn deindex(&mut self, id: EffectId) {
        let deps = core::mem::take(&mut self.slots[id.index as usize].deps);
        for &dep in &deps {
            if let Some(bucket) = self.dep_index.get_mut(dep.index() as usize) {
                bucket.retain(|&e| e != id);
            }
        }
        self.slots[id.index as usize].deps = deps;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::component::{BuildCx, LeafStyle};
    use crate::node::NodeArena;
    use std::cell::RefCell;
    use std::rc::Rc;

    fn state(store: &mut StateStore, v: StateValue) -> StateId {
        store.alloc(v)
    }
    fn node(arena: &mut NodeArena) -> NodeId {
        arena.alloc()
    }

    /// Build a single leaf into `store` and return its id — a real
    /// `NodeStore`-resident node so `mark_dirty`/`dirty` have somewhere to land
    /// (a bare `NodeArena` id has no SoA row).
    fn sink_node(store: &mut NodeStore) -> NodeId {
        BuildCx::new(store).leaf(LeafStyle::default()).id()
    }

    // --- Computed -----------------------------------------------------------

    #[test]
    fn computed_first_eval_records_deps_and_reports_change() {
        let mut states = StateStore::new();
        let mut arena = NodeArena::new();
        let mut computed = ComputedStore::new();

        let n = state(&mut states, StateValue::Int(2));
        let sink = node(&mut arena);

        // A derivation that doubles `n`, driving the sink node's PAINT.
        let c = computed.alloc(sink, DirtyClass::PAINT, move |cx| match cx.get(n) {
            Some(StateValue::Int(v)) => StateValue::Int(v * 2),
            _ => StateValue::Int(0),
        });

        // First eval: value goes from "unset" to 4, so it reports changed, and
        // records `n` as a dependency so a later wake finds it.
        assert!(computed.eval(c, &states));
        assert_eq!(computed.value(c), Some(StateValue::Int(4)));
    }

    #[test]
    fn computed_reeval_after_dep_change_reports_change() {
        let mut states = StateStore::new();
        let mut arena = NodeArena::new();
        let mut computed = ComputedStore::new();

        let n = state(&mut states, StateValue::Int(2));
        let sink = node(&mut arena);
        let c = computed.alloc(sink, DirtyClass::PAINT, move |cx| match cx.get(n) {
            Some(StateValue::Int(v)) => StateValue::Int(v * 2),
            _ => StateValue::Int(0),
        });

        computed.eval(c, &states);
        states.set(n, StateValue::Int(5));
        assert!(
            computed.eval(c, &states),
            "a dependency change that alters the result reports changed"
        );
        assert_eq!(computed.value(c), Some(StateValue::Int(10)));
    }

    #[test]
    fn unchanged_computed_result_propagates_nothing() {
        let mut states = StateStore::new();
        let mut arena = NodeArena::new();
        let mut computed = ComputedStore::new();

        // Result is a step function: 0 for n < 10, else 1. Bumping n within the
        // same step must not report a change — the memo boundary.
        let n = state(&mut states, StateValue::Int(2));
        let sink = node(&mut arena);
        let c = computed.alloc(sink, DirtyClass::PAINT, move |cx| match cx.get(n) {
            Some(StateValue::Int(v)) if v >= 10 => StateValue::Int(1),
            _ => StateValue::Int(0),
        });

        assert!(computed.eval(c, &states), "first eval changes");
        states.set(n, StateValue::Int(3));
        assert!(
            !computed.eval(c, &states),
            "same step → unchanged result → no propagation"
        );
        states.set(n, StateValue::Int(42));
        assert!(
            computed.eval(c, &states),
            "crossing the step boundary changes the result"
        );
    }

    #[test]
    fn freed_computed_is_stale() {
        let mut states = StateStore::new();
        let mut arena = NodeArena::new();
        let mut computed = ComputedStore::new();
        let n = state(&mut states, StateValue::Int(1));
        let sink = node(&mut arena);
        let c = computed.alloc(sink, DirtyClass::PAINT, move |cx| {
            cx.get(n);
            StateValue::Int(0)
        });
        assert!(computed.is_live(c));
        assert!(computed.free(c));
        assert!(!computed.is_live(c));
        assert!(!computed.free(c), "double free is rejected");
        assert!(
            !computed.eval(c, &states),
            "a stale handle evaluates nothing"
        );
    }

    #[test]
    fn computed_wake_dirties_node_only_on_change() {
        let mut states = StateStore::new();
        let mut store = NodeStore::new();
        let mut computed = ComputedStore::new();

        // A real node so `mark_dirty` has somewhere to land; a step function so a
        // write can leave the derived value unchanged.
        let sink = sink_node(&mut store);
        let n = state(&mut states, StateValue::Int(2));
        let c = computed.alloc(sink, DirtyClass::PAINT, move |cx| match cx.get(n) {
            Some(StateValue::Int(v)) if v >= 10 => StateValue::Int(1),
            _ => StateValue::Int(0),
        });

        // Seed: first eval records the dependency (its value is irrelevant here).
        computed.eval(c, &states);
        store.clear_dirty();

        // Same-step write: wake re-evaluates, value is unchanged, node stays clean.
        states.set(n, StateValue::Int(3));
        let mut changed = Vec::new();
        states.take_pending(&mut changed);
        assert_eq!(
            computed.wake_computed(&changed, &states, &mut store),
            0,
            "unchanged derivation dirties nothing"
        );
        assert!(
            store.dirty(sink).is_empty(),
            "node untouched on unchanged eval"
        );

        // Step-crossing write: wake re-evaluates, value changes, node goes dirty.
        states.set(n, StateValue::Int(42));
        changed.clear();
        states.take_pending(&mut changed);
        assert_eq!(computed.wake_computed(&changed, &states, &mut store), 1);
        assert!(
            store.dirty(sink).contains(DirtyClass::PAINT),
            "changed derivation dirties its node's class"
        );
    }

    #[test]
    fn computed_stops_waking_on_dropped_dependency() {
        let mut states = StateStore::new();
        let mut store = NodeStore::new();
        let mut computed = ComputedStore::new();

        let sink = sink_node(&mut store);
        let gate = state(&mut states, StateValue::Bool(true));
        let inner = state(&mut states, StateValue::Int(0));

        // Reads `inner` only while `gate` is true. Once `gate` flips false the
        // body stops reading `inner`, so `inner` must stop waking it.
        let c = computed.alloc(sink, DirtyClass::PAINT, move |cx| {
            if matches!(cx.get(gate), Some(StateValue::Bool(true))) {
                cx.get(inner)
            } else {
                Some(StateValue::Int(-1))
            }
            .unwrap_or(StateValue::Int(0))
        });
        computed.eval(c, &states); // deps = {gate, inner}
        store.clear_dirty();

        // Flip the gate: wakes via `gate`, value changes, and this eval does NOT
        // read `inner`, so `inner` drops out of the reverse index.
        states.set(gate, StateValue::Bool(false));
        let mut changed = Vec::new();
        states.take_pending(&mut changed);
        assert_eq!(computed.wake_computed(&changed, &states, &mut store), 1);
        store.clear_dirty();

        // Writing `inner` now wakes nothing — it is no longer a dependency.
        states.set(inner, StateValue::Int(99));
        changed.clear();
        states.take_pending(&mut changed);
        assert_eq!(
            computed.wake_computed(&changed, &states, &mut store),
            0,
            "dropped dep stops waking"
        );
    }

    #[test]
    fn freed_computed_drops_from_reverse_index() {
        let mut states = StateStore::new();
        let mut store = NodeStore::new();
        let mut computed = ComputedStore::new();

        let sink = sink_node(&mut store);
        let n = state(&mut states, StateValue::Int(0));
        let c = computed.alloc(sink, DirtyClass::PAINT, move |cx| {
            cx.get(n).unwrap_or(StateValue::Int(0))
        });
        computed.eval(c, &states);
        assert!(computed.free(c));

        // A write to a former dependency of the freed derivation wakes nothing.
        states.set(n, StateValue::Int(7));
        let mut changed = Vec::new();
        states.take_pending(&mut changed);
        assert_eq!(
            computed.wake_computed(&changed, &states, &mut store),
            0,
            "a freed derivation is out of the reverse index"
        );
    }

    // --- Effect -------------------------------------------------------------

    #[test]
    fn effect_dep_change_runs_cleanup_then_reruns() {
        let mut states = StateStore::new();
        let mut arena = NodeArena::new();
        let mut effects = EffectStore::new();

        let n = state(&mut states, StateValue::Int(1));
        let owner = node(&mut arena);

        // The body appends "run" and its cleanup appends "cleanup", so the log
        // records the exact lifecycle order.
        let log: Rc<RefCell<Vec<&'static str>>> = Rc::new(RefCell::new(Vec::new()));
        let body_log = Rc::clone(&log);
        let e = effects.alloc(owner, move |cx| {
            cx.get(n);
            body_log.borrow_mut().push("run");
            let cl = Rc::clone(&body_log);
            let cleanup: Cleanup = Box::new(move || cl.borrow_mut().push("cleanup"));
            Some(cleanup)
        });

        // First run: body only, no prior cleanup.
        assert!(effects.run(e, &states));
        assert_eq!(*log.borrow(), ["run"]);

        // A dependency write, delivered through wake, restarts the effect:
        // the prior cleanup runs, then the body.
        states.set(n, StateValue::Int(2));
        let mut changed = Vec::new();
        states.take_pending(&mut changed);
        assert_eq!(effects.wake(&changed, &states), 1, "one effect re-ran");
        assert_eq!(*log.borrow(), ["run", "cleanup", "run"]);
    }

    #[test]
    fn effect_only_wakes_on_its_own_deps() {
        let mut states = StateStore::new();
        let mut arena = NodeArena::new();
        let mut effects = EffectStore::new();

        let watched = state(&mut states, StateValue::Int(0));
        let other = state(&mut states, StateValue::Int(0));
        let owner = node(&mut arena);

        let runs = Rc::new(RefCell::new(0u32));
        let runs_body = Rc::clone(&runs);
        let e = effects.alloc(owner, move |cx| {
            cx.get(watched);
            *runs_body.borrow_mut() += 1;
            None
        });
        assert!(effects.run(e, &states));
        assert_eq!(*runs.borrow(), 1);

        // Writing an unrelated state wakes nothing.
        states.set(other, StateValue::Int(9));
        let mut changed = Vec::new();
        states.take_pending(&mut changed);
        assert_eq!(effects.wake(&changed, &states), 0);
        assert_eq!(*runs.borrow(), 1, "unrelated write does not re-run");

        // Writing the watched state does.
        states.set(watched, StateValue::Int(1));
        changed.clear();
        states.take_pending(&mut changed);
        assert_eq!(effects.wake(&changed, &states), 1);
        assert_eq!(*runs.borrow(), 2);
    }

    #[test]
    fn effect_reading_two_changed_states_reruns_once() {
        let mut states = StateStore::new();
        let mut arena = NodeArena::new();
        let mut effects = EffectStore::new();

        let a = state(&mut states, StateValue::Int(0));
        let b = state(&mut states, StateValue::Int(0));
        let owner = node(&mut arena);

        let runs = Rc::new(RefCell::new(0u32));
        let runs_body = Rc::clone(&runs);
        let e = effects.alloc(owner, move |cx| {
            cx.get(a);
            cx.get(b);
            *runs_body.borrow_mut() += 1;
            None
        });
        effects.run(e, &states);
        assert_eq!(*runs.borrow(), 1);

        // Both dependencies change in one transaction; the effect restarts once.
        states.set(a, StateValue::Int(1));
        states.set(b, StateValue::Int(1));
        let mut changed = Vec::new();
        states.take_pending(&mut changed);
        assert_eq!(
            effects.wake(&changed, &states),
            1,
            "restarts once, not twice"
        );
        assert_eq!(*runs.borrow(), 2);
    }

    #[test]
    fn node_unmount_runs_effect_cleanup() {
        let mut states = StateStore::new();
        let mut arena = NodeArena::new();
        let mut effects = EffectStore::new();

        let n = state(&mut states, StateValue::Int(1));
        let owner = node(&mut arena);

        let cleaned = Rc::new(RefCell::new(false));
        let cleaned_body = Rc::clone(&cleaned);
        let e = effects.alloc(owner, move |cx| {
            cx.get(n);
            let flag = Rc::clone(&cleaned_body);
            let cleanup: Cleanup = Box::new(move || *flag.borrow_mut() = true);
            Some(cleanup)
        });
        effects.run(e, &states);
        assert!(!*cleaned.borrow());

        // Unmount: freeing the node cancels its effects, running each cleanup.
        assert_eq!(effects.cancel_for_node(owner), 1);
        assert!(*cleaned.borrow(), "unmount ran the pending cleanup");
        assert!(!effects.is_live(e), "cancelled effect is stale");

        // And a later write to what was a dependency wakes nothing.
        states.set(n, StateValue::Int(2));
        let mut changed = Vec::new();
        states.take_pending(&mut changed);
        assert_eq!(
            effects.wake(&changed, &states),
            0,
            "a cancelled effect is out of the reverse index"
        );
    }

    #[test]
    fn effect_stops_waking_on_dropped_dependency() {
        let mut states = StateStore::new();
        let mut arena = NodeArena::new();
        let mut effects = EffectStore::new();

        let gate = state(&mut states, StateValue::Bool(true));
        let inner = state(&mut states, StateValue::Int(0));
        let owner = node(&mut arena);

        // The body reads `inner` only while `gate` is true. Once `gate` flips
        // false the body stops reading `inner`, so `inner` must stop waking it.
        let runs = Rc::new(RefCell::new(0u32));
        let runs_body = Rc::clone(&runs);
        let e = effects.alloc(owner, move |cx| {
            *runs_body.borrow_mut() += 1;
            if matches!(cx.get(gate), Some(StateValue::Bool(true))) {
                cx.get(inner);
            }
            None
        });
        effects.run(e, &states); // runs=1, deps={gate, inner}

        // Flip the gate: wakes via `gate`, re-runs, and this run does NOT read
        // `inner`, so `inner` drops out of the reverse index.
        states.set(gate, StateValue::Bool(false));
        let mut changed = Vec::new();
        states.take_pending(&mut changed);
        assert_eq!(effects.wake(&changed, &states), 1); // runs=2, deps={gate}
        assert_eq!(*runs.borrow(), 2);

        // Now writing `inner` must wake nothing — it is no longer a dependency.
        states.set(inner, StateValue::Int(99));
        changed.clear();
        states.take_pending(&mut changed);
        assert_eq!(
            effects.wake(&changed, &states),
            0,
            "dropped dep stops waking"
        );
        assert_eq!(*runs.borrow(), 2);
    }
}
