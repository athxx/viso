//! Microbench for the `DataGrid` control over a realistic table — 1000 data rows
//! of 8 text columns in an 800x600 viewport: the cost of one authoring pass, one
//! `layout` and one `paint_tree` of the mounted screenful, and — the interactive
//! hot path — one page of scrolling.
//!
//! The grid is a virtual list scrolling both ways, so its cost story is "what
//! does a screenful cost", not "what do 8000 cells cost": only the rows in view
//! (plus overscan and the pinned header row) mount, each building the cells of
//! the columns in view. Four phases:
//!
//! - `data_grid/build` authors the grid through the public [`data_grid`] builder:
//!   the viewport, its handlers, the column geometry and the structure hook. No
//!   row is built until the first list reconcile.
//! - `data_grid/layout` lays the mounted screenful out (after the first frames
//!   mounted its rows and cells).
//! - `data_grid/paint_tree` lowers the laid-out screenful into the primitive
//!   buffer.
//! - `data_grid/scroll_page_frame` times one wheel page down (or back up): the
//!   wheel routes to the viewport, the state flush runs, the list reconcile
//!   recycles every row that left the window and rebuilds its cells at its new
//!   data row, and the frame relays out. Pages alternate down and up so every
//!   frame crosses a full page of rows.
//!
//! It drives only `viso-ui` + `viso-widgets`, so it needs no facade or render
//! dev-dependency and adds no dependency edge. Run release
//! (`CARGO_TARGET_DIR=/tmp/rust_tmp cargo bench -p viso-widgets --bench data_grid_build`);
//! debug timing is not a perf result.

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use viso_ui::virtual_list::reconcile;
use viso_ui::{
    BindingTable, BuildCx, EffectStore, Modifiers, NodeId, NodeStore, Rect, Role,
    SemanticProjector, Sizing, StateStore, TextEdits, VirtualLists, paint_tree,
    run_structure_hooks,
};
use viso_ui::{ScrollEvent, ScrollPhase, ScrollRouter};
use viso_widgets::{DataGrid, data_grid, grid_column};

const W: f32 = 800.0;
const H: f32 = 600.0;
const ROWS: usize = 1000;
const COLUMNS: usize = 8;
/// One page of the wheel: the viewport less the pinned header row.
const PAGE: f32 = H - 32.0;

/// A 1000x8 grid of text cells, the first two columns sortable and the numeric
/// ones aligned to their end — the shape an orders table declares.
fn fixture() -> DataGrid {
    let columns = (0..COLUMNS)
        .map(|c| {
            let column = grid_column(format!("Column {c}")).width(120.0);
            match c {
                0 | 1 => column.sortable(),
                6 | 7 => column.align_end(),
                _ => column,
            }
        })
        .collect();
    data_grid("Orders", ROWS, columns)
        .text(|row, column| format!("r{row}c{column}"))
        .size(Sizing::fixed(W, H))
        .on_sort(|_, _, _| {})
        .on_select(|_, _| {})
}

/// A built grid with the stores a frame drives it through, so a bench can
/// route scrolls and settle the frame the way the window driver does.
struct Frame {
    store: NodeStore,
    states: StateStore,
    bindings: BindingTable,
    effects: EffectStore,
    lists: VirtualLists,
    /// Kept alive beside the store; the frame edits no text and reads no
    /// semantics.
    _text_edits: TextEdits,
    _projectors: SemanticProjector,
    root: NodeId,
    scratch: Vec<u32>,
    changed: Vec<viso_ui::StateId>,
}

impl Frame {
    /// Build the grid and run frames until its first screenful is mounted.
    fn new() -> Self {
        let mut store = NodeStore::new();
        let mut states = StateStore::new();
        let mut bindings = BindingTable::new();
        let mut lists = VirtualLists::new();
        let mut text_edits = TextEdits::new();
        let mut projectors = SemanticProjector::new();
        let root = {
            let mut cx = BuildCx::with_reactive(
                &mut store,
                &mut states,
                &mut bindings,
                &mut lists,
                &mut text_edits,
                &mut projectors,
            );
            fixture().build(&mut cx);
            cx.root().expect("data grid declares a root")
        };
        let mut frame = Frame {
            store,
            states,
            bindings,
            effects: EffectStore::default(),
            lists,
            _text_edits: text_edits,
            _projectors: projectors,
            root,
            scratch: Vec::new(),
            changed: Vec::new(),
        };
        frame.store.layout(root, surface(), &mut frame.scratch);
        // The first reconcile sees the viewport's extent and mounts the window;
        // a few more settle the cells across and the sticky header.
        for _ in 0..4 {
            if frame.frame() == 0 {
                break;
            }
        }
        frame
    }

    /// One frame's flush, list reconcile and relayout; the rows (re)bound.
    fn frame(&mut self) -> u32 {
        self.states.take_pending(&mut self.changed);
        self.store
            .flush_state_transactions(&self.changed, &self.bindings);
        run_structure_hooks(
            &mut self.store,
            &mut self.states,
            &mut self.bindings,
            &mut self.effects,
            &mut self.lists,
            &self.changed,
        );
        let bound = reconcile(
            &mut self.store,
            &mut self.lists,
            &mut self.states,
            &mut self.bindings,
            &mut self.effects,
        );
        self.store.layout(self.root, surface(), &mut self.scratch);
        bound
    }

