//! The `animation` benchmark category (AGENTS section 36): the per-frame cost of
//! [`AnimationRegistry::tick`], the transform-only animation hot path a sheet
//! drawer (and every future translate animation) rides on.
//!
//! Section 28 lists animation ticks as a hot path that must carry no hidden
//! allocation, and section 8.7 fixes the contract: a tick advances each live
//! animation by writing an interpolated world-space offset through
//! `NodeStore::set_translate` — a `TRANSFORM | HIT_TEST | PAINT` write that never
//! re-measures or re-lays out. This bench measures that steady per-frame cost at
//! one and at many concurrent animations, so a regression (an allocation, a map
//! insert, a per-node lookup) is measurable rather than assumed.
//!
//! Each bench pre-builds a laid-out store of N leaves and starts one animation
//! per leaf, then times a single `tick`. Criterion runs the closure hundreds of
//! millions of times, so the tick advances by a zero delta (see [`DELTA`]): that
//! keeps every animation permanently live — the timed loop never drains to an
//! empty registry — while exercising the identical steady per-tick path (reverse
//! walk, liveness check, eased interpolation, `set_translate` write). The
//! registry is a flat `Vec` walked once per tick; these numbers are the floor a
//! still-animating frame pays.
//!
//! Run release (`CARGO_TARGET_DIR=/tmp/rust_tmp cargo bench -p viso-ui`);
//! criterion defaults to a release profile. Debug timing is not a perf result.

use std::hint::black_box;
use std::time::Duration;

use criterion::{Criterion, criterion_group, criterion_main};
use viso_ui::{
    AnimationRegistry, BuildCx, Easing, LeafStyle, NodeId, NodeStore, Rect, Size, TranslateAnim,
    Vec2,
};

/// The surface the leaves lay out against — large enough to place a column of
/// leaves without clipping affecting the (transform-only) tick.
const SURFACE: Rect = Rect {
    x: 0.0,
    y: 0.0,
    w: 400.0,
    h: 100_000.0,
};

/// The per-tick delta. Criterion runs each closure hundreds of millions of
/// times, so any positive delta would eventually retire every animation and
/// leave the timed loop measuring an empty registry — understating the real
/// cost. A zero delta keeps every animation permanently live while exercising the
/// identical steady path: the reverse walk, the liveness check, the eased
/// `current()` interpolation, and the `set_translate` write (dirty marks + store
/// write). Only the never-taken "finished" branch differs, and that one-shot
/// completion is deliberately outside this steady-cost category.
const DELTA: Duration = Duration::ZERO;

/// The animation duration. Any positive value works since [`DELTA`] never
/// advances `elapsed` past it; a long value documents the intent (a real slide).
const LONG: Duration = Duration::from_secs(3600);

/// Build a store of `n` stacked leaves laid out over the surface, and return it
/// with every leaf id. Each leaf is a fixed 40x40 box in a column, so every node
/// has a real laid-out `bounds` for `set_translate` to shift.
fn build_leaves(n: usize) -> (NodeStore, Vec<NodeId>) {
    let mut store = NodeStore::new();
    let mut ids = Vec::with_capacity(n);
    let root = {
        let mut cx = BuildCx::new(&mut store);
        for _ in 0..n {
            let h = cx.leaf(LeafStyle {
                size: Size::fixed(40.0, 40.0),
                ..Default::default()
            });
            ids.push(h.id());
        }
        cx.root().expect("a leaf declares a root")
    };
    let mut scratch = Vec::new();
    store.layout(root, SURFACE, &mut scratch);
    (store, ids)
}

/// A registry with one slide started per id. Paired with a zero per-tick
/// [`DELTA`], every animation stays live across the whole timed loop.
fn registry_for(ids: &[NodeId]) -> AnimationRegistry {
    let mut reg = AnimationRegistry::new();
    for &id in ids {
        reg.start(TranslateAnim::new(
            id,
            Vec2::ZERO,
            Vec2 { x: 0.0, y: -1000.0 },
            LONG,
            Easing::EaseOut,
        ));
    }
    reg
}

fn bench_tick(c: &mut Criterion) {
    // Single animation: the cost of one node's per-frame advance + world-space
    // write — the floor a lone sliding sheet pays each frame.
    {
        let (mut store, ids) = build_leaves(1);
        let mut reg = registry_for(&ids);
        assert_eq!(reg.len(), 1);
        c.bench_function("animation_tick/1", |b| {
            b.iter(|| {
                reg.tick(&mut store, DELTA);
                black_box(&store);
            });
        });
    }

    // Many concurrent animations: the flat-`Vec` pass cost at scale — the ceiling
    // a screenful of simultaneously animating nodes pays. A regression to a
    // per-node map or a per-tick allocation shows here.
    for &n in &[64usize, 1024] {
        let (mut store, ids) = build_leaves(n);
        let mut reg = registry_for(&ids);
        assert_eq!(reg.len(), n);
        c.bench_function(&format!("animation_tick/{n}"), |b| {
            b.iter(|| {
                reg.tick(&mut store, DELTA);
                black_box(&store);
            });
        });
    }
}

criterion_group!(benches, bench_tick);
criterion_main!(benches);
