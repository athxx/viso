//! Two architecture contracts the Inspector slice must *measure*, not assert:
//!
//! 1. **Identity performance contract (architecture 10.4.13).** The runtime win
//!    is "hash / probe / pointer-chasing -> direct indexed access": node
//!    liveness and per-node hot data resolve by a dense array index keyed on the
//!    generational `NodeId`, never a string or a stable-symbol hash lookup. This
//!    bench contrasts the dense-index path against a `HashMap<NodeId, _>`
//!    baseline over 1M lookups so the gap is a number, not a claim — the exact
//!    comparison 10.4.13 names ("stable Symbol HashMap lookup vs dense array
//!    lookup").
//!
//! 2. **The Inspector surfaces are cold (architecture 60 / 7.2).** 9.A4 landed
//!    `inspect_tree` / `paint_ranges` / `derive_semantics` / `snapshot_ui` as
//!    read-only `&NodeStore` readouts that mutate nothing. Architecture 35 / 7.3
//!    forbid claiming "cold / doesn't tax the frame" without a measurement, so a
//!    startup assertion pins the steady-frame counter (the `paint_tree` primitive
//!    count) as *unchanged* whether or not the inspect surfaces run between
//!    frames, and a `steady_frame` / `steady_frame_with_inspect_between` pair
//!    times the hot path with and without a snapshot appended.
//!
//! Run release (`cargo bench -p viso-ui`); criterion defaults to a release
//! profile. Debug timing is not a performance result.

use std::collections::HashMap;
use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use viso_render::{FrameStats, InspectBatches, Primitive, Rect, Rgba};
use viso_ui::{
    Align, Axis, BoxStyle, BuildCx, FlexStyle, Inset, Justify, LeafStyle, Length, NodeArena,
    NodeId, NodeStore, Size, paint_ranges, paint_tree, snapshot_ui,
};

// ---------------------------------------------------------------------------
// Part 1 — Identity contract: dense index vs HashMap baseline.
// ---------------------------------------------------------------------------

/// Nodes in the arena the identity benches probe. A bushy tree (fanout 16) is
/// closer to real UI than a linked list, matching `node_arena`'s substrate.
const NODE_COUNT: usize = 100_000;
const FANOUT: usize = 16;
/// Total lookups each identity bench performs per iteration — one million probes
/// over the 100k-node population, the 10.4.13 "…_1m" workload size.
const LOOKUPS: usize = 1_000_000;

/// Allocate `NODE_COUNT` nodes into a fanout-`FANOUT` tree (node `i`'s parent is
/// `(i - 1) / FANOUT`), then free a sparse fraction so the arena holds a mix of
/// live and freed slots — a `is_live` / `contains_key` probe must reject stale
/// ids, not just confirm live ones. Returns the arena and every id ever handed
/// out in allocation order (including the now-freed ones), so the probe set
/// exercises both outcomes.
fn build_arena() -> (NodeArena, Vec<NodeId>) {
    let mut arena = NodeArena::new();
    let mut ids = Vec::with_capacity(NODE_COUNT);
    let root = arena.alloc();
    ids.push(root);
    for i in 1..NODE_COUNT {
        let child = arena.alloc();
        let parent = ids[(i - 1) / FANOUT];
        arena.append_child(parent, child);
        ids.push(child);
    }
    // Free every 37th leaf-ish node (skip the root). Detach first so the arena's
    // free path is the real one; the id stays in `ids` and is now stale, so a
    // later `is_live(id)` on it must return false (generation mismatch).
    for i in (1..NODE_COUNT).step_by(37) {
        let id = ids[i];
        arena.detach_child(id);
        arena.free(id);
    }
    (arena, ids)
}

/// A fixed probe order over the id population: `LOOKUPS` ids drawn by cycling
/// through `ids` (so ~1M probes over 100k distinct ids, live and stale mixed).
/// Precomputed once so neither the dense nor the HashMap timing pays for it.
fn probe_ids(ids: &[NodeId]) -> Vec<NodeId> {
    (0..LOOKUPS).map(|i| ids[i % ids.len()]).collect()
}

