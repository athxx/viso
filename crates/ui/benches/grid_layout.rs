//! The `layout` benchmark category for Grid: build a moderately large grid once,
//! then time a steady re-layout frame — the cost the grid adds to a frame when
//! nothing structural changed. Establishes the baseline before any perf claim.
//!
//! Run release (`cargo bench -p viso-ui`); criterion defaults to a release
//! profile. Debug timing is not a performance result.

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use viso_render::Rect;
use viso_ui::grid::{GridStyle, TrackSizing};
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
    let mut store = NodeStore::new();
    let grid = store.alloc_grid(GridStyle {
        columns: vec![TrackSizing::Fr(1.0); cols],
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
}

criterion_group!(benches, grid_relayout);
criterion_main!(benches);
