//! The allocation profile of the touch path: a warmed finger that presses a
//! control inside a scroll viewport, pans past the slop, and lifts — and a
//! second finger pressing and lifting alongside — must allocate nothing once
//! the per-pointer contact table and the route scratch have reached their
//! high-water mark. Section 7.1 forbids asserting that without measuring, so a
//! counting allocator arms around real samples.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::Ordering;

use viso_ui::{
    Axis, BindingTable, BuildCx, FlexStyle, LeafStyle, Modifiers, NodeId, NodeStore,
    PointerButtons, PointerContact, PointerEvent, PointerId, PointerPhase, PointerRouter, Rect,
    ScrollStyle, Size, StateStore,
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

struct Harness {
    store: NodeStore,
    states: StateStore,
    bindings: BindingTable,
    root: NodeId,
    chain: Vec<NodeId>,
}

fn setup() -> Harness {
    let mut store = NodeStore::new();
    let root = {
        let mut cx = BuildCx::new(&mut store);
        cx.scroll(
            ScrollStyle {
                axis: Axis::Column,
                size: Size::fixed(100.0, 100.0),
                ..Default::default()
            },
            |cx| {
                cx.flex(
                    FlexStyle {
                        axis: Axis::Column,
                        size: Size::fixed(100.0, 10_000.0),
                        ..Default::default()
                    },
                    |cx| {
                        let l = cx.leaf(LeafStyle {
                            size: Size::fixed(100.0, 10_000.0),
                            ..Default::default()
                        });
                        cx.on_pointer(l, move |_ev| {});
                    },
                );
            },
        );
        cx.root().unwrap()
    };
    let mut scratch = Vec::new();
    store.layout(
        root,
        Rect {
            x: 0.0,
            y: 0.0,
            w: 100.0,
            h: 100.0,
        },
        &mut scratch,
    );
    Harness {
        store,
        states: StateStore::new(),
        bindings: BindingTable::new(),
        root,
        chain: Vec::new(),
    }
}

fn touch(h: &mut Harness, finger: u64, y: f32, phase: PointerPhase) {
    PointerRouter::route_contact(
        &mut h.store,
        &mut h.states,
        &h.bindings,
        h.root,
        PointerContact {
            id: PointerId(finger),
            direct: true,
        },
        PointerEvent {
            x: 50.0,
            y,
            phase,
            buttons: PointerButtons::PRIMARY,
            modifiers: Modifiers::default(),
        },
        &mut h.chain,
    );
}

/// One finger pans up by 40 pt while a second finger taps.
fn gesture(h: &mut Harness) {
    touch(h, 1, 80.0, PointerPhase::Down);
    touch(h, 2, 50.0, PointerPhase::Down);
    for step in 1..=4 {
        touch(h, 1, 80.0 - step as f32 * 10.0, PointerPhase::Move);
    }
    touch(h, 2, 50.0, PointerPhase::Up);
    touch(h, 1, 40.0, PointerPhase::Up);
}

#[test]
fn steady_multi_touch_pan_is_allocation_free() {
    let mut h = setup();
    gesture(&mut h);
    assert!(
        h.store.scroll(h.root).y > 0.0,
        "the warm-up pan scrolled the viewport"
    );

    ALLOCS.store(0, Ordering::Relaxed);
    ARMED.store(true, Ordering::Relaxed);
    gesture(&mut h);
    ARMED.store(false, Ordering::Relaxed);
    assert_eq!(
        ALLOCS.load(Ordering::Relaxed),
        0,
        "a warmed two-finger press + pan allocates nothing"
    );
}