/// The dense identity path: `NodeArena::is_live` is an index into the dense
/// generation array plus a generation compare — no hash, no probe. Sum the live
/// hits so the loop is not optimized away.
fn identity_is_live_dense(c: &mut Criterion) {
    let (arena, ids) = build_arena();
    let probes = probe_ids(&ids);

    c.bench_function("identity_node_generation_check_1m", |b| {
        b.iter(|| {
            let mut live = 0u64;
            for &id in &probes {
                if arena.is_live(black_box(id)) {
                    live += 1;
                }
            }
            black_box(live)
        });
    });
}

/// The HashMap baseline for the same liveness question: a `HashMap<NodeId, ()>`
/// holding exactly the live ids, probed with `contains_key`. This is the
/// "stable-symbol hash lookup" 10.4.13 measures the dense path against. The map
/// is built once at startup — the per-iteration cost is pure lookup, matching
/// the dense bench.
fn identity_is_live_hashmap(c: &mut Criterion) {
    let (arena, ids) = build_arena();
    let probes = probe_ids(&ids);

    // Build the live set once. Startup allocation, not per-iteration.
    let live_set: HashMap<NodeId, ()> = ids
        .iter()
        .filter(|&&id| arena.is_live(id))
        .map(|&id| (id, ()))
        .collect();

    c.bench_function("identity_generation_check_hashmap_1m", |b| {
        b.iter(|| {
            let mut live = 0u64;
            for &id in &probes {
                if live_set.contains_key(black_box(&id)) {
                    live += 1;
                }
            }
            black_box(live)
        });
    });
}

// ---------------------------------------------------------------------------
// Part 2 — The Inspector surfaces are cold.
// ---------------------------------------------------------------------------

const W: f32 = 200.0;
const H: f32 = 120.0;

const DARK: Rgba = Rgba {
    r: 0.15,
    g: 0.16,
    b: 0.20,
    a: 1.0,
};
const RED: Rgba = Rgba {
    r: 0.9,
    g: 0.1,
    b: 0.1,
    a: 1.0,
};
const GREEN: Rgba = Rgba {
    r: 0.1,
    g: 0.7,
    b: 0.3,
    a: 1.0,
};

/// The same padded Row + two colored leaves the `inspect_snapshot` facade test
/// builds — a real `BuildCx`-authored retained tree that lays out and paints, so
/// the steady-frame hot path (`paint_tree`) and every cold inspect surface run
/// against genuine node data rather than a fabricated store.
fn build(store: &mut NodeStore) -> NodeId {
    let mut cx = BuildCx::new(store);
    cx.flex(
        FlexStyle {
            axis: Axis::Row,
            gap: 8.0,
            padding: Inset::all(12.0),
            align: Align::Center,
            justify: Justify::Start,
            size: Size::fill(),
            style: BoxStyle::solid(DARK),
        },
        |cx| {
            cx.leaf(LeafStyle {
                size: Size::fixed(48.0, 40.0),
                style: BoxStyle::solid(RED).with_radius(8.0),
            });
            cx.leaf(LeafStyle {
                size: Size {
                    width: Length::fill(),
                    height: Length::Fixed(56.0),
                },
                style: BoxStyle::solid(GREEN).with_radius(4.0),
            });
        },
    );
    cx.root().expect("scene has a root")
}

/// Build the scene and lay it out once so `paint_tree` and the inspect surfaces
/// see a settled tree. Returns the store and root.
fn settled_scene() -> (NodeStore, NodeId) {
    let mut store = NodeStore::new();
    let root = build(&mut store);
    let surface = Rect {
        x: 0.0,
        y: 0.0,
        w: W,
        h: H,
    };
    let mut scratch = Vec::new();
    store.layout(root, surface, &mut scratch);
    (store, root)
}

/// The steady-frame counter: how many primitives one `paint_tree` walk emits.
/// This is the instance-count source the renderer's `FrameStats` derives, so a
/// change here is a change to what the frame draws.
fn paint_count(store: &NodeStore, root: NodeId, out: &mut Vec<Primitive>) -> usize {
    out.clear();
    paint_tree(store, root, out);
    out.len()
}

