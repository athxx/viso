//! Microbench skeleton for the `Modal` overlay control: the cost of one authoring
//! pass (`Modal::build`), one `layout`, and one `paint_tree` lowering.
//!
//! This is the section 71 microbench slot for the fourth Tier 4 (navigation /
//! overlay) widget — a scrim-backed dialog layer with no in-place anchor that
//! opens over the whole surface — and copies the `popup_build.rs` template. It
//! exercises the real `build` -> `layout` -> `paint_tree` path so a regression is
//! measurable; the baseline numbers are recorded in a later slice (per the plan:
//! establish the framework first).
//!
//! `Modal` authors one `Bool` open cell (`cx.state`) and binds it to the modal's
//! `PAINT`, so it must build through `BuildCx::with_reactive` (a plain
//! `BuildCx::new` panics on `cx.state`). It flags BOTH its scrim and its content
//! an *overlay* (top layer): the scrim is authored first and the content second,
//! so the `paint_tree` phase carries the overlay top-layer collection/deferral
//! cost that is the point of this control — both layers are built once in place
//! but painted last, scrim under content. Unlike `Popup`, a `Modal` has no anchor,
//! so a closed modal (both overlays hidden) paints nothing; the bench opens the
//! modal (shows the scrim AND the content) before laying out and painting so the
//! meaningful open-state cost is measured, not folded out by the closed `hidden`
//! flags. It drives only `viso-ui`, so it needs no facade or render dev-dependency
//! and adds no dependency edge.
//!
//! Run release (`CARGO_TARGET_DIR=/tmp/rust_tmp cargo bench -p viso-widgets`);
//! criterion defaults to a release profile. Debug timing is not a perf result
//! (AGENTS section 36).

use std::cell::RefCell;
use std::hint::black_box;
use std::rc::Rc;

use criterion::{Criterion, criterion_group, criterion_main};
use viso_ui::{
    BindingTable, BoxStyle, BuildCx, Component, LeafStyle, NodeId, NodeStore, Rect, Rgba,
    SemanticProjector, Size, StateStore, TextEdits, VirtualLists, paint_tree,
};
use viso_widgets::{ModalHandleSlot, modal};

const W: f32 = 400.0;
const H: f32 = 240.0;

/// An opaque scrim wash so the open-state paint carries a real fill for both
/// layers rather than a transparent no-op.
const SCRIM: Rgba = Rgba {
    r: 0.10,
    g: 0.10,
    b: 0.12,
    a: 1.0,
};

/// The reactive stores a `with_reactive` `BuildCx` needs, kept alive alongside
/// the node store so the built modal's binding/state references stay valid.
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

/// Author a single `Modal` (scrim + content + an app-captured handle) into a
/// fresh store, open it (show BOTH overlay layers), and return the store plus its
/// root — the input the phases below run on. Opening shows the scrim and content
/// so `layout`/`paint_tree` measure the visible-overlay path; a closed modal has
/// no anchor and would paint nothing.
fn build_scene() -> (NodeStore, NodeId) {
    let mut store = NodeStore::new();
    let mut r = Reactive::new();
    let slot: ModalHandleSlot = Rc::new(RefCell::new(None));
    let root = {
        let mut cx = BuildCx::with_reactive(
            &mut store,
            &mut r.states,
            &mut r.bindings,
            &mut r.lists,
            &mut r.text_edits,
            &mut r.projectors,
        );
        modal()
            .scrim(SCRIM)
            .content(fill)
            .size(Size::fill())
            .handle(&slot)
            .build(&mut cx);
        cx.root().expect("modal declares a root")
    };
    // Open the modal: show both overlay layers (scrim@0 under content@1) for the
    // layout/paint phases.
    let parts = children_of(&store, root);
    store.set_hidden(parts[0], false);
    store.set_hidden(parts[1], false);
    (store, root)
}

/// A node's direct children (arena sibling chain) — used to reach the scrim and
/// content overlay layers.
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

fn bench_modal(c: &mut Criterion) {
    // build: author the Modal into a fresh NodeStore through a reactive cx.
    c.bench_function("modal/build", |b| {
        b.iter(|| {
            let mut store = NodeStore::new();
            let mut r = Reactive::new();
            let slot: ModalHandleSlot = Rc::new(RefCell::new(None));
            let mut cx = BuildCx::with_reactive(
                &mut store,
                &mut r.states,
                &mut r.bindings,
                &mut r.lists,
                &mut r.text_edits,
                &mut r.projectors,
            );
            modal()
                .scrim(SCRIM)
                .content(fill)
                .size(Size::fill())
                .handle(&slot)
                .build(&mut cx);
            black_box(&store);
        });
    });

    // layout: place the opened scrim and content overlay layers over the region.
    c.bench_function("modal/layout", |b| {
        let (mut store, root) = build_scene();
        let rect = surface();
        let mut scratch = Vec::new();
        b.iter(|| {
            store.layout(root, rect, &mut scratch);
            black_box(&store);
        });
    });

    // paint_tree: lower the laid-out modal into the reused primitive buffer; this
    // carries the overlay top-layer collection/deferral cost for BOTH layers (the
    // scrim is painted last-but-one and the content last, over the scrim).
    c.bench_function("modal/paint_tree", |b| {
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

criterion_group!(benches, bench_modal);
criterion_main!(benches);
