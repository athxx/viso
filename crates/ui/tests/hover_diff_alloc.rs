//! The allocation-profile dimension of the hover-synthesis steady path: a
//! `Move` sample that stays within the node it already hovers must allocate
//! nothing. ADR 0022 states the steady-state move within a hovered node is "a
//! hit-test plus one `new == old` comparison: no dispatch, no allocation";
//! section 35 / section 7.3 forbid asserting that without measuring it, so this
//! test arms a counting global allocator and drives real within-node moves.
//!
//! The route reuses the caller's `chain` scratch `Vec` (cleared, not
//! reallocated, once warmed) and, on the steady path, dispatches nothing — so a
//! warmed within-node move touches no heap. A regression that reallocates the
//! chain, or that dispatches on an unchanged hover target, is what this pins.
//!
//! Run single-threaded (`--test-threads=1`): the counter is process-wide, so a
//! concurrent test's allocations would contaminate the armed window.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::Ordering;

use viso_ui::{
    Axis, BindingTable, BoxStyle, BuildCx, FlexStyle, LeafStyle, Modifiers, NodeId, NodeStore,
    PointerButtons, PointerEvent, PointerPhase, PointerRouter, Rect, Size, StateStore,
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

const SURFACE: Rect = Rect {
    x: 0.0,
    y: 0.0,
    w: 200.0,
    h: 100.0,
};

/// One 200x100 leaf under a flex root filling the surface, with an empty pointer
/// handler so the hover dispatch takes the real take-handler → restore path.
struct Harness {
    store: NodeStore,
    states: StateStore,
    bindings: BindingTable,
    root: NodeId,
    leaf: NodeId,
    chain: Vec<NodeId>,
}

fn mv(x: f32) -> PointerEvent {
    PointerEvent {
        x,
        y: 50.0,
        phase: PointerPhase::Move,
        buttons: PointerButtons::NONE,
        modifiers: Modifiers::default(),
    }
}

fn setup() -> Harness {
    let mut store = NodeStore::new();
    let states = StateStore::new();
    let bindings = BindingTable::new();

    let (root, leaf) = {
        let mut cx = BuildCx::new(&mut store);
        let mut leaf = None;
        cx.flex(
            FlexStyle {
                axis: Axis::Row,
                size: Size::fixed(200.0, 100.0),
                style: BoxStyle::default(),
                ..Default::default()
            },
            |cx| {
                let l = cx.leaf(LeafStyle {
                    size: Size::fixed(200.0, 100.0),
                    style: BoxStyle::default(),
                });
                cx.on_pointer(l, move |_ev| {});
                leaf = Some(l);
            },
        );
        (cx.root().unwrap(), leaf.unwrap().id())
    };

    let mut scratch = Vec::new();
    store.layout(root, SURFACE, &mut scratch);

    Harness {
        store,
        states,
        bindings,
        root,
        leaf,
        chain: Vec::new(),
    }
}

fn route(h: &mut Harness, x: f32) {
    PointerRouter::route(
        &mut h.store,
        &mut h.states,
        &h.bindings,
        h.root,
        mv(x),
        &mut h.chain,
    );
}

#[test]
fn steady_within_node_move_is_allocation_free() {
    let mut h = setup();

    // Warm up: the first move commits hover to the leaf and grows the reused
    // `chain` scratch to the single-node dispatch depth. Subsequent within-node
    // moves are `new == old` — no dispatch, chain reused in place.
    for i in 0..8 {
        // Vary x within the one leaf (0..200) so each is a real sample but never
        // crosses out of the node.
        route(&mut h, 20.0 + i as f32 * 10.0);
    }
    assert_eq!(
        h.store.hovered(),
        Some(h.leaf),
        "the warm-up leaves hover on the single leaf"
    );

    // Two armed within-node moves: each is a hit test plus a `new == old`
    // comparison, no dispatch, chain reused — zero heap.
    let mut frame_allocs = [0usize; 2];
    let mut x = 100.0f32;
    for slot in frame_allocs.iter_mut() {
        x = if x > 150.0 { 60.0 } else { x + 10.0 };
        ALLOCS.store(0, Ordering::Relaxed);
        ARMED.store(true, Ordering::Relaxed);
        route(&mut h, x);
        ARMED.store(false, Ordering::Relaxed);
        *slot = ALLOCS.load(Ordering::Relaxed);
    }

    assert_eq!(
        frame_allocs,
        [0, 0],
        "a warmed-up within-node hover move allocates nothing"
    );
}
