//! Microbench skeleton for the `Image` texture control: the cost of one
//! authoring pass (`Image::build`), one `layout`, and one `paint_tree` lowering.
//!
//! This is the section 71 microbench slot for the third content control (after
//! `View` and `Label`), and copies the `label_build.rs` template. It is
//! deliberately a skeleton this slice — it exercises the real `build` ->
//! `layout` -> `paint_tree` path so a regression is measurable, but the baseline
//! numbers are recorded in a later slice (per the plan: establish the framework
//! first).
//!
//! Unlike `Label`, `Image` has no shaping step: the texture is already resident,
//! so `Image::build` writes the image content payload directly. A dummy
//! `TextureId` is enough here — the bench times the widget authoring/layout/paint
//! cost, not any GPU work, and never rasterizes. It drives only `viso-ui`, so it
//! needs no facade or render dev-dependency and adds no dependency edge.
//!
//! Run release (`CARGO_TARGET_DIR=/tmp/rust_tmp cargo bench -p viso-widgets`);
//! criterion defaults to a release profile. Debug timing is not a perf result
//! (AGENTS section 36).

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use viso_ui::{BuildCx, Component, NodeStore, Rect, TextureId, paint_tree};
use viso_widgets::image;

const W: f32 = 160.0;
const H: f32 = 96.0;

/// A dummy resident texture id — the bench never paints to a real GPU, so any
/// id is fine.
const TEXTURE: TextureId = TextureId(0);

/// Author a single `Image` into a fresh store and return the store plus its
/// root — the input the layout/paint phases below run on.
fn build_scene() -> (NodeStore, viso_ui::NodeId) {
    let mut store = NodeStore::new();
    let root = {
        let mut cx = BuildCx::new(&mut store);
        image(TEXTURE, 64.0, 64.0).build(&mut cx);
        cx.root().expect("image declares a root")
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

fn bench_image(c: &mut Criterion) {
    // build: author the Image into a fresh NodeStore.
    c.bench_function("image/build", |b| {
        b.iter(|| {
            let mut store = NodeStore::new();
            let mut cx = BuildCx::new(&mut store);
            image(TEXTURE, 64.0, 64.0).build(&mut cx);
            black_box(&store);
        });
    });

    // layout: lay the built image out into the surface rect.
    c.bench_function("image/layout", |b| {
        let (mut store, root) = build_scene();
        let rect = surface();
        let mut scratch = Vec::new();
        b.iter(|| {
            store.layout(root, rect, &mut scratch);
            black_box(&store);
        });
    });

    // paint_tree: lower the laid-out image into the reused primitive buffer.
    c.bench_function("image/paint_tree", |b| {
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

criterion_group!(benches, bench_image);
criterion_main!(benches);
