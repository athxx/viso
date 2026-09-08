//! The allocation-profile dimension of the file tree's expand/collapse reconcile:
//! a warmed reconcile frame with no pending toggle must touch no heap, while a real
//! toggle (a structural change) allocates a bounded, recorded amount.
//!
//! The file tree's reconcile step (reconcile.rs) is the store-mutating half of the
//! handler-writes-intent / reconcile-mutates-store split (ADR 0023/0024): a command
//! only pushes an [`Intent`], and this step — holding `&mut NodeStore` and
//! `&mut VirtualLists` — drains the queue, edits the open set, reflattens the forest
//! into the shared visible-row cell, and drives the keyed list's item count. Its
//! defining fast-path property is the empty-queue early return: a frame with no
//! expand/collapse does *no* work — no reflatten, no allocation. Section 7.1 forbids
//! per-frame allocation on a steady path, and section 7.3 forbids claiming that
//! zero-alloc property without measuring it. [`FileTreeHandle::reconcile`] is public,
//! so this arms a counting global allocator and drives the real handle: a warmed
//! reconcile with an empty intent queue (the per-frame steady case — the tree is not
//! being toggled) allocates nothing.
//!
//! A second test pins the structural counterpart: pushing a toggle intent and
//! reconciling reflattens the visible rows and resizes the list's height cache — a
//! genuine structural change (section 8.1), so it allocates a *bounded, recorded*
//! amount over a small fixture. A regression that made the steady no-toggle path
//! reflatten every frame, or that unbounded the toggle path (cloning the tree, say),
//! is caught either way.
//!
//! Run single-threaded (`--test-threads=1`): the counter is process-wide, so a
//! concurrent test's allocations would contaminate the armed window.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::Ordering;

use viso_ui::{
    BindingTable, BuildCx, Component, NodeStore, PointerButtons, PointerEvent, PointerPhase,
    SemanticProjector, StateStore, TextEdits, VirtualLists,
};
use viso_widgets::{FileTreeHandle, FileTreeHandleSlot, NodeKey, TreeNode, file_tree};

/// Counts heap allocations while `ARMED`; off by default so setup allocations are
/// never counted. Mirrors the other alloc packs (dock_reconcile_alloc.rs).
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

/// The reactive stores a `with_reactive` `BuildCx` needs, kept alive alongside the
/// node store so the built tree's binding/state references stay valid.
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

/// A single root directory (key 0) over `n` file children (keys `1..=n`) — a small
/// directory listing a toggle grows and shrinks.
fn roots(n: u64) -> Vec<TreeNode> {
    let children: Vec<TreeNode> = (1..=n).map(|k| TreeNode::file(NodeKey(k), "f")).collect();
    vec![TreeNode::dir(NodeKey(0), "root", children)]
}

/// Build a `FileTree` over `roots(n)`, open, into a fresh store, returning the store,
/// the reactive stores (kept alive so list state stays valid), and the filled handle.
fn build_scene(n: u64) -> (NodeStore, Reactive, FileTreeHandle) {
    let mut store = NodeStore::new();
    let mut r = Reactive::new();
    let slot: FileTreeHandleSlot = FileTreeHandleSlot::default();
    {
        let mut cx = BuildCx::with_reactive(
            &mut store,
            &mut r.states,
            &mut r.bindings,
            &mut r.lists,
            &mut r.text_edits,
            &mut r.projectors,
        );
        file_tree(roots(n))
            .open(NodeKey(0))
            .handle(FileTreeHandleSlot::clone(&slot))
            .build(&mut cx);
    }
    let handle = slot.borrow().clone().expect("build filled the handle slot");
    (store, r, handle)
}

/// A still (no-button) pointer sample for driving a command that reads no pointer.
fn still_pointer() -> PointerEvent {
    PointerEvent {
        x: 0.0,
        y: 0.0,
        phase: PointerPhase::Move,
        buttons: PointerButtons::NONE,
        modifiers: viso_ui::Modifiers::default(),
    }
}

