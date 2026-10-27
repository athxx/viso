//! Microbench for the `DatePicker` control: the cost of one authoring pass
//! (`DatePicker::build`), one `layout`, and one `paint_tree` lowering.
//!
//! A date picker authors its field (the short date beside a calendar glyph) and,
//! hidden below it, the anchored panel holding a whole [`Calendar`] month, so the
//! build pays for the field and the month together. The bench measures the
//! resting, closed picker: the panel is hidden, so `layout`/`paint_tree` see the
//! field only. The fixture pins `today` and the value so every run authors the
//! same month.
//!
//! [`Calendar`]: viso_widgets::Calendar
//!
//! `DatePicker` authors reactive cells (`cx.state`) and structure hooks, so it
//! must build through `BuildCx::with_reactive`. It drives only `viso-ui`, so it
//! needs no facade or render dev-dependency and adds no dependency edge.
//!
//! Run release (`CARGO_TARGET_DIR=/tmp/rust_tmp cargo bench -p viso-widgets --bench date_picker_build`);
//! debug timing is not a perf result.

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use viso_ui::calendar::Date;
use viso_ui::{
    BindingTable, BuildCx, Component, NodeStore, Rect, Role, SemanticProjector, StateStore,
    TextEdits, VirtualLists, paint_tree,
};
use viso_widgets::{DatePicker, date_picker};

const W: f32 = 320.0;
const H: f32 = 400.0;

/// The reactive stores a `with_reactive` `BuildCx` needs, kept alive alongside
/// the node store so the built picker's state references stay valid.
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

/// A picker with a picked date on a fixed month, today pinned.
fn fixture() -> DatePicker {
    date_picker("Start")
        .value(Date::new(2026, 10, 24).expect("valid date"))
        .today(Date::new(2026, 10, 28).expect("valid date"))
        .on_change(|_, _| {})
}

/// Author the picker into a fresh store and return the store plus its root —
/// the input the layout/paint phases below run on.
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
        fixture().build(&mut cx);
        cx.root().expect("date picker declares a root")
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

/// Count the live nodes whose semantics take `role`.
fn count_role(store: &NodeStore, role: Role) -> usize {
    (0..store.arena().slot_count() as u32)
        .filter_map(|i| store.arena().live_id(i))
        .filter(|&id| store.semantics(id).map(|s| s.role) == Some(role))
        .count()
}

/// Check the fixture authors the field and the hidden month, and say what is
/// measured.
fn describe() {
    let (mut store, root) = build_scene();
    let nodes = store.arena().live_count();
    assert_eq!(count_role(&store, Role::ComboBox), 1, "one field");
    let days = count_role(&store, Role::Cell);
    assert_eq!(days, 42, "the panel holds a month of days");
    store.layout(root, surface(), &mut Vec::new());
    let mut primitives = Vec::new();
    paint_tree(&store, root, &mut primitives);
    assert!(!primitives.is_empty(), "the closed field paints");
    eprintln!(
        "date_picker: closed field over a hidden {days}-day month ({nodes} nodes); \
         paint_tree emits {} primitives",
        primitives.len()
    );
}

fn bench_date_picker(c: &mut Criterion) {
    describe();

    // build: author the field and its panel into a fresh NodeStore.
    c.bench_function("date_picker/build", |b| {
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
            fixture().build(&mut cx);
            black_box(&store);
        });
    });

    // layout: lay the closed picker out into the surface rect.
    c.bench_function("date_picker/layout", |b| {
        let (mut store, root) = build_scene();
        let rect = surface();
        let mut scratch = Vec::new();
        b.iter(|| {
            store.layout(root, rect, &mut scratch);
            black_box(&store);
        });
    });

    // paint_tree: lower the laid-out closed picker into the reused buffer.
    c.bench_function("date_picker/paint_tree", |b| {
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

criterion_group!(benches, bench_date_picker);
criterion_main!(benches);
