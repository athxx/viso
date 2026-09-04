//! Microbench skeleton for the `Label` static-text control: the cost of one
//! authoring pass (`Label::build`), one `layout`, and one `paint_tree` lowering.
//!
//! This is the section 71 microbench slot for the first content control after
//! `View`, and copies the `view_build.rs` template. It is deliberately a
//! skeleton this slice — it exercises the real `build` -> `layout` ->
//! `paint_tree` path so a regression is measurable, but the baseline numbers are
//! recorded in a later slice (per the plan: establish the framework first).
//!
//! Note this drives `Label::build` and `layout` only — not shaping. `viso-ui`
//! has no font stack (the facade owns the `TextSystem`), so the label leaf
//! carries an unshaped text request and no glyph content here; the bench times
//! the widget authoring/layout/paint cost, not text shaping. It drives only
//! `viso-ui`, so it needs no facade or render dev-dependency and adds no
//! dependency edge.
//!
//! Run release (`CARGO_TARGET_DIR=/tmp/rust_tmp cargo bench -p viso-widgets`);
//! criterion defaults to a release profile. Debug timing is not a perf result
//! (AGENTS section 36).

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use viso_ui::{BuildCx, Component, NodeStore, Rect, paint_tree};
use viso_widgets::label;

const W: f32 = 160.0;
const H: f32 = 96.0;

/// Author a single `Label` into a fresh store and return the store plus its
/// root — the input the layout/paint phases below run on.
fn build_scene() -> (NodeStore, viso_ui::NodeId) {
    let mut store = NodeStore::new();
    let root = {
        let mut cx = BuildCx::new(&mut store);
        label("Save").font_size(18.0).build(&mut cx);
        cx.root().expect("label declares a root")
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

fn bench_label(c: &mut Criterion) {
    // build: author the Label into a fresh NodeStore.
    c.bench_function("label/build", |b| {
        b.iter(|| {
            let mut store = NodeStore::new();
            let mut cx = BuildCx::new(&mut store);
            label("Save").font_size(18.0).build(&mut cx);
            black_box(&store);
        });
    });

    // layout: lay the built label out into the surface rect.
    c.bench_function("label/layout", |b| {
        let (mut store, root) = build_scene();
        let rect = surface();
        let mut scratch = Vec::new();
        b.iter(|| {
            store.layout(root, rect, &mut scratch);
            black_box(&store);
        });
    });

    // paint_tree: lower the laid-out label into the reused primitive buffer.
    c.bench_function("label/paint_tree", |b| {
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

criterion_group!(benches, bench_label);
criterion_main!(benches);
