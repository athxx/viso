//! The system memory-pressure signal on Apple targets, delivered as a libdispatch
//! source. AppKit has no memory-warning callback, so the backend listens to the
//! kernel's pressure notifications directly and turns each warning or critical
//! transition into [`RawEvent::LowMemory`](crate::event::RawEvent::LowMemory).

use std::ffi::c_void;

#[repr(C)]
struct DispatchObject {
    _opaque: [u8; 0],
}

type DispatchSource = *mut DispatchObject;
type DispatchQueue = *mut DispatchObject;

const DISPATCH_MEMORYPRESSURE_WARN: usize = 0x02;
const DISPATCH_MEMORYPRESSURE_CRITICAL: usize = 0x04;

// libdispatch ships in libSystem, which every Apple binary links.
unsafe extern "C" {
    static _dispatch_source_type_memorypressure: DispatchObject;
    static _dispatch_main_q: DispatchObject;
    fn dispatch_source_create(
        kind: *const DispatchObject,
        handle: usize,
        mask: usize,
        queue: DispatchQueue,
    ) -> DispatchSource;
    fn dispatch_set_context(object: DispatchSource, context: *mut c_void);
    fn dispatch_source_set_event_handler_f(
        source: DispatchSource,
        handler: extern "C" fn(*mut c_void),
    );
    fn dispatch_source_set_cancel_handler_f(
        source: DispatchSource,
        handler: extern "C" fn(*mut c_void),
    );
    fn dispatch_resume(object: DispatchSource);
    fn dispatch_source_cancel(source: DispatchSource);
    fn dispatch_release(object: DispatchSource);
    #[cfg(test)]
    fn dispatch_queue_create(label: *const std::ffi::c_char, attr: *const c_void) -> DispatchQueue;
}

type Handler = Box<dyn FnMut()>;

/// A live pressure subscription. Dropping it cancels the source; the handler is
/// freed by the source's cancel callback on its own queue, so a notification
/// already queued never observes a freed handler.
pub(crate) struct MemoryPressure {
    source: DispatchSource,
}

impl MemoryPressure {
    /// Call `on_pressure` on the main queue whenever the system reports warning
    /// or critical memory pressure. Main-thread only: the handler is not `Send`
    /// and runs where the pump runs.
    pub(crate) fn on_main_queue(on_pressure: impl FnMut() + 'static) -> Option<Self> {
        // SAFETY: `_dispatch_main_q` is libdispatch's static main-queue object,
        // valid for the life of the process; the handler is `!Send` and the main
        // queue only ever runs on the main thread, which is where the caller is.
        unsafe {
            Self::on_queue(
                &raw const _dispatch_main_q as DispatchQueue,
                Box::new(on_pressure),
            )
        }
    }

    /// # Safety
    ///
    /// `queue` must be a live dispatch queue, and `handler` must be safe to call
    /// and drop on whichever thread that queue runs on.
    unsafe fn on_queue(queue: DispatchQueue, handler: Handler) -> Option<Self> {
        // SAFETY: the type symbol is libdispatch's static source-type descriptor;
        // the mask is the documented pressure-level set; `queue` is live per the
        // caller's contract. A null return means the source type is unavailable.
        let source = unsafe {
            dispatch_source_create(
                &raw const _dispatch_source_type_memorypressure,
                0,
                DISPATCH_MEMORYPRESSURE_WARN | DISPATCH_MEMORYPRESSURE_CRITICAL,
                queue,
            )
        };
        if source.is_null() {
            return None;
        }
        let context = Box::into_raw(Box::new(handler)).cast::<c_void>();
        // SAFETY: `source` was just created and is suspended. The context is a
        // leaked `Box<Handler>` owned by the source from here on: the event
        // callback borrows it and the cancel callback, which libdispatch runs
        // exactly once after the last event callback, reclaims it.
        unsafe {
            dispatch_set_context(source, context);
            dispatch_source_set_event_handler_f(source, fire);
            dispatch_source_set_cancel_handler_f(source, reclaim);
            dispatch_resume(source);
        }
        Some(Self { source })
    }
}

impl Drop for MemoryPressure {
    fn drop(&mut self) {
        // SAFETY: `source` is the live source this value owns. Cancelling stops
        // further event callbacks and schedules the cancel callback that frees
        // the handler; releasing drops this value's reference, and libdispatch
        // keeps the source alive until its cancel callback has run.
        unsafe {
            dispatch_source_cancel(self.source);
            dispatch_release(self.source);
        }
    }
}

extern "C" fn fire(context: *mut c_void) {
    // SAFETY: the context is the `Box<Handler>` installed in `on_queue`, alive
    // until `reclaim` runs; libdispatch never runs an event callback after the
    // cancel callback and serializes callbacks on the source's queue.
    let handler = unsafe { &mut *context.cast::<Handler>() };
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(handler));
}

extern "C" fn reclaim(context: *mut c_void) {
    // SAFETY: libdispatch runs the cancel callback exactly once, after the last
    // event callback, with the context `on_queue` leaked from a `Box<Handler>`.
    drop(unsafe { Box::from_raw(context.cast::<Handler>()) });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    struct Flag(Arc<AtomicBool>);

    impl Drop for Flag {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    #[test]
    fn dropping_the_subscription_frees_its_handler_on_the_source_queue() {
        let freed = Arc::new(AtomicBool::new(false));
        let flag = Flag(Arc::clone(&freed));
        // SAFETY: a private serial queue created here and never released lives
        // for the test; the handler only holds a `Send` flag.
        let pressure = unsafe {
            let queue = dispatch_queue_create(c"viso.test.pressure".as_ptr(), std::ptr::null());
            MemoryPressure::on_queue(
                queue,
                Box::new(move || {
                    let _ = &flag;
                }),
            )
        }
        .expect("the pressure source type exists on Apple targets");
        assert!(
            !freed.load(Ordering::SeqCst),
            "a live subscription keeps its handler"
        );
        drop(pressure);
        let deadline = Instant::now() + Duration::from_secs(5);
        while !freed.load(Ordering::SeqCst) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(freed.load(Ordering::SeqCst), "cancel frees the handler");
    }
}
