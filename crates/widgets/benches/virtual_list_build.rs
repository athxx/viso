//! Microbench skeleton for the `VirtualList` virtualized-list control: the cost
//! of one authoring pass (`VirtualList::build`), one `layout`, and one
//! `paint_tree` lowering.
//!
//! This is the section 71 microbench slot for the second Tier 3 (layout
//! structure) control — a virtualized scrolling list — and copies the
//! `text_input_build.rs` reactive template. It is deliberately a skeleton this
//! slice: it exercises the real `build` -> `layout` -> `paint_tree` path so a
//! regression is measurable, but the baseline numbers are recorded in a later
//! slice (per the plan: establish the framework first).
//!
//! `VirtualList` registers per-list state into the reactive cx's `VirtualLists`,
//! so it must build through `BuildCx::with_reactive` (a plain `BuildCx::new` has
//! no list registry). It declares an `item_count` and a per-row builder; the
//! bench authors 100k logical rows so the boxing of the row builder is included,
//! but only the viewport + fixed-extent canvas are mounted at build time (no row
//! is built until the driver's first reconcile). It drives only `viso-ui`, so it
//! needs no facade or render dev-dependency and adds no dependency edge.
//!
//! Run release (`CARGO_TARGET_DIR=/tmp/rust_tmp cargo bench -p viso-widgets`);
//! criterion defaults to a release profile. Debug timing is not a perf result
//! (AGENTS section 36).

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use viso_ui::{
    Axis, BindingTable, BuildCx, Component, LeafStyle, Length, NodeStore, Rect, Size, StateStore,
    TextEdits, VirtualLists, paint_tree,
};
use viso_widgets::{VirtualListViewStyle, virtual_list};

const W: f32 = 200.0;
const H: f32 = 300.0;
const ITEM_COUNT: usize = 100_000;
const ROW_H: f32 = 30.0;

/// The reactive stores a `with_reactive` `BuildCx` needs, kept alive alongside
/// the node store so the built list's registered state stays valid.
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

/// One row's body: a fill-width, fixed-height leaf. Cold — the driver invokes it
/// only when a row is (re)mounted, never in the timed `build`/`paint_tree` loops
/// here (a fresh build mounts no rows).
fn row(_index: usize, cx: &mut BuildCx<'_>) {
    cx.leaf(LeafStyle {
        size: Size {
            width: Length::fill(),
            height: Length::Fixed(ROW_H),
        },
        ..Default::default()
    });
}

fn list_style() -> VirtualListViewStyle {
    VirtualListViewStyle {
        axis: Axis::Column,
        size: Size::fixed(W, H),
        estimated_row: ROW_H,
        ..Default::default()
    }
}

/// Author a single `VirtualList` (100k logical rows) into a fresh store and
/// return the store plus its root — the input the layout/paint phases run on.
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
        virtual_list(list_style())
            .items(ITEM_COUNT, row)
            .build(&mut cx);
        cx.root().expect("virtual_list declares a root")
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

fn bench_virtual_list(c: &mut Criterion) {
    // build: author the VirtualList into a fresh NodeStore through a reactive cx.
    c.bench_function("virtual_list/build", |b| {
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
            virtual_list(list_style())
                .items(ITEM_COUNT, row)
                .build(&mut cx);
            black_box(&store);
        });
    });

    // layout: lay the built viewport + canvas out into the surface rect.
    c.bench_function("virtual_list/layout", |b| {
        let (mut store, root) = build_scene();
        let rect = surface();
        let mut scratch = Vec::new();
        b.iter(|| {
            store.layout(root, rect, &mut scratch);
            black_box(&store);
        });
    });

    // paint_tree: lower the laid-out list into the reused primitive buffer.
    c.bench_function("virtual_list/paint_tree", |b| {
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

criterion_group!(benches, bench_virtual_list);
criterion_main!(benches);
