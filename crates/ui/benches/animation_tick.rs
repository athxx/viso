//! The `animation` benchmark category: the per-frame cost of
//! [`NodeStore::tick_transitions`] advancing translate animations
//! ([`NodeStore::animate`]), the hot path a sliding sheet (and every animation
//! a handler starts) rides on.
//!
//! Animation ticks are a hot path that must carry no hidden allocation: a tick
//! advances each live animation by writing the value its node presents — a
//! `TRANSFORM`-only write for a translate that never re-measures or re-lays
//! out. This bench measures that steady per-frame cost at one and at many
//! concurrent animations, so a regression (an allocation, a map insert, a
//! per-node lookup) is measurable rather than assumed.
//!
//! Each bench pre-builds a laid-out store of N leaves and starts one animation
//! per leaf, then times a single tick. Criterion runs the closure hundreds of
//! millions of times, so the tick advances by a zero delta (see [`DELTA`]): that
//! keeps every animation permanently live — the timed loop never drains to an
//! empty list — while exercising the identical steady per-tick path.
//!
//! Run release (`CARGO_TARGET_DIR=/tmp/rust_tmp cargo bench -p viso-ui`);
//! criterion defaults to a release profile. Debug timing is not a perf result.

use std::hint::black_box;
use std::time::Duration;

use criterion::{Criterion, criterion_group, criterion_main};
use viso_ui::{
    BuildCx, Easing, LeafStyle, LookValue, NodeId, NodeStore, Rect, Sizing, Timing, Vec2,
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
/// leave the timed loop measuring an empty list — understating the real cost.
/// A zero delta keeps every animation permanently live while exercising the
/// identical steady path: the reverse walk, the liveness check, the eased
/// interpolation and the translate write. Only the never-taken "arrived"
/// branch differs, and that one-shot completion is outside this steady-cost
/// category.
const DELTA: Duration = Duration::ZERO;

/// The animation duration. Any positive value works since [`DELTA`] never
/// advances past it; a long value documents the intent (a real slide).
const LONG: Duration = Duration::from_secs(3600);

/// Build a store of `n` stacked leaves laid out over the surface, each sliding
/// up by 1000 dp over [`LONG`].
fn sliding_leaves(n: usize) -> NodeStore {
    let mut store = NodeStore::new();
    let mut ids: Vec<NodeId> = Vec::with_capacity(n);
    let root = {
        let mut cx = BuildCx::new(&mut store);
        for _ in 0..n {
            let h = cx.leaf(LeafStyle {
                size: Sizing::fixed(40.0, 40.0),
                ..Default::default()
            });
            ids.push(h.id());
        }
        cx.root().expect("a leaf declares a root")
    };
    let mut scratch = Vec::new();
    store.layout(root, SURFACE, &mut scratch);
    let slide = Timing::eased(LONG, Easing::EASE_OUT);
    for id in ids {
        let to = LookValue::Translate(Vec2 { x: 0.0, y: -1000.0 });
        store.animate(id, to, slide, None).expect("a live leaf");
    }
    store
}

fn bench_tick(c: &mut Criterion) {
    // One animation: the floor a lone sliding sheet pays each frame. Many: the
    // flat pass at scale — the ceiling a screenful of simultaneously animating
    // nodes pays; a per-node map or a per-tick allocation shows here.
    for &n in &[1usize, 64, 1024] {
        let mut store = sliding_leaves(n);
        assert!(store.is_transitioning());
        c.bench_function(&format!("animation_tick/{n}"), |b| {
            b.iter(|| {
                store.tick_transitions(DELTA);
                black_box(&store);
            });
        });
    }
}

criterion_group!(benches, bench_tick);
criterion_main!(benches);
