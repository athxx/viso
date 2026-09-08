//! Microbench for the `Dock` control: the cost of authoring a nested dock, laying
//! it out, lowering it to primitives, and — the interactive hot path — one
//! seam-drag reconcile frame.
//!
//! This is the section 71 microbench slot for the first Tier 5 (composite
//! container) control, and copies the `splitter_build.rs` template. Four phases:
//!
//! - `dock/build_32` authors a balanced 32-panel dock (a deep nest of binary
//!   splits) through the public [`dock`] builder, so the build-walk cost — flex
//!   authoring, seam wiring, keyed panel registration — is measured at realistic
//!   depth.
//! - `dock/layout` and `dock/paint_tree` run the built dock through the real
//!   `layout` -> `paint_tree` path.
//! - `dock/seam_reconcile_frame` times the exact work one live drag frame does:
//!   rewrite both panes' fill weights via
//!   [`NodeStore::set_flex_child_weight`](viso_ui::NodeStore::set_flex_child_weight)
//!   and re-lay-out the split. The dock's `reconcile` step is `pub(super)`, so the
//!   bench drives its identical viso-ui foundation on a two-fill-pane split (the
//!   shape a dock split authors) rather than reaching into the crate internals —
//!   the reconcile loop is nothing but this pair per seam, so this is the cost that
//!   scales with a drag.
//!
//! It drives only `viso-ui` + `viso-widgets`, so it needs no facade or render
//! dev-dependency and adds no dependency edge. Run release
//! (`CARGO_TARGET_DIR=/tmp/rust_tmp cargo bench -p viso-widgets --bench dock`);
//! debug timing is not a perf result (AGENTS section 36).

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use viso_ui::{
    Align, Axis, BindingTable, BoxStyle, BuildCx, Component, FlexStyle, Inset, Justify, LeafStyle,
    NodeId, NodeStore, Rect, SemanticProjector, Size, StateStore, TextEdits, VirtualLists,
    paint_tree,
};
use viso_widgets::{Dock, DockNode, DockTree, PanelKey, dock};

const W: f32 = 1200.0;
const H: f32 = 800.0;

/// The reactive stores a `with_reactive` `BuildCx` needs, kept alive alongside the
/// node store so the built dock's binding/state references stay valid.
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

/// A balanced binary split tree of `2^depth` panels, keyed `0..2^depth`, so the
/// dock nests to a realistic depth. `depth == 5` yields 32 panels.
fn balanced_tree(depth: u32, next_key: &mut u32) -> DockNode {
    if depth == 0 {
        let k = *next_key;
        *next_key += 1;
        return DockNode::panel(PanelKey(k));
    }
    let axis = if depth.is_multiple_of(2) {
        Axis::Row
    } else {
        Axis::Column
    };
    let a = balanced_tree(depth - 1, next_key);
    let b = balanced_tree(depth - 1, next_key);
    DockNode::split(axis, 0.5, a, b)
}

/// A 32-panel dock with every panel registered to a single fill leaf.
fn dock_32() -> Dock {
    let mut next = 0;
    let tree = DockTree::new(balanced_tree(5, &mut next));
    let mut d = dock(tree);
    for k in 0..next {
        d = d.panel(PanelKey(k), |cx: &mut BuildCx<'_>| {
            cx.leaf(LeafStyle {
                size: Size::fill(),
                style: BoxStyle::NONE,
            });
        });
    }
    d
}

/// Author the 32-panel dock into a fresh store and return the store plus its root.
fn build_dock_scene() -> (NodeStore, NodeId) {
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
        dock_32().build(&mut cx);
        cx.root().expect("dock declares a root")
    };
    (store, root)
}

/// A row flex of two fill panes — the shape one dock split authors — plus the two
/// pane node ids a seam reconcile drives.
fn build_split_scene() -> (NodeStore, NodeId, NodeId, NodeId) {
    let mut store = NodeStore::new();
    let mut r = Reactive::new();
    let mut pane_a = None;
    let mut pane_b = None;
    let root = {
        let mut cx = BuildCx::with_reactive(
            &mut store,
            &mut r.states,
            &mut r.bindings,
            &mut r.lists,
            &mut r.text_edits,
            &mut r.projectors,
        );
        let a_out = &mut pane_a;
        let b_out = &mut pane_b;
        cx.flex(
            FlexStyle {
                axis: Axis::Row,
                gap: 0.0,
                padding: Inset::all(0.0),
                align: Align::Stretch,
                justify: Justify::Start,
                size: Size::fill(),
                style: BoxStyle::NONE,
            },
            |cx| {
                let a = cx.leaf(LeafStyle {
                    size: Size::fill(),
                    style: BoxStyle::NONE,
                });
                let b = cx.leaf(LeafStyle {
                    size: Size::fill(),
                    style: BoxStyle::NONE,
                });
                *a_out = Some(a.id());
                *b_out = Some(b.id());
            },
        )
        .id()
    };
    (
        store,
        root,
        pane_a.expect("pane A built"),
        pane_b.expect("pane B built"),
    )
}

fn surface() -> Rect {
    Rect {
        x: 0.0,
        y: 0.0,
        w: W,
        h: H,
    }
}

fn bench_dock(c: &mut Criterion) {
    // build_32: author a balanced 32-panel dock into a fresh NodeStore.
    c.bench_function("dock/build_32", |b| {
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
            dock_32().build(&mut cx);
            black_box(&store);
        });
    });

    // layout: place every split, pane, and seam of the 32-panel dock.
    c.bench_function("dock/layout", |b| {
        let (mut store, root) = build_dock_scene();
        let rect = surface();
        let mut scratch = Vec::new();
        b.iter(|| {
            store.layout(root, rect, &mut scratch);
            black_box(&store);
        });
    });

    // paint_tree: lower the laid-out dock into the reused primitive buffer.
    c.bench_function("dock/paint_tree", |b| {
        let (mut store, root) = build_dock_scene();
        store.layout(root, surface(), &mut Vec::new());
        let mut primitives = Vec::new();
        paint_tree(&store, root, &mut primitives);
        b.iter(|| {
            primitives.clear();
            paint_tree(&store, root, &mut primitives);
            black_box(&primitives);
        });
    });

    // seam_reconcile_frame: one live drag frame — rewrite both pane weights and
    // re-lay-out the split. The reconcile loop is exactly this per seam.
    c.bench_function("dock/seam_reconcile_frame", |b| {
        let (mut store, root, pane_a, pane_b) = build_split_scene();
        let rect = surface();
        let mut scratch = Vec::new();
        store.layout(root, rect, &mut scratch);
        let mut f = 0.5_f32;
        b.iter(|| {
            // Nudge the fraction each iteration so the weights genuinely change.
            f = if f > 0.7 { 0.3 } else { f + 0.001 };
            store.set_flex_child_weight(pane_a, Axis::Row, f);
            store.set_flex_child_weight(pane_b, Axis::Row, 1.0 - f);
            store.layout(root, rect, &mut scratch);
            black_box(&store);
        });
    });
}

criterion_group!(benches, bench_dock);
criterion_main!(benches);
