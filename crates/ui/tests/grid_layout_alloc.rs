//! The allocation-profile dimension of grid re-layout: a warmed steady-state
//! `layout_grid` pass over a stable grid must touch no heap. ADR 0009's
//! allocation-shape consequence noted that `layout_grid` allocated ~12 per-call
//! `Vec`s (placements, occupied bitset, cell regions, per-axis track/auto/size/
//! offset buffers) every frame; section 7.1 forbids per-frame allocation in
//! layout loops, and section 7.3 forbids claiming the hoist without measuring
//! it. This arms a counting global allocator and drives a real re-layout of a
//! 12x20 grid — the same shape the `grid_relayout_12x20` bench times — proving
//! the hoisted `GridScratch` pool is checked out, cleared, and returned with no
//! allocation once warmed.
//!
//! The pool grows only to the deepest grid nesting seen (a nested subgrid checks
//! out a distinct buffer, so nesting never clobbers an ancestor); after the warm
//! passes it is at steady size and every buffer is at capacity, so a warmed pass
//! reuses in place. A regression that re-allocates any of the per-call buffers,
//! or that returns a fresh `Vec` from the prefix-offset helper, is what this pins.
//!
//! Run single-threaded (`--test-threads=1`): the counter is process-wide, so a
//! concurrent test's allocations would contaminate the armed window.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use viso_ui::grid::{GridStyle, TrackSizing};
use viso_ui::layout::{layout, measure};
use viso_ui::{NodeStore, Rect, Size};

/// Counts heap allocations while `ARMED`; off by default so setup allocations
/// are never counted. Mirrors the other alloc packs.
struct CountingAlloc;
static ALLOCS: AtomicUsize = AtomicUsize::new(0);
static ARMED: AtomicBool = AtomicBool::new(false);

// SAFETY: forwards every call to the system allocator unchanged; the only added
// behavior is a relaxed counter increment on allocation while armed.
unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if ARMED.load(Ordering::Relaxed) {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if ARMED.load(Ordering::Relaxed) {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAlloc = CountingAlloc;

const SURFACE: Rect = Rect {
    x: 0.0,
    y: 0.0,
    w: 1200.0,
    h: 800.0,
};

/// A track-less grid node used as a fill-leaf stand-in — it measures to its own
/// `Size` and is placed into its parent grid's cell like any sized child. Mirrors
/// the `grid_layout` bench's `fill_cell`.
fn fill_cell(store: &mut NodeStore) -> viso_ui::NodeId {
    store.alloc_grid(GridStyle {
        columns: Vec::new(),
        rows: Vec::new(),
        size: Size::fill(),
        ..Default::default()
    })
}

/// Build a `cols x rows` Fr grid, fill every cell, and run the initial measure
/// pass. Same shape as the `grid_relayout_12x20` bench.
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

#[test]
fn steady_grid_relayout_is_allocation_free() {
    let (mut store, grid) = build_grid(12, 20); // 240 cells
    let mut scratch = Vec::new();

    // Warm up: the first pass grows the shared id scratch and the thread-local
    // `GridScratch` pool's buffers to their steady capacities; a few more passes
    // confirm nothing else grows. After this the pool holds a cleared-not-freed
    // `GridScratch` whose 12 buffers are all at capacity.
    for _ in 0..8 {
        layout(&mut store, grid, SURFACE, &mut scratch);
    }

    // Two armed re-layouts. Each checks out the pooled `GridScratch`, clears (does
    // not free) its buffers, refills them, lays out all 240 cells, and returns the
    // buffer to the pool — all reuse, no heap. The id scratch is warmed too.
    let mut frame_allocs = [0usize; 2];
    for slot in frame_allocs.iter_mut() {
        ALLOCS.store(0, Ordering::Relaxed);
        ARMED.store(true, Ordering::Relaxed);
        layout(&mut store, grid, SURFACE, &mut scratch);
        ARMED.store(false, Ordering::Relaxed);
        *slot = ALLOCS.load(Ordering::Relaxed);
    }

    assert_eq!(
        frame_allocs,
        [0, 0],
        "a warmed steady-state grid re-layout reuses the hoisted GridScratch with no heap"
    );
}
