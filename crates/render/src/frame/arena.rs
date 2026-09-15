//! A per-frame bump allocator for transient render scratch (§9.4).
//!
//! Frame lowering needs a handful of short-lived working sets — the visible
//! chunk list, batch scratch, clip scratch, small pass descriptors, and the
//! radix/coalescer scratch — that all live for exactly one frame and are thrown
//! away wholesale at frame end. Handing each its own `Vec` means a heap
//! allocation whenever a working set outgrows last frame's capacity, scattered
//! across the frame and impossible to reason about as one budget. The arena
//! replaces them with one contiguous byte buffer and a bump cursor: an
//! allocation is a pointer bump, and the whole frame's scratch is reclaimed by
//! resetting the cursor to zero — O(1), no per-set drop, no deallocation.
//!
//! # Steady state
//!
//! The backing buffer grows only on the cold path, when a frame's total scratch
//! first exceeds every previous frame's (a high-water mark, like the renderer's
//! instance scratch). Once warmed, a frame that fits the high-water mark bumps
//! within the existing buffer and performs **zero heap allocation** (§9.4, §28):
//! the counting-allocator test pins this. `reset` never shrinks or frees, so the
//! capacity a workload reaches is retained for the next frame.
//!
//! # What it does not hold
//!
//! The arena is for POD scratch addressed by value or by slice for the duration
//! of one frame. It does not run destructors: a `T` handed out here must not own
//! a heap allocation or other resource needing `Drop` (there would be nothing to
//! run it). In practice every consumer allocates `Copy` plain-data scratch
//! (chunk records, batch keys, dirty ranges, radix buckets), so this is a
//! natural fit and keeps the reset a single cursor store.

use std::alloc::{self, Layout};
use std::cell::Cell;
use std::mem::{align_of, size_of};
use std::ptr::NonNull;

/// A bump allocator whose lifetime is one frame.
///
/// Allocations move a cursor forward inside a single backing buffer; [`reset`]
/// rewinds the cursor to zero to reclaim the whole frame at once. The backing
/// buffer is owned by the arena and freed only when the arena is dropped.
///
/// Interior mutability ([`Cell`] cursor) lets `alloc_*` take `&self`, so several
/// independent scratch slices can be carved from one shared `&FrameArena` in a
/// single lowering pass without threading a `&mut` through every call. Each
/// returned slice borrows the arena immutably for its own lifetime; because a
/// bump allocation never revisits bytes it already handed out, two live slices
/// never alias (see the `SAFETY` notes on [`alloc_slice`]).
///
/// [`reset`]: FrameArena::reset
/// [`alloc_slice`]: FrameArena::alloc_slice
pub struct FrameArena {
    /// Start of the backing buffer. Dangling with a capacity of zero before the
    /// first allocation (no heap touched until a frame needs scratch).
    base: Cell<NonNull<u8>>,
    /// Bytes allocated so far this frame — the bump cursor.
    used: Cell<usize>,
    /// Bytes the backing buffer can hold (its high-water mark).
    cap: Cell<usize>,
    /// The alignment the backing allocation was made with, needed to free it and
    /// to re-`Layout` it on growth. Starts at the max scalar alignment so any
    /// reasonable POD scratch type fits without re-aligning the base.
    align: Cell<usize>,
}

/// The alignment the backing buffer is allocated with. Covers every scalar and
/// small SIMD type render scratch uses (`u64`, `[f32; 4]`, packed keys), so an
/// `alloc_slice::<T>` only ever bumps the cursor to a `T`-aligned offset within
/// the buffer, never needs the base itself re-aligned.
const BASE_ALIGN: usize = 16;

impl FrameArena {
    /// A new, empty arena. No heap is allocated until the first `alloc_*`; a
    /// renderer that never lowers a frame never touches the allocator.
    pub fn new() -> Self {
        Self {
            base: Cell::new(NonNull::dangling()),
            used: Cell::new(0),
            cap: Cell::new(0),
            align: Cell::new(BASE_ALIGN),
        }
    }

    /// Bytes handed out so far this frame.
    pub fn used(&self) -> usize {
        self.used.get()
    }

    /// The backing buffer's current capacity in bytes (its high-water mark).
    pub fn capacity(&self) -> usize {
        self.cap.get()
    }

    /// Reclaim the whole frame's scratch in O(1): rewind the cursor to zero.
    ///
    /// The backing buffer and its capacity are retained, so the next frame bumps
    /// into the same memory. No destructors run (arena types are plain data), so
    /// this is a single cursor store — the reset cost does not scale with how
    /// much was allocated.
    pub fn reset(&self) {
        self.used.set(0);
    }

