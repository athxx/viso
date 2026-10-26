//! Off-loop work: jobs that run on a resident pool of worker threads, shared
//! by everything in the process that has heavy, self-contained work to do
//! (decoding, building geometry) and must keep it off the UI thread.
//!
//! The pool is sized once, by [`configure`] before the first job; a build
//! that configures no threads, or a target that cannot spawn one, keeps the
//! jobs queued for the loop thread to run in idle slices
//! ([`run_inline`]), so no caller may assume a worker exists. A job hands its
//! result back however it likes — a completer that wakes the task awaiting
//! it is the usual way — and should check its own cancellation flag first,
//! so stale work costs nothing.
//!
//! Jobs run in the order submitted: a caller that cares about priority
//! submits what matters most first.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex, MutexGuard, OnceLock};
use std::time::Duration;

type Job = Box<dyn FnOnce() + Send + 'static>;

/// No size configured yet: the pool sizes itself to the machine.
const UNSET: usize = usize::MAX;

static THREADS: AtomicUsize = AtomicUsize::new(UNSET);
static POOL: OnceLock<Pool> = OnceLock::new();

struct Pool {
    queue: Mutex<VecDeque<Job>>,
    ready: Condvar,
    /// The worker threads running; `0` runs every job inline.
    threads: usize,
}

fn lock(queue: &Mutex<VecDeque<Job>>) -> MutexGuard<'_, VecDeque<Job>> {
    queue.lock().unwrap_or_else(|e| e.into_inner())
}

/// The threads a pool sized to the machine runs: one fewer than its cores,
/// at least one and at most eight; none where threads cannot be spawned.
fn machine_threads() -> usize {
    if cfg!(all(target_arch = "wasm32", target_os = "unknown")) {
        return 0;
    }
    std::thread::available_parallelism().map_or(1, |n| n.get().saturating_sub(1).clamp(1, 8))
}

fn pool() -> &'static Pool {
    POOL.get_or_init(|| {
        let wanted = match THREADS.load(Ordering::Relaxed) {
            UNSET => machine_threads(),
            n => n,
        };
        let mut spawned = 0;
        for i in 0..wanted {
            let ok = std::thread::Builder::new()
                .name(format!("viso-work-{i}"))
                .spawn(work)
                .is_ok();
            if !ok {
                break;
            }
            spawned += 1;
        }
        Pool {
            queue: Mutex::new(VecDeque::new()),
            ready: Condvar::new(),
            threads: spawned,
        }
    })
}

/// A worker thread's loop: take the oldest job and run it, forever.
fn work() {
    let pool = pool();
    loop {
        let job = {
            let mut queue = lock(&pool.queue);
            loop {
                if let Some(job) = queue.pop_front() {
                    break job;
                }
                queue = pool.ready.wait(queue).unwrap_or_else(|e| e.into_inner());
            }
        };
        job();
    }
}

/// Size the pool at `threads` worker threads, `0` running every job on the
/// loop thread. Only a call before the pool's first job counts; later calls
/// change nothing.
pub fn configure(threads: usize) {
    if POOL.get().is_none() {
        THREADS.store(threads, Ordering::Relaxed);
    }
}

/// The worker threads the pool runs; `0` when jobs run inline.
pub fn threads() -> usize {
    pool().threads
}

/// Run `job` on a worker thread, or queue it for [`run_inline`] where there
/// is none.
pub fn submit(job: impl FnOnce() + Send + 'static) {
    let pool = pool();
    lock(&pool.queue).push_back(Box::new(job));
    if pool.threads > 0 {
        pool.ready.notify_one();
    }
}

/// Whether jobs wait for the loop thread to run them: a pool with no threads
/// holding work.
pub fn inline_pending() -> bool {
    POOL.get()
        .is_some_and(|pool| pool.threads == 0 && !lock(&pool.queue).is_empty())
}

/// With no worker threads, run queued jobs on this thread until `slice` is
/// spent — at least one, so a backlog always drains — and say whether any
/// remain. A pool with threads runs its own; this is then a no-op.
pub fn run_inline(slice: Duration) -> bool {
    let Some(pool) = POOL.get().filter(|pool| pool.threads == 0) else {
        return false;
    };
    let start = viso_runtime::Instant::now();
    loop {
        let Some(job) = lock(&pool.queue).pop_front() else {
            return false;
        };
        job();
        if start.elapsed() >= slice {
            return !lock(&pool.queue).is_empty();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::AtomicU32;

    #[test]
    fn jobs_run_once_each() {
        let done = Arc::new(AtomicU32::new(0));
        for _ in 0..64 {
            let done = Arc::clone(&done);
            submit(move || {
                done.fetch_add(1, Ordering::SeqCst);
            });
        }
        let start = std::time::Instant::now();
        while done.load(Ordering::SeqCst) < 64 && start.elapsed() < Duration::from_secs(10) {
            run_inline(Duration::MAX);
            std::thread::yield_now();
        }
        assert_eq!(done.load(Ordering::SeqCst), 64);
        assert!(!inline_pending());
    }
}
