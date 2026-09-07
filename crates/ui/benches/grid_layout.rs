//! The `layout` benchmark category for Grid: build a moderately large grid once,
//! then time a steady re-layout frame — the cost the grid adds to a frame when
//! nothing structural changed. Establishes the baseline before any perf claim.
//!
//! Run release (`cargo bench -p viso-ui`); criterion defaults to a release
//! profile. Debug timing is not a performance result.

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use viso_render::Rect;
use viso_ui::grid::{AdaptiveColumns, GridStyle, TrackMax, TrackSizing};
use viso_ui::layout::{layout, measure};
use viso_ui::{NodeStore, Size};

/// A track-less grid node used as a fill leaf stand-in: it measures to its own
/// `Size` and is placed into its parent grid's cell like any sized child.
fn fill_cell(store: &mut NodeStore) -> viso_ui::NodeId {
    store.alloc_grid(GridStyle {
        columns: Vec::new(),
        rows: Vec::new(),
        size: Size::fill(),
        ..Default::default()
    })
}

fn build_grid(cols: usize, rows: usize) -> (NodeStore, u32) {
    build_grid_with(vec![TrackSizing::Fr(1.0); cols], rows)
}

/// Build a `columns.len() x rows` grid with an explicit column template, filling
/// every cell with a fill-leaf stand-in and running the initial measure pass.
fn build_grid_with(columns: Vec<TrackSizing>, rows: usize) -> (NodeStore, u32) {
    let cols = columns.len();
    let mut store = NodeStore::new();
    let grid = store.alloc_grid(GridStyle {
        columns,
        rows: vec![TrackSizing::Fr(1.0); rows],
        size: Size::fixed(1200.0, 800.0),
        ..Default::default()
    });
    for _ in 0..cols * rows {
        let k = fill_cell(&mut store);
        store.arena_append_child(grid, k);
    }
    let idx = grid.index();
    let mut scratch = Vec::new();
    measure(&mut store, idx, &mut scratch);
    (store, idx)
}

fn grid_relayout(c: &mut Criterion) {
    let (mut store, grid) = build_grid(12, 20); // 240 cells
    let surface = Rect {
        x: 0.0,
        y: 0.0,
        w: 1200.0,
        h: 800.0,
    };
    let mut scratch = Vec::new();
    // Startup zero-growth assertion: a stable grid re-laid across frames must not
    // grow the shared scratch buffer — a hot-path regression fails the bench binary.
    layout(&mut store, grid, surface, &mut scratch);
    let cap = scratch.capacity();
    for _ in 0..64 {
        layout(&mut store, grid, surface, &mut scratch);
    }
    assert_eq!(
        scratch.capacity(),
        cap,
        "shared layout scratch must not grow per frame"
    );
    c.bench_function("grid_relayout_12x20", |b| {
        b.iter(|| {
            layout(
                &mut store,
                black_box(grid),
                black_box(surface),
                &mut scratch,
            );
        });
    });

    // A mixed template with Minmax / FitContent columns among the Fr tracks: the
    // fixed-size arms resolve from the content channel before the Fr sweep. Same
    // 12x20 shape, so it baselines the new arms' cost against the pure-Fr grid.
    let mut mixed_cols = vec![
        TrackSizing::Minmax(40.0, 120.0),
        TrackSizing::FitContent(90.0),
    ];
    mixed_cols.extend(std::iter::repeat_n(TrackSizing::Fr(1.0), 10));
    let (mut mstore, mgrid) = build_grid_with(mixed_cols, 20);
    let mut mscratch = Vec::new();
    layout(&mut mstore, mgrid, surface, &mut mscratch);
    c.bench_function("grid_relayout_12x20_minmax", |b| {
        b.iter(|| {
            layout(
                &mut mstore,
                black_box(mgrid),
                black_box(surface),
                &mut mscratch,
            );
        });
    });

    // An `auto-fill minmax(100px, 1fr)` adaptive grid: at 1200px content width the
    // column count solves to 12, so 240 cells over 20 rows — the same shape as the
    // pure-Fr baseline. This times the per-frame extra: the count solve from the
    // container width plus rebuilding the `col_tracks` template each pass.
    let mut astore = NodeStore::new();
    let agrid = astore.alloc_grid(GridStyle {
        columns: Vec::new(),
        rows: vec![TrackSizing::Fr(1.0); 20],
        adaptive_columns: Some(AdaptiveColumns::auto_fill(100.0, TrackMax::Fr(1.0))),
        size: Size::fixed(1200.0, 800.0),
        ..Default::default()
    });
    for _ in 0..12 * 20 {
        let k = fill_cell(&mut astore);
        astore.arena_append_child(agrid, k);
    }
    let agrid = agrid.index();
    let mut ascratch = Vec::new();
    measure(&mut astore, agrid, &mut ascratch);
    layout(&mut astore, agrid, surface, &mut ascratch);
    c.bench_function("grid_relayout_adaptive", |b| {
        b.iter(|| {
            layout(
                &mut astore,
                black_box(agrid),
                black_box(surface),
                &mut ascratch,
            );
        });
    });
}

criterion_group!(benches, grid_relayout);
criterion_main!(benches);