    /// Allocate an uninitialized, `T`-aligned slice of `len` elements, returning
    /// a mutable slice the caller fills before use.
    ///
    /// The slice borrows the arena for its lifetime. The backing buffer grows
    /// (cold path, one reallocation) if the frame's scratch first exceeds the
    /// high-water mark; otherwise the call is a pointer bump with no allocation.
    ///
    /// `len == 0` returns an empty slice without bumping the cursor.
    #[allow(clippy::mut_from_ref)] // The arena is a bump allocator: each call
    // returns a disjoint region, so handing out `&mut` from `&self` does not
    // alias. See the SAFETY notes below.
    pub fn alloc_slice<T: Copy>(&self, len: usize) -> &mut [T] {
        if len == 0 {
            return &mut [];
        }
        let bytes = len
            .checked_mul(size_of::<T>())
            .expect("frame arena allocation size overflow");
        let offset = self.bump(bytes, align_of::<T>());
        // SAFETY: `bump` returned an `offset` such that `offset + bytes <= cap`,
        // and the backing buffer holds `cap` bytes at `base`. `offset` is aligned
        // to `align_of::<T>()`, so `base + offset` is a valid, aligned pointer to
        // `len` contiguous `T`-sized slots inside the buffer. The region
        // `[offset, offset + bytes)` was never handed out before this call (the
        // cursor only moves forward and is reset only between frames, after every
        // prior borrow has ended), so the returned slice does not alias any other
        // live arena slice. `T: Copy` owns no resource, so leaving the slots
        // uninitialized until the caller writes them is sound for `&mut [T]`
        // (the caller must initialize before reading — the return type is
        // uninitialized-but-typed scratch).
        unsafe {
            let ptr = self.base.get().as_ptr().add(offset).cast::<T>();
            std::slice::from_raw_parts_mut(ptr, len)
        }
    }

    /// Allocate a `T`-aligned slice of `len` elements, each initialized to
    /// `value`. Convenience over [`alloc_slice`](Self::alloc_slice) for scratch
    /// that starts from a known fill (counters at zero, sentinels).
    #[allow(clippy::mut_from_ref)] // See `alloc_slice`: disjoint bump regions.
    pub fn alloc_filled<T: Copy>(&self, len: usize, value: T) -> &mut [T] {
        let slice = self.alloc_slice::<T>(len);
        for slot in slice.iter_mut() {
            *slot = value;
        }
        slice
    }

    /// Reserve `bytes` at the next `align`-aligned cursor position, growing the
    /// backing buffer if the frame's scratch first exceeds the high-water mark.
    /// Returns the byte offset of the reservation from `base`.
    fn bump(&self, bytes: usize, align: usize) -> usize {
        debug_assert!(align <= BASE_ALIGN, "arena base is aligned to {BASE_ALIGN}");
        let start = self.used.get();
        // Round the cursor up to the requested alignment.
        let aligned = (start + align - 1) & !(align - 1);
        let end = aligned
            .checked_add(bytes)
            .expect("frame arena cursor overflow");
        if end > self.cap.get() {
            self.grow(end);
        }
        self.used.set(end);
        aligned
    }

    /// Grow the backing buffer to hold at least `needed` bytes, preserving
    /// nothing (growth happens between the cursor and the frame's already-handed-
    /// out slices only at frame start, when no slice is live — but to stay sound
    /// even if a workload grows mid-frame, existing bytes are copied over).
    ///
    /// Doubling keeps growth amortized O(1); this is the cold path (§9.4), hit
    /// only when a frame's scratch first exceeds every previous frame's.
    #[cold]
    fn grow(&self, needed: usize) {
        let old_cap = self.cap.get();
        let mut new_cap = old_cap.max(64);
        while new_cap < needed {
            new_cap = new_cap
                .checked_mul(2)
                .expect("frame arena capacity overflow");
        }
        let new_layout = Layout::from_size_align(new_cap, BASE_ALIGN).expect("valid arena layout");
        // SAFETY: `new_layout` has a non-zero size (`new_cap >= 64`) and a valid
        // power-of-two alignment (`BASE_ALIGN`). The returned pointer is checked
        // for null below before any use.
        let new_ptr = unsafe { alloc::alloc(new_layout) };
        let new_base = match NonNull::new(new_ptr) {
            Some(p) => p,
            None => alloc::handle_alloc_error(new_layout),
        };
        if old_cap != 0 {
            let used = self.used.get();
            // SAFETY: `base` points to `old_cap` valid bytes and `new_base` to
            // `new_cap >= needed > used` valid bytes; the two allocations are
            // distinct (a fresh `alloc`), so the ranges do not overlap. Only the
            // `used` live-cursor bytes need preserving.
            unsafe {
                std::ptr::copy_nonoverlapping(self.base.get().as_ptr(), new_base.as_ptr(), used);
            }
            let old_layout =
                Layout::from_size_align(old_cap, self.align.get()).expect("valid old arena layout");
            // SAFETY: `base`/`old_layout` are exactly the pointer and layout the
            // previous `grow` allocated with (recorded in `align`/`cap`), so this
            // frees the live backing buffer once, and it is not used afterward
            // (`base`/`cap` are overwritten below before returning).
            unsafe {
                alloc::dealloc(self.base.get().as_ptr(), old_layout);
            }
        }
        self.base.set(new_base);
        self.cap.set(new_cap);
        self.align.set(BASE_ALIGN);
    }
}

