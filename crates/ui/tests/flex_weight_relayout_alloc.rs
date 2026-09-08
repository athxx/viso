//! The allocation-profile dimension of a live flex-weight re-layout: rewriting a
//! flex child's fill weight in place and re-running layout must touch no heap. This
//! is the exact hot path the dock's seam-drag reconcile stands on — a committed
//! drag fraction becomes live pane geometry through
//! [`NodeStore::set_flex_child_weight`] plus a layout pass, nothing else — so if
//! that pair allocates, every dock resize allocates.
//!
//! `set_flex_child_weight` rewrites a `Length::Fill { weight }` in the child's
//! layout store in place (mirroring `set_absolute_rows_extent`) and marks
//! `LAYOUT | PAINT`; a warmed `layout` pass reuses its caller-owned scratch. So a
//! steady-state weight rewrite + relayout allocates zero. A regression that
//! reallocated the layout scratch, boxed a closure per call, or rebuilt the flex
//! children instead of re-sizing them is what this pins (section 7.1 / section 28).
//!
//! Run single-threaded (`--test-threads=1`): the counter is process-wide, so a
//! concurrent test's allocations would contaminate the armed window.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use viso_ui::{
    Align, Axis, BindingTable, BoxStyle, BuildCx, FlexStyle, Inset, Justify, LeafStyle, Length,
    NodeId, NodeStore, Rect, SemanticProjector, Size, StateStore, TextEdits, VirtualLists,
};

/// Counts heap allocations while `ARMED`; off by default so setup allocations are
/// never counted. Mirrors the other alloc packs.
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
    w: 1000.0,
    h: 400.0,
};

/// A row flex of two fill children — the shape of a dock split's two panes — plus
/// the reactive stores it was built into and the two pane node ids the weight
/// rewrite drives.
struct Harness {
    store: NodeStore,
    // The build cx borrows these reactive stores; the harness owns them so they
    // outlive the build, but the weight-rewrite/layout path never reads them after.
    #[allow(dead_code)]
    states: StateStore,
    #[allow(dead_code)]
    bindings: BindingTable,
    #[allow(dead_code)]
    lists: VirtualLists,
    #[allow(dead_code)]
    text_edits: TextEdits,
    #[allow(dead_code)]
    projectors: SemanticProjector,
    root: NodeId,
    pane_a: NodeId,
    pane_b: NodeId,
    scratch: Vec<u32>,
}

fn setup() -> Harness {
    let mut store = NodeStore::new();
    let mut states = StateStore::new();
    let mut bindings = BindingTable::new();
    let mut lists = VirtualLists::new();
    let mut text_edits = TextEdits::new();
    let mut projectors = SemanticProjector::new();

    // The two fill children's ids are captured out of the build closure so the test
    // can drive their weights afterward — a dock seam records the same pair.
    let a_slot: Rc<Cell<Option<NodeId>>> = Rc::new(Cell::new(None));
    let b_slot: Rc<Cell<Option<NodeId>>> = Rc::new(Cell::new(None));

    let root = {
        let mut cx = BuildCx::with_reactive(
            &mut store,
            &mut states,
            &mut bindings,
            &mut lists,
            &mut text_edits,
            &mut projectors,
        );
        let a_out = a_slot.clone();
        let b_out = b_slot.clone();
        cx.flex(
            FlexStyle {
                axis: Axis::Row,
                gap: 0.0,
                padding: Inset::all(0.0),
                align: Align::Stretch,
                justify: Justify::Start,
                size: Size::fill(),
                style: BoxStyle::NONE,
            },
            move |cx| {
                let a = cx.leaf(LeafStyle {
                    size: Size {
                        width: Length::fill(),
                        height: Length::fill(),
                    },
                    style: BoxStyle::NONE,
                });
                let b = cx.leaf(LeafStyle {
                    size: Size {
                        width: Length::fill(),
                        height: Length::fill(),
                    },
                    style: BoxStyle::NONE,
                });
                a_out.set(Some(a.id()));
                b_out.set(Some(b.id()));
            },
        )
        .id()
    };

    Harness {
        store,
        states,
        bindings,
        lists,
        text_edits,
        projectors,
        root,
        pane_a: a_slot.get().expect("pane A built"),
        pane_b: b_slot.get().expect("pane B built"),
        scratch: Vec::new(),
    }
}

/// A steady-state seam-drag reconcile touches no heap: rewriting both panes' fill
/// weights in place and re-laying-out the split allocates zero, once warmed. This is
/// the dock's live-resize hot path measured directly against its viso-ui foundation.
#[test]
fn set_flex_child_weight_plus_relayout_is_allocation_free() {
    let mut h = setup();

    // Warm: grow the layout scratch and settle every measurement cache before
    // arming, so the armed window measures only the steady state.
    for i in 0..8 {
        let f = 0.5 + (i as f32) * 0.01;
        h.store.set_flex_child_weight(h.pane_a, Axis::Row, f);
        h.store.set_flex_child_weight(h.pane_b, Axis::Row, 1.0 - f);
        h.store.layout(h.root, SURFACE, &mut h.scratch);
    }

    // Two armed frames of a live drag: each rewrites both weights and re-lays-out;
    // both must allocate nothing.
    let mut frame_allocs = [0usize; 2];
    let fracs = [0.62_f32, 0.44_f32];
    for (slot, &f) in frame_allocs.iter_mut().zip(fracs.iter()) {
        ALLOCS.store(0, Ordering::Relaxed);
        ARMED.store(true, Ordering::Relaxed);
        h.store.set_flex_child_weight(h.pane_a, Axis::Row, f);
        h.store.set_flex_child_weight(h.pane_b, Axis::Row, 1.0 - f);
        h.store.layout(h.root, SURFACE, &mut h.scratch);
        ARMED.store(false, Ordering::Relaxed);
        *slot = ALLOCS.load(Ordering::Relaxed);
    }

    assert_eq!(
        frame_allocs,
        [0, 0],
        "a warmed seam-drag reconcile (set_flex_child_weight x2 + relayout) must not allocate; got {frame_allocs:?}",
    );

    // Guard the harness stays live so the drive above was not a no-op on a dead node.
    let a_w = h.store.bounds_main(h.pane_a, Axis::Row);
    assert!(
        (a_w - 0.44 * SURFACE.w).abs() < 1.0,
        "pane A resized live to the last weight: got {a_w}",
    );
}
