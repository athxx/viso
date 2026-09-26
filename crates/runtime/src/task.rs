//! UI task identity, wakeup and cancellation (ADR 0032).
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
    woken: Mutex<Vec<TaskId>>,
    /// Set by the first wake after a drain; the drain clears it before taking
    /// the list, so a wake that lands after the take kicks the loop again.
    signalled: AtomicBool,
    loop_waker: LoopWaker,
}

struct TaskWaker {
    id: TaskId,
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
            .push(self.id);
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

struct Task<O, T> {
    id: TaskId,
    tag: T,
    future: Pin<Box<dyn Future<Output = O>>>,
    waker: Waker,
}

/// Loop-thread futures, each tagged with its owner, polled when woken.
pub struct TaskSet<O, T> {
    /// Sorted by id.
    tasks: Vec<Task<O, T>>,
    queue: Arc<WakeQueue>,
    /// Spawned since the last poll: due their first poll.
    spawned: Vec<TaskId>,
    /// Reused by `poll` for the ids it drains.
    scratch: Vec<TaskId>,
}

impl<O, T: Copy> TaskSet<O, T> {
    /// An empty set whose wakes kick `loop_waker`.
    pub fn new(loop_waker: LoopWaker) -> Self {
        Self {
            tasks: Vec::new(),
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
        let waker = Waker::from(Arc::new(TaskWaker {
            id,
            queue: self.queue.clone(),
        }));
        let at = self.tasks.partition_point(|t| t.id < id);
        self.tasks.insert(
            at,
            Task {
                id,
                tag,
                future,
                waker,
            },
        );
        self.spawned.push(id);
        self.queue.signal();
    }

    /// Drop task `id`'s future; `false` when no such task is live.
    pub fn cancel(&mut self, id: TaskId) -> bool {
        match self.tasks.binary_search_by_key(&id, |t| t.id) {
            Ok(at) => {
                self.tasks.remove(at);
                true
            }
            Err(_) => false,
        }
    }

    /// Drop every task whose tag satisfies `owned`.
    pub fn cancel_where(&mut self, mut owned: impl FnMut(T) -> bool) {
        self.tasks.retain(|t| !owned(t.tag));
    }

    /// Drop every task.
    pub fn clear(&mut self) {
        self.tasks.clear();
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
        self.scratch.sort_unstable();
        self.scratch.dedup();
        for &id in &self.scratch {
            // A task cancelled or finished since its wake is simply gone.
            let Ok(at) = self.tasks.binary_search_by_key(&id, |t| t.id) else {
                continue;
            };
            let task = &mut self.tasks[at];
            let mut cx = Context::from_waker(&task.waker);
            if let Poll::Ready(output) = task.future.as_mut().poll(&mut cx) {
                self.tasks.remove(at);
                out.push(output);
            }
        }
    }

    /// The number of live tasks.
    pub fn len(&self) -> usize {
        self.tasks.len()
    }

    /// Whether no task is live.
    pub fn is_empty(&self) -> bool {
        self.tasks.is_empty()
    }
}

impl<O, T: Copy> Default for TaskSet<O, T> {
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