/// Push a toggle intent for `key` through a throwaway `EventCx`, reusing `r`'s stores.
fn toggle(r: &mut Reactive, handle: &FileTreeHandle, key: NodeKey) {
    let ev = still_pointer();
    let mut cx = viso_ui::EventCx::__new_pointer(&mut r.states, &r.bindings, &ev);
    handle.toggle(&mut cx, key);
}

/// A warmed reconcile frame with no pending toggle allocates nothing. The reconcile
/// step's empty-queue early return is its steady-state contract: a frame in which the
/// user did not expand or collapse anything does no reflatten and touches no heap.
#[test]
fn steady_reconcile_frame_without_toggle_is_allocation_free() {
    let (mut store, mut r, handle) = build_scene(64);

    // Warm every buffer to steady capacity: one real toggle + reconcile grows the
    // visible cell and the list's height cache, and reconciling back leaves them at
    // capacity. After this the open set is back to just the root (open).
    toggle(&mut r, &handle, NodeKey(0));
    handle.reconcile(&mut store, &mut r.lists);
    toggle(&mut r, &handle, NodeKey(0));
    handle.reconcile(&mut store, &mut r.lists);

    // Two armed reconcile frames with an empty intent queue: each hits the early
    // return and does nothing — no reflatten, no set_item_count, no heap.
    let mut frame_allocs = [0usize; 2];
    for slot in frame_allocs.iter_mut() {
        ALLOCS.store(0, Ordering::Relaxed);
        ARMED.store(true, Ordering::Relaxed);
        handle.reconcile(&mut store, &mut r.lists);
        ARMED.store(false, Ordering::Relaxed);
        *slot = ALLOCS.load(Ordering::Relaxed);
    }

    assert_eq!(
        frame_allocs,
        [0, 0],
        "a warmed reconcile with no pending expand/collapse hits the empty-queue \
         early return and touches no heap"
    );
}

/// The structural counterpart: a real toggle reflattens the visible rows and resizes
/// the list's height cache (section 8.1), so it may allocate — but a bounded, recorded
/// amount over a small fixture. This catches a regression that unbounds the toggle
/// path (e.g. cloning the tree per toggle) or that silently made the steady no-toggle
/// path structural.
#[test]
fn toggle_reconcile_allocation_is_bounded() {
    // A small directory (8 children) so the reflatten's growth is a handful of Vec
    // reallocations plus the height-cache resize, not a large listing.
    let (mut store, mut r, handle) = build_scene(8);

    // Warm to steady capacity first (toggle closed then open again), so the armed
    // toggle below reflattens into buffers that are already at capacity — the armed
    // count is the toggle's own irreducible structural allocation, not warm-up.
    toggle(&mut r, &handle, NodeKey(0));
    handle.reconcile(&mut store, &mut r.lists);
    toggle(&mut r, &handle, NodeKey(0));
    handle.reconcile(&mut store, &mut r.lists);

    // One armed toggle + reconcile: collapse the root (9 rows -> 1). The reconcile
    // drains the intent, reflattens the forest into the shared cell, and drives the
    // list's item count — a genuine structural change that does allocate.
    toggle(&mut r, &handle, NodeKey(0));
    ALLOCS.store(0, Ordering::Relaxed);
    ARMED.store(true, Ordering::Relaxed);
    handle.reconcile(&mut store, &mut r.lists);
    ARMED.store(false, Ordering::Relaxed);
    let structural_allocs = ALLOCS.load(Ordering::Relaxed);

    assert!(
        structural_allocs > 0,
        "a toggle reflattens the visible rows and re-drives the list — a genuine \
         structural change that does allocate; got {structural_allocs}"
    );
    // Bounded: draining one intent, reflattening a handful of rows into a fresh Vec,
    // and resizing the list's height cache is a small constant number of allocations,
    // not proportional to the whole tree. A regression that cloned the tree or the
    // full label index per toggle would blow past this.
    assert!(
        structural_allocs <= 32,
        "a single small-directory toggle reconcile allocates a small bounded amount, \
         not a per-node or full-tree clone; got {structural_allocs}"
    );
}
