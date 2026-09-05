//! Microbench skeleton for the `Button` interactive control: the cost of one
//! authoring pass (`Button::build`), one `layout`, and one `paint_tree`
//! lowering.
//!
//! This is the section 71 microbench slot for the first Tier 2 (interactive)
//! control, and copies the `icon_build.rs` template. It is deliberately a
//! skeleton this slice — it exercises the real `build` -> `layout` ->
//! `paint_tree` path so a regression is measurable, but the baseline numbers are
//! recorded in a later slice (per the plan: establish the framework first).
//!
//! Unlike the Tier 1 presentational controls, `Button` authors a reactive
//! `pressed` state cell (`cx.state`) and wires a `pressed -> PAINT` binding, so
//! it must build through `BuildCx::with_reactive` (a plain `BuildCx::new` panics
//! on `cx.state`). It also attaches pointer/key handlers driven by an `on_click`
//! callback — the bench authors one so the handler-boxing cost is included. It
//! drives only `viso-ui`, so it needs no facade or render dev-dependency and adds
//! no dependency edge.
//!
//! Run release (`CARGO_TARGET_DIR=/tmp/rust_tmp cargo bench -p viso-widgets`);
//! criterion defaults to a release profile. Debug timing is not a perf result
//! (AGENTS section 36).

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use viso_ui::{
    BindingTable, BuildCx, Component, NodeStore, Rect, StateStore, VirtualLists, paint_tree,
};
use viso_widgets::button;

const W: f32 = 160.0;
const H: f32 = 96.0;

/// The reactive stores a `with_reactive` `BuildCx` needs, kept alive alongside
/// the node store so the built button's binding/state references stay valid.
struct Reactive {
    states: StateStore,
    bindings: BindingTable,
    lists: VirtualLists,
}

impl Reactive {
    fn new() -> Self {
        Reactive {
            states: StateStore::new(),
            bindings: BindingTable::new(),
            lists: VirtualLists::new(),
        }
    }
}

/// Author a single `Button` (with an `on_click`) into a fresh store and return
/// the store plus its root — the input the layout/paint phases below run on.
fn build_scene() -> (NodeStore, viso_ui::NodeId) {
    let mut store = NodeStore::new();
    let mut r = Reactive::new();
    let root = {
        let mut cx =
            BuildCx::with_reactive(&mut store, &mut r.states, &mut r.bindings, &mut r.lists);
        button("OK").on_click(|_| {}).build(&mut cx);
        cx.root().expect("button declares a root")
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

fn bench_button(c: &mut Criterion) {
    // build: author the Button into a fresh NodeStore through a reactive cx.
    c.bench_function("button/build", |b| {
        b.iter(|| {
            let mut store = NodeStore::new();
            let mut r = Reactive::new();
            let mut cx =
                BuildCx::with_reactive(&mut store, &mut r.states, &mut r.bindings, &mut r.lists);
            button("OK").on_click(|_| {}).build(&mut cx);
            black_box(&store);
        });
    });

    // layout: lay the built button out into the surface rect.
    c.bench_function("button/layout", |b| {
        let (mut store, root) = build_scene();
        let rect = surface();
        let mut scratch = Vec::new();
        b.iter(|| {
            store.layout(root, rect, &mut scratch);
            black_box(&store);
        });
    });

    // paint_tree: lower the laid-out button into the reused primitive buffer.
    c.bench_function("button/paint_tree", |b| {
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

criterion_group!(benches, bench_button);
criterion_main!(benches);
