//! Microbench skeleton for the `View` container widget: the cost of one
//! authoring pass (`View::build`), one `layout`, and one `paint_tree` lowering.
//!
//! This is the section 71 microbench slot for the first Tier 1 widget and the
//! template later Tier 1 controls copy. It is deliberately a skeleton this
//! slice — it exercises the real `build` -> `layout` -> `paint_tree` path so a
//! regression is measurable, but the baseline numbers are recorded in a later
//! slice (per the Slice 2 plan: establish the framework first, don't go deep).
//!
//! It drives only `viso-ui` — `build`/`layout`/`paint_tree` all live there — so
//! the bench needs no facade or render dev-dependency and adds no dependency
//! edge. The paint output buffer's element type is inferred from `paint_tree`,
//! so `viso_render::Primitive` never has to be named.
//!
//! Run release (`CARGO_TARGET_DIR=/tmp/rust_tmp cargo bench -p viso-widgets`);
//! criterion defaults to a release profile. Debug timing is not a perf result
//! (AGENTS section 36).

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use viso_ui::{
    Axis, BoxStyle, BuildCx, Component, LeafStyle, NodeStore, Rect, Rgba, Size, paint_tree,
};
use viso_widgets::{ViewStyle, view};

const W: f32 = 160.0;
const H: f32 = 96.0;

const DARK: Rgba = Rgba {
    r: 0.1,
    g: 0.1,
    b: 0.12,
    a: 1.0,
};
const TEAL: Rgba = Rgba {
    r: 0.1,
    g: 0.5,
    b: 0.5,
    a: 1.0,
};

/// Author a small `View` row with two solid child boxes into a fresh store and
/// return the store plus its root — the input every phase below runs on.
fn build_scene() -> (NodeStore, viso_ui::NodeId) {
    let container = view(ViewStyle {
        axis: Axis::Row,
        gap: 12.0,
        size: Size::fill(),
        background: BoxStyle::solid(DARK),
        ..Default::default()
    })
    .children(|cx| {
        cx.leaf(LeafStyle {
            size: Size::fixed(48.0, 48.0),
            style: BoxStyle::solid(TEAL),
        });
        cx.leaf(LeafStyle {
            size: Size::fixed(48.0, 48.0),
            style: BoxStyle::solid(TEAL),
        });
    });

    let mut store = NodeStore::new();
    let root = {
        let mut cx = BuildCx::new(&mut store);
        container.build(&mut cx);
        cx.root().expect("view declares a root")
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

fn bench_view(c: &mut Criterion) {
    // build: author the View subtree into a fresh NodeStore.
    c.bench_function("view/build", |b| {
        let container = || {
            view(ViewStyle {
                axis: Axis::Row,
                gap: 12.0,
                background: BoxStyle::solid(DARK),
                ..Default::default()
            })
            .children(|cx| {
                cx.leaf(LeafStyle {
                    size: Size::fixed(48.0, 48.0),
                    style: BoxStyle::solid(TEAL),
                });
                cx.leaf(LeafStyle {
                    size: Size::fixed(48.0, 48.0),
                    style: BoxStyle::solid(TEAL),
                });
            })
        };
        b.iter(|| {
            let mut store = NodeStore::new();
            let mut cx = BuildCx::new(&mut store);
            container().build(&mut cx);
            black_box(&store);
        });
    });

    // layout: lay the built tree out into the surface rect.
    c.bench_function("view/layout", |b| {
        let (mut store, root) = build_scene();
        let rect = surface();
        let mut scratch = Vec::new();
        b.iter(|| {
            store.layout(root, rect, &mut scratch);
            black_box(&store);
        });
    });

    // paint_tree: lower the laid-out tree into the reused primitive buffer.
    c.bench_function("view/paint_tree", |b| {
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

criterion_group!(benches, bench_view);
criterion_main!(benches);
