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

use core::cell::Cell;

use crate::dirty::DirtyClass;
use crate::node::NodeId;
use crate::state::StateId;

/// The reactive-binding observability counters (AGENTS section 10.3).
///
/// A compiler-known typed binding must never silently fall back to runtime
/// dynamic tracking, and the dynamic path must never define the cost of the
/// static fast path — so the two paths are counted apart. These are strippable
/// observability counters (an inspector/CI reads them to confirm a strict typed
/// example does zero dynamic fallback); they never gate correctness and never
/// ride a hot inner loop beyond a single increment per edge evaluated.
///
/// The evaluation counters are `Cell`s so a `&BindingTable` flush can bump them
/// while the table stays shared-borrowed.
#[derive(Debug, Default)]
pub struct ReactiveCounters {
    /// Static edges evaluated (a compiled `for_state` edge walked on flush).
    static_binding_eval: Cell<u64>,
    /// Dynamic edges evaluated (a runtime `dynamic_for_state` edge walked).
    dynamic_binding_eval: Cell<u64>,
    /// Runtime dynamic subscriptions registered (each `bind_dynamic` call).
    dynamic_subscribe: Cell<u64>,
    /// Distinct nodes that took a dynamic edge — the fallback surface a strict
    /// typed example must keep at zero.
    dynamic_fallback_nodes: Cell<u64>,
}

impl ReactiveCounters {
    /// Static edges evaluated since the last reset.
    #[inline]
    pub fn static_binding_eval(&self) -> u64 {
        self.static_binding_eval.get()
    }
    /// Dynamic edges evaluated since the last reset.
    #[inline]
    pub fn dynamic_binding_eval(&self) -> u64 {
        self.dynamic_binding_eval.get()
    }
    /// Runtime dynamic subscriptions registered since the last reset.
    #[inline]
    pub fn dynamic_subscribe(&self) -> u64 {
        self.dynamic_subscribe.get()
    }
    /// Distinct nodes that took a dynamic edge since the last reset.
    #[inline]
    pub fn dynamic_fallback_nodes(&self) -> u64 {
        self.dynamic_fallback_nodes.get()
    }

    /// Zero every counter — call at frame or benchmark-iteration boundaries so a
    /// count reflects one measured window.
    pub fn reset(&self) {
        self.static_binding_eval.set(0);
        self.dynamic_binding_eval.set(0);
        self.dynamic_subscribe.set(0);
        self.dynamic_fallback_nodes.set(0);
    }

