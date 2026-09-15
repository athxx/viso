//! Frame-scoped transient state (§9.4).
//!
//! Everything under this module lives for exactly one frame and is reclaimed
//! wholesale at frame end. Today that is the [`FrameArena`] — a bump allocator
//! for the visible chunk list, batch/clip scratch, small pass descriptors, and
//! radix/coalescer scratch that the planner (F4.5) and coalescer (F4.4) carve
//! from. The renderer owns one [`Frame`] for the process's lifetime and calls
//! [`Frame::reset`] at the start of each lowering pass, so the arena's backing
//! buffer is retained and steady-state frames allocate nothing on the heap.

mod arena;

pub use arena::FrameArena;

/// The renderer's frame-scoped scratch, owned once and reset each frame.
///
/// Held for the process lifetime by `Renderer`; [`reset`](Self::reset) rewinds
/// its arena in O(1) at the start of every lowering pass so a warmed frame
/// reuses the same backing memory (§9.4, §28). Grouping the frame's transient
/// state behind one handle keeps the reset a single call and gives the planner
/// and coalescer one place to draw scratch from.
#[derive(Default)]
pub struct Frame {
    /// Bump allocator for this frame's transient working sets.
    pub arena: FrameArena,
}

impl Frame {
    /// A new frame with an empty arena (no heap touched until first use).
    pub fn new() -> Self {
        Self::default()
    }

    /// Reclaim the whole frame's transient scratch in O(1), retaining capacity.
    /// Called at the start of each lowering pass.
    pub fn reset(&mut self) {
        self.arena.reset();
    }
}
