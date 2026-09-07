//! The allocation-profile dimension of the dock's live seam-drag reconcile: a
//! warmed steady-state reconcile frame must touch no heap.
//!
//! The dock's reconcile step (reconcile.rs) is, per seam, exactly a pair of
//! `NodeStore::set_flex_child_weight` writes followed by a `layout` pass — it
//! rewrites two `Length::Fill` weights in place and re-runs the parent's
//! leftover-space sweep, reusing the caller's layout scratch. Section 7.1 forbids
//! per-frame allocation in a layout loop, and section 7.3 forbids claiming that
//! zero-alloc property without measuring it. `reconcile_seams` is `pub(super)`, so
//! (exactly as the `dock/seam_reconcile_frame` bench does) this arms a counting
//! global allocator and drives the reconcile step's *identical viso-ui foundation*
//! — the two-fill-pane split a dock split authors — proving a warmed drag frame
//! (weight rewrite + relayout) allocates nothing.
//!
//! A second test pins the structural counterpart: adding a live pane to the split
//! and relaying out (the shape a redock produces — a genuine structural change,
//! section 8.1) allocates a *bounded, recorded* amount, so a regression that turns
//! the steady drag path structural, or that unbounds the structural path, is
//! caught either way.
//!
//! Run single-threaded (`--test-threads=1`): the counter is process-wide, so a
//! concurrent test's allocations would contaminate the armed window.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use viso_ui::grid::GridStyle;
use viso_ui::{
    Align, Axis, BindingTable, BoxStyle, BuildCx, FlexStyle, Inset, LeafStyle, NodeId, NodeStore,
    Rect, SemanticProjector, Size, StateStore, TextEdits, VirtualLists,
};

/// Counts heap allocations while `ARMED`; off by default so setup allocations
/// are never counted. Mirrors the other alloc packs (grid_layout_alloc.rs).
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

/// The reactive stores a `with_reactive` `BuildCx` needs, kept alive alongside the
/// node store so the built split's binding/state references stay valid.
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

/// A row flex of two fill panes — the shape one dock split authors, and the exact
/// foundation the reconcile step drives per seam — plus the two pane node ids a
/// seam reconcile rewrites. Mirrors the bench's `build_split_scene`.
fn build_split_scene() -> (NodeStore, NodeId, NodeId, NodeId) {
    let mut store = NodeStore::new();
    let mut r = Reactive::new();
    let mut pane_a = None;
    let mut pane_b = None;
    let root = {
        let mut cx = BuildCx::with_reactive(
            &mut store,
            &mut r.states,
            &mut r.bindings,
            &mut r.lists,
            &mut r.text_edits,
            &mut r.projectors,
        );
        let a_out = &mut pane_a;
        let b_out = &mut pane_b;
        cx.flex(
            FlexStyle {
                axis: Axis::Row,
                gap: 0.0,
                padding: Inset::all(0.0),
                align: Align::Stretch,
                size: Size::fill(),
                style: BoxStyle::NONE,
            },
            |cx| {
                let a = cx.leaf(LeafStyle {
                    size: Size::fill(),
                    style: BoxStyle::NONE,
                });
                let b = cx.leaf(LeafStyle {
                    size: Size::fill(),
                    style: BoxStyle::NONE,
                });
                *a_out = Some(a.id());
                *b_out = Some(b.id());
            },
        )
        .id()
    };
    (
        store,
        root,
        pane_a.expect("pane A built"),
        pane_b.expect("pane B built"),
    )
}

