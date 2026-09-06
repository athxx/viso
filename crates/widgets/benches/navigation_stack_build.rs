//! Microbench skeleton for the `NavigationStack` page-stack control: the cost of
//! one authoring pass (`NavigationStack::build`), one `layout`, and one
//! `paint_tree` lowering.
//!
//! This is the section 71 microbench slot for the second Tier 4 (navigation /
//! overlay) widget — a stack of pages of which only the top is visible — and
//! copies the `tabs_build.rs` template. It exercises the real `build` ->
//! `layout` -> `paint_tree` path so a regression is measurable; the baseline
//! numbers are recorded in a later slice (per the plan: establish the framework
//! first).
//!
//! `NavigationStack` authors one `Int` depth cell (`cx.state`) and binds it to the
//! stack's `PAINT`, so it must build through `BuildCx::with_reactive` (a plain
//! `BuildCx::new` panics on `cx.state`). It also attaches a key handler for the
//! back gesture driven by an `on_navigate` callback — the bench authors one so the
//! handler-boxing cost is included — and three page builders so the composed
//! subtree (an overlapping stretched column, all pages built once with the
//! non-top pages folded out through the `hidden` seam) is realistic. It drives only
//! `viso-ui`, so it needs no facade or render dev-dependency and adds no
//! dependency edge.
//!
//! Run release (`CARGO_TARGET_DIR=/tmp/rust_tmp cargo bench -p viso-widgets`);
//! criterion defaults to a release profile. Debug timing is not a perf result
//! (AGENTS section 36).

use std::cell::RefCell;
use std::hint::black_box;
use std::rc::Rc;

use criterion::{Criterion, criterion_group, criterion_main};
use viso_ui::{
    BindingTable, BoxStyle, BuildCx, Component, LeafStyle, NodeStore, Rect, Size, StateStore,
    TextEdits, VirtualLists, paint_tree,
};
use viso_widgets::{NavHandleSlot, navigation_stack};

const W: f32 = 400.0;
const H: f32 = 240.0;

/// The reactive stores a `with_reactive` `BuildCx` needs, kept alive alongside
/// the node store so the built stack's binding/state references stay valid.
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

/// A page's content: a single fill leaf, so each page composes a realistic
/// subtree rather than an empty node.
fn page(cx: &mut BuildCx<'_>) {
    cx.leaf(LeafStyle {
        size: Size::fill(),
        style: BoxStyle::NONE,
    });
}

/// Author a single `NavigationStack` (three pages, an app-captured handle, and an
/// `on_navigate`) into a fresh store and return the store plus its root — the
/// input the phases below run on.
fn build_scene() -> (NodeStore, viso_ui::NodeId) {
    let mut store = NodeStore::new();
    let mut r = Reactive::new();
    let slot: NavHandleSlot = Rc::new(RefCell::new(None));
    let root = {
        let mut cx = BuildCx::with_reactive(
            &mut store,
            &mut r.states,
            &mut r.bindings,
            &mut r.lists,
            &mut r.text_edits,
        );
        navigation_stack()
            .page(page)
            .page(page)
            .page(page)
            .handle(&slot)
            .on_navigate(|_, _| {})
            .build(&mut cx);
        cx.root().expect("navigation stack declares a root")
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

fn bench_navigation_stack(c: &mut Criterion) {
    // build: author the NavigationStack into a fresh NodeStore through a reactive cx.
    c.bench_function("navigation_stack/build", |b| {
        b.iter(|| {
            let mut store = NodeStore::new();
            let mut r = Reactive::new();
            let slot: NavHandleSlot = Rc::new(RefCell::new(None));
            let mut cx = BuildCx::with_reactive(
                &mut store,
                &mut r.states,
                &mut r.bindings,
                &mut r.lists,
                &mut r.text_edits,
            );
            navigation_stack()
                .page(page)
                .page(page)
                .page(page)
                .handle(&slot)
                .on_navigate(|_, _| {})
                .build(&mut cx);
            black_box(&store);
        });
    });

    // layout: place the top page over the region; the hidden pages fold to zero.
    c.bench_function("navigation_stack/layout", |b| {
        let (mut store, root) = build_scene();
        let rect = surface();
        let mut scratch = Vec::new();
        b.iter(|| {
            store.layout(root, rect, &mut scratch);
            black_box(&store);
        });
    });

    // paint_tree: lower the laid-out stack into the reused primitive buffer.
    c.bench_function("navigation_stack/paint_tree", |b| {
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

criterion_group!(benches, bench_navigation_stack);
criterion_main!(benches);
