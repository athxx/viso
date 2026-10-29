//! Threads beside the loop's: started, counted, and the locks they share
//! with it taken.
//!
//! Native targets start an OS thread. A threaded web build (a shared-memory
//! module, its standard library built with atomics) starts a dedicated Web
//! Worker on the page's glue — the worker instantiates the module over the
//! page's memory and runs the job — and a web build without threads starts
//! none.
//!
//! The page's own thread may not block: a wait on it traps. [`lock`] never
//! waits there, spinning instead on a lock a worker holds, so a lock shared
//! with workers is taken with it on every thread.

use std::io;
use std::sync::{Mutex, MutexGuard};

/// A job a thread runs.
type Job = Box<dyn FnOnce() + Send + 'static>;

/// Starts `job` on a thread of its own named `name`.
///
/// # Errors
///
/// A target that starts no thread, or one that refused.
pub fn spawn(name: String, job: impl FnOnce() + Send + 'static) -> io::Result<()> {
    imp::spawn(name, Box::new(job))
}

/// The threads the machine runs at once: at least one.
pub fn parallelism() -> usize {
    imp::parallelism().max(1)
}

/// `mutex` locked, poisoned or not. On a threaded web build it never waits,
/// as the page's thread may not: it spins while another thread holds it.
pub fn lock<T: ?Sized>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    #[cfg(all(target_arch = "wasm32", target_feature = "atomics"))]
    loop {
        match mutex.try_lock() {
            Ok(guard) => return guard,
            Err(std::sync::TryLockError::Poisoned(poisoned)) => return poisoned.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => std::hint::spin_loop(),
        }
    }
    #[cfg(not(all(target_arch = "wasm32", target_feature = "atomics")))]
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
mod imp {
    use super::Job;

    pub(super) fn spawn(name: String, job: Job) -> std::io::Result<()> {
        std::thread::Builder::new().name(name).spawn(job).map(drop)
    }

    pub(super) fn parallelism() -> usize {
        std::thread::available_parallelism().map_or(1, std::num::NonZero::get)
    }
}

#[cfg(all(
    target_arch = "wasm32",
    target_os = "unknown",
    not(target_feature = "atomics")
))]
mod imp {
    use super::Job;

    pub(super) fn spawn(_name: String, _job: Job) -> std::io::Result<()> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "a web build without threads starts none",
        ))
    }

    pub(super) fn parallelism() -> usize {
        super::web_parallelism()
    }
}

#[cfg(all(
    target_arch = "wasm32",
    target_os = "unknown",
    target_feature = "atomics"
))]
mod imp {
    use std::cell::RefCell;
    use std::io;

    use js_sys::Array;
    use wasm_bindgen::JsCast;
    use wasm_bindgen::prelude::{Closure, JsValue, wasm_bindgen};
    use web_sys::{MessageEvent, Worker, WorkerOptions, WorkerType};

    use super::Job;

    #[wasm_bindgen]
    extern "C" {
        /// The page's glue, which its threads part names on every thread.
        #[wasm_bindgen(thread_local_v2, js_name = visoGlue)]
        static GLUE: JsValue;
    }

    thread_local! {
        /// The workers this thread started, kept for the wakes they post.
        static WORKERS: RefCell<Vec<Worker>> = const { RefCell::new(Vec::new()) };
        static ON_MESSAGE: Closure<dyn FnMut(MessageEvent)> = Closure::new(|event: MessageEvent| {
            if event.data().as_string().as_deref() == Some(crate::backend::web::WAKE) {
                crate::backend::web::woken();
            }
        });
    }

    fn refused(error: &JsValue) -> io::Error {
        io::Error::other(format!("the worker did not start: {error:?}"))
    }

    pub(super) fn spawn(name: String, job: Job) -> io::Result<()> {
        let glue = GLUE.with(JsValue::as_string).ok_or_else(|| {
            io::Error::new(io::ErrorKind::Unsupported, "the page's glue names no URL")
        })?;
        let options = WorkerOptions::new();
        options.set_type(WorkerType::Module);
        options.set_name(&name);
        let worker = Worker::new_with_options(&glue, &options).map_err(|e| refused(&e))?;
        ON_MESSAGE.with(|on| worker.set_onmessage(Some(on.as_ref().unchecked_ref())));
        let job = Box::into_raw(Box::new(job));
        let message = Array::of4(
            &JsValue::from_str("viso-thread"),
            &wasm_bindgen::module(),
            &wasm_bindgen::memory(),
            &JsValue::from(job as usize as u32),
        );
        if let Err(error) = worker.post_message(&message) {
            // SAFETY: the box was leaked above and nothing else holds it.
            drop(unsafe { Box::from_raw(job) });
            worker.terminate();
            return Err(refused(&error));
        }
        WORKERS.with(|workers| workers.borrow_mut().push(worker));
        Ok(())
    }

    pub(super) fn parallelism() -> usize {
        super::web_parallelism()
    }

    /// A worker's entry, which the glue's threads part calls once the
    /// module runs on it: the job [`spawn`] posted.
    #[wasm_bindgen]
    pub fn __viso_thread(job: u32) {
        // SAFETY: `job` is the box `spawn` leaked and posted to this worker
        // alone, and the worker calls this once.
        let job = unsafe { Box::from_raw(job as usize as *mut Job) };
        job();
    }
}

/// `navigator.hardwareConcurrency`, on the page or in a worker.
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
fn web_parallelism() -> usize {
    let navigator = js_sys::Reflect::get(&js_sys::global(), &"navigator".into());
    navigator
        .and_then(|n| js_sys::Reflect::get(&n, &"hardwareConcurrency".into()))
        .ok()
        .and_then(|n| n.as_f64())
        .map_or(1, |n| n as usize)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::mpsc;

    #[test]
    fn a_thread_runs_its_job_and_a_lock_outlives_a_poisoning() {
        let (done, ran) = mpsc::channel();
        spawn("viso-test".to_owned(), move || {
            let _ = done.send(std::thread::current().name().map(str::to_owned));
        })
        .unwrap();
        assert_eq!(ran.recv().unwrap().as_deref(), Some("viso-test"));
        assert!(parallelism() >= 1);

        let shared = Arc::new(Mutex::new(1));
        let poisoner = Arc::clone(&shared);
        let _ = std::thread::spawn(move || {
            let _guard = poisoner.lock().unwrap();
            panic!("poisons");
        })
        .join();
        *lock(&shared) += 1;
        assert_eq!(*lock(&shared), 2);
    }
}
