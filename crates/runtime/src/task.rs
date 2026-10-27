//! UI task identity, wakeup and cancellation.
//!
//! A [`TaskSet`] owns futures that run on the loop thread. Their wakers may be
//! woken from any thread: a wake records the task's id on a shared ready list
//! and, the first time after a drain, kicks the platform loop through its
//! [`LoopWaker`]. The set polls only the tasks that woke, so an idle set costs
//! nothing: no lock and no poll.
//!
//! This is not an executor for runtime-bound futures. A future that needs a
//! reactor (a Tokio socket or timer) runs on that runtime through an adapter,
//! and the UI task awaits the adapter's handle.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Wake, Waker};

use viso_platform::LoopWaker;

/// A task's process-unique identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TaskId(u64);

impl TaskId {
    /// A fresh id, never handed out before in this process.
    pub fn fresh() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }
}

/// The ready list every task's waker writes to.
struct WakeQueue {
    woken: Mutex<Vec<(TaskId, u32)>>,
    /// Set by the first wake after a drain; the drain clears it before taking
    /// the list, so a wake that lands after the take kicks the loop again.
    signalled: AtomicBool,
    loop_waker: LoopWaker,
}

struct TaskWaker {
    id: TaskId,
    slot: u32,
    queue: Arc<WakeQueue>,
}

impl Wake for TaskWaker {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.queue
            .woken
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((self.id, self.slot));
        self.queue.signal();
    }
}

impl WakeQueue {
    /// Mark the set due a poll, kicking the loop on the first mark after a
    /// drain.
    fn signal(&self) {
        if !self.signalled.swap(true, Ordering::AcqRel) {
            self.loop_waker.wake();
        }
    }
}

/// The empty end of an owner chain.
const NIL: u32 = u32::MAX;

/// A task's owner, named by a small key so the set can chain each owner's
/// tasks and drop them without a scan. Owners that share a key (a slot reused
/// under a new generation) share a chain; [`TaskSet::cancel_owned`] drops only
/// the exact owner's tasks.
pub trait TaskOwner: Copy + Eq {
    /// The chain this owner's tasks hang on.
    fn key(self) -> u32;
}

macro_rules! owner_is_itself {
    ($($t:ty),*) => {$(
        impl TaskOwner for $t {
            #[inline]
            fn key(self) -> u32 {
                self as u32
            }
        }
    )*};
}
owner_is_itself!(u8, u16, u32);

struct Task<O, T> {
    id: TaskId,
    tag: T,
    future: Pin<Box<dyn Future<Output = O>>>,
    waker: Waker,
    /// Neighbours on the owner's chain: slots, or [`NIL`].
    prev: u32,
    next: u32,
}

/// Loop-thread futures, each tagged with its owner, polled when woken.
pub struct TaskSet<O, T> {
    /// Live tasks by slot; an empty slot is on `free`.
    slots: Vec<Option<Task<O, T>>>,
    free: Vec<u32>,
    /// `(id, slot)` of every live task, sorted by id.
    by_id: Vec<(TaskId, u32)>,
    /// `(owner key, first slot of its chain)`, sorted by key.
    heads: Vec<(u32, u32)>,
    queue: Arc<WakeQueue>,
    /// Spawned since the last poll: due their first poll.
    spawned: Vec<(TaskId, u32)>,
    /// Reused by `poll` for the tasks it drains.
    scratch: Vec<(TaskId, u32)>,
}

impl<O, T: TaskOwner> TaskSet<O, T> {
    /// An empty set whose wakes kick `loop_waker`.
    pub fn new(loop_waker: LoopWaker) -> Self {
        Self {
            slots: Vec::new(),
            free: Vec::new(),
            by_id: Vec::new(),
            heads: Vec::new(),
            queue: Arc::new(WakeQueue {
                woken: Mutex::new(Vec::new()),
                signalled: AtomicBool::new(false),
                loop_waker,
            }),
            spawned: Vec::new(),
            scratch: Vec::new(),
        }
    }

