//! Microbench skeleton for the `Scroll` viewport widget: the cost of one
//! authoring pass (`Scroll::build`), one `layout`, and one `paint_tree`
//! lowering.
//!
//! This is the section 71 microbench slot for the first Tier 3 (layout
//! structure) widget — a scroll viewport — and copies the `view_build.rs`
//! template. It exercises the real `build` -> `layout` -> `paint_tree` path so a
//! regression is measurable; the baseline numbers are recorded in a later slice
//! (per the plan: establish the framework first).
//!
//! It drives only `viso-ui` — `build`/`layout`/`paint_tree` all live there — so
//! the bench needs no facade or render dev-dependency and adds no dependency
//! edge.
//!
//! Run release (`CARGO_TARGET_DIR=/tmp/rust_tmp cargo bench -p viso-widgets`);
//! criterion defaults to a release profile. Debug timing is not a perf result
//! (AGENTS section 36).

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use viso_ui::{Axis, BoxStyle, BuildCx, Component, LeafStyle, NodeStore, Rect, Size, paint_tree};
use viso_widgets::{ScrollViewStyle, scroll};

const W: f32 = 200.0;
const H: f32 = 300.0;

/// Author a `Scroll` viewport over a single tall content child (so it overflows
/// and is scrollable) into a fresh store and return the store plus its root —
/// the input every phase below runs on.
fn build_scene() -> (NodeStore, viso_ui::NodeId) {
    let region = scroll(ScrollViewStyle {
        axis: Axis::Column,
        size: Size::fixed(W, H),
        ..Default::default()
    })
    .content(|cx| {
        cx.leaf(LeafStyle {
            size: Size::fixed(W, H * 4.0),
            style: BoxStyle::NONE,
        });
    });

    let mut store = NodeStore::new();
    let root = {
        let mut cx = BuildCx::new(&mut store);
        region.build(&mut cx);
        cx.root().expect("scroll declares a root")
    };
    (store, root)
}

fn surface() -> Rect {
    Rect {
        x: 0.0,
        y: 0.0,
        w: W,
        h: H,
    }
}

fn bench_scroll(c: &mut Criterion) {
    // build: author the Scroll subtree into a fresh NodeStore.
    c.bench_function("scroll/build", |b| {
        b.iter(|| {
            let mut store = NodeStore::new();
            let mut cx = BuildCx::new(&mut store);
            scroll(ScrollViewStyle {
                axis: Axis::Column,
                size: Size::fixed(W, H),
                ..Default::default()
            })
            .content(|cx| {
                cx.leaf(LeafStyle {
                    size: Size::fixed(W, H * 4.0),
                    style: BoxStyle::NONE,
                });
            })
            .build(&mut cx);
            black_box(&store);
        });
    });

    // layout: lay the built viewport out into the surface rect.
    c.bench_function("scroll/layout", |b| {
        let (mut store, root) = build_scene();
        let rect = surface();
        let mut scratch = Vec::new();
        b.iter(|| {
            store.layout(root, rect, &mut scratch);
            black_box(&store);
        });
    });

    // paint_tree: lower the laid-out viewport into the reused primitive buffer.
    c.bench_function("scroll/paint_tree", |b| {
        let (mut store, root) = build_scene();
        store.layout(root, surface(), &mut Vec::new());
        let mut primitives = Vec::new();
        // Warm the buffer to steady capacity so the timed loop does not grow it.
        paint_tree(&store, root, &mut primitives);
        b.iter(|| {
            primitives.clear();
            paint_tree(&store, root, &mut primitives);
            black_box(&primitives);
        });
    });
}

criterion_group!(benches, bench_scroll);
criterion_main!(benches);
