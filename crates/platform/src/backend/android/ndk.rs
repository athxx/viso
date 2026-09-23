//! The few NDK entry points the backend calls, declared by hand from the
//! NDK headers (`android/looper.h`, `android/choreographer.h`,
//! `android/native_window_jni.h`, `android/log.h`).

use std::ffi::{c_char, c_int, c_long, c_void};

use jni_sys::{JNIEnv, jobject};

#[repr(C)]
pub(super) struct ALooper {
    _opaque: [u8; 0],
}

#[repr(C)]
pub(super) struct AChoreographer {
    _opaque: [u8; 0],
}

#[repr(C)]
pub(super) struct ANativeWindow {
    _opaque: [u8; 0],
}

pub(super) const ALOOPER_PREPARE_ALLOW_NON_CALLBACKS: c_int = 1;
pub(super) const ALOOPER_POLL_ERROR: c_int = -4;

pub(super) const ANDROID_LOG_INFO: c_int = 4;
pub(super) const ANDROID_LOG_WARN: c_int = 5;

/// `AChoreographer_frameCallback`: the frame time as a C `long`.
pub(super) type FrameCallback = unsafe extern "C" fn(frame_time_nanos: c_long, data: *mut c_void);
/// `AChoreographer_frameCallback64` (API 29).
pub(super) type FrameCallback64 = unsafe extern "C" fn(frame_time_nanos: i64, data: *mut c_void);
/// `AChoreographer_postFrameCallback64`'s signature, resolved at run time.
pub(super) type PostFrameCallback64 =
    unsafe extern "C" fn(*mut AChoreographer, FrameCallback64, *mut c_void);

#[link(name = "android")]
unsafe extern "C" {
    pub(super) fn ALooper_prepare(opts: c_int) -> *mut ALooper;
    pub(super) fn ALooper_acquire(looper: *mut ALooper);
    pub(super) fn ALooper_wake(looper: *mut ALooper);
    pub(super) fn ALooper_pollOnce(
        timeout_millis: c_int,
        out_fd: *mut c_int,
        out_events: *mut c_int,
        out_data: *mut *mut c_void,
    ) -> c_int;

    pub(super) fn AChoreographer_getInstance() -> *mut AChoreographer;
    pub(super) fn AChoreographer_postFrameCallback(
        choreographer: *mut AChoreographer,
        callback: FrameCallback,
        data: *mut c_void,
    );

    pub(super) fn ANativeWindow_fromSurface(
        env: *mut JNIEnv,
        surface: jobject,
    ) -> *mut ANativeWindow;
    pub(super) fn ANativeWindow_release(window: *mut ANativeWindow);
}

#[link(name = "log")]
unsafe extern "C" {
    pub(super) fn __android_log_write(
        prio: c_int,
        tag: *const c_char,
        text: *const c_char,
    ) -> c_int;
}
