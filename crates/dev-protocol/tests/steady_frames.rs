//! Dev frames built in one reused encoder allocate nothing per frame once it
//! has held the largest.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use viso_dev_protocol::wire::{LogLevel, RuntimeMessage, encode, frame_into, write_frame_with};
use viso_ende::Encoder;

/// Counts every allocation and reallocation of the test binary.
struct Counting;

static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

// SAFETY: every call forwards to `System` with the caller's own arguments,
// so `System`'s guarantees are this allocator's.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        // SAFETY: the caller upholds `alloc`'s contract for `layout`.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `ptr` came from this allocator, which is `System`'s.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        // SAFETY: as in `dealloc`, and the caller upholds `realloc`'s contract.
        unsafe { System.realloc(ptr, layout, size) }
    }
}

#[global_allocator]
static COUNTING: Counting = Counting;

#[test]
fn a_steady_frame_into_a_reused_encoder_allocates_nothing() {
    let messages = [
        RuntimeMessage::Dropped { count: 3 },
        RuntimeMessage::Log {
            level: LogLevel::Info,
            line: "a line the runtime logged".to_owned(),
        },
    ];
    let mut enc = Encoder::new();
    let mut sink = Vec::with_capacity(4096);
    for message in &messages {
        write_frame_with(&mut sink, message, &mut enc).expect("a frame");
    }
    let before = ALLOCATIONS.load(Ordering::Relaxed);
    for _ in 0..100 {
        for message in &messages {
            sink.clear();
            write_frame_with(&mut sink, message, &mut enc).expect("a frame");
        }
    }
    let allocated = ALLOCATIONS.load(Ordering::Relaxed) - before;
    assert_eq!(allocated, 0, "{allocated} allocations over 200 frames");
    let frame = frame_into(&messages[1], &mut enc).expect("a frame");
    assert_eq!(&frame[4..], encode(&messages[1]), "a length, then the body");
}