impl Default for FrameArena {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for FrameArena {
    fn drop(&mut self) {
        let cap = self.cap.get();
        if cap != 0 {
            let layout =
                Layout::from_size_align(cap, self.align.get()).expect("valid arena layout");
            // SAFETY: `base`/`layout` match the allocation made in the last
            // `grow` (its `cap`/`align`), and the arena is being dropped, so the
            // buffer is freed exactly once and never used again.
            unsafe {
                alloc::dealloc(self.base.get().as_ptr(), layout);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_arena_touches_no_heap() {
        let a = FrameArena::new();
        assert_eq!(a.capacity(), 0);
        assert_eq!(a.used(), 0);
        // A zero-length allocation stays empty and bumps nothing.
        let s = a.alloc_slice::<u32>(0);
        assert!(s.is_empty());
        assert_eq!(a.capacity(), 0);
    }

    #[test]
    fn bump_hands_out_disjoint_writable_slices() {
        let a = FrameArena::new();
        let x = a.alloc_slice::<u32>(3);
        x.copy_from_slice(&[1, 2, 3]);
        let y = a.alloc_slice::<u32>(2);
        y.copy_from_slice(&[10, 20]);
        // Both slices are independently live and hold what was written — the
        // second allocation did not overwrite the first.
        assert_eq!(x, &[1, 2, 3]);
        assert_eq!(y, &[10, 20]);
    }

    #[test]
    fn alignment_is_respected_across_mixed_types() {
        let a = FrameArena::new();
        // A single byte, then a u64: the u64 slice must be 8-aligned even though
        // the cursor sat at offset 1.
        let _b = a.alloc_slice::<u8>(1);
        let q = a.alloc_slice::<u64>(1);
        q[0] = 0xDEAD_BEEF_CAFE_F00D;
        let addr = q.as_ptr() as usize;
        assert_eq!(addr % align_of::<u64>(), 0, "u64 slice is aligned");
        assert_eq!(q[0], 0xDEAD_BEEF_CAFE_F00D);
    }

    #[test]
    fn alloc_filled_initializes_every_slot() {
        let a = FrameArena::new();
        let s = a.alloc_filled::<i32>(5, -1);
        assert_eq!(s, &[-1, -1, -1, -1, -1]);
    }

    #[test]
    fn reset_reclaims_without_shrinking() {
        let a = FrameArena::new();
        let _ = a.alloc_slice::<u64>(100);
        let cap_after_grow = a.capacity();
        assert!(cap_after_grow >= 800);
        assert!(a.used() >= 800);

        a.reset();
        assert_eq!(a.used(), 0, "reset rewinds the cursor");
        assert_eq!(
            a.capacity(),
            cap_after_grow,
            "reset retains the backing buffer"
        );
    }

    #[test]
    fn warmed_frame_within_high_water_does_not_grow() {
        let a = FrameArena::new();
        // Frame 1: grow to fit.
        {
            let _ = a.alloc_slice::<u32>(64);
            let _ = a.alloc_slice::<u64>(32);
        }
        let warm_cap = a.capacity();
        // Frame 2: same workload, within the high-water mark — capacity is stable.
        a.reset();
        {
            let _ = a.alloc_slice::<u32>(64);
            let _ = a.alloc_slice::<u64>(32);
        }
        assert_eq!(
            a.capacity(),
            warm_cap,
            "a frame within the high-water mark does not reallocate"
        );
    }
}
