//! Frame arena steady-state invariant (§9.4, §28).
//!
//! The frame arena is the allocation foundation the F4 planner and coalescer
//! draw their per-frame scratch from. Its whole reason to exist is that a warmed
//! frame — one whose scratch fits the high-water mark reached by earlier frames —
//! must not touch the heap: it bumps a cursor inside a retained buffer and is
//! reclaimed by an O(1) reset. This test pins that contract with a counting
//! global allocator, the same technique the steady-state renderer bench uses, so
//! a regression that reintroduces per-frame allocation fails loudly.
//!
//! Two frames are run with an identical workload. The first may allocate (it
//! grows the backing buffer to the high-water mark); the second, within that
//! mark after a reset, must perform **zero** heap allocations and bump to the
//! exact same byte total.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use viso_render::frame::Frame;

/// A global allocator that counts heap allocations while `ARMED`, so an arena
/// frame's allocation behavior can be asserted directly. Off by default so the
/// test harness's own allocations are never counted.
struct CountingAlloc;

static ALLOCS: AtomicUsize = AtomicUsize::new(0);
static ARMED: AtomicBool = AtomicBool::new(false);

// SAFETY: forwards every call to the system allocator unchanged; the only added
// behavior is a relaxed counter increment on allocation/reallocation while armed.
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

/// A representative frame's worth of mixed scratch: a chunk list, a batch-key
/// scratch, a clip scratch, and a small radix/coalescer scratch — the shapes the
/// planner and coalescer allocate. Returns the byte total the arena reached.
fn lower_one_frame(frame: &Frame) -> usize {
    // Distinct element types and alignments, mirroring real scratch: packed u32
    // batch keys, u64 order/revision scratch, and byte-sized clip flags.
    let chunks = frame.arena.alloc_slice::<u64>(96);
    for (i, slot) in chunks.iter_mut().enumerate() {
        *slot = i as u64;
    }
    let keys = frame.arena.alloc_slice::<u32>(48);
    for (i, slot) in keys.iter_mut().enumerate() {
        *slot = (i as u32) * 7;
    }
    let clips = frame.arena.alloc_filled::<u8>(64, 0);
    clips[0] = 1;
    let radix = frame.arena.alloc_filled::<u64>(256, 0);
    radix[255] = 42;
    frame.arena.used()
}

#[test]
fn warmed_frame_is_allocation_free_and_byte_stable() {
    let mut frame = Frame::new();

    // Frame 1: cold — grows the backing buffer to the high-water mark. Not
    // asserted allocation-free (this is the one-time growth path).
    let frame1_bytes = lower_one_frame(&frame);
    assert!(frame1_bytes > 0, "the frame allocated some scratch");
    let warm_cap = frame.arena.capacity();

    // Reset reclaims the whole frame in O(1) without freeing the buffer.
    frame.reset();
    assert_eq!(frame.arena.used(), 0, "reset rewinds the cursor to zero");
    assert_eq!(
        frame.arena.capacity(),
        warm_cap,
        "reset retains the backing buffer"
    );

    // Frame 2: warmed — identical workload within the high-water mark. Arm the
    // counting allocator only around this frame so nothing else is counted.
    ARMED.store(true, Ordering::SeqCst);
    let frame2_bytes = lower_one_frame(&frame);
    let allocs = ALLOCS.load(Ordering::SeqCst);
    ARMED.store(false, Ordering::SeqCst);

    assert_eq!(
        allocs, 0,
        "a warmed frame within the high-water mark performs zero heap allocations"
    );
    assert_eq!(
        frame2_bytes, frame1_bytes,
        "the same workload bumps to the exact same byte total each frame"
    );
    assert_eq!(
        frame.arena.capacity(),
        warm_cap,
        "a warmed frame does not reallocate the backing buffer"
    );
}
