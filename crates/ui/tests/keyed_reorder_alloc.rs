//! The allocation-profile dimension of the keyed virtual-list reorder path: a
//! warmed within-window reorder (a pure permutation of the visible key set) must
//! MOVE the surviving hosts, not rebuild them — and moving is a re-anchor
//! (`set_row_offset` + a dirty mark), which touches no heap. section 12.4 lists
//! stable item keys as a virtualization contract clause; section 35 / section 7.3
//! forbid asserting "reorder reuses without allocating" without measuring it, so
//! this arms a counting global allocator and drives a real keyed reconcile over
//! a within-window swap.
//!
//! The reconcile's `new_keys` scratch is taken from and restored to the list
//! state each pass (grows once on the first crossing, then reused in place), and
//! a survivor's host is re-anchored rather than rebuilt — so a warmed reorder
//! touches no heap. A regression that rebuilds survivors (freeing + re-authoring
//! their bodies), or that reallocates `new_keys`, is what this pins.
//!
//! Run single-threaded (`--test-threads=1`): the counter is process-wide, so a
//! concurrent test's allocations would contaminate the armed window.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::Ordering;

use viso_ui::virtual_list::{ItemKey, absorb_measurements, reconcile};
use viso_ui::{
    Axis, BindingTable, BoxStyle, BuildCx, DirtyClass, EffectStore, LeafStyle, Length, NodeId,
    NodeStore, Rect, SemanticProjector, Size, StateStore, TextEdits, VirtualListStyle,
    VirtualLists,
};

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

const ITEMS: usize = 1000;
const ROW_H: f32 = 30.0;
const VIEWPORT_H: f32 = 300.0;
const SURFACE: Rect = Rect {
    x: 0.0,
    y: 0.0,
    w: 100.0,
    h: VIEWPORT_H,
};

/// A keyed virtual list plus the reactive stores it drives, and the shared
/// index→key mapping the test rewrites (by swapping the `RefCell` contents with a
/// pre-built vec, so the rewrite itself allocates nothing inside the armed window).
struct Harness {
    store: NodeStore,
    states: StateStore,
    bindings: BindingTable,
    effects: EffectStore,
    lists: VirtualLists,
    viewport: NodeId,
    keys: Rc<RefCell<Vec<u64>>>,
    scratch: Vec<u32>,
    redo: Vec<NodeId>,
}

fn setup() -> Harness {
    let keys = Rc::new(RefCell::new((0..ITEMS as u64).collect::<Vec<_>>()));
    let mut store = NodeStore::new();
    let mut states = StateStore::new();
    let mut bindings = BindingTable::new();
    let mut lists = VirtualLists::new();
    let mut text_edits = TextEdits::new();
    let mut projectors = SemanticProjector::new();

    let viewport = {
        let mut cx = BuildCx::with_reactive(
            &mut store,
            &mut states,
            &mut bindings,
            &mut lists,
            &mut text_edits,
            &mut projectors,
        );
        let key_src = keys.clone();
        cx.virtual_list_keyed(
            VirtualListStyle {
                axis: Axis::Column,
                size: Size {
                    width: Length::Fixed(100.0),
                    height: Length::Fixed(VIEWPORT_H),
                },
                overscan: 2,
                estimated_row: ROW_H,
                style: BoxStyle::NONE,
            },
            ITEMS,
            move |i| ItemKey(key_src.borrow()[i]),
            move |_i, cx| {
                cx.leaf(LeafStyle {
                    size: Size {
                        width: Length::fill(),
                        height: Length::Fixed(ROW_H),
                    },
                    ..Default::default()
                });
            },
        )
        .id()
    };
    store.mark_dirty(
        viewport,
        DirtyClass::MEASURE | DirtyClass::LAYOUT | DirtyClass::PAINT,
    );

    Harness {
        store,
        states,
        bindings,
        effects: EffectStore::new(),
        lists,
        viewport,
        keys,
        scratch: Vec::new(),
        redo: Vec::new(),
    }
}

