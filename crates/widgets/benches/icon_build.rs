//! Microbench skeleton for the `Icon` vector control: the cost of one authoring
//! pass (`Icon::build`), one `layout`, and one `paint_tree` lowering.
//!
//! This is the section 71 microbench slot for the fourth content control (after
//! `View`, `Label`, and `Image`), and copies the `image_build.rs` template. It is
//! deliberately a skeleton this slice — it exercises the real `build` ->
//! `layout` -> `paint_tree` path so a regression is measurable, but the baseline
//! numbers are recorded in a later slice (per the plan: establish the framework
//! first).
//!
//! Like `Image`, `Icon` has no shaping step: the geometry is authored inline, so
//! `Icon::build` writes the path content payload directly. `paint_tree` lowers it
//! to a path primitive without rasterizing (tessellation happens in the renderer,
//! not here), so the bench times the widget authoring/layout/paint cost, not any
//! GPU work. It drives only `viso-ui`, so it needs no facade or render
//! dev-dependency and adds no dependency edge.
//!
//! Run release (`CARGO_TARGET_DIR=/tmp/rust_tmp cargo bench -p viso-widgets`);
//! criterion defaults to a release profile. Debug timing is not a perf result
//! (AGENTS section 36).

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use viso_ui::{BuildCx, Component, NodeStore, PathCmd, Point, Rect, paint_tree};
use viso_widgets::icon;

const W: f32 = 160.0;
const H: f32 = 96.0;

/// A small deterministic outline (a filled triangle in a 24x24 local space) —
/// the geometry the bench authors. The bench never rasterizes, so the exact
/// shape only needs to be representative.
fn glyph() -> Vec<PathCmd> {
    vec![
        PathCmd::MoveTo(Point::new(12.0, 2.0)),
        PathCmd::LineTo(Point::new(22.0, 22.0)),
        PathCmd::LineTo(Point::new(2.0, 22.0)),
        PathCmd::Close,
    ]
}

/// Author a single `Icon` into a fresh store and return the store plus its
/// root — the input the layout/paint phases below run on.
fn build_scene() -> (NodeStore, viso_ui::NodeId) {
    let mut store = NodeStore::new();
    let root = {
        let mut cx = BuildCx::new(&mut store);
        icon(glyph(), 24.0, 24.0).build(&mut cx);
        cx.root().expect("icon declares a root")
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

fn bench_icon(c: &mut Criterion) {
    // build: author the Icon into a fresh NodeStore.
    c.bench_function("icon/build", |b| {
        b.iter(|| {
            let mut store = NodeStore::new();
            let mut cx = BuildCx::new(&mut store);
            icon(glyph(), 24.0, 24.0).build(&mut cx);
            black_box(&store);
        });
    });

    // layout: lay the built icon out into the surface rect.
    c.bench_function("icon/layout", |b| {
        let (mut store, root) = build_scene();
        let rect = surface();
        let mut scratch = Vec::new();
        b.iter(|| {
            store.layout(root, rect, &mut scratch);
            black_box(&store);
        });
    });

    // paint_tree: lower the laid-out icon into the reused primitive buffer.
    c.bench_function("icon/paint_tree", |b| {
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

criterion_group!(benches, bench_icon);
criterion_main!(benches);
