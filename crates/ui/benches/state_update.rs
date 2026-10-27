//! The "state invalidation" benchmark category end to end: one state write,
//! then the frame's whole reactive flush (`settle_states`: derivations,
//! bindings, structure hooks, projections, effects) until it settles, with 1
//! or 1000 compiled bindings reading the written state. A thousand labels are
//! mounted either way, so the two differ only in the bindings the write
//! reaches.
//!
//! Run release (`cargo bench -p viso-ui --bench state_update`); debug timing
//! is not a performance result.

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use viso_ui::{
    BindingTable, BuildCx, ComputedStore, DirtyClass, EffectStore, FlexStyle, LeafStyle, NodeId,
    NodeStore, SemanticProjector, StateId, StateStore, StateValue, VirtualLists, settle_states,
};

/// Labels mounted under the root.
const LABELS: usize = 1000;

/// Everything the frame's flush reads and writes.
struct Harness {
    store: NodeStore,
    states: StateStore,
    bindings: BindingTable,
    computeds: ComputedStore,
    projectors: SemanticProjector,
    effects: EffectStore,
    lists: VirtualLists,
    changed: Vec<StateId>,
    source: StateId,
}

/// A label's text changing: re-measured, re-laid, re-painted, re-announced.
fn text_class() -> DirtyClass {
    DirtyClass::MEASURE | DirtyClass::LAYOUT | DirtyClass::PAINT | DirtyClass::SEMANTICS
}

/// `LABELS` labels under one column, the first `bound` of them bound to one
/// state.
fn setup(bound: usize) -> Harness {
    let mut store = NodeStore::new();
    let mut states = StateStore::new();
    let mut bindings = BindingTable::new();
    let source = states.alloc(StateValue::Int(0));
    let mut labels: Vec<NodeId> = Vec::with_capacity(LABELS);
    {
        let mut cx = BuildCx::new(&mut store);
        cx.flex(FlexStyle::default(), |cx| {
            for _ in 0..LABELS {
                labels.push(cx.leaf(LeafStyle::default()).id());
            }
        });
    }
    for &label in &labels[..bound] {
        bindings.bind(source, label, text_class());
    }
    store.clear_dirty();
    Harness {
        store,
        states,
        bindings,
        computeds: ComputedStore::new(),
        projectors: SemanticProjector::new(),
        effects: EffectStore::new(),
        lists: VirtualLists::new(),
        changed: Vec::new(),
        source,
    }
}

/// One frame's write and flush. Returns the rounds the flush ran.
fn update(h: &mut Harness, value: i32) -> u32 {
    h.states.set(h.source, StateValue::Int(value));
    let rounds = settle_states(
        &mut h.store,
        &mut h.states,
        &mut h.bindings,
        &mut h.computeds,
        &mut h.projectors,
        &mut h.effects,
        &mut h.lists,
        &mut h.changed,
    )
    .expect("one write settles");
    h.store.clear_dirty();
    rounds
}

fn bench_state_update(c: &mut Criterion) {
    for (name, bound) in [
        ("state_update_1_binding", 1),
        ("state_update_1k_bindings", 1000),
    ] {
        let mut h = setup(bound);
        // Checked once: the write runs one round that walks exactly the bound
        // edges and marks each bound label.
        h.states.set(h.source, StateValue::Int(-1));
        let walked = h.bindings.counters().static_binding_eval();
        let rounds = settle_states(
            &mut h.store,
            &mut h.states,
            &mut h.bindings,
            &mut h.computeds,
            &mut h.projectors,
            &mut h.effects,
            &mut h.lists,
            &mut h.changed,
        )
        .expect("one write settles");
        let walked = h.bindings.counters().static_binding_eval() - walked;
        let dirty = h.store.dirty_count();
        assert_eq!(rounds, 1, "{name}: one write settles in one round");
        assert_eq!(walked, bound as u64, "{name}: every bound edge is walked");
        assert!(
            dirty > bound,
            "{name}: every bound label and the root are marked"
        );
        h.store.clear_dirty();
        eprintln!(
            "{name}: {LABELS} labels, a write to a state {bound} read settles in {rounds} round, \
             walking {walked} edges and marking {dirty} nodes"
        );

        let mut n = 0i32;
        c.bench_function(name, |b| {
            b.iter(|| {
                n = n.wrapping_add(1);
                black_box(update(black_box(&mut h), n))
            })
        });
    }
}

criterion_group!(benches, bench_state_update);
criterion_main!(benches);
