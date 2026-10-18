//! What the Makepad apps of `cargo xtask footprint` share with the Viso
//! reference app, so both sides are measured the same way: the heap counted as
//! `viso::alloc::Counting` counts it, and the control thread that answers the
//! measuring tool on stdin when `VISO_FOOTPRINT_CONTROL` is set (`heap`
//! writes `footprint: heap live=<bytes> peak=<bytes>` to stderr; the end of
//! stdin ends the process).

use std::alloc::{GlobalAlloc, Layout, System};
use std::io::BufRead;
use std::sync::atomic::{AtomicU64, Ordering};

static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);
static ALLOCATED_BYTES: AtomicU64 = AtomicU64::new(0);
static LIVE_BYTES: AtomicU64 = AtomicU64::new(0);
static PEAK_BYTES: AtomicU64 = AtomicU64::new(0);

/// The system allocator, counted.
pub struct Counting;

fn counted(bytes: usize) {
    ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
    ALLOCATED_BYTES.fetch_add(bytes as u64, Ordering::Relaxed);
    grew(bytes);
}

fn grew(bytes: usize) {
    let live = LIVE_BYTES.fetch_add(bytes as u64, Ordering::Relaxed) + bytes as u64;
    PEAK_BYTES.fetch_max(live, Ordering::Relaxed);
}

fn shrank(bytes: usize) {
    LIVE_BYTES.fetch_sub(bytes as u64, Ordering::Relaxed);
}

// SAFETY: every call is forwarded unchanged to `System`, which upholds the
// `GlobalAlloc` contract; counting touches only atomics and allocates nothing.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller's contract for `layout` is `System`'s.
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            counted(layout.size());
        }
        p
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: as for `alloc`.
        let p = unsafe { System.alloc_zeroed(layout) };
        if !p.is_null() {
            counted(layout.size());
        }
        p
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        shrank(layout.size());
        // SAFETY: `ptr` came from `System` with `layout`, as the caller
        // guarantees.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        ALLOCATED_BYTES.fetch_add(new_size as u64, Ordering::Relaxed);
        // SAFETY: as for `dealloc`, `new_size` checked by the caller.
        let moved = unsafe { System.realloc(ptr, layout, new_size) };
        if !moved.is_null() {
            shrank(layout.size());
            grew(new_size);
        }
        moved
    }
}

/// Answers the measuring tool on stdin, when it asks to be answered.
pub fn listen() {
    if std::env::var_os("VISO_FOOTPRINT_CONTROL").is_none() {
        return;
    }
    std::thread::spawn(|| {
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            if line.trim() == "heap" {
                eprintln!(
                    "footprint: heap live={} peak={}",
                    LIVE_BYTES.load(Ordering::Relaxed),
                    PEAK_BYTES.load(Ordering::Relaxed)
                );
            }
        }
        std::process::exit(0);
    });
}
