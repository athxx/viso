//! Reactive wiring: the facade's `FlushStateTransactions` phase drains this
//! frame's pending writes once and fans the same changed set through three
//! downstream reactors in order — derivations (memo-gated), direct bindings, and
//! effects. These tests drive that exact call order end to end.
//!
//! Like `reactive_flush`, they drive the stores directly (`StateStore` +
//! `ComputedStore` + `EffectStore` + `BindingTable` + `NodeStore`, all public
//! through `viso::ui`) rather than through the live scheduler, because the
//! `AppDriver` internals are private. The `flush` helper below is the exact
//! sequence the facade runs; asserting on it proves the wiring without needing
//! facade-private access. Correctness is asserted with dirty sets and re-run
//! counts, not pixels.

use std::cell::RefCell;
use std::rc::Rc;

use viso::ui::{
    BindingTable, BuildCx, Cleanup, ComputeCx, ComputedStore, DirtyClass, EffectStore, LeafStyle,
    NodeId, NodeStore, StateStore, StateValue,
};

/// Build a single leaf into `store` and return its id — a real
/// `NodeStore`-resident node so `mark_dirty`/`dirty` have somewhere to land.
fn sink_node(store: &mut NodeStore) -> NodeId {
    BuildCx::new(store).leaf(LeafStyle::default()).id()
}

/// Run the flush phase exactly as the facade's `FlushStateTransactions` does:
/// drain the pending write-set once, then fan it through derivations (memo-gated
/// wake), direct bindings, and effects, in that order. Returns
/// `(computeds_dirtied, edges_applied, effects_run)`.
fn flush(
    states: &mut StateStore,
    computeds: &mut ComputedStore,
    bindings: &BindingTable,
    effects: &mut EffectStore,
    store: &mut NodeStore,
) -> (u32, u32, u32) {
    let mut changed = Vec::new();
    states.take_pending(&mut changed);
    let dirtied = computeds.wake_computed(&changed, states, store);
    let applied = store.flush_state_transactions(&changed, bindings);
    let ran = effects.wake(&changed, states);
    (dirtied, applied, ran)
}

#[test]
fn state_write_drives_computed_reeval_that_dirties_only_on_change() {
    let mut store = NodeStore::new();
    let sink = sink_node(&mut store);

    let mut states = StateStore::new();
    let mut computeds = ComputedStore::new();
    let bindings = BindingTable::new();
    let mut effects = EffectStore::new();

    // A step derivation over `n`: 0 below 10, else 1. It drives the sink's PAINT.
    let n = states.alloc(StateValue::Int(2));
    let c = computeds.alloc(
        sink,
        DirtyClass::PAINT,
        move |cx: &mut ComputeCx<'_>| match cx.get(n) {
            Some(StateValue::Int(v)) if v >= 10 => StateValue::Int(1),
            _ => StateValue::Int(0),
        },
    );
    // Seed: first eval records `n` as a dependency (initial dirtying is irrelevant
    // here — the settled state is what the flush assertions build on).
    computeds.eval(c, &states);
    store.clear_dirty();

    // A same-step write flows through the flush: the derivation re-evaluates,
    // its value is unchanged, so nothing dirties.
    assert!(states.set(n, StateValue::Int(3)));
    let (dirtied, applied, ran) = flush(
        &mut states,
        &mut computeds,
        &bindings,
        &mut effects,
        &mut store,
    );
    assert_eq!((dirtied, applied, ran), (0, 0, 0));
    assert!(
        store.dirty(sink).is_empty(),
        "an unchanged derivation dirties nothing (the memo boundary)"
    );

    // A step-crossing write changes the derived value, so the flush dirties the
    // downstream node's bound class.
    assert!(states.set(n, StateValue::Int(42)));
    let (dirtied, _, _) = flush(
        &mut states,
        &mut computeds,
        &bindings,
        &mut effects,
        &mut store,
    );
    assert_eq!(dirtied, 1, "the changed derivation dirtied its node");
    assert!(
        store.dirty(sink).contains(DirtyClass::PAINT),
        "the derivation's node carries its bound class"
    );
}

#[test]
fn state_write_reruns_effect_scoped_to_same_state() {
    let mut store = NodeStore::new();
    let owner = sink_node(&mut store);

    let mut states = StateStore::new();
    let mut computeds = ComputedStore::new();
    let bindings = BindingTable::new();
    let mut effects = EffectStore::new();

    // One state read by both a derivation and an effect. The effect logs its
    // lifecycle so we can see the flush re-run it.
    let n = states.alloc(StateValue::Int(0));
    let c = computeds.alloc(owner, DirtyClass::PAINT, move |cx: &mut ComputeCx<'_>| {
        cx.get(n).unwrap_or(StateValue::Int(0))
    });
    computeds.eval(c, &states);

    let log: Rc<RefCell<Vec<&'static str>>> = Rc::new(RefCell::new(Vec::new()));
    let body_log = Rc::clone(&log);
    let e = effects.alloc(owner, move |cx: &mut ComputeCx<'_>| {
        cx.get(n);
        body_log.borrow_mut().push("run");
        let cl = Rc::clone(&body_log);
        let cleanup: Cleanup = Box::new(move || cl.borrow_mut().push("cleanup"));
        Some(cleanup)
    });
    assert!(effects.run(e, &states));
    assert_eq!(*log.borrow(), ["run"], "first run, no prior cleanup");
    store.clear_dirty();

    // A write flows through the flush: the derivation changes (dirtying its
    // node) and the effect restarts (cleanup then re-run).
    assert!(states.set(n, StateValue::Int(5)));
    let (dirtied, _, ran) = flush(
        &mut states,
        &mut computeds,
        &bindings,
        &mut effects,
        &mut store,
    );
    assert_eq!(dirtied, 1, "derivation changed and dirtied its node");
    assert_eq!(ran, 1, "the effect scoped to the same state re-ran");
    assert_eq!(
        *log.borrow(),
        ["run", "cleanup", "run"],
        "dependency restart runs the prior cleanup before the body"
    );
    assert!(store.dirty(owner).contains(DirtyClass::PAINT));
}

#[test]
fn unrelated_write_leaves_derivation_and_effect_untouched() {
    let mut store = NodeStore::new();
    let owner = sink_node(&mut store);

    let mut states = StateStore::new();
    let mut computeds = ComputedStore::new();
    let bindings = BindingTable::new();
    let mut effects = EffectStore::new();

    let watched = states.alloc(StateValue::Int(0));
    let other = states.alloc(StateValue::Int(0));

    let c = computeds.alloc(owner, DirtyClass::PAINT, move |cx: &mut ComputeCx<'_>| {
        cx.get(watched).unwrap_or(StateValue::Int(0))
    });
    computeds.eval(c, &states);

    let runs = Rc::new(RefCell::new(0u32));
    let runs_body = Rc::clone(&runs);
    let e = effects.alloc(owner, move |cx: &mut ComputeCx<'_>| {
        cx.get(watched);
        *runs_body.borrow_mut() += 1;
        None
    });
    effects.run(e, &states);
    store.clear_dirty();

    // Writing a state that neither the derivation nor the effect reads wakes
    // nothing across the whole flush.
    assert!(states.set(other, StateValue::Int(9)));
    let (dirtied, applied, ran) = flush(
        &mut states,
        &mut computeds,
        &bindings,
        &mut effects,
        &mut store,
    );
    assert_eq!(
        (dirtied, applied, ran),
        (0, 0, 0),
        "unrelated write is inert"
    );
    assert!(store.dirty(owner).is_empty(), "no node dirtied");
    assert_eq!(*runs.borrow(), 1, "the effect did not re-run");
}
