//! The binding table: the compiled edge from a [`StateId`] to the nodes it
//! invalidates and the dirty classes each edge carries.
//!
//! This is the static fast path of the reactive model. A binding is registered
//! once (at build/reconcile time) and read every flush; it turns "state `count`
//! changed" into "mark node `label` MEASURE|LAYOUT|PAINT, mark node `badge`
//! PAINT" without any per-signal subscriber list, closure boxing, or string
//! lookup. Lookups are index-aligned to the [`StateStore`]: `for_state(id)`
//! slices one contiguous run, so the flush walks bindings with cache locality
//! and zero allocation on the steady path.
//!
//! A separate dynamic region carries bindings registered at runtime (by a
//! dynamic script or a `Computed` re-eval, landing in a later slice). It shares
//! the same [`Binding`] shape but is stored apart so the static edges keep
//! their dense, compiled layout — the dynamic fallback must never define the
//! cost of the static path.

use crate::dirty::DirtyClass;
use crate::node::NodeId;
use crate::state::StateId;

/// One reactive edge: when the source state changes, mark `node` dirty with
/// `class`. A single state may have many bindings (one per dependent node /
/// property), so the flush marks each in turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Binding {
    /// The node this edge invalidates.
    pub node: NodeId,
    /// The dirty classes the change contributes to that node (e.g. a bound
    /// width is `MEASURE | LAYOUT`; a bound color is `PAINT`).
    pub class: DirtyClass,
}

/// Compiled reactive edges keyed by [`StateId`], plus a dynamic-region overflow
/// for runtime-registered edges.
///
/// The static edges are grouped by source state so that `for_state` returns a
/// contiguous slice. Because states are allocated densely and rarely freed
/// mid-run, grouping by `StateId::index()` keeps the common case a single
/// range lookup.
#[derive(Default)]
pub struct BindingTable {
    /// All static bindings, sorted so every source state's edges are contiguous.
    edges: Vec<Binding>,
    /// `runs[i]` is the `(start, len)` slice of `edges` owned by the state at
    /// dense index `i`. Index-aligned to the state store; a state with no
    /// bindings has `len == 0`.
    runs: Vec<Run>,
    /// Runtime-registered edges (dynamic scripts, `Computed`). Kept apart from
    /// the compiled `edges` so the static path stays dense. Grouped the same
    /// way via `dynamic_runs`.
    dynamic: Vec<Binding>,
    dynamic_runs: Vec<Run>,
}

/// A contiguous slice `[start, start+len)` into a binding array.
#[derive(Debug, Clone, Copy, Default)]
struct Run {
    start: u32,
    len: u32,
}

impl BindingTable {
    /// An empty table.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a static reactive edge: when `state` changes, mark `node`
    /// dirty with `class`.
    ///
    /// Intended for build/reconcile time. Edges for one state are kept
    /// contiguous; registering out of order still groups correctly, at the
    /// cost of an insertion shift. Repeated bindings of the same `(node,
    /// class)` are merged by folding the class in, so a state bound twice to
    /// the same node stays one edge.
    pub fn bind(&mut self, state: StateId, node: NodeId, class: DirtyClass) {
        let idx = state.index() as usize;
        if idx >= self.runs.len() {
            self.runs.resize(idx + 1, Run::default());
        }
        Self::insert_edge(&mut self.edges, &mut self.runs, idx, node, class);
    }

    /// The static bindings for a state — the edges to walk when it changes.
    /// Empty for a state with no static bindings (or an out-of-range id).
    #[inline]
    pub fn for_state(&self, state: StateId) -> &[Binding] {
        Self::slice(&self.edges, &self.runs, state)
    }

    /// Register a dynamic (runtime) reactive edge. Shares [`Binding`] shape with
    /// the static edges but lives in the separate dynamic region so it never
    /// perturbs the compiled layout. Used by dynamic scripts and, in a later
    /// slice, by `Computed` dependency registration.
    pub fn bind_dynamic(&mut self, state: StateId, node: NodeId, class: DirtyClass) {
        let idx = state.index() as usize;
        if idx >= self.dynamic_runs.len() {
            self.dynamic_runs.resize(idx + 1, Run::default());
        }
        Self::insert_edge(&mut self.dynamic, &mut self.dynamic_runs, idx, node, class);
    }

