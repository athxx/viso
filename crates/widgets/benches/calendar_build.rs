//! Microbench for the `Calendar` control: the cost of one authoring pass
//! (`Calendar::build`), one `layout`, and one `paint_tree` lowering.
//!
//! A calendar authors a whole month up front — the header with its two month
//! buttons, the weekday row and six weeks of seven days, each day a focusable
//! cell with its own semantics and handlers — plus the reactive date/month cells
//! and the structure hook that relabels the days. The fixture pins `today` and
//! the value so every run lays out the same month, and turns on week numbers so
//! the widest variant is measured.
//!
//! `Calendar` authors reactive cells (`cx.state`) and a structure hook, so it
//! must build through `BuildCx::with_reactive`. It drives only `viso-ui`, so it
//! needs no facade or render dev-dependency and adds no dependency edge.
//!
//! Run release (`CARGO_TARGET_DIR=/tmp/rust_tmp cargo bench -p viso-widgets --bench calendar_build`);
//! debug timing is not a perf result.

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use viso_ui::calendar::Date;
use viso_ui::{
    BindingTable, BuildCx, Component, NodeStore, Rect, Role, SemanticProjector, StateStore,
    TextEdits, VirtualLists, paint_tree,
};
use viso_widgets::{Calendar, calendar};

const W: f32 = 320.0;
const H: f32 = 340.0;

/// The reactive stores a `with_reactive` `BuildCx` needs, kept alive alongside
/// the node store so the built calendar's state references stay valid.
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

/// A calendar on a fixed month with a picked day, today, a lower bound and week
/// numbers — every per-day look the structure hook marks.
fn fixture() -> Calendar {
    calendar("Due")
        .value(Date::new(2026, 10, 24).expect("valid date"))
        .today(Date::new(2026, 10, 28).expect("valid date"))
        .min(Date::new(2026, 10, 3).expect("valid date"))
        .week_numbers(true)
        .on_change(|_, _| {})
}

/// Author the calendar into a fresh store and return the store plus its root —
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
        cx.root().expect("calendar declares a root")
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

/// Check the fixture authors a full month and paints, and say what is measured.
fn describe() {
    let (mut store, root) = build_scene();
    let nodes = store.arena().live_count();
    let days = (0..store.arena().slot_count() as u32)
        .filter_map(|i| store.arena().live_id(i))
        .filter(|&id| store.semantics(id).map(|s| s.role) == Some(Role::Cell))
        .count();
    assert_eq!(days, 42, "a calendar authors six weeks of days");
    store.layout(root, surface(), &mut Vec::new());
    let mut primitives = Vec::new();
    paint_tree(&store, root, &mut primitives);
    assert!(!primitives.is_empty(), "a laid-out calendar paints");
    eprintln!(
        "calendar: one month ({days} day cells, {nodes} nodes) with week numbers; \
         paint_tree emits {} primitives",
        primitives.len()
    );
}

fn bench_calendar(c: &mut Criterion) {
    describe();

    // build: author the month into a fresh NodeStore through a reactive cx.
    c.bench_function("calendar/build", |b| {
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

    // layout: lay the built month out into the surface rect.
    c.bench_function("calendar/layout", |b| {
        let (mut store, root) = build_scene();
        let rect = surface();
        let mut scratch = Vec::new();
        b.iter(|| {
            store.layout(root, rect, &mut scratch);
            black_box(&store);
        });
    });

    // paint_tree: lower the laid-out month into the reused primitive buffer.
    c.bench_function("calendar/paint_tree", |b| {
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

criterion_group!(benches, bench_calendar);
criterion_main!(benches);