/// A warmed steady-state seam-drag reconcile frame — the per-seam work of
/// `reconcile_seams`: rewrite both pane weights, then re-lay-out the split —
/// allocates nothing. This is the section 8 zero-alloc proof: a fill child
/// contributes no natural size, so a weight rewrite marks only `LAYOUT | PAINT`
/// and the relayout re-runs the parent's fill sweep into the reused scratch with
/// no heap.
#[test]
fn steady_seam_reconcile_frame_is_allocation_free() {
    let (mut store, root, pane_a, pane_b) = build_split_scene();
    let mut scratch = Vec::new();

    // Warm the layout scratch and the store's internal buffers to steady capacity
    // by driving several full reconcile frames at varying fractions.
    let mut f = 0.5_f32;
    for _ in 0..8 {
        f = if f > 0.7 { 0.3 } else { f + 0.05 };
        store.set_flex_child_weight(pane_a, Axis::Row, f);
        store.set_flex_child_weight(pane_b, Axis::Row, 1.0 - f);
        store.layout(root, SURFACE, &mut scratch);
    }

    // Two armed reconcile frames. Each rewrites both weights in place and re-runs
    // the parent's leftover-space sweep into the warmed scratch — all reuse, no
    // heap.
    let mut frame_allocs = [0usize; 2];
    for slot in frame_allocs.iter_mut() {
        f = if f > 0.7 { 0.3 } else { f + 0.05 };
        ALLOCS.store(0, Ordering::Relaxed);
        ARMED.store(true, Ordering::Relaxed);
        store.set_flex_child_weight(pane_a, Axis::Row, f);
        store.set_flex_child_weight(pane_b, Axis::Row, 1.0 - f);
        store.layout(root, SURFACE, &mut scratch);
        ARMED.store(false, Ordering::Relaxed);
        *slot = ALLOCS.load(Ordering::Relaxed);
    }

    assert_eq!(
        frame_allocs,
        [0, 0],
        "a warmed steady-state seam-drag reconcile frame rewrites two Fill weights \
         in place and re-runs the parent fill sweep into reused scratch with no heap"
    );
}

/// The structural counterpart: a redock genuinely changes structure (section 8.1),
/// so it may allocate — but a bounded, recorded amount. This appends a third live
/// fill pane to the warmed split (the shape a redock produces) and relays out,
/// asserting the allocation count is non-zero yet bounded, so a regression that
/// unbounds the structural path — or that silently made the *steady* drag path
/// structural — is caught.
#[test]
fn structural_redock_relayout_allocation_is_bounded() {
    let (mut store, root, pane_a, pane_b) = build_split_scene();
    let mut scratch = Vec::new();

    // Warm the split to steady state, exactly as the steady-state test does.
    let mut f = 0.5_f32;
    for _ in 0..8 {
        f = if f > 0.7 { 0.3 } else { f + 0.05 };
        store.set_flex_child_weight(pane_a, Axis::Row, f);
        store.set_flex_child_weight(pane_b, Axis::Row, 1.0 - f);
        store.layout(root, SURFACE, &mut scratch);
    }

    // A redock appends a live pane into the split's region and relays out — a real
    // structural change (a new node + a new child edge), so a bounded allocation is
    // legitimate. Rebalance the existing weights to make room, as a redock would.
    ALLOCS.store(0, Ordering::Relaxed);
    ARMED.store(true, Ordering::Relaxed);
    // A track-less fill grid stands in for the redocked pane's region: it measures
    // to its own fill `Size` and drops into the parent flex like any child. (The
    // leaf/flex constructors live on `BuildCx`; `alloc_grid` is the `NodeStore`
    // node constructor a store-mutating reconcile step has to hand — the same
    // stand-in `fill_cell` uses in the grid alloc pack.)
    let pane_c = store.alloc_grid(GridStyle {
        columns: Vec::new(),
        rows: Vec::new(),
        size: Size::fill(),
        ..Default::default()
    });
    store.arena_append_child(root, pane_c);
    store.set_flex_child_weight(pane_a, Axis::Row, 0.4);
    store.set_flex_child_weight(pane_b, Axis::Row, 0.3);
    store.set_flex_child_weight(pane_c, Axis::Row, 0.3);
    store.layout(root, SURFACE, &mut scratch);
    ARMED.store(false, Ordering::Relaxed);
    let structural_allocs = ALLOCS.load(Ordering::Relaxed);

    assert!(
        structural_allocs > 0,
        "a redock adds a node and a child edge — a genuine structural change that \
         does allocate; got {structural_allocs}"
    );
    // Bounded: allocating a single new node's storage plus its arena edge is a
    // handful of allocations, not proportional to the tree. A regression that
    // rebuilt the subtree, or cloned the tree per redock, would blow past this.
    assert!(
        structural_allocs <= 32,
        "a single-pane redock's relayout allocates a small bounded amount, not a \
         per-node or full-subtree rebuild; got {structural_allocs}"
    );
}