    /// Add `future` as task `id`, owned by `tag`. Its first poll is at the
    /// next [`poll`](Self::poll); the spawn kicks the loop like a wake, so a
    /// frame comes to run it.
    pub fn spawn(&mut self, id: TaskId, tag: T, future: Pin<Box<dyn Future<Output = O>>>) {
        let slot = self.free.pop().unwrap_or_else(|| {
            self.slots.push(None);
            (self.slots.len() - 1) as u32
        });
        let waker = Waker::from(Arc::new(TaskWaker {
            id,
            slot,
            queue: self.queue.clone(),
        }));
        let key = tag.key();
        let next = match self.heads.binary_search_by_key(&key, |&(k, _)| k) {
            Ok(at) => {
                let first = std::mem::replace(&mut self.heads[at].1, slot);
                self.task_mut(first).prev = slot;
                first
            }
            Err(at) => {
                self.heads.insert(at, (key, slot));
                NIL
            }
        };
        self.slots[slot as usize] = Some(Task {
            id,
            tag,
            future,
            waker,
            prev: NIL,
            next,
        });
        // Ids are handed out in order, so this is nearly always a push.
        let at = self.by_id.partition_point(|&(other, _)| other < id);
        self.by_id.insert(at, (id, slot));
        self.spawned.push((id, slot));
        self.queue.signal();
    }

    /// Drop task `id`'s future; `false` when no such task is live.
    pub fn cancel(&mut self, id: TaskId) -> bool {
        match self.by_id.binary_search_by_key(&id, |&(id, _)| id) {
            Ok(at) => {
                let slot = self.by_id[at].1;
                self.remove(slot);
                true
            }
            Err(_) => false,
        }
    }

    /// Drop every task `owner` owns, walking its chain alone.
    pub fn cancel_owned(&mut self, owner: T) {
        let Ok(at) = self.heads.binary_search_by_key(&owner.key(), |&(k, _)| k) else {
            return;
        };
        let mut slot = self.heads[at].1;
        while slot != NIL {
            let task = self.task_mut(slot);
            let next = task.next;
            if task.tag == owner {
                self.remove(slot);
            }
            slot = next;
        }
    }

    /// Drop every task whose tag satisfies `owned`.
    pub fn cancel_where(&mut self, mut owned: impl FnMut(T) -> bool) {
        for slot in 0..self.slots.len() as u32 {
            if self.slots[slot as usize]
                .as_ref()
                .is_some_and(|task| owned(task.tag))
            {
                self.remove(slot);
            }
        }
    }

    /// Drop every task.
    pub fn clear(&mut self) {
        self.slots.clear();
        self.free.clear();
        self.by_id.clear();
        self.heads.clear();
        self.spawned.clear();
    }

    /// Whether a task is due a poll: freshly spawned, or woken since the last
    /// poll.
    pub fn is_woken(&self) -> bool {
        self.queue.signalled.load(Ordering::Acquire)
    }

    /// Poll every due task once, in id order, and push the output of each
    /// that finished onto `out`. Finished tasks leave the set.
    pub fn poll(&mut self, out: &mut Vec<O>) {
        self.scratch.clear();
        self.scratch.append(&mut self.spawned);
        if self.queue.signalled.swap(false, Ordering::AcqRel) {
            let mut woken = self.queue.woken.lock().unwrap_or_else(|e| e.into_inner());
            self.scratch.append(&mut woken);
        }
        if self.scratch.is_empty() {
            return;
        }
        self.scratch.sort_unstable_by_key(|&(id, _)| id);
        self.scratch.dedup_by_key(|&mut (id, _)| id);
        for at in 0..self.scratch.len() {
            let (id, slot) = self.scratch[at];
            // A task cancelled or finished since its wake is gone, or its slot
            // holds another task.
            let Some(Some(task)) = self.slots.get_mut(slot as usize) else {
                continue;
            };
            if task.id != id {
                continue;
            }
            let mut cx = Context::from_waker(&task.waker);
            if let Poll::Ready(output) = task.future.as_mut().poll(&mut cx) {
                self.remove(slot);
                out.push(output);
            }
        }
    }

    /// The number of live tasks.
    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    /// Whether no task is live.
    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }

    fn task_mut(&mut self, slot: u32) -> &mut Task<O, T> {
        self.slots[slot as usize]
            .as_mut()
            .expect("a chained slot holds a task")
    }

    /// Unchain the task in `slot`, forget its id and free the slot, dropping
    /// its future.
    fn remove(&mut self, slot: u32) {
        let task = self.slots[slot as usize]
            .take()
            .expect("a removed slot holds a task");
        if task.prev == NIL {
            let key = task.tag.key();
            let at = self
                .heads
                .binary_search_by_key(&key, |&(k, _)| k)
                .expect("a chain's first task has a head");
            if task.next == NIL {
                self.heads.remove(at);
            } else {
                self.heads[at].1 = task.next;
            }
        } else {
            self.task_mut(task.prev).next = task.next;
        }
        if task.next != NIL {
            self.task_mut(task.next).prev = task.prev;
        }
        if let Ok(at) = self.by_id.binary_search_by_key(&task.id, |&(id, _)| id) {
            self.by_id.remove(at);
        }
        self.free.push(slot);
    }
}