/// Startup hard assertion (architecture 66 headless-assertion convention): the
/// steady-frame counter is deterministic across frames, and running every cold
/// inspect surface between two frames does not change it. Panicking here fails
/// the bench binary, so a regression that lets inspection perturb the frame is
/// caught before any timing runs.
fn assert_inspect_is_cold() {
    let (store, root) = settled_scene();
    let mut prims = Vec::new();

    // Two consecutive frames produce the identical primitive count: the steady
    // path is deterministic, giving us a stable baseline counter `n`.
    let n = paint_count(&store, root, &mut prims);
    assert_eq!(
        paint_count(&store, root, &mut prims),
        n,
        "two consecutive steady frames must emit the same primitive count"
    );

    // Run every cold inspect surface. They borrow `&store` and mutate nothing;
    // `snapshot_ui` folds `inspect_tree` + `paint_ranges` + `derive_semantics`
    // together with the renderer's batch/stats snapshot.
    let _tree = store.inspect_tree(root);
    let mut range_prims = Vec::new();
    let ranges = paint_ranges(&store, root, &mut range_prims);
    let _sem = store.derive_semantics(root);
    let snap = snapshot_ui(
        &store,
        root,
        InspectBatches::default(),
        FrameStats {
            draw_calls: 0,
            instances: 0,
        },
    );
    black_box((&ranges, &snap));

    // The next frame's counter is unchanged: inspection did not perturb the
    // store or what the frame paints.
    assert_eq!(
        paint_count(&store, root, &mut prims),
        n,
        "a frame after running every inspect surface must emit the same primitive count \
         (inspection must not mutate the steady-frame counter)"
    );

    // Counter-consistency sentinel: the cold `paint_ranges` twin reaches the
    // exact same primitive count as the hot `paint_tree` walk (the property 9.A4
    // proved), so the two never disagree on the instance count.
    assert_eq!(
        ranges.total_primitives(),
        n,
        "paint_ranges must reach the same primitive count as paint_tree"
    );

    // The snapshot's paint ranges agree with the hot count too.
    assert_eq!(
        snap.paint_ranges.total_primitives(),
        n,
        "snapshot paint ranges must reach the same primitive count as paint_tree"
    );
}

/// Time the steady frame's hot path alone: one `paint_tree` walk into a reused
/// buffer, the exact work a redraw does. Baselines the cost the inspect surfaces
/// must stay off of.
fn steady_frame(c: &mut Criterion) {
    let (store, root) = settled_scene();
    let mut prims = Vec::new();
    // Warm the buffer capacity, then pin it: the steady walk must not grow it.
    let _ = paint_count(&store, root, &mut prims);
    let cap = prims.capacity();

    c.bench_function("steady_frame", |b| {
        b.iter(|| {
            black_box(paint_count(&store, black_box(root), &mut prims));
        });
    });

    assert_eq!(
        prims.capacity(),
        cap,
        "the steady paint buffer must not grow across frames (hidden allocation)"
    );
}

/// Time the hot path with a full cold snapshot appended after it: the frame's
/// `paint_tree` plus one `snapshot_ui`. The delta against `steady_frame` is the
/// *additive* cost of inspection — it must not fold into or change the hot walk,
/// proving inspection is a separable cold surface rather than a frame tax.
fn steady_frame_with_inspect_between(c: &mut Criterion) {
    let (store, root) = settled_scene();
    let mut prims = Vec::new();

    c.bench_function("steady_frame_with_inspect_between", |b| {
        b.iter(|| {
            black_box(paint_count(&store, black_box(root), &mut prims));
            let snap = snapshot_ui(
                &store,
                black_box(root),
                InspectBatches::default(),
                FrameStats {
                    draw_calls: 0,
                    instances: 0,
                },
            );
            black_box(snap);
        });
    });
}

fn identity(c: &mut Criterion) {
    identity_is_live_dense(c);
    identity_is_live_hashmap(c);
}

fn inspect_cold(c: &mut Criterion) {
    assert_inspect_is_cold();
    steady_frame(c);
    steady_frame_with_inspect_between(c);
}

criterion_group!(benches, identity, inspect_cold);
criterion_main!(benches);
