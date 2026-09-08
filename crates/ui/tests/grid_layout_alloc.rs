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
use std::sync::atomic::Ordering;

use viso_ui::grid::{AdaptiveColumns, GridStyle, TrackMax, TrackSizing};
use viso_ui::layout::{layout, measure};
use viso_ui::{NodeStore, Rect, Size};

/// Counts heap allocations while `ARMED`; off by default so setup allocations
/// are never counted. Mirrors the other alloc packs.
struct CountingAlloc;
// Thread-local counters: cargo runs the `#[test]`s in this binary in parallel
// on separate threads, all sharing this one process-global allocator. A
// process-global armed flag / counter would let a sibling test's allocations,
// happening on another thread while this test is inside its armed measurement
// window, race into this test's count and make the steady-state assertion
// flaky. Scoping arm state and the count to the measuring thread makes each
// test see only its own allocations — the frame path under test is
// synchronous, so every allocation it performs is on the arming thread. The
// `.load`/`.store`/`.fetch_add` API and its `Ordering` argument are kept so the
// call sites and the `GlobalAlloc` impl below are unchanged (the ordering is
// irrelevant for thread-local state and is ignored).
thread_local! {
    static ALLOCS_CELL: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static ARMED_CELL: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}
struct TlsBool;
struct TlsUsize;
impl TlsBool {
    fn load(&self, _: Ordering) -> bool {
        ARMED_CELL.with(std::cell::Cell::get)
    }
    fn store(&self, v: bool, _: Ordering) {
        ARMED_CELL.with(|c| c.set(v));
    }
}
impl TlsUsize {
    fn load(&self, _: Ordering) -> usize {
        ALLOCS_CELL.with(std::cell::Cell::get)
    }
    fn store(&self, v: usize, _: Ordering) {
        ALLOCS_CELL.with(|c| c.set(v));
    }
    fn fetch_add(&self, v: usize, _: Ordering) {
        ALLOCS_CELL.with(|c| c.set(c.get() + v));
    }
}
static ALLOCS: TlsUsize = TlsUsize;
static ARMED: TlsBool = TlsBool;

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

/// Build an `auto-fill minmax(min, 1fr)` adaptive grid of `child_count` cells in
/// a fixed-width container, and run the initial measure pass. The column *count*
/// is solved from the container width each layout pass — this pins that the extra
/// count solve + per-pass `col_tracks` rebuild still touch no heap once warmed.
fn build_adaptive_grid(child_count: usize) -> (NodeStore, u32) {
    let mut store = NodeStore::new();
    let grid = store.alloc_grid(GridStyle {
        columns: Vec::new(),
        rows: vec![TrackSizing::Fixed(60.0)],
        adaptive_columns: Some(AdaptiveColumns::auto_fill(120.0, TrackMax::Fr(1.0))),
        size: Size::fixed(1200.0, 800.0),
        ..Default::default()
    });
    for _ in 0..child_count {
        let k = fill_cell(&mut store);
        store.arena_append_child(grid, k);
    }
    let idx = grid.index();
    let mut scratch = Vec::new();
    measure(&mut store, idx, &mut scratch);
    (store, idx)
}

#[test]
fn steady_adaptive_grid_relayout_is_allocation_free() {
    // 1200px / (120 + 0) = 10 columns; 40 cells wrap onto 4 implicit rows.
    let (mut store, grid) = build_adaptive_grid(40);
    let mut scratch = Vec::new();

    // Warm the id scratch and the pooled `GridScratch` buffers to steady capacity.
    for _ in 0..8 {
        layout(&mut store, grid, SURFACE, &mut scratch);
    }

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
        "a warmed adaptive grid re-layout solves the column count and rebuilds the \
         track template into pooled buffers with no heap"
    );
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
