//! The allocation-profile dimension of section 8.1's reactive semantic-state
//! path: a warmed-up projection wake must allocate nothing. `SemanticProjector`
//! documents that "a wake allocates nothing on the steady path"; section 35 /
//! section 7.3 forbid asserting that without measuring it, so this test arms a
//! counting global allocator and drives real wakes.
//!
//! Steady state matters because the mechanism has three reused buffers that only
//! reach their capacity after the first wakes: the `wake_scratch` target list,
//! the per-projection dependency `Vec`s in the reverse index, and the projector's
//! reused dependency cursor. A fresh cursor per projection (the shape before this
//! section's fix) would allocate once per woken projection every wake — this test
//! is what pins that regression.
//!
//! Run single-threaded (`--test-threads=1`): the counter is process-wide, so a
//! concurrent test's allocations would contaminate the armed window.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use viso_ui::{
    BindingTable, BuildCx, FlexStyle, LeafStyle, NodeId, NodeStore, Role, SemanticProjector,
    SemanticState, Semantics, StateId, StateStore, StateValue, TextEdits, VirtualLists,
};

/// Counts heap allocations while `ARMED`; off by default so setup allocations are
/// never counted. Mirrors the widget-level alloc packs.
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

/// The radio-group shape: `FANOUT` option leaves under one flex root, each bound
/// to a single shared `Int` cell with a `checked = index == cell` projection —
/// the widest fan-out of the four section-8.1 controls.
const FANOUT: usize = 32;

struct Harness {
    store: NodeStore,
    states: StateStore,
    projectors: SemanticProjector,
    source: StateId,
    #[allow(dead_code)]
    root: NodeId,
    changed: Vec<StateId>,
}

fn setup() -> Harness {
    let mut store = NodeStore::new();
    let mut states = StateStore::new();
    let mut bindings = BindingTable::new();
    let mut lists = VirtualLists::new();
    let mut text_edits = TextEdits::new();
    let mut projectors = SemanticProjector::new();

    let source = states.alloc(StateValue::Int(0));

    let root = {
        let mut cx = BuildCx::with_reactive(
            &mut store,
            &mut states,
            &mut bindings,
            &mut lists,
            &mut text_edits,
            &mut projectors,
        );
        let root = cx.flex(FlexStyle::default(), |cx| {
            for index in 0..FANOUT {
                let node = cx.leaf(LeafStyle::default());
                cx.bind_semantic_state(node, move |cx| {
                    let chosen =
                        matches!(cx.get(source), Some(StateValue::Int(i)) if i as usize == index);
                    SemanticState::checked(chosen)
                });
                cx.semantics(node, Semantics::role(Role::Radio));
            }
        });
        root.id()
    };

    Harness {
        store,
        states,
        projectors,
        source,
        root,
        changed: Vec::new(),
    }
}

/// One projection wake: flip the shared cell to `next`, drain the pending batch,
/// run the projector. Returns how many projections re-ran.
fn wake(h: &mut Harness, next: i32) -> u32 {
    h.states.set(h.source, StateValue::Int(next));
    h.changed.clear();
    h.states.take_pending(&mut h.changed);
    h.projectors.wake(&h.changed, &h.states, &mut h.store)
}

#[test]
fn steady_projection_wake_is_allocation_free() {
    let mut h = setup();

    // Warm up: the first wakes grow `wake_scratch`, each projection's reverse-index
    // dependency `Vec`, and the reused cursor to their steady capacities. Alternate
    // the selection so every wake is a real change that re-runs every projection.
    for i in 0..8 {
        // 1..=8, never the initial 0, so every wake is a real change.
        let ran = wake(&mut h, i + 1);
        assert_eq!(
            ran, FANOUT as u32,
            "a selection change re-runs every bound option's projection"
        );
    }

    // Two armed wakes, both against a real change: a steady wake allocates nothing.
    let mut frame_allocs = [0usize; 2];
    let mut next = 100i32;
    for slot in frame_allocs.iter_mut() {
        next = (next + 1) % FANOUT as i32;
        ALLOCS.store(0, Ordering::Relaxed);
        ARMED.store(true, Ordering::Relaxed);
        let ran = wake(&mut h, next);
        ARMED.store(false, Ordering::Relaxed);
        *slot = ALLOCS.load(Ordering::Relaxed);
        assert_eq!(
            ran, FANOUT as u32,
            "each armed wake still re-runs every option"
        );
    }

    assert_eq!(
        frame_allocs,
        [0, 0],
        "a warmed-up semantic-state projection wake allocates nothing"
    );
}
