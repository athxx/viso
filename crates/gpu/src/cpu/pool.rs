//! The raster's worker threads: started once, parked between passes, so a
//! frame neither spawns a thread nor allocates to run its tiles in parallel.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

/// A job: run on every worker with its index, borrowed for one
/// [`Pool::broadcast`].
type Job<'a> = dyn Fn(usize) + Sync + 'a;

/// A pointer to the current job with its lifetime erased; only dereferenced
/// between the [`Pool::broadcast`] that set it and the wait that ends it.
#[derive(Clone, Copy)]
struct JobPtr(*const Job<'static>);

// SAFETY: the pointee is `Sync`, and `broadcast` keeps it alive until every
// worker that received the pointer has finished with it.
unsafe impl Send for JobPtr {}

#[derive(Default)]
struct State {
    job: Option<JobPtr>,
    /// Bumped per broadcast, so a worker runs each job once.
    generation: u64,
    /// Workers still running the current job.
    running: usize,
    panicked: bool,
    shutdown: bool,
}

#[derive(Default)]
struct Shared {
    state: Mutex<State>,
    wake: Condvar,
    done: Condvar,
}

/// `n` parked worker threads; the calling thread is worker `0` of every job.
pub(super) struct Pool {
    shared: Arc<Shared>,
    threads: Vec<JoinHandle<()>>,
}

impl Pool {
    /// A pool of `threads` workers beside the caller.
    pub(super) fn new(threads: usize) -> Pool {
        let shared = Arc::new(Shared::default());
        let threads = (1..=threads)
            .map(|index| {
                let shared = Arc::clone(&shared);
                std::thread::Builder::new()
                    .name(format!("viso-raster-{index}"))
                    .spawn(move || work(&shared, index))
                    .expect("spawns a raster worker")
            })
            .collect();
        Pool { shared, threads }
    }

    /// Workers per job, the caller included.
    pub(super) fn width(&self) -> usize {
        self.threads.len() + 1
    }

    /// Runs `job(worker)` on every worker and the caller, returning once all
    /// have; a worker's panic is raised here.
    pub(super) fn broadcast(&self, job: &Job<'_>) {
        if self.threads.is_empty() {
            job(0);
            return;
        }
        // SAFETY: `'static` is a lie told to the pointer only; `Wait` below
        // blocks — on return and on unwind alike — until no worker runs the
        // job, and only then may `job`'s borrow end.
        let erased: &'static Job<'static> =
            unsafe { std::mem::transmute::<&Job<'_>, &'static Job<'static>>(job) };
        {
            let mut state = self.shared.state.lock().expect("raster pool lock");
            state.job = Some(JobPtr(erased));
            state.generation += 1;
            state.running = self.threads.len();
            state.panicked = false;
        }
        self.shared.wake.notify_all();
        let wait = Wait(&self.shared);
        job(0);
        drop(wait);
        let panicked =
            std::mem::take(&mut self.shared.state.lock().expect("raster pool lock").panicked);
        assert!(!panicked, "a raster worker panicked");
    }
}

/// Blocks until the current job's workers have all finished, then clears it.
struct Wait<'a>(&'a Shared);

impl Drop for Wait<'_> {
    fn drop(&mut self) {
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        while state.running > 0 {
            state = self.0.done.wait(state).unwrap_or_else(|e| e.into_inner());
        }
        state.job = None;
    }
}

fn work(shared: &Shared, index: usize) {
    let mut seen = 0;
    loop {
        let job = {
            let mut state = shared.state.lock().expect("raster pool lock");
            while !state.shutdown && (state.generation == seen || state.job.is_none()) {
                state = shared.wake.wait(state).expect("raster pool lock");
            }
            if state.shutdown {
                return;
            }
            seen = state.generation;
            state.job.expect("checked")
        };
        // SAFETY: the job pointer stays valid until `running` reaches zero,
        // which this worker's decrement below is part of.
        let ok = catch_unwind(AssertUnwindSafe(|| unsafe { (*job.0)(index) })).is_ok();
        let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
        state.panicked |= !ok;
        state.running -= 1;
        if state.running == 0 {
            shared.done.notify_all();
        }
    }
}

impl Drop for Pool {
    fn drop(&mut self) {
        self.shared
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .shutdown = true;
        self.shared.wake.notify_all();
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn every_worker_runs_each_job_once_and_the_caller_waits() {
        let pool = Pool::new(3);
        assert_eq!(pool.width(), 4);
        for round in 1..=50 {
            let hits = AtomicUsize::new(0);
            let mask = AtomicUsize::new(0);
            pool.broadcast(&|w| {
                hits.fetch_add(1, Ordering::Relaxed);
                mask.fetch_or(1 << w, Ordering::Relaxed);
            });
            assert_eq!(hits.load(Ordering::Relaxed), 4, "round {round}");
            assert_eq!(mask.load(Ordering::Relaxed), 0b1111);
        }
    }

    #[test]
    fn a_worker_panic_reaches_the_caller_and_the_pool_survives() {
        let pool = Pool::new(2);
        let hit = catch_unwind(AssertUnwindSafe(|| {
            pool.broadcast(&|w| assert_ne!(w, 2, "worker two fails"));
        }));
        assert!(hit.is_err());
        let count = AtomicUsize::new(0);
        pool.broadcast(&|_| {
            count.fetch_add(1, Ordering::Relaxed);
        });
        assert_eq!(count.load(Ordering::Relaxed), 3);
    }
}
