//! Microbench skeleton for the `Tabs` panel-switching control: the cost of one
//! authoring pass (`Tabs::build`), one `layout`, and one `paint_tree` lowering.
//!
//! This is the section 71 microbench slot for the first Tier 4 (navigation /
//! overlay) widget — a strip of selectable tabs over a shared panel area — and
//! copies the `splitter_build.rs` template. It exercises the real `build` ->
//! `layout` -> `paint_tree` path so a regression is measurable; the baseline
//! numbers are recorded in a later slice (per the plan: establish the framework
//! first).
//!
//! `Tabs` authors one `Int` selection cell (`cx.state`) and binds it to each tab
//! button's `PAINT`, so it must build through `BuildCx::with_reactive` (a plain
//! `BuildCx::new` panics on `cx.state`). It also attaches pointer/key handlers
//! driven by an `on_change` callback — the bench authors one so the
//! handler-boxing cost is included — and two panel builders so the composed
//! subtree (a `TabList` strip plus two panels, all built once with the non-selected
//! panel folded out through the `hidden` seam) is realistic. It drives only
//! `viso-ui`, so it needs no facade or render dev-dependency and adds no
//! dependency edge.
//!
//! Run release (`CARGO_TARGET_DIR=/tmp/rust_tmp cargo bench -p viso-widgets`);
//! criterion defaults to a release profile. Debug timing is not a perf result
//! (AGENTS section 36).

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use viso_ui::{
    BindingTable, BoxStyle, BuildCx, Component, LeafStyle, NodeStore, Rect, SemanticProjector,
    Size, StateStore, TextEdits, VirtualLists, paint_tree,
};
use viso_widgets::tabs;

const W: f32 = 400.0;
const H: f32 = 240.0;

/// The reactive stores a `with_reactive` `BuildCx` needs, kept alive alongside
/// the node store so the built tabs' binding/state references stay valid.
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

/// A panel's content: a single fill leaf, so each panel composes a realistic
/// subtree rather than an empty node.
fn panel(cx: &mut BuildCx<'_>) {
    cx.leaf(LeafStyle {
        size: Size::fill(),
        style: BoxStyle::NONE,
    });
}

/// Author a single `Tabs` (two named tabs, panels, and an `on_change`) into a
/// fresh store and return the store plus its root — the input the phases below
/// run on.
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
            &mut r.projectors,
        );
        tabs()
            .tab("Details", panel)
            .tab("History", panel)
            .on_change(|_, _| {})
            .build(&mut cx);
        cx.root().expect("tabs declares a root")
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

fn bench_tabs(c: &mut Criterion) {
    // build: author the Tabs into a fresh NodeStore through a reactive cx.
    c.bench_function("tabs/build", |b| {
        b.iter(|| {
            let mut store = NodeStore::new();
            let mut r = Reactive::new();
            let mut cx = BuildCx::with_reactive(
                &mut store,
                &mut r.states,
                &mut r.bindings,
                &mut r.lists,
                &mut r.text_edits,
                &mut r.projectors,
            );
            tabs()
                .tab("Details", panel)
                .tab("History", panel)
                .on_change(|_, _| {})
                .build(&mut cx);
            black_box(&store);
        });
    });

    // layout: place the tab strip and the selected panel; the hidden panel folds
    // to zero.
    c.bench_function("tabs/layout", |b| {
        let (mut store, root) = build_scene();
        let rect = surface();
        let mut scratch = Vec::new();
        b.iter(|| {
            store.layout(root, rect, &mut scratch);
            black_box(&store);
        });
    });

    // paint_tree: lower the laid-out tabs into the reused primitive buffer.
    c.bench_function("tabs/paint_tree", |b| {
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

criterion_group!(benches, bench_tabs);
criterion_main!(benches);
