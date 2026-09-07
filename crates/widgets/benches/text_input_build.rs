//! Microbench skeleton for the `TextInput` interactive control: the cost of one
//! authoring pass (`TextInput::build`), one `layout`, and one `paint_tree`
//! lowering.
//!
//! This is the section 71 microbench slot for the sixth Tier 2 (interactive)
//! control — a single-line editable text field — and copies the
//! `slider_build.rs` template. It is deliberately a skeleton this slice: it
//! exercises the real `build` -> `layout` -> `paint_tree` path so a regression
//! is measurable, but the baseline numbers are recorded in a later slice (per
//! the plan: establish the framework first).
//!
//! `TextInput` is a single focusable leaf backed by an edit `Buffer` registered
//! into the reactive cx's `TextEdits`, so it must build through
//! `BuildCx::with_reactive` (a plain `BuildCx::new` has no buffer registry). It
//! attaches pointer/key/IME handlers driven by an `on_change` callback — the
//! bench authors one so the handler-boxing cost is included. It drives only
//! `viso-ui`, so it needs no facade or render dev-dependency and adds no
//! dependency edge.
//!
//! Run release (`CARGO_TARGET_DIR=/tmp/rust_tmp cargo bench -p viso-widgets`);
//! criterion defaults to a release profile. Debug timing is not a perf result
//! (AGENTS section 36).

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use viso_ui::{
    BindingTable, BuildCx, Component, NodeStore, Rect, SemanticProjector, StateStore, TextEdits,
    VirtualLists, paint_tree,
};
use viso_widgets::text_input;

const W: f32 = 200.0;
const H: f32 = 64.0;

/// The reactive stores a `with_reactive` `BuildCx` needs, kept alive alongside
/// the node store so the built field's buffer/binding references stay valid.
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

/// Author a single `TextInput` (with an `on_change`) into a fresh store and
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
            &mut r.projectors,
        );
        text_input("Name")
            .value("Ann")
            .on_change(|_, _| {})
            .build(&mut cx);
        cx.root().expect("text_input declares a root")
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

fn bench_text_input(c: &mut Criterion) {
    // build: author the TextInput into a fresh NodeStore through a reactive cx.
    c.bench_function("text_input/build", |b| {
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
            text_input("Name")
                .value("Ann")
                .on_change(|_, _| {})
                .build(&mut cx);
            black_box(&store);
        });
    });

    // layout: lay the built field out into the surface rect.
    c.bench_function("text_input/layout", |b| {
        let (mut store, root) = build_scene();
        let rect = surface();
        let mut scratch = Vec::new();
        b.iter(|| {
            store.layout(root, rect, &mut scratch);
            black_box(&store);
        });
    });

    // paint_tree: lower the laid-out field into the reused primitive buffer.
    c.bench_function("text_input/paint_tree", |b| {
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

criterion_group!(benches, bench_text_input);
criterion_main!(benches);