impl<O, T: TaskOwner> Default for TaskSet<O, T> {
    fn default() -> Self {
        Self::new(LoopWaker::inert())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;
    use std::sync::atomic::AtomicUsize;

    /// A future that finishes once `open` is set, stashing its waker.
    struct Gate {
        open: Arc<AtomicBool>,
        waker: Arc<Mutex<Option<Waker>>>,
        value: u32,
    }

    impl Future for Gate {
        type Output = u32;
        fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<u32> {
            if self.open.load(Ordering::Acquire) {
                return Poll::Ready(self.value);
            }
            *self.waker.lock().unwrap() = Some(cx.waker().clone());
            Poll::Pending
        }
    }

    fn gate(value: u32) -> (Gate, Arc<AtomicBool>, Arc<Mutex<Option<Waker>>>) {
        let open = Arc::new(AtomicBool::new(false));
        let waker = Arc::new(Mutex::new(None));
        (
            Gate {
                open: open.clone(),
                waker: waker.clone(),
                value,
            },
            open,
            waker,
        )
    }

    fn counting_waker() -> (LoopWaker, Arc<AtomicUsize>) {
        let kicks = Arc::new(AtomicUsize::new(0));
        let counter = kicks.clone();
        (
            LoopWaker::new(move || {
                counter.fetch_add(1, Ordering::Relaxed);
            }),
            kicks,
        )
    }

    #[test]
    fn a_ready_future_finishes_on_its_first_poll() {
        let mut set: TaskSet<u32, u8> = TaskSet::default();
        set.spawn(TaskId::fresh(), 0, Box::pin(async { 7 }));
        assert!(set.is_woken());
        let mut out = Vec::new();
        set.poll(&mut out);
        assert_eq!(out, [7]);
        assert!(set.is_empty());
        assert!(!set.is_woken());
    }

    #[test]
    fn a_wake_from_another_thread_kicks_once_and_polls_the_task() {
        let (loop_waker, kicks) = counting_waker();
        let mut set: TaskSet<u32, u8> = TaskSet::new(loop_waker);
        let (future, open, waker) = gate(3);
        set.spawn(TaskId::fresh(), 0, Box::pin(future));
        assert_eq!(kicks.load(Ordering::Relaxed), 1, "a spawn kicks the loop");
        let mut out = Vec::new();
        set.poll(&mut out);
        assert!(out.is_empty());
        assert!(!set.is_woken(), "a pending task costs nothing until woken");

        let waker = waker.lock().unwrap().take().unwrap();
        std::thread::spawn(move || {
            open.store(true, Ordering::Release);
            waker.wake_by_ref();
            waker.wake();
        })
        .join()
        .unwrap();
        assert_eq!(kicks.load(Ordering::Relaxed), 2, "wakes coalesce");
        assert!(set.is_woken());
        set.poll(&mut out);
        assert_eq!(out, [3]);
        assert!(set.is_empty());
    }

    #[test]
    fn a_wake_after_a_drain_kicks_again() {
        let (loop_waker, kicks) = counting_waker();
        let mut set: TaskSet<u32, u8> = TaskSet::new(loop_waker);
        let (future, _open, waker) = gate(0);
        set.spawn(TaskId::fresh(), 0, Box::pin(future));
        let mut out = Vec::new();
        set.poll(&mut out);
        waker.lock().unwrap().take().unwrap().wake();
        set.poll(&mut out);
        waker.lock().unwrap().take().unwrap().wake();
        assert_eq!(
            kicks.load(Ordering::Relaxed),
            3,
            "the spawn, then one per drain"
        );
    }

    #[test]
    fn cancelling_drops_the_future_and_its_output() {
        struct Dropped(Rc<Cell<bool>>);
        impl Drop for Dropped {
            fn drop(&mut self) {
                self.0.set(true);
            }
        }
        let dropped = Rc::new(Cell::new(false));
        let mut set: TaskSet<u32, u8> = TaskSet::default();
        let (future, open, _waker) = gate(1);
        let guard = Dropped(dropped.clone());
        let id = TaskId::fresh();
        set.spawn(
            id,
            0,
            Box::pin(async move {
                let _guard = guard;
                future.await
            }),
        );
        let mut out = Vec::new();
        set.poll(&mut out);
        assert!(set.cancel(id));
        assert!(dropped.get());
        assert!(!set.cancel(id));
        open.store(true, Ordering::Release);
        set.poll(&mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn cancel_where_drops_only_the_matching_owner() {
        let mut set: TaskSet<u32, u8> = TaskSet::default();
        let (a, _a_open, _) = gate(1);
        let (b, b_open, b_waker) = gate(2);
        set.spawn(TaskId::fresh(), 1, Box::pin(a));
        set.spawn(TaskId::fresh(), 2, Box::pin(b));
        let mut out = Vec::new();
        set.poll(&mut out);
        set.cancel_where(|owner| owner == 1);
        assert_eq!(set.len(), 1);
        b_open.store(true, Ordering::Release);
        b_waker.lock().unwrap().take().unwrap().wake();
        set.poll(&mut out);
        assert_eq!(out, [2]);
        assert!(set.is_empty());
    }

    /// An owner whose key outlives it, as a reused slot does.
    #[derive(Clone, Copy, PartialEq, Eq)]
    struct Slot {
        key: u32,
        generation: u32,
    }

    impl TaskOwner for Slot {
        fn key(self) -> u32 {
            self.key
        }
    }

    #[test]
    fn cancel_owned_drops_the_owner_s_chain_and_spares_a_key_sharer() {
        let mut set: TaskSet<u32, Slot> = TaskSet::default();
        let old = Slot {
            key: 4,
            generation: 0,
        };
        let new = Slot {
            key: 4,
            generation: 1,
        };
        let other = Slot {
            key: 9,
            generation: 0,
        };
        let mut opens = Vec::new();
        let mut wakers = Vec::new();
        for (value, owner) in [(1, old), (2, new), (3, old), (4, other), (5, old)] {
            let (future, open, waker) = gate(value);
            set.spawn(TaskId::fresh(), owner, Box::pin(future));
            opens.push(open);
            wakers.push(waker);
        }
        let mut out = Vec::new();
        set.poll(&mut out);
        set.cancel_owned(old);
        assert_eq!(set.len(), 2);
        set.cancel_owned(old);
        assert_eq!(set.len(), 2, "a second cancel finds nothing");
        for (open, waker) in opens.iter().zip(&wakers) {
            open.store(true, Ordering::Release);
            if let Some(waker) = waker.lock().unwrap().take() {
                waker.wake();
            }
        }
        set.poll(&mut out);
        assert_eq!(out, [2, 4]);
        assert!(set.is_empty());
        set.cancel_owned(new);
        set.cancel_owned(other);
    }

    #[test]
    fn a_chain_survives_cancels_and_finishes_from_its_middle() {
        let mut set: TaskSet<u32, u8> = TaskSet::default();
        let ids: Vec<TaskId> = (0..5).map(|_| TaskId::fresh()).collect();
        let mut opens = Vec::new();
        for (n, &id) in ids.iter().enumerate() {
            let (future, open, _) = gate(n as u32);
            set.spawn(id, 1, Box::pin(future));
            opens.push(open);
        }
        let mut out = Vec::new();
        set.poll(&mut out);
        assert!(set.cancel(ids[2]));
        assert!(set.cancel(ids[4]));
        assert!(set.cancel(ids[0]));
        // A slot freed above is reused, and joins the same chain.
        let (future, _open, _) = gate(9);
        set.spawn(TaskId::fresh(), 1, Box::pin(future));
        assert_eq!(set.len(), 3);
        set.cancel_owned(1);
        assert!(set.is_empty());
        assert!(set.heads.is_empty());
        assert_eq!(set.free.len(), set.slots.len());
    }

    #[test]
    fn tasks_poll_in_id_order_whatever_the_spawn_order() {
        let mut set: TaskSet<u32, u8> = TaskSet::default();
        let first = TaskId::fresh();
        let second = TaskId::fresh();
        set.spawn(second, 0, Box::pin(async { 2 }));
        set.spawn(first, 0, Box::pin(async { 1 }));
        let mut out = Vec::new();
        set.poll(&mut out);
        assert_eq!(out, [1, 2]);
    }
}
