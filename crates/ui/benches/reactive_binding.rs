//! The "state invalidation" benchmark category, specialized to the three reactive
//! dependency paths section 10.3 requires kept measurable and distinct: a fully
//! *static* (compiled) binding path, a fully *dynamic* (runtime-registered) path, and
//! a *mixed* path. Each is one changed state fanning out to `FANOUT` bound nodes,
//! flushed through the public `flush_state_transactions` seam — the same call the
//! frame loop runs.
//!
//! Two things are measured, both through the public API (a bench is an external crate
//! and cannot touch crate internals):
//!
//! 1. A startup assertion that the *static* path is exactly that — a compiled edge
//!    walk that records `static_binding_eval` and never touches the dynamic counters.
//!    A typed binding silently drifting onto the dynamic fallback (a `dynamic_*`
//!    counter moving off zero on the static harness) fails the bench binary
//!    immediately, the section 10.3 "strict typed example does zero dynamic fallback"
//!    guard, mirroring `large_list.rs`'s steady-state assertion.
//! 2. The per-flush cost of each path, so a regression that makes the static fast
//!    path pay dynamic-path cost — or the reverse — shows up as a timing delta
//!    between the three functions rather than hiding in an aggregate.
//!
//! Run release (`cargo bench -p viso-ui`); criterion defaults to a release profile.
//! Debug timing is not a performance result.

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use viso_ui::{
    BindingTable, BuildCx, DirtyClass, FlexStyle, LeafStyle, NodeId, NodeStore, StateId,
    StateStore, StateValue,
};

/// One state fans out to this many bound nodes — a fan-out wide enough that the
/// per-edge walk dominates the per-state bookkeeping, so the three paths' costs are
/// comparable edge-for-edge.
const FANOUT: usize = 256;

/// The text-content dirty class every edge carries, so the three paths differ only in
/// static-vs-dynamic registration, not in the invalidation work each edge does.
fn text_class() -> DirtyClass {
    DirtyClass::MEASURE | DirtyClass::LAYOUT | DirtyClass::PAINT | DirtyClass::SEMANTICS
}

/// A flushable harness: the node store the edges dirty, the changed-state batch, and
/// the compiled binding table wired for one of the three paths.
struct Harness {
    store: NodeStore,
    bindings: BindingTable,
    changed: Vec<StateId>,
}

/// How the `FANOUT` edges of the single changed state are registered.
#[derive(Clone, Copy)]
enum Path {
    /// Every edge compiled (`bind`) — the static fast path.
    Static,
    /// Every edge runtime-registered (`bind_dynamic`) — the fallback path.
    Dynamic,
    /// Half compiled, half runtime — the mixed path.
    Mixed,
}

/// Build a harness: allocate one source state and `FANOUT` leaf nodes, then register
/// one edge per node under `path`. The nodes are real allocations so `mark_dirty`
/// during flush hits live side-storage exactly as it would in a frame.
fn setup(path: Path) -> Harness {
    let mut store = NodeStore::new();
    let mut states = StateStore::new();
    let mut bindings = BindingTable::new();

    let source = states.alloc(StateValue::Int(0));
    let class = text_class();

    // Mount `FANOUT` real leaves under one flex root and collect their ids. Real
    // nodes give `mark_dirty` live side-storage to touch during flush, exactly as a
    // frame would; the container is just a mounting parent.
    let mut nodes: Vec<NodeId> = Vec::with_capacity(FANOUT);
    {
        let mut cx = BuildCx::new(&mut store);
        cx.flex(FlexStyle::default(), |cx| {
            for _ in 0..FANOUT {
                nodes.push(cx.leaf(LeafStyle::default()).id());
            }
        });
    }

    for (i, &node) in nodes.iter().enumerate() {
        let dynamic = match path {
            Path::Static => false,
            Path::Dynamic => true,
            Path::Mixed => i % 2 == 1,
        };
        if dynamic {
            bindings.bind_dynamic(source, node, class);
        } else {
            bindings.bind(source, node, class);
        }
    }

    Harness {
        store,
        bindings,
        changed: vec![source],
    }
}

