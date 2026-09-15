//! Deferred destruction: the retire queue and the frame fence.
//!
//! A resource the renderer is done with cannot be freed the instant it is
//! destroyed — the GPU may still be reading it for a frame that is in flight.
//! Freeing it early and handing its slot to a new resource would let an
//! in-flight draw sample the wrong memory. So destruction is *deferred*: the
//! handle is parked in the [`RetireQueue`] stamped with the [`Epoch`] it was
//! retired in, and its storage slot is reclaimed only once the GPU has finished
//! every frame up to and including that epoch.
//!
//! ## The fence is a single monotonic counter
//!
//! Frames are numbered by a monotonic [`Epoch`]; each `begin_frame` advances it.
//! Work is submitted to one in-order queue, so a frame completes only after
//! every earlier frame has — completion of epoch `N` therefore implies
//! completion of all epochs `≤ N`. That lets the whole GPU-progress state
//! collapse to one number: [`Fence::completed`], the highest epoch known
//! finished. A parked slot is safe to reclaim exactly when `completed ≥` its
//! retire epoch.
//!
//! ## Reclamation bumps the generation
//!
//! Reclaiming a slot returns it to its [`SlotMap`](crate::slots::SlotMap)'s
//! free-list with a bumped generation, so the handle that was retired — and any
//! copy of it — now resolves to `None` instead of to whatever new resource later
//! takes the slot. Deferred destruction and generation-safe handles are the same
//! mechanism seen from two ends.

/// A frame sequence number. Monotonic; each acquired frame gets the next value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Epoch(pub u64);

impl Epoch {
    /// The first epoch. `begin_frame` advances to `Epoch(1)` for the first frame,
    /// so `Epoch(0)` names "before any frame" — no work has ever been submitted.
    pub const START: Epoch = Epoch(0);

    /// The next epoch.
    #[inline]
    pub const fn next(self) -> Epoch {
        Epoch(self.0 + 1)
    }
}

/// The GPU-progress fence: the highest [`Epoch`] the GPU has finished.
///
/// Because frames complete in submission order, this one number captures all of
/// GPU progress — a parked slot retired in epoch `e` is reclaimable once
/// [`completed`](Self::completed) `≥ e`. On the Metal backend it is advanced from
/// a command-buffer completion handler (`fetch_max`, monotonic); on the headless
/// backend, which has no asynchronous GPU, a presented frame completes
/// immediately.
#[derive(Debug, Clone, Copy, Default)]
pub struct Fence {
    completed: u64,
}

impl Fence {
    /// A fence at [`Epoch::START`] — nothing completed yet.
    pub const fn new() -> Self {
        Self { completed: 0 }
    }

    /// A fence reporting `epoch` as the highest completed frame. The Metal
    /// backend builds one per `begin_frame` from its shared completion counter;
    /// the headless backend advances a persistent fence with [`signal`](Self::signal).
    pub const fn at(epoch: Epoch) -> Self {
        Self { completed: epoch.0 }
    }

    /// The highest epoch the GPU has finished.
    #[inline]
    pub const fn completed(&self) -> Epoch {
        Epoch(self.completed)
    }

    /// Record that `epoch` has completed. Idempotent and order-independent: the
    /// stored value only ever moves forward, so an out-of-order or duplicate
    /// signal cannot walk the fence backwards.
    #[inline]
    pub fn signal(&mut self, epoch: Epoch) {
        self.completed = self.completed.max(epoch.0);
    }
}

/// Which resource store a parked handle belongs to, so the backend can route it
/// to the right [`SlotMap`](crate::slots::SlotMap) at reclamation. Surfaces are
/// absent: a swapchain is not deferred-destroyed but rebuilt in place on the
/// device-loss / resize path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceKind {
    /// A buffer store slot.
    Buffer,
    /// A texture store slot.
    Texture,
    /// A sampler store slot.
    Sampler,
    /// A pipeline store slot.
    Pipeline,
    /// A bind-group store slot.
    BindGroup,
}

