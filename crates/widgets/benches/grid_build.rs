//! Microbench skeleton for the `Grid` two-dimensional layout widget: the cost of
//! one authoring pass (`Grid::build`), one `layout`, and one `paint_tree`
//! lowering.
//!
//! This is the section 71 microbench slot for the third Tier 3 (layout
//! structure) widget — a track-based grid — and copies the `scroll_build.rs`
//! template. It exercises the real `build` -> `layout` -> `paint_tree` path so a
//! regression is measurable; the baseline numbers are recorded in a later slice
//! (per the plan: establish the framework first).
//!
//! It drives only `viso-ui` — `build`/`layout`/`paint_tree` all live there — so
//! the bench needs no facade or render dev-dependency and adds no dependency
//! edge. `Grid` is non-reactive (like `View`/`Scroll`, unlike `VirtualList`), so
//! a plain `BuildCx::new` suffices — no list registry / reactive stores.
//!
//! Run release (`CARGO_TARGET_DIR=/tmp/rust_tmp cargo bench -p viso-widgets`);
//! criterion defaults to a release profile. Debug timing is not a perf result
//! (AGENTS section 36).

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use viso_ui::{
    BoxStyle, BuildCx, Component, GridPlacement, LeafStyle, NodeStore, Rect, Size, TrackSizing,
    paint_tree,
};
use viso_widgets::{GridViewStyle, grid};

const W: f32 = 240.0;
const H: f32 = 180.0;
/// Cells authored into the grid: a 4x3 dashboard's worth of leaves, so the
/// track-solver runs over a realistic template rather than a single cell.
const COLS: u16 = 4;
const ROWS: u16 = 3;

/// The dashboard grid style used by every phase: four fractional columns, three
/// fixed rows, small gaps and padding — the shape a real dashboard declares.
fn grid_style() -> GridViewStyle {
    GridViewStyle {
        columns: vec![TrackSizing::Fr(1.0); COLS as usize],
        rows: vec![TrackSizing::Fixed(48.0); ROWS as usize],
        column_gap: 8.0,
        row_gap: 8.0,
        size: Size::fixed(W, H),
        background: BoxStyle::NONE,
        ..Default::default()
    }
}

/// Author the grid's cells: one fill leaf per cell, auto-flowing row-major. Cold
/// — run once per `build`, never on the timed `layout`/`paint_tree` loops.
fn cells(cx: &mut BuildCx<'_>) {
    for _ in 0..(COLS * ROWS) {
        cx.leaf(LeafStyle {
            size: Size::fill(),
            style: BoxStyle::NONE,
        });
    }
}

/// Author a `Grid` with a full template of auto-flowed cells into a fresh store
/// and return the store plus its root — the input every phase below runs on.
fn build_scene() -> (NodeStore, viso_ui::NodeId) {
    let g = grid(grid_style()).children(cells);

    let mut store = NodeStore::new();
    let root = {
        let mut cx = BuildCx::new(&mut store);
        g.build(&mut cx);
        cx.root().expect("grid declares a root")
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

fn bench_grid(c: &mut Criterion) {
    // build: author the Grid subtree into a fresh NodeStore.
    c.bench_function("grid/build", |b| {
        b.iter(|| {
            let mut store = NodeStore::new();
            let mut cx = BuildCx::new(&mut store);
            grid(grid_style()).children(cells).build(&mut cx);
            black_box(&store);
        });
    });

    // build_placed: author the same template but pin every cell explicitly, so
    // the placement path (not just auto-flow) is exercised.
    c.bench_function("grid/build_placed", |b| {
        b.iter(|| {
            let mut store = NodeStore::new();
            let mut cx = BuildCx::new(&mut store);
            grid(grid_style())
                .children(|cx| {
                    for row in 0..ROWS {
                        for col in 0..COLS {
                            cx.place(GridPlacement {
                                column: Some(col),
                                row: Some(row),
                                column_span: 1,
                                row_span: 1,
                            });
                            cx.leaf(LeafStyle {
                                size: Size::fill(),
                                style: BoxStyle::NONE,
                            });
                        }
                    }
                })
                .build(&mut cx);
            black_box(&store);
        });
    });

    // layout: solve the track sizing and place every cell into the surface rect.
    c.bench_function("grid/layout", |b| {
        let (mut store, root) = build_scene();
        let rect = surface();
        let mut scratch = Vec::new();
        b.iter(|| {
            store.layout(root, rect, &mut scratch);
            black_box(&store);
        });
    });

    // paint_tree: lower the laid-out grid into the reused primitive buffer.
    c.bench_function("grid/paint_tree", |b| {
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

criterion_group!(benches, bench_grid);
criterion_main!(benches);
