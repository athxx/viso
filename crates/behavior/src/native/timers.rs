//! The host timers `viso::time::sleep` waits on, and the timer thread it
//! falls back to when the host installs none.

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

/// The timers a host provides, installed on a [`Vm`](crate::Vm) as a
/// `Box<dyn Timers>`: a headless host installs one driven by its own clock.
pub trait Timers {
    /// Work that finishes once `duration` has passed.
    fn sleep(&mut self, duration: Duration) -> Pin<Box<dyn Future<Output = ()>>>;
}

/// The process's timer thread, started by the first sleep that finds no host
/// timers: one thread waits for the earliest deadline and wakes its sleeper.
pub(crate) fn sleep_on_thread(duration: Duration) -> Pin<Box<dyn Future<Output = ()>>> {
    static THREAD: OnceLock<Arc<Shared>> = OnceLock::new();
    let shared = THREAD.get_or_init(|| {
        let shared = Arc::new(Shared::default());
        let worker = Arc::clone(&shared);
        std::thread::Builder::new()
            .name("viso-timers".into())
            .spawn(move || worker.run())
            .expect("the timer thread starts");
        shared
    });
    let sleeper = Arc::new(Sleeper::default());
    let deadline = Instant::now() + duration;
    {
        let mut queue = shared.queue.lock().unwrap_or_else(|e| e.into_inner());
        queue.push(Due {
            at: Reverse(deadline),
            sleeper: Arc::clone(&sleeper),
        });
    }
    shared.changed.notify_one();
    Box::pin(Sleep(sleeper))
}

#[derive(Default)]
struct Shared {
    queue: Mutex<BinaryHeap<Due>>,
    changed: Condvar,
}

impl Shared {
    fn run(&self) {
        let mut queue = self.queue.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            let now = Instant::now();
            while queue.peek().is_some_and(|due| due.at.0 <= now) {
                if let Some(due) = queue.pop() {
                    due.sleeper.fire();
                }
            }
            queue = match queue.peek().map(|due| due.at.0 - now) {
                Some(wait) => {
                    self.changed
                        .wait_timeout(queue, wait)
                        .unwrap_or_else(|e| e.into_inner())
                        .0
                }
                None => self.changed.wait(queue).unwrap_or_else(|e| e.into_inner()),
            };
        }
    }
}

/// A deadline and the sleeper it wakes, earliest first in the heap.
struct Due {
    at: Reverse<Instant>,
    sleeper: Arc<Sleeper>,
}

impl PartialEq for Due {
    fn eq(&self, other: &Due) -> bool {
        self.at == other.at
    }
}

impl Eq for Due {}

impl PartialOrd for Due {
    fn partial_cmp(&self, other: &Due) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Due {
    fn cmp(&self, other: &Due) -> std::cmp::Ordering {
        self.at.cmp(&other.at)
    }
}

struct Sleeper {
    /// `None` once fired; otherwise the waker of the last poll.
    state: Mutex<Option<Option<Waker>>>,
}

impl Default for Sleeper {
    fn default() -> Sleeper {
        Sleeper {
            state: Mutex::new(Some(None)),
        }
    }
}

impl Sleeper {
    fn fire(&self) {
        let waker = self.state.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(Some(waker)) = waker {
            waker.wake();
        }
    }
}

struct Sleep(Arc<Sleeper>);

impl Future for Sleep {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        match state.as_mut() {
            None => Poll::Ready(()),
            Some(waker) => {
                *waker = Some(cx.waker().clone());
                Poll::Pending
            }
        }
    }
}