/// One parked handle awaiting reclamation: which store, which slot, and the
/// epoch after which it is safe to free.
#[derive(Debug, Clone, Copy)]
pub struct Retired {
    /// Which store the slot lives in.
    pub kind: ResourceKind,
    /// The slot to reclaim (the exact `{index, generation}` that was retired).
    pub id: crate::slots::RawId,
    /// The epoch the resource was retired in; reclaimable once the fence reaches it.
    pub epoch: Epoch,
}

/// A FIFO of resources awaiting deferred destruction.
///
/// Entries are pushed in retire order, which is non-decreasing in [`Epoch`]
/// (epochs only advance), so the queue stays sorted by epoch and reclamation is
/// a cheap front-drain: everything at or below the fence is contiguous at the
/// front. In steady state the queue length is bounded by the resources retired
/// within the in-flight window (typically the buffers grown this frame), and it
/// drains to empty as those frames complete — the renderer bench asserts it does
/// not grow without bound.
#[derive(Debug, Default)]
pub struct RetireQueue {
    parked: std::collections::VecDeque<Retired>,
}

impl RetireQueue {
    /// An empty queue.
    pub const fn new() -> Self {
        Self {
            parked: std::collections::VecDeque::new(),
        }
    }

    /// Park a destroyed resource for reclamation once `epoch` has completed.
    #[inline]
    pub fn retire(&mut self, kind: ResourceKind, id: crate::slots::RawId, epoch: Epoch) {
        self.parked.push_back(Retired { kind, id, epoch });
    }

    /// Remove and return every parked entry whose epoch the `fence` has passed,
    /// in retire order. The caller frees each returned slot in its store. Entries
    /// still in flight stay parked.
    pub fn drain_completed(&mut self, fence: Fence, out: &mut Vec<Retired>) {
        let done = fence.completed();
        while let Some(front) = self.parked.front() {
            if front.epoch > done {
                break;
            }
            out.push(self.parked.pop_front().expect("front was Some"));
        }
    }

    /// The number of slots still parked. Steady state keeps this bounded; the
    /// bench asserts it drains to zero once frames complete.
    #[inline]
    pub fn len(&self) -> usize {
        self.parked.len()
    }

    /// Whether nothing is awaiting reclamation.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.parked.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::slots::RawId;

    fn id(index: u32) -> RawId {
        RawId {
            index,
            generation: 0,
        }
    }

    #[test]
    fn fence_only_moves_forward() {
        let mut fence = Fence::new();
        assert_eq!(fence.completed(), Epoch::START);
        fence.signal(Epoch(3));
        assert_eq!(fence.completed(), Epoch(3));
        // An out-of-order / duplicate signal cannot walk it back.
        fence.signal(Epoch(1));
        assert_eq!(fence.completed(), Epoch(3));
    }

    #[test]
    fn drain_releases_only_completed_epochs() {
        let mut q = RetireQueue::new();
        q.retire(ResourceKind::Buffer, id(0), Epoch(1));
        q.retire(ResourceKind::Buffer, id(1), Epoch(2));
        q.retire(ResourceKind::Texture, id(2), Epoch(2));

        let mut fence = Fence::new();
        fence.signal(Epoch(1));

        let mut out = Vec::new();
        q.drain_completed(fence, &mut out);
        // Only the epoch-1 entry is past the fence.
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].epoch, Epoch(1));
        assert_eq!(q.len(), 2);

        // Advancing the fence releases the rest, in retire order.
        fence.signal(Epoch(2));
        out.clear();
        q.drain_completed(fence, &mut out);
        assert_eq!(out.len(), 2);
        assert!(q.is_empty());
    }

    #[test]
    fn nothing_drains_before_its_epoch_completes() {
        let mut q = RetireQueue::new();
        q.retire(ResourceKind::Buffer, id(0), Epoch(5));
        let mut out = Vec::new();
        // Fence at START: the parked slot is still in flight, so it stays.
        q.drain_completed(Fence::new(), &mut out);
        assert!(out.is_empty());
        assert_eq!(q.len(), 1);
    }
}