    /// A wheel turn of `delta_y` over the grid's middle, then the frame it
    /// asks for; the rows (re)bound.
    fn wheel(&mut self, delta_y: f32) -> u32 {
        ScrollRouter::route(
            &mut self.store,
            self.root,
            ScrollEvent {
                x: W * 0.5,
                y: H * 0.5,
                delta_x: 0.0,
                delta_y,
                modifiers: Modifiers::default(),
                phase: ScrollPhase::None,
            },
        );
        self.frame()
    }

    /// The first data row mounted (past the header row).
    fn first_row(&self) -> usize {
        self.lists
            .get(self.root)
            .expect("the grid is a virtual list")
            .mounted_rows()
            .map(|(logical, _)| logical)
            .filter(|&logical| logical > 0)
            .min()
            .expect("rows are mounted")
    }

    /// How many grid cells are built.
    fn cells(&self) -> usize {
        (0..self.store.arena().slot_count() as u32)
            .filter_map(|i| self.store.arena().live_id(i))
            .filter(|&id| self.store.semantics(id).map(|s| s.role) == Some(Role::GridCell))
            .count()
    }
}

fn surface() -> Rect {
    Rect {
        x: 0.0,
        y: 0.0,
        w: W,
        h: H,
    }
}

/// Check only a screenful mounts, that a page scroll rebinds rows, and say what
/// is measured.
fn describe() {
    let mut frame = Frame::new();
    let rows = frame.lists.get(frame.root).map_or(0, |l| l.mounted_count());
    let cells = frame.cells();
    assert!(
        rows > 1 && rows < 60,
        "only a window mounts, saw {rows} rows"
    );
    assert!(
        cells > 0 && cells < ROWS * COLUMNS / 10,
        "only the cells in view build, saw {cells}"
    );
    let mut primitives = Vec::new();
    paint_tree(&frame.store, frame.root, &mut primitives);
    assert!(!primitives.is_empty(), "the mounted grid paints");
    let top = frame.first_row();
    let bound = frame.wheel(PAGE);
    let paged = frame.first_row();
    assert!(bound > 0 && paged > top, "a page down rebinds rows");
    assert!(frame.wheel(-PAGE) > 0, "a page up rebinds rows");
    assert_eq!(frame.first_row(), top, "a page up returns");
    eprintln!(
        "data_grid: {ROWS}x{COLUMNS} grid in {W}x{H}, {rows} rows / {cells} cells mounted, \
         paint_tree emits {} primitives; scroll_page_frame = wheel {PAGE}pt -> list reconcile \
         rebinds {bound} rows (first mounted row {top} -> {paged}) -> relayout",
        primitives.len()
    );
}

fn bench_data_grid(c: &mut Criterion) {
    describe();

    // build: author the grid into a fresh NodeStore through a reactive cx.
    c.bench_function("data_grid/build", |b| {
        b.iter(|| {
            let mut store = NodeStore::new();
            let mut states = StateStore::new();
            let mut bindings = BindingTable::new();
            let mut lists = VirtualLists::new();
            let mut text_edits = TextEdits::new();
            let mut projectors = SemanticProjector::new();
            let mut cx = BuildCx::with_reactive(
                &mut store,
                &mut states,
                &mut bindings,
                &mut lists,
                &mut text_edits,
                &mut projectors,
            );
            fixture().build(&mut cx);
            black_box(&store);
        });
    });

    // layout: lay the mounted screenful out into the surface rect.
    c.bench_function("data_grid/layout", |b| {
        let mut frame = Frame::new();
        let rect = surface();
        b.iter(|| {
            frame.store.layout(frame.root, rect, &mut frame.scratch);
            black_box(&frame.store);
        });
    });

    // paint_tree: lower the laid-out screenful into the reused primitive buffer.
    c.bench_function("data_grid/paint_tree", |b| {
        let frame = Frame::new();
        let mut primitives = Vec::new();
        // Warm the buffer to steady capacity so the timed loop does not grow it.
        paint_tree(&frame.store, frame.root, &mut primitives);
        b.iter(|| {
            primitives.clear();
            paint_tree(&frame.store, frame.root, &mut primitives);
            black_box(&primitives);
        });
    });

    // scroll_page_frame: one wheel page, then the frame that remounts the rows
    // it crossed. Down and up alternate so every frame rebinds a full page.
    c.bench_function("data_grid/scroll_page_frame", |b| {
        let mut frame = Frame::new();
        let mut down = true;
        let mut bound = 0;
        b.iter(|| {
            bound += frame.wheel(if down { PAGE } else { -PAGE });
            down = !down;
            black_box(&frame.store);
        });
        assert!(bound > 0, "the timed pages rebound rows");
    });
}

criterion_group!(benches, bench_data_grid);
criterion_main!(benches);
