//! Hops onto the main dispatch queue from any thread, on Apple targets.

use std::ffi::c_void;

#[repr(C)]
struct DispatchQueue {
    _opaque: [u8; 0],
}

// libdispatch ships in libSystem, which every Apple binary links.
unsafe extern "C" {
    static _dispatch_main_q: DispatchQueue;
    fn dispatch_async_f(
        queue: *const DispatchQueue,
        context: *mut c_void,
        work: extern "C" fn(*mut c_void),
    );
}

/// Run `work` on the main thread, soon, from whichever thread calls this.
/// `work` receives a null context.
pub(crate) fn post_to_main(work: extern "C" fn(*mut c_void)) {
    // SAFETY: `_dispatch_main_q` is libdispatch's static main-queue object,
    // valid for the life of the process, and `dispatch_async_f` may be called
    // from any thread. The context is null and `work` reads none.
    unsafe { dispatch_async_f(&raw const _dispatch_main_q, std::ptr::null_mut(), work) }
}