    /// Insert `(node, class)` into the run owned by dense state index `idx`,
    /// folding into an existing edge to the same node, else appending at the
    /// end of the run and shifting every later run right by one. Shared by the
    /// static and dynamic regions.
    fn insert_edge(
        edges: &mut Vec<Binding>,
        runs: &mut [Run],
        idx: usize,
        node: NodeId,
        class: DirtyClass,
    ) {
        // An empty run has no meaningful start yet — its first edge appends at
        // the end of the highest existing run below it. Anchor it there so the
        // insertion point and the later-run shift stay coherent regardless of
        // registration order.
        if runs[idx].len == 0 {
            let anchor = runs[..idx]
                .iter()
                .filter(|r| r.len > 0)
                .map(|r| r.start + r.len)
                .max()
                .unwrap_or(0);
            runs[idx].start = anchor;
        }
        let run = runs[idx];
        let start = run.start as usize;
        let end = start + run.len as usize;
        for edge in &mut edges[start..end] {
            if edge.node == node {
                edge.class |= class;
                return;
            }
        }
        edges.insert(end, Binding { node, class });
        runs[idx].len += 1;
        // Every run positioned at or after the insertion point moves right by
        // one. Non-empty runs shift by their start; empty runs carry no edges
        // and are re-anchored on their first insert, so they need no bump here.
        for later in &mut runs[idx + 1..] {
            if later.len > 0 && later.start as usize >= end {
                later.start += 1;
            }
        }
    }

    /// The dynamic bindings for a state.
    #[inline]
    pub fn dynamic_for_state(&self, state: StateId) -> &[Binding] {
        Self::slice(&self.dynamic, &self.dynamic_runs, state)
    }

    /// Shared run-slice lookup for either region.
    #[inline]
    fn slice<'a>(edges: &'a [Binding], runs: &[Run], state: StateId) -> &'a [Binding] {
        match runs.get(state.index() as usize) {
            Some(run) if run.len > 0 => {
                let start = run.start as usize;
                &edges[start..start + run.len as usize]
            }
            _ => &[],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::NodeArena;
    use crate::state::{StateStore, StateValue};

    // The binding table never dereferences its ids; it only groups by
    // `StateId::index()` and hands back `NodeId`s, so allocating real ids from
    // the stores is enough to exercise it.
    fn state(store: &mut StateStore) -> StateId {
        store.alloc(StateValue::Int(0))
    }
    fn node(arena: &mut NodeArena) -> NodeId {
        arena.alloc()
    }

    #[test]
    fn bind_and_lookup_single_state() {
        let mut states = StateStore::new();
        let mut arena = NodeArena::new();
        let mut table = BindingTable::new();

        let s = state(&mut states);
        let a = node(&mut arena);
        let b = node(&mut arena);

        table.bind(s, a, DirtyClass::MEASURE | DirtyClass::LAYOUT);
        table.bind(s, b, DirtyClass::PAINT);

        let edges = table.for_state(s);
        assert_eq!(edges.len(), 2);
        assert_eq!(edges[0].node, a);
        assert!(
            edges[0]
                .class
                .contains(DirtyClass::MEASURE | DirtyClass::LAYOUT)
        );
        assert_eq!(edges[1].node, b);
        assert!(edges[1].class.contains(DirtyClass::PAINT));
    }

    #[test]
    fn repeated_bind_same_node_folds_class() {
        let mut states = StateStore::new();
        let mut arena = NodeArena::new();
        let mut table = BindingTable::new();

        let s = state(&mut states);
        let a = node(&mut arena);
        table.bind(s, a, DirtyClass::MEASURE);
        table.bind(s, a, DirtyClass::PAINT);

        let edges = table.for_state(s);
        assert_eq!(edges.len(), 1, "same node stays one edge");
        assert!(
            edges[0]
                .class
                .contains(DirtyClass::MEASURE | DirtyClass::PAINT)
        );
    }

    #[test]
    fn multiple_states_keep_contiguous_runs() {
        let mut states = StateStore::new();
        let mut arena = NodeArena::new();
        let mut table = BindingTable::new();

        let s0 = state(&mut states);
        let s1 = state(&mut states);
        let a = node(&mut arena);
        let b = node(&mut arena);
        let c = node(&mut arena);

        // Interleave registration order to prove grouping is by state, not order.
        table.bind(s0, a, DirtyClass::PAINT);
        table.bind(s1, b, DirtyClass::LAYOUT);
        table.bind(s0, c, DirtyClass::PAINT);

        let e0 = table.for_state(s0);
        assert_eq!(e0.len(), 2);
        assert_eq!(e0[0].node, a);
        assert_eq!(e0[1].node, c);

        let e1 = table.for_state(s1);
        assert_eq!(e1.len(), 1);
        assert_eq!(e1[0].node, b);
    }

    #[test]
    fn unbound_state_has_no_edges() {
        let mut states = StateStore::new();
        let table = BindingTable::new();
        let s = state(&mut states);
        assert!(table.for_state(s).is_empty());
    }

    #[test]
    fn dynamic_region_is_separate() {
        let mut states = StateStore::new();
        let mut arena = NodeArena::new();
        let mut table = BindingTable::new();

        let s = state(&mut states);
        let a = node(&mut arena);
        let b = node(&mut arena);
        table.bind(s, a, DirtyClass::PAINT);
        table.bind_dynamic(s, b, DirtyClass::LAYOUT);

        assert_eq!(table.for_state(s).len(), 1);
        assert_eq!(table.for_state(s)[0].node, a);
        assert_eq!(table.dynamic_for_state(s).len(), 1);
        assert_eq!(table.dynamic_for_state(s)[0].node, b);
    }
}