    /// Record `n` static edges as evaluated. Called at the flush edge-walk site
    /// (each state's `for_state` slice length), so the count reflects one flush's
    /// compiled-path work without threading a `&mut` counter through the shared
    /// `flush_state_transactions` signature.
    #[inline]
    pub fn record_static_eval(&self, n: u64) {
        self.static_binding_eval
            .set(self.static_binding_eval.get() + n);
    }
    /// Record `n` dynamic edges as evaluated, the dynamic-path counterpart of
    /// [`Self::record_static_eval`].
    #[inline]
    pub fn record_dynamic_eval(&self, n: u64) {
        self.dynamic_binding_eval
            .set(self.dynamic_binding_eval.get() + n);
    }
}

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
    /// Reactive-path observability (section 10.3). Bumped at bind time (dynamic
    /// subscriptions / fallback nodes) and at evaluation time (static/dynamic
    /// edges walked). Strippable; never gates correctness.
    counters: ReactiveCounters,
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
        // A subscription is registered on every dynamic bind; a *new* node edge
        // (not a fold into an existing one) is a fresh fallback node.
        self.counters
            .dynamic_subscribe
            .set(self.counters.dynamic_subscribe.get() + 1);
        let before = self.dynamic.len();
        Self::insert_edge(&mut self.dynamic, &mut self.dynamic_runs, idx, node, class);
        if self.dynamic.len() > before {
            self.counters
                .dynamic_fallback_nodes
                .set(self.counters.dynamic_fallback_nodes.get() + 1);
        }
    }

    /// Drop every static edge, leaving the dynamic region and counters intact.
    ///
    /// A transactional hot reload replaces the compiled binding set wholesale:
    /// the recompiled Binding IR is the new source of truth, so the prior static
    /// edges must be voided before the new ones are installed (there is no
    /// unbind — a compiled edge set is rebuilt, not patched). The dynamic region
    /// is left untouched because it is an explicit runtime escape hatch (section
    /// 10.3),
    /// not part of the compiled template. Cold path (reload only).
    pub fn clear_static(&mut self) {
        self.edges.clear();
        self.runs.clear();
    }

    /// Replace the static region with a fresh compiled edge set.
    ///
    /// Equivalent to [`Self::clear_static`] followed by a [`Self::bind`] for each
    /// edge, but reserving once. Edges may arrive in any order; grouping and
    /// same-node class folding match `bind`, so the resulting `for_state` slices
    /// keep their dense, contiguous layout. Cold path (reload only).
    pub fn rebuild_static(&mut self, edges: impl IntoIterator<Item = (StateId, Binding)>) {
        self.clear_static();
        for (state, binding) in edges {
            self.bind(state, binding.node, binding.class);
        }
    }

    /// The reactive-path counters (section 10.3), for an inspector, a test, or a
    /// benchmark comparing the static / mixed / dynamic paths.
    #[inline]
    pub fn counters(&self) -> &ReactiveCounters {
        &self.counters
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
    fn bind_dynamic_counts_subscribe_and_fallback_nodes() {
        let mut states = StateStore::new();
        let mut arena = NodeArena::new();
        let mut table = BindingTable::new();

        let s = state(&mut states);
        let a = node(&mut arena);
        let b = node(&mut arena);

        table.bind_dynamic(s, a, DirtyClass::PAINT);
        // A second edge to a *new* node is a fresh fallback node.
        table.bind_dynamic(s, b, DirtyClass::LAYOUT);
        // Re-binding an existing node folds its class — a subscription, not a
        // new fallback node.
        table.bind_dynamic(s, a, DirtyClass::MEASURE);

        let c = table.counters();
        assert_eq!(c.dynamic_subscribe(), 3, "one per bind_dynamic call");
        assert_eq!(c.dynamic_fallback_nodes(), 2, "two distinct dynamic nodes");
    }

    #[test]
    fn counters_reset_zeroes_all() {
        let mut states = StateStore::new();
        let mut arena = NodeArena::new();
        let mut table = BindingTable::new();

        let s = state(&mut states);
        let a = node(&mut arena);
        table.bind_dynamic(s, a, DirtyClass::PAINT);
        table.counters().record_static_eval(4);
        table.counters().record_dynamic_eval(2);

        table.counters().reset();
        let c = table.counters();
        assert_eq!(c.static_binding_eval(), 0);
        assert_eq!(c.dynamic_binding_eval(), 0);
        assert_eq!(c.dynamic_subscribe(), 0);
        assert_eq!(c.dynamic_fallback_nodes(), 0);
    }

    #[test]
    fn record_eval_accumulates_each_path_separately() {
        let table = BindingTable::new();
        let c = table.counters();
        c.record_static_eval(3);
        c.record_static_eval(2);
        c.record_dynamic_eval(1);
        assert_eq!(c.static_binding_eval(), 5, "static edges accumulate");
        assert_eq!(c.dynamic_binding_eval(), 1, "counted apart from static");
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

    #[test]
    fn rebuild_static_replaces_edges_and_keeps_dynamic() {
        let mut states = StateStore::new();
        let mut arena = NodeArena::new();
        let mut table = BindingTable::new();

        let s = state(&mut states);
        let old = node(&mut arena);
        let dynamic_node = node(&mut arena);
        table.bind(s, old, DirtyClass::PAINT);
        table.bind_dynamic(s, dynamic_node, DirtyClass::LAYOUT);

        // A reload installs a fresh compiled edge set for the same state.
        let fresh = node(&mut arena);
        table.rebuild_static([(
            s,
            Binding {
                node: fresh,
                class: DirtyClass::MEASURE | DirtyClass::LAYOUT,
            },
        )]);

        let edges = table.for_state(s);
        assert_eq!(edges.len(), 1, "old static edge dropped");
        assert_eq!(edges[0].node, fresh);
        assert!(
            edges[0]
                .class
                .contains(DirtyClass::MEASURE | DirtyClass::LAYOUT)
        );
        // Dynamic region is untouched by a static rebuild.
        assert_eq!(table.dynamic_for_state(s).len(), 1);
        assert_eq!(table.dynamic_for_state(s)[0].node, dynamic_node);
    }

    #[test]
    fn clear_static_leaves_no_edges() {
        let mut states = StateStore::new();
        let mut arena = NodeArena::new();
        let mut table = BindingTable::new();

        let s = state(&mut states);
        let a = node(&mut arena);
        table.bind(s, a, DirtyClass::PAINT);
        table.clear_static();
        assert!(table.for_state(s).is_empty());
    }
}
