//! Microbench skeleton for the `Sheet` edge-drawer control: the cost of one
//! authoring pass (`Sheet::build`), one `layout`, and one `paint_tree` lowering.
//!
//! `Sheet` is a scrim-backed drawer that slides in from a surface edge. The bench
//! exercises the real `build` -> `layout` -> `paint_tree` path so a regression is
//! measurable; the slide itself is a transform-only animation ticked elsewhere (see
//! `viso-ui`'s `animation_tick` bench), so this bench measures the static open-state
//! scene cost, not the motion.
//!
//! `Sheet` authors one `Bool` open cell (`cx.state`) and binds it to the sheet's
//! `PAINT`, so it must build through `BuildCx::with_reactive` (a plain
//! `BuildCx::new` panics on `cx.state`). The sheet is one layer whose own box
//! is the scrim and whose child is the panel pinned to the bottom edge. A closed
//! sheet is hidden and paints nothing, so the bench presents it before laying
//! out and painting so the meaningful open-state cost is measured. It drives
//! only `viso-ui`, so it needs no facade or render dev-dependency and adds no
//! dependency edge.
//!
//! Run release (`CARGO_TARGET_DIR=/tmp/rust_tmp cargo bench -p viso-widgets`);
//! criterion defaults to a release profile. Debug timing is not a perf result.

use std::cell::RefCell;
use std::hint::black_box;
use std::rc::Rc;

use criterion::{Criterion, criterion_group, criterion_main};
use viso_ui::{
    BindingTable, BoxStyle, BuildCx, Component, LeafStyle, NodeId, NodeStore, Rect, Rgba,
    SemanticProjector, Size, StateStore, TextEdits, VirtualLists, paint_tree,
};
use viso_widgets::{SheetEdge, SheetHandleSlot, sheet};

const W: f32 = 400.0;
const H: f32 = 240.0;

/// The drawer's fixed extent along the slide axis (its height for a bottom sheet).
const EXTENT: f32 = 96.0;

/// An opaque scrim wash so the open-state paint carries a real fill for both
/// layers rather than a transparent no-op.
const SCRIM: Rgba = Rgba {
    r: 0.10,
    g: 0.10,
    b: 0.12,
    a: 1.0,
};

/// The reactive stores a `with_reactive` `BuildCx` needs, kept alive alongside
/// the node store so the built sheet's binding/state references stay valid.
struct Reactive {
    states: StateStore,
    bindings: BindingTable,
    lists: VirtualLists,
    text_edits: TextEdits,
    projectors: SemanticProjector,
}

impl Reactive {
    fn new() -> Self {
        Reactive {
            states: StateStore::new(),
            bindings: BindingTable::new(),
            lists: VirtualLists::new(),
            text_edits: TextEdits::new(),
            projectors: SemanticProjector::new(),
        }
    }
}

/// A content fill: a single leaf, so the content layer composes a realistic
/// subtree rather than an empty node.
fn fill(cx: &mut BuildCx<'_>) {
    cx.leaf(LeafStyle {
        size: Size::fill(),
        style: BoxStyle::NONE,
    });
}

/// Author a single bottom `Sheet` (scrim layer + panel + an app-captured
/// handle) into a fresh store and present it — the input the phases below run
/// on; a closed sheet paints nothing.
fn build_scene() -> (NodeStore, NodeId) {
    let mut store = NodeStore::new();
    let mut r = Reactive::new();
    let slot: SheetHandleSlot = Rc::new(RefCell::new(None));
    let root = {
        let mut cx = BuildCx::with_reactive(
            &mut store,
            &mut r.states,
            &mut r.bindings,
            &mut r.lists,
            &mut r.text_edits,
            &mut r.projectors,
        );
        sheet()
            .edge(SheetEdge::Bottom)
            .extent(EXTENT)
            .scrim(SCRIM)
            .content(fill)
            .handle(&slot)
            .build(&mut cx);
        cx.root().expect("sheet declares a root")
    };
    store.present_sheet(root, true);
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

fn bench_sheet(c: &mut Criterion) {
    // build: author the Sheet into a fresh NodeStore through a reactive cx.
    c.bench_function("sheet/build", |b| {
        b.iter(|| {
            let mut store = NodeStore::new();
            let mut r = Reactive::new();
            let slot: SheetHandleSlot = Rc::new(RefCell::new(None));
            let mut cx = BuildCx::with_reactive(
                &mut store,
                &mut r.states,
                &mut r.bindings,
                &mut r.lists,
                &mut r.text_edits,
                &mut r.projectors,
            );
            sheet()
                .edge(SheetEdge::Bottom)
                .extent(EXTENT)
                .scrim(SCRIM)
                .content(fill)
                .handle(&slot)
                .build(&mut cx);
            black_box(&store);
        });
    });

    // layout: place the presented scrim layer and its panel over the region.
    c.bench_function("sheet/layout", |b| {
        let (mut store, root) = build_scene();
        let rect = surface();
        let mut scratch = Vec::new();
        b.iter(|| {
            store.layout(root, rect, &mut scratch);
            black_box(&store);
        });
    });

    // paint_tree: lower the laid-out sheet into the reused primitive buffer; this
    // carries the layer's collection/deferral cost (the scrim painted under the
    // panel).
    c.bench_function("sheet/paint_tree", |b| {
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

criterion_group!(benches, bench_sheet);
criterion_main!(benches);