/// Just the reconcile step — the keyed diff whose within-window reorder must
/// re-anchor survivors without allocating. Layout/paint of the moved hosts is a
/// separate concern measured by the layout alloc packs; this pins the reconcile.
fn reconcile_only(h: &mut Harness) -> u32 {
    reconcile(
        &mut h.store,
        &mut h.lists,
        &mut h.states,
        &mut h.bindings,
        &mut h.effects,
    )
}

/// One frame the way the facade Layout phase does: reconcile → relayout → absorb.
fn frame(h: &mut Harness) -> u32 {
    if h.store.bounds_main(h.viewport, Axis::Column) <= 0.0 {
        h.store.layout(h.viewport, SURFACE, &mut h.scratch);
    }
    let bound = reconcile_only(h);
    h.store
        .relayout_dirty(h.viewport, SURFACE, &mut h.scratch, &mut h.redo);
    absorb_measurements(&h.store, &mut h.lists);
    h.store.clear_dirty();
    bound
}

#[test]
fn steady_keyed_reorder_reuses_hosts_allocation_free() {
    let mut h = setup();

    // Pre-build the two orderings the armed loop toggles between, so swapping the
    // mapping never allocates inside the armed window. `base` is identity; `swapped`
    // reverses the first two visible items — a pure within-window permutation.
    let base: Vec<u64> = (0..ITEMS as u64).collect();
    let mut swapped = base.clone();
    swapped.swap(0, 1);

    // Warm up: mount the window, then reorder a few times so `new_keys` grows to the
    // window size and the reconcile / layout scratch buffers reach capacity.
    frame(&mut h);
    for i in 0..8 {
        let next = if i % 2 == 0 {
            swapped.clone()
        } else {
            base.clone()
        };
        // Swap the mapping in place (drops the old vec, moves the new one in — the
        // clone above is outside the armed window).
        h.keys.replace(next);
        h.lists.get_mut(h.viewport).unwrap().mark_data_dirty();
        let bound = frame(&mut h);
        assert_eq!(bound, 0, "a warmed within-window reorder rebuilds nothing");
    }

    // Two armed reorders. We measure the RECONCILE step specifically — the keyed
    // diff whose contract (section 12.4 stable keys) is "reorder re-anchors the
    // surviving host rather than rebuilding it". Mutating the mapping and marking
    // data dirty happen before arming; only `reconcile_only` runs armed. Its
    // `new_keys` scratch is warmed, and every visible key survives, so the diff
    // re-anchors in place and touches no heap. (Layout/paint of the moved hosts is
    // a general relayout concern the layout alloc packs cover; we run it unarmed to
    // keep the tree consistent for the next pass.)
    let targets = [swapped.clone(), base.clone()];
    let mut frame_allocs = [0usize; 2];
    let mut scratch_slot: Vec<u64> = Vec::new();
    for (slot, target) in frame_allocs.iter_mut().zip(targets) {
        // Move `target` into the RefCell and pull the old vec out into a reusable
        // local — both are moves, no allocation.
        let old = h.keys.replace(target);
        h.lists.get_mut(h.viewport).unwrap().mark_data_dirty();

        ALLOCS.store(0, Ordering::Relaxed);
        ARMED.store(true, Ordering::Relaxed);
        let bound = reconcile_only(&mut h);
        ARMED.store(false, Ordering::Relaxed);
        *slot = ALLOCS.load(Ordering::Relaxed);

        // Settle the tree (layout/paint of the re-anchored hosts) unarmed so the
        // next pass starts from a consistent, laid-out state.
        h.store
            .relayout_dirty(h.viewport, SURFACE, &mut h.scratch, &mut h.redo);
        absorb_measurements(&h.store, &mut h.lists);
        h.store.clear_dirty();

        scratch_slot = old; // keep the old vec alive / reuse the binding
        assert_eq!(bound, 0, "armed reorder still rebuilds nothing");
    }
    drop(scratch_slot);

    assert_eq!(
        frame_allocs,
        [0, 0],
        "a warmed keyed within-window reorder re-anchors survivors with no heap"
    );
}
