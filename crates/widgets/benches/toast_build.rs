//! Microbench skeleton for the `Toast` control: the cost of one authoring pass
//! (`Toast::build`), one `layout`, and one `paint_tree` lowering.
//!
//! This is the section 71 microbench slot for the sixth Tier 4 (navigation /
//! overlay) widget — a non-modal, auto-dismissing status overlay pinned to a
//! surface edge — and copies the `sheet_build.rs` template. It exercises the real
//! `build` -> `layout` -> `paint_tree` path so a regression is measurable. Unlike
//! the `Sheet` the toast has no scrim and no slide animation, so this bench
//! measures the static shown-state scene cost with nothing ticked elsewhere; the
//! one-shot auto-dismiss timer's per-frame cost lives in `viso-ui`'s `timer_arm`
//! bench, not here.
//!
//! `Toast` authors one `Bool` open cell (`cx.state`) and binds it, so it must
//! build through `BuildCx::with_reactive` (a plain `BuildCx::new` panics on
//! `cx.state`). For the default `Top` edge the root has a single child — the
//! content `Fit x Fit` panel, an *overlay* (top layer) pinned at main-start; a
//! far-edge (`Bottom`/`Trailing`) toast instead carries a leading `Fill` spacer,
//! so the content is always the root's *last* child. A hidden toast paints
//! nothing, so the bench shows the toast (reveals the content) before laying out
//! and painting so the meaningful shown-state cost is measured, not folded out by
//! the `hidden` flag. It drives only `viso-ui`, so it needs no facade or render
//! dev-dependency and adds no dependency edge.
//!
//! Run release (`CARGO_TARGET_DIR=/tmp/rust_tmp cargo bench -p viso-widgets`);
//! criterion defaults to a release profile. Debug timing is not a perf result
//! (AGENTS section 36).

use std::cell::RefCell;
use std::hint::black_box;
use std::rc::Rc;
use std::time::Duration;

use criterion::{Criterion, criterion_group, criterion_main};
use viso_ui::{
    BindingTable, BoxStyle, BuildCx, Component, LeafStyle, NodeId, NodeStore, Rect, Rgba,
    SemanticProjector, Size, StateStore, TextEdits, VirtualLists, paint_tree,
};
use viso_widgets::{ToastEdge, ToastHandleSlot, toast};

const W: f32 = 400.0;
const H: f32 = 240.0;

/// The fixed panel size — a small centered notification body, matching the
/// validation pack's content leaf.
const PANEL_W: f32 = 160.0;
const PANEL_H: f32 = 32.0;

/// The auto-dismiss duration; unused by the static phases here but part of the
/// built style so the scene matches what a real shown toast carries.
const DURATION: Duration = Duration::from_millis(4000);

/// An opaque panel wash so the shown-state paint carries a real fill rather than a
/// transparent no-op.
const PANEL: Rgba = Rgba {
    r: 0.16,
    g: 0.42,
    b: 0.30,
    a: 1.0,
};

/// The reactive stores a `with_reactive` `BuildCx` needs, kept alive alongside the
/// node store so the built toast's binding/state references stay valid.
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

/// A fixed-size content fill: a single colored leaf, so the `Fit x Fit` panel gets
/// a realistic deterministic size rather than collapsing to zero.
fn panel(cx: &mut BuildCx<'_>) {
    cx.leaf(LeafStyle {
        size: Size::fixed(PANEL_W, PANEL_H),
        style: BoxStyle::solid(PANEL),
    });
}

/// Author a single top `Toast` (content + an app-captured handle) into a fresh
/// store, show it (reveal the content overlay), and return the store plus its root
/// — the input the phases below run on. Showing reveals the content so
/// `layout`/`paint_tree` measure the visible-overlay path; a hidden toast paints
/// nothing.
fn build_scene() -> (NodeStore, NodeId) {
    let mut store = NodeStore::new();
    let mut r = Reactive::new();
    let slot: ToastHandleSlot = Rc::new(RefCell::new(None));
    let root = {
        let mut cx = BuildCx::with_reactive(
            &mut store,
            &mut r.states,
            &mut r.bindings,
            &mut r.lists,
            &mut r.text_edits,
            &mut r.projectors,
        );
        toast()
            .edge(ToastEdge::Top)
            .duration(DURATION)
            .content(panel)
            .handle(&slot)
            .build(&mut cx);
        cx.root().expect("toast declares a root")
    };
    // Show the toast: reveal the content (the root's last child) for the
    // layout/paint phases.
    let content = *children_of(&store, root)
        .last()
        .expect("a toast has a content child");
    store.set_hidden(content, false);
    (store, root)
}

/// A node's direct children (arena sibling chain) — used to reach the content
/// overlay layer.
fn children_of(store: &NodeStore, parent: NodeId) -> Vec<NodeId> {
    let arena = store.arena();
    let mut out = Vec::new();
    let mut child = arena.links(parent).and_then(|l| l.first_child);
    while let Some(c) = child {
        out.push(c);
        child = arena.links(c).and_then(|l| l.next_sibling);
    }
    out
}

fn surface() -> Rect {
    Rect {
        x: 0.0,
        y: 0.0,
        w: W,
        h: H,
    }
}

fn bench_toast(c: &mut Criterion) {
    // build: author the Toast into a fresh NodeStore through a reactive cx.
    c.bench_function("toast/build", |b| {
        b.iter(|| {
            let mut store = NodeStore::new();
            let mut r = Reactive::new();
            let slot: ToastHandleSlot = Rc::new(RefCell::new(None));
            let mut cx = BuildCx::with_reactive(
                &mut store,
                &mut r.states,
                &mut r.bindings,
                &mut r.lists,
                &mut r.text_edits,
                &mut r.projectors,
            );
            toast()
                .edge(ToastEdge::Top)
                .duration(DURATION)
                .content(panel)
                .handle(&slot)
                .build(&mut cx);
            black_box(&store);
        });
    });

    // layout: place the shown content panel over the region — the toast's edge-pin
    // (main-start for Top) and cross-axis center resolve here.
    c.bench_function("toast/layout", |b| {
        let (mut store, root) = build_scene();
        let rect = surface();
        let mut scratch = Vec::new();
        b.iter(|| {
            store.layout(root, rect, &mut scratch);
            black_box(&store);
        });
    });

    // paint_tree: lower the laid-out toast into the reused primitive buffer; this
    // carries the overlay top-layer collection/deferral cost for the content layer.
    c.bench_function("toast/paint_tree", |b| {
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

criterion_group!(benches, bench_toast);
criterion_main!(benches);