/// One flush: fan the changed state through its edges. Returns the applied edge
/// count. The `dirty` set is cleared first so each flush re-marks from a clean slate.
///
/// Counters are deliberately *not* reset here. The two evaluation counters
/// (`static_binding_eval` / `dynamic_binding_eval`) are flush-time and accumulate
/// across iterations — criterion measures wall time, not counts, so accumulation is
/// harmless. The two registration counters (`dynamic_subscribe` /
/// `dynamic_fallback_nodes`) are recorded once at bind time in `setup`; resetting
/// here would erase them before the startup assertion could read them.
fn flush(h: &mut Harness) -> u32 {
    h.store.clear_dirty();
    h.store.flush_state_transactions(&h.changed, &h.bindings)
}

/// The section 10.3 strict-typed guard, checked before benchmarking: the static path
/// walks exactly `FANOUT` *static* edges and never moves a dynamic counter — a typed
/// binding does zero dynamic fallback. The dynamic and mixed paths are checked as the
/// negative controls, so the assertion is not trivially true for an unwired table.
fn assert_static_path_does_zero_dynamic_fallback() {
    let mut s = setup(Path::Static);
    let applied = flush(&mut s);
    let c = s.bindings.counters();
    assert_eq!(applied, FANOUT as u32, "static: every edge applied");
    assert_eq!(
        c.static_binding_eval(),
        FANOUT as u64,
        "static: every edge walked the compiled path"
    );
    assert_eq!(
        c.dynamic_binding_eval(),
        0,
        "static: a typed binding walks no dynamic edge"
    );
    assert_eq!(
        c.dynamic_subscribe(),
        0,
        "static: a typed binding registers no dynamic subscription"
    );
    assert_eq!(
        c.dynamic_fallback_nodes(),
        0,
        "static: a typed binding never falls back to a dynamic node (strict guard)"
    );

    // Negative control: the fully dynamic path is the mirror image — all dynamic, no
    // static — so the static assertion above is meaningfully path-dependent.
    let mut d = setup(Path::Dynamic);
    let applied = flush(&mut d);
    let c = d.bindings.counters();
    assert_eq!(applied, FANOUT as u32, "dynamic: every edge applied");
    assert_eq!(
        c.static_binding_eval(),
        0,
        "dynamic: no compiled edge is walked"
    );
    assert_eq!(
        c.dynamic_binding_eval(),
        FANOUT as u64,
        "dynamic: every edge walked the dynamic path"
    );
    assert_eq!(
        c.dynamic_fallback_nodes(),
        FANOUT as u64,
        "dynamic: every node is a distinct fallback node"
    );

    // Mixed: exactly the static/dynamic split the harness registered.
    let half = FANOUT as u64 / 2;
    let mut m = setup(Path::Mixed);
    let applied = flush(&mut m);
    let c = m.bindings.counters();
    assert_eq!(applied, FANOUT as u32, "mixed: every edge applied");
    assert_eq!(
        c.static_binding_eval(),
        half,
        "mixed: half the edges walked the compiled path"
    );
    assert_eq!(
        c.dynamic_binding_eval(),
        half,
        "mixed: half the edges walked the dynamic path"
    );
    assert_eq!(
        c.dynamic_fallback_nodes(),
        half,
        "mixed: exactly the dynamic half are fallback nodes"
    );
}

fn bench_reactive_binding(c: &mut Criterion) {
    assert_static_path_does_zero_dynamic_fallback();

    for (name, path) in [
        ("flush_static", Path::Static),
        ("flush_mixed", Path::Mixed),
        ("flush_dynamic", Path::Dynamic),
    ] {
        c.bench_function(name, |b| {
            let mut h = setup(path);
            b.iter(|| black_box(flush(black_box(&mut h))));
        });
    }
}

criterion_group!(benches, bench_reactive_binding);
criterion_main!(benches);
