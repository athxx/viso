//! Microbench skeleton for the `Popup` overlay control: the cost of one authoring
//! pass (`Popup::build`), one `layout`, and one `paint_tree` lowering.
//!
//! This is the section 71 microbench slot for the third Tier 4 (navigation /
//! overlay) widget — a persistent anchor with a floating content layer that opens
//! over the scene — and copies the `navigation_stack_build.rs` template. It
//! exercises the real `build` -> `layout` -> `paint_tree` path so a regression is
//! measurable; the baseline numbers are recorded in a later slice (per the plan:
//! establish the framework first).
//!
//! `Popup` authors one `Bool` open cell (`cx.state`) and binds it to the popup's
//! `PAINT`, so it must build through `BuildCx::with_reactive` (a plain
//! `BuildCx::new` panics on `cx.state`). It flags its content an *overlay* (top
//! layer) and attaches a key handler for Escape, so the `paint_tree` phase carries
//! the overlay top-layer collection/deferral cost that is the point of this
//! control: the content is built once in place but painted last. The bench opens
//! the popup (shows the content) before laying out and painting so that cost is
//! measured, not folded out by the closed `hidden` flag. It drives only `viso-ui`,
//! so it needs no facade or render dev-dependency and adds no dependency edge.
//!
//! Run release (`CARGO_TARGET_DIR=/tmp/rust_tmp cargo bench -p viso-widgets`);
//! criterion defaults to a release profile. Debug timing is not a perf result
//! (AGENTS section 36).

use std::cell::RefCell;
use std::hint::black_box;
use std::rc::Rc;

use criterion::{Criterion, criterion_group, criterion_main};
use viso_ui::{
    BindingTable, BoxStyle, BuildCx, Component, LeafStyle, NodeId, NodeStore, Rect, Size,
    StateStore, TextEdits, VirtualLists, paint_tree,
};
use viso_widgets::{PopupHandleSlot, popup};

const W: f32 = 400.0;
const H: f32 = 240.0;

/// The reactive stores a `with_reactive` `BuildCx` needs, kept alive alongside
/// the node store so the built popup's binding/state references stay valid.
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

/// An anchor or content fill: a single leaf, so each layer composes a realistic
/// subtree rather than an empty node.
fn fill(cx: &mut BuildCx<'_>) {
    cx.leaf(LeafStyle {
        size: Size::fill(),
        style: BoxStyle::NONE,
    });
}

/// Author a single `Popup` (anchor + content + a scrim + an app-captured handle)
/// into a fresh store, open it (show the content overlay), and return the store
/// plus its root — the input the phases below run on. Opening shows the content so
/// `layout`/`paint_tree` measure the visible-overlay path.
fn build_scene() -> (NodeStore, NodeId) {
    let mut store = NodeStore::new();
    let mut r = Reactive::new();
    let slot: PopupHandleSlot = Rc::new(RefCell::new(None));
    let root = {
        let mut cx = BuildCx::with_reactive(
            &mut store,
            &mut r.states,
            &mut r.bindings,
            &mut r.lists,
            &mut r.text_edits,
        );
        popup()
            .anchor(fill)
            .content(fill)
            .size(Size::fill())
            .handle(&slot)
            .build(&mut cx);
        cx.root().expect("popup declares a root")
    };
    // Open the popup so the content overlay is shown for the layout/paint phases.
    let content = children_of(&store, root)[1];
    store.set_hidden(content, false);
    (store, root)
}

/// A node's direct children (arena sibling chain) — used to reach the content.
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

fn bench_popup(c: &mut Criterion) {
    // build: author the Popup into a fresh NodeStore through a reactive cx.
    c.bench_function("popup/build", |b| {
        b.iter(|| {
            let mut store = NodeStore::new();
            let mut r = Reactive::new();
            let slot: PopupHandleSlot = Rc::new(RefCell::new(None));
            let mut cx = BuildCx::with_reactive(
                &mut store,
                &mut r.states,
                &mut r.bindings,
                &mut r.lists,
                &mut r.text_edits,
            );
            popup()
                .anchor(fill)
                .content(fill)
                .size(Size::fill())
                .handle(&slot)
                .build(&mut cx);
            black_box(&store);
        });
    });

    // layout: place the anchor and the opened content overlay over the region.
    c.bench_function("popup/layout", |b| {
        let (mut store, root) = build_scene();
        let rect = surface();
        let mut scratch = Vec::new();
        b.iter(|| {
            store.layout(root, rect, &mut scratch);
            black_box(&store);
        });
    });

    // paint_tree: lower the laid-out popup into the reused primitive buffer; this
    // carries the overlay top-layer collection/deferral cost (the content is
    // painted last, after the anchor).
    c.bench_function("popup/paint_tree", |b| {
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

criterion_group!(benches, bench_popup);
criterion_main!(benches);
