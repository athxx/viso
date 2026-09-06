//! Microbench skeleton for the `Splitter` interactive control: the cost of one
//! authoring pass (`Splitter::build`), one `layout`, and one `paint_tree`
//! lowering.
//!
//! This is the section 71 microbench slot for the fourth Tier 3 (layout
//! structure) widget — a draggable two-pane divider — and copies the
//! `slider_build.rs` template. It exercises the real `build` -> `layout` ->
//! `paint_tree` path so a regression is measurable; the baseline numbers are
//! recorded in a later slice (per the plan: establish the framework first).
//!
//! `Splitter` authors three float cells (`cx.state`: the split fraction plus the
//! drag anchor pair) and binds the fraction to the root's `PAINT`, so it must
//! build through `BuildCx::with_reactive` (a plain `BuildCx::new` panics on
//! `cx.state`). It also attaches pointer/key handlers driven by an `on_change`
//! callback — the bench authors one so the handler-boxing cost is included, and
//! two pane builders so the composed subtree is realistic. It drives only
//! `viso-ui`, so it needs no facade or render dev-dependency and adds no
//! dependency edge.
//!
//! Run release (`CARGO_TARGET_DIR=/tmp/rust_tmp cargo bench -p viso-widgets`);
//! criterion defaults to a release profile. Debug timing is not a perf result
//! (AGENTS section 36).

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use viso_ui::{
    BindingTable, BoxStyle, BuildCx, Component, LeafStyle, NodeStore, Rect, Size, StateStore,
    TextEdits, VirtualLists, paint_tree,
};
use viso_widgets::splitter;

const W: f32 = 400.0;
const H: f32 = 240.0;

/// The reactive stores a `with_reactive` `BuildCx` needs, kept alive alongside
/// the node store so the built splitter's binding/state references stay valid.
struct Reactive {
    states: StateStore,
    bindings: BindingTable,
    lists: VirtualLists,
    text_edits: TextEdits,
}

impl Reactive {
    fn new() -> Self {
        Reactive {
            states: StateStore::new(),
            bindings: BindingTable::new(),
            lists: VirtualLists::new(),
            text_edits: TextEdits::new(),
        }
    }
}

/// A pane's content: a single fill leaf, so the divider composes a realistic
/// two-child subtree rather than two empty panes.
fn pane(cx: &mut BuildCx<'_>) {
    cx.leaf(LeafStyle {
        size: Size::fill(),
        style: BoxStyle::NONE,
    });
}

/// Author a single `Splitter` (with panes and an `on_change`) into a fresh store
/// and return the store plus its root — the input the phases below run on.
fn build_scene() -> (NodeStore, viso_ui::NodeId) {
    let mut store = NodeStore::new();
    let mut r = Reactive::new();
    let root = {
        let mut cx = BuildCx::with_reactive(
            &mut store,
            &mut r.states,
            &mut r.bindings,
            &mut r.lists,
            &mut r.text_edits,
        );
        splitter("Editor / Preview")
            .extent(W)
            .panes(pane, pane)
            .on_change(|_, _| {})
            .build(&mut cx);
        cx.root().expect("splitter declares a root")
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

fn bench_splitter(c: &mut Criterion) {
    // build: author the Splitter into a fresh NodeStore through a reactive cx.
    c.bench_function("splitter/build", |b| {
        b.iter(|| {
            let mut store = NodeStore::new();
            let mut r = Reactive::new();
            let mut cx = BuildCx::with_reactive(
                &mut store,
                &mut r.states,
                &mut r.bindings,
                &mut r.lists,
                &mut r.text_edits,
            );
            splitter("Editor / Preview")
                .extent(W)
                .panes(pane, pane)
                .on_change(|_, _| {})
                .build(&mut cx);
            black_box(&store);
        });
    });

    // layout: split the surface, place both panes and the divider bar.
    c.bench_function("splitter/layout", |b| {
        let (mut store, root) = build_scene();
        let rect = surface();
        let mut scratch = Vec::new();
        b.iter(|| {
            store.layout(root, rect, &mut scratch);
            black_box(&store);
        });
    });

    // paint_tree: lower the laid-out splitter into the reused primitive buffer.
    c.bench_function("splitter/paint_tree", |b| {
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

criterion_group!(benches, bench_splitter);
criterion_main!(benches);
