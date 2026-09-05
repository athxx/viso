//! Microbench skeleton for the `CheckBox` interactive control: the cost of one
//! authoring pass (`CheckBox::build`), one `layout`, and one `paint_tree`
//! lowering.
//!
//! This is the section 71 microbench slot for the second Tier 2 (interactive)
//! control, and copies the `button_build.rs` template. It is deliberately a
//! skeleton this slice — it exercises the real `build` -> `layout` ->
//! `paint_tree` path so a regression is measurable, but the baseline numbers are
//! recorded in a later slice (per the plan: establish the framework first).
//!
//! Like `Button`, `CheckBox` authors a reactive `checked` state cell
//! (`cx.state`) and wires a `checked -> PAINT` binding, so it must build through
//! `BuildCx::with_reactive` (a plain `BuildCx::new` panics on `cx.state`). It
//! also attaches pointer/key handlers driven by an `on_change` callback — the
//! bench authors one so the handler-boxing cost is included. It drives only
//! `viso-ui`, so it needs no facade or render dev-dependency and adds no
//! dependency edge.
//!
//! Run release (`CARGO_TARGET_DIR=/tmp/rust_tmp cargo bench -p viso-widgets`);
//! criterion defaults to a release profile. Debug timing is not a perf result
//! (AGENTS section 36).

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use viso_ui::{
    BindingTable, BuildCx, Component, NodeStore, Rect, StateStore, VirtualLists, paint_tree,
};
use viso_widgets::checkbox;

const W: f32 = 160.0;
const H: f32 = 96.0;

/// The reactive stores a `with_reactive` `BuildCx` needs, kept alive alongside
/// the node store so the built checkbox's binding/state references stay valid.
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

/// Author a single `CheckBox` (with an `on_change`) into a fresh store and
/// return the store plus its root — the input the layout/paint phases below run
/// on.
fn build_scene() -> (NodeStore, viso_ui::NodeId) {
    let mut store = NodeStore::new();
    let mut r = Reactive::new();
    let root = {
        let mut cx =
            BuildCx::with_reactive(&mut store, &mut r.states, &mut r.bindings, &mut r.lists);
        checkbox("Sound").on_change(|_, _| {}).build(&mut cx);
        cx.root().expect("checkbox declares a root")
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

fn bench_checkbox(c: &mut Criterion) {
    // build: author the CheckBox into a fresh NodeStore through a reactive cx.
    c.bench_function("checkbox/build", |b| {
        b.iter(|| {
            let mut store = NodeStore::new();
            let mut r = Reactive::new();
            let mut cx =
                BuildCx::with_reactive(&mut store, &mut r.states, &mut r.bindings, &mut r.lists);
            checkbox("Sound").on_change(|_, _| {}).build(&mut cx);
            black_box(&store);
        });
    });

    // layout: lay the built checkbox out into the surface rect.
    c.bench_function("checkbox/layout", |b| {
        let (mut store, root) = build_scene();
        let rect = surface();
        let mut scratch = Vec::new();
        b.iter(|| {
            store.layout(root, rect, &mut scratch);
            black_box(&store);
        });
    });

    // paint_tree: lower the laid-out checkbox into the reused primitive buffer.
    c.bench_function("checkbox/paint_tree", |b| {
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

criterion_group!(benches, bench_checkbox);
criterion_main!(benches);
