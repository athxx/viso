//! Counting the heap allocations an app makes, for `viso profile`.
//!
//! The runtime installs no allocator of its own; an app that wants its frames'
//! allocations counted declares one:
//!
//! ```no_run
//! #[global_allocator]
//! static ALLOCATOR: viso::alloc::Counting = viso::alloc::Counting::system();
//! # fn main() {}
//! ```
//!
//! or wraps the allocator it already uses with [`Counting::new`]. Each
//! allocation then costs a few relaxed atomic operations, on every thread; a
//! profile reports how many a frame made and how many bytes they asked for,
//! the app's other threads' included, and the live and peak heap
//! ([`heap`]) is what a [`MemoryBudget`](crate::MemoryBudget)'s `heap` limit
//! is checked against.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU64, Ordering};

static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);
static ALLOCATED_BYTES: AtomicU64 = AtomicU64::new(0);
static LIVE_BYTES: AtomicU64 = AtomicU64::new(0);
static PEAK_BYTES: AtomicU64 = AtomicU64::new(0);

/// An allocator that counts what it allocates and hands the work to `A`.
pub struct Counting<A = System> {
    inner: A,
}

impl Counting<System> {
    /// The system allocator, counted.
    pub const fn system() -> Counting<System> {
        Counting { inner: System }
    }
}

impl<A> Counting<A> {
    /// `inner`, counted.
    pub const fn new(inner: A) -> Counting<A> {
        Counting { inner }
    }
}

#[inline]
fn counted(bytes: usize) {
    ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
    ALLOCATED_BYTES.fetch_add(bytes as u64, Ordering::Relaxed);
    grew(bytes);
}

#[inline]
fn grew(bytes: usize) {
    let live = LIVE_BYTES.fetch_add(bytes as u64, Ordering::Relaxed) + bytes as u64;
    PEAK_BYTES.fetch_max(live, Ordering::Relaxed);
}

#[inline]
fn shrank(bytes: usize) {
    LIVE_BYTES.fetch_sub(bytes as u64, Ordering::Relaxed);
}

/// The heap a [`Counting`] allocator sees: the bytes live now and the most
/// that were live at once. `None` when none is installed.
pub fn heap() -> Option<Heap> {
    let peak = PEAK_BYTES.load(Ordering::Relaxed);
    (peak > 0).then(|| Heap {
        live: LIVE_BYTES.load(Ordering::Relaxed),
        peak,
    })
}

/// The live and peak heap, in bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Heap {
    pub live: u64,
    pub peak: u64,
}

// SAFETY: every call is forwarded unchanged to `inner`, which upholds the
// `GlobalAlloc` contract; counting touches only atomics and allocates
// nothing.
unsafe impl<A: GlobalAlloc> GlobalAlloc for Counting<A> {
    #[inline]
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller's contract for `layout` is `inner`'s.
        let p = unsafe { self.inner.alloc(layout) };
        if !p.is_null() {
            counted(layout.size());
        }
        p
    }

    #[inline]
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: as for `alloc`.
        let p = unsafe { self.inner.alloc_zeroed(layout) };
        if !p.is_null() {
            counted(layout.size());
        }
        p
    }

    #[inline]
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        shrank(layout.size());
        // SAFETY: `ptr` came from `inner` with `layout`, as the caller
        // guarantees.
        unsafe { self.inner.dealloc(ptr, layout) }
    }

    #[inline]
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        ALLOCATED_BYTES.fetch_add(new_size as u64, Ordering::Relaxed);
        // SAFETY: as for `dealloc`, `new_size` checked by the caller.
        let moved = unsafe { self.inner.realloc(ptr, layout, new_size) };
        if !moved.is_null() {
            shrank(layout.size());
            grew(new_size);
        }
        moved
    }
}

/// The allocations made so far and the bytes they asked for; `None` when no
/// [`Counting`] allocator is installed.
#[cfg(any(feature = "hot-reload", feature = "headless"))]
pub(crate) fn counts() -> Option<(u64, u64)> {
    // A process allocates before its first frame, so an installed counter
    // has counted by the time anything reads it.
    let allocations = ALLOCATIONS.load(Ordering::Relaxed);
    (allocations > 0).then(|| (allocations, ALLOCATED_BYTES.load(Ordering::Relaxed)))
}
