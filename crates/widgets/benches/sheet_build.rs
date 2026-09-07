//! Microbench skeleton for the `Sheet` edge-drawer control: the cost of one
//! authoring pass (`Sheet::build`), one `layout`, and one `paint_tree` lowering.
//!
//! This is the section 71 microbench slot for the fifth Tier 4 (navigation /
//! overlay) widget — a scrim-backed drawer that slides in from a surface edge —
//! and copies the `modal_build.rs` template. It exercises the real `build` ->
//! `layout` -> `paint_tree` path so a regression is measurable; the slide itself
//! is a transform-only animation ticked elsewhere (see `viso-ui`'s
//! `animation_tick` bench), so this bench measures the static open-state scene
//! cost, not the motion.
//!
//! `Sheet` authors one `Bool` open cell (`cx.state`) and binds it to the sheet's
//! `PAINT`, so it must build through `BuildCx::with_reactive` (a plain
//! `BuildCx::new` panics on `cx.state`). For the default `Bottom` edge the root's
//! direct children are `[scrim@0, spacer@1, content@2]`: the scrim and the content
//! are *overlay* (top layer) — scrim authored first, content second — and a `Fill`
//! spacer pins the content to the bottom edge (the layout engine's `Align` is
//! cross-axis only, so a far-edge anchor needs a spacer sibling). A closed sheet
//! keeps both overlays hidden and paints nothing, so the bench opens the sheet
//! (shows the scrim AND the content) before laying out and painting so the
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
use std::time::Duration;

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

/// A brisk slide duration; unused by the static phases here but part of the built
/// style so the scene matches what a real open sheet carries.
const SLIDE: Duration = Duration::from_millis(200);

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

/// Author a single bottom `Sheet` (scrim + spacer + content + an app-captured
/// handle) into a fresh store, open it (show the scrim and content overlays), and
/// return the store plus its root — the input the phases below run on. Opening
/// shows the scrim@0 and content@2 so `layout`/`paint_tree` measure the
/// visible-overlay path; a closed sheet paints nothing.
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
            .duration(SLIDE)
            .scrim(SCRIM)
            .content(fill)
            .handle(&slot)
            .build(&mut cx);
        cx.root().expect("sheet declares a root")
    };
    // Open the sheet: show the scrim@0 and the content@2 (the spacer@1 never
    // paints) for the layout/paint phases.
    let parts = children_of(&store, root);
    store.set_hidden(parts[0], false);
    store.set_hidden(
        *parts.last().expect("a bottom sheet has a content child"),
        false,
    );
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
                .duration(SLIDE)
                .scrim(SCRIM)
                .content(fill)
                .handle(&slot)
                .build(&mut cx);
            black_box(&store);
        });
    });

    // layout: place the opened scrim + spacer + content over the region — the
    // spacer's `Fill` distribution pins the content to the bottom edge.
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
    // carries the overlay top-layer collection/deferral cost for both layers (the
    // scrim painted under the content).
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
