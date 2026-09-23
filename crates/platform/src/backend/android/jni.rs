//! The JNI half of the Android backend.
//!
//! `JNI_OnLoad` caches the VM and registers the `VisoActivity` natives. The
//! natives run on the UI thread and only translate their arguments into
//! [`Msg`]s for the loop thread, except for the two handshakes the activity
//! must wait on: a surface being destroyed and the activity finishing.
//! [`nativeStart`](native_start) runs the app's `main` on the loop thread the
//! first time an activity is created; later activities (after a recreation)
//! only replace the activity the loop calls back into.

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::io::{BufRead, BufReader};
use std::os::fd::FromRawFd;
use std::ptr;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use jni_sys::{
    JNI_ERR, JNI_OK, JNI_VERSION_1_6, JNIEnv, JNINativeMethod, JavaVM, jboolean, jclass, jfloat,
    jfloatArray, jint, jintArray, jmethodID, jobject, jstring, jvalue,
};

use super::ndk;
use super::{Msg, Surface, Touch, send, send_and_wait};

/// Call a `JNIEnv` function.
macro_rules! env_call {
    ($env:expr, $f:ident $(, $arg:expr)* $(,)?) => {
        ((**$env).v1_6.$f)($env $(, $arg)*)
    };
}

/// A JNI global reference or method id. Both are valid on every thread for
/// as long as the VM runs (global references until deleted).
#[derive(Clone, Copy)]
struct Global<T>(T);

// SAFETY: see `Global`: global references and method ids are not tied to
// the thread that made them.
unsafe impl<T> Send for Global<T> {}
// SAFETY: as above; the handles are immutable values.
unsafe impl<T> Sync for Global<T> {}

/// The `VisoActivity` methods the loop calls.
struct Methods {
    set_soft_keyboard: Global<jmethodID>,
    set_text_input: Global<jmethodID>,
    set_pointer_icon: Global<jmethodID>,
    set_clipboard: Global<jmethodID>,
    get_clipboard: Global<jmethodID>,
    set_title_text: Global<jmethodID>,
    finish: Global<jmethodID>,
}

struct Java {
    vm: Global<*mut JavaVM>,
    methods: Methods,
    /// The live activity, a global reference; replaced when the system
    /// recreates the activity.
    activity: Mutex<Global<jobject>>,
}

static JAVA: OnceLock<Java> = OnceLock::new();

/// The launch parameters `nativeStart` received.
pub(super) struct Launch {
    pub density: f64,
    pub appearance: i32,
}

static LAUNCH: OnceLock<Launch> = OnceLock::new();

pub(super) fn launch_parameters() -> &'static Launch {
    LAUNCH
        .get()
        .expect("the loop thread starts after nativeStart")
}

/// The calling thread's `JNIEnv`, if it is attached to the VM.
fn env() -> Option<*mut JNIEnv> {
    let java = JAVA.get()?;
    let vm = java.vm.0;
    let mut env: *mut c_void = ptr::null_mut();
    // SAFETY: `vm` is the VM `JNI_OnLoad` received, valid for the process;
    // `GetEnv` only writes the out-pointer.
    let status = unsafe { ((**vm).v1_4.GetEnv)(vm, &mut env, JNI_VERSION_1_6) };
    (status == JNI_OK).then_some(env.cast())
}

/// Clear a pending Java exception after a call, logging it.
///
/// # Safety
/// `env` must be the calling thread's attached environment.
unsafe fn check(env: *mut JNIEnv) -> bool {
    // SAFETY: the caller's contract.
    unsafe {
        if env_call!(env, ExceptionCheck) {
            env_call!(env, ExceptionDescribe);
            env_call!(env, ExceptionClear);
            return false;
        }
    }
    true
}

/// Call a `void` method of the activity from the (attached) loop thread.
fn call_void(method: impl FnOnce(&Methods) -> Global<jmethodID>, args: &[jvalue]) {
    let (Some(java), Some(env)) = (JAVA.get(), env()) else {
        return;
    };
    let activity = java.activity.lock().unwrap_or_else(|e| e.into_inner()).0;
    if activity.is_null() {
        return;
    }
    let id = method(&java.methods).0;
    // SAFETY: `activity` is a live global reference to a `VisoActivity`,
    // `id` one of its methods, and `args` matches that method's signature
    // (each caller below passes the arguments its Java declaration takes).
    unsafe {
        env_call!(env, CallVoidMethodA, activity, id, args.as_ptr());
        check(env);
    }
}

/// A Java string from `text`, as a local reference, or null.
///
/// # Safety
/// `env` must be the calling thread's attached environment.
unsafe fn new_string(env: *mut JNIEnv, text: &str) -> jstring {
    let units: Vec<u16> = text.encode_utf16().collect();
    // SAFETY: the caller's contract; `NewString` copies `units`.
    unsafe { env_call!(env, NewString, units.as_ptr(), units.len() as jint) }
}

/// A Rust string from a Java one (null is `None`).
///
/// # Safety
/// `env` must be the calling thread's attached environment and `s` a live
/// string reference or null.
unsafe fn read_string(env: *mut JNIEnv, s: jstring) -> Option<String> {
    if s.is_null() {
        return None;
    }
    // SAFETY: the caller's contract; the region read stays within the
    // string's length.
    unsafe {
        let len = env_call!(env, GetStringLength, s);
        let mut units = vec![0u16; len.max(0) as usize];
        env_call!(env, GetStringRegion, s, 0, len, units.as_mut_ptr());
        Some(String::from_utf16_lossy(&units))
    }
}

pub(super) fn set_soft_keyboard(show: bool) {
    call_void(|m| m.set_soft_keyboard, &[jvalue { z: show }]);
}

pub(super) fn set_text_input(caret: Option<[f32; 4]>) {
    let [x, y, w, h] = caret.unwrap_or_default();
    call_void(
        |m| m.set_text_input,
        &[
            jvalue { z: caret.is_some() },
            jvalue { f: x },
            jvalue { f: y },
            jvalue { f: w },
            jvalue { f: h },
        ],
    );
}

pub(super) fn set_pointer_icon(icon: i32) {
    call_void(|m| m.set_pointer_icon, &[jvalue { i: icon }]);
}

pub(super) fn set_title(title: &str) {
    with_string(title, |s| {
        call_void(|m| m.set_title_text, &[jvalue { l: s }])
    });
}

pub(super) fn set_clipboard(text: &str) {
    with_string(text, |s| call_void(|m| m.set_clipboard, &[jvalue { l: s }]));
}

pub(super) fn finish() {
    call_void(|m| m.finish, &[]);
}

/// Run `f` with `text` as a Java string local reference.
fn with_string(text: &str, f: impl FnOnce(jobject)) {
    let Some(env) = env() else { return };
    // SAFETY: `env` is this thread's environment; the local reference is
    // deleted once `f` is done with it.
    unsafe {
        let s = new_string(env, text);
        if !check(env) || s.is_null() {
            return;
        }
        f(s);
        env_call!(env, DeleteLocalRef, s);
    }
}

pub(super) fn clipboard() -> Option<String> {
    let (java, env) = (JAVA.get()?, env()?);
    let activity = java.activity.lock().unwrap_or_else(|e| e.into_inner()).0;
    if activity.is_null() {
        return None;
    }
    // SAFETY: as in `call_void`; `getClipboard()` takes no arguments and
    // returns a String (or null), whose local reference is deleted here.
    unsafe {
        let s = env_call!(
            env,
            CallObjectMethodA,
            activity,
            java.methods.get_clipboard.0,
            ptr::null()
        );
        if !check(env) {
            return None;
        }
        let text = read_string(env, s);
        if !s.is_null() {
            env_call!(env, DeleteLocalRef, s);
        }
        text
    }
}

/// Attach the calling thread to the VM under `name`, for its life.
pub(super) fn attach_current_thread(name: &CStr) -> bool {
    let Some(java) = JAVA.get() else { return false };
    let vm = java.vm.0;
    #[repr(C)]
    struct AttachArgs {
        version: jint,
        name: *const c_char,
        group: jobject,
    }
    let mut args = AttachArgs {
        version: JNI_VERSION_1_6,
        name: name.as_ptr(),
        group: ptr::null_mut(),
    };
    let mut env: *mut c_void = ptr::null_mut();
    // SAFETY: `vm` is live for the process; `args` is a
    // `JavaVMAttachArgs` that outlives the call.
    unsafe { ((**vm).v1_4.AttachCurrentThread)(vm, &mut env, (&raw mut args).cast()) == JNI_OK }
}

pub(super) fn detach_current_thread() {
    if let Some(java) = JAVA.get() {
        let vm = java.vm.0;
        // SAFETY: called once by the loop thread as it ends, after its last
        // JNI call.
        unsafe { ((**vm).v1_4.DetachCurrentThread)(vm) };
    }
}

/// Route stdout and stderr to logcat, so panics and prints are visible.
fn redirect_output_to_logcat() {
    let mut fds = [0 as c_int; 2];
    // SAFETY: `pipe` fills `fds`; `dup2` replaces descriptors 1 and 2,
    // which the process owns, with the pipe's write end.
    unsafe {
        if libc::pipe(fds.as_mut_ptr()) != 0 {
            return;
        }
        libc::dup2(fds[1], 1);
        libc::dup2(fds[1], 2);
        libc::close(fds[1]);
    }
    // SAFETY: `fds[0]` is the pipe's read end, owned from here by the file.
    let reader = unsafe { std::fs::File::from_raw_fd(fds[0]) };
    let _ = std::thread::Builder::new()
        .name("viso-log".into())
        .spawn(move || {
            let tag = c"viso";
            for line in BufReader::new(reader).split(b'\n').map_while(Result::ok) {
                let Ok(text) = CString::new(line) else {
                    continue;
                };
                // SAFETY: both strings are NUL-terminated and live for the
                // call.
                unsafe {
                    ndk::__android_log_write(ndk::ANDROID_LOG_INFO, tag.as_ptr(), text.as_ptr())
                };
            }
        });
}

/// Write a warning to logcat directly (usable before the redirect).
pub(super) fn log_warning(text: &str) {
    if let Ok(text) = CString::new(text) {
        // SAFETY: both strings are NUL-terminated and live for the call.
        unsafe { ndk::__android_log_write(ndk::ANDROID_LOG_WARN, c"viso".as_ptr(), text.as_ptr()) };
    }
}

/// The entry point the VM calls when `System.loadLibrary` loads the app.
///
/// # Safety
/// Called by the VM with a valid `JavaVM`.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn JNI_OnLoad(vm: *mut JavaVM, _reserved: *mut c_void) -> jint {
    let mut env: *mut c_void = ptr::null_mut();
    // SAFETY: the VM calls this on a thread attached to it.
    if unsafe { ((**vm).v1_4.GetEnv)(vm, &mut env, JNI_VERSION_1_6) } != JNI_OK {
        return JNI_ERR;
    }
    let env: *mut JNIEnv = env.cast();
    // SAFETY: `env` is this thread's environment; `FindClass` runs from
    // `System.loadLibrary`, so it resolves through the app's class loader.
    // The method names and signatures match `VisoActivity.java`.
    unsafe {
        let class = env_call!(env, FindClass, c"dev/viso/VisoActivity".as_ptr());
        if !check(env) || class.is_null() {
            log_warning("dev.viso.VisoActivity not found");
            return JNI_ERR;
        }
        let method = |name: &CStr, sig: &CStr| {
            Global(env_call!(
                env,
                GetMethodID,
                class,
                name.as_ptr(),
                sig.as_ptr()
            ))
        };
        let methods = Methods {
            set_soft_keyboard: method(c"setSoftKeyboard", c"(Z)V"),
            set_text_input: method(c"setTextInput", c"(ZFFFF)V"),
            set_pointer_icon: method(c"setPointerIcon", c"(I)V"),
            set_clipboard: method(c"setClipboard", c"(Ljava/lang/String;)V"),
            get_clipboard: method(c"getClipboard", c"()Ljava/lang/String;"),
            set_title_text: method(c"setTitleText", c"(Ljava/lang/String;)V"),
            finish: method(c"finishFromNative", c"()V"),
        };
        if !check(env) {
            return JNI_ERR;
        }
        if !register_natives(env, class) {
            return JNI_ERR;
        }
        env_call!(env, DeleteLocalRef, class);
        let _ = JAVA.set(Java {
            vm: Global(vm),
            methods,
            activity: Mutex::new(Global(ptr::null_mut())),
        });
    }
    JNI_VERSION_1_6
}

/// # Safety
/// `env` must be the calling thread's environment and `class` the
/// `VisoActivity` class.
unsafe fn register_natives(env: *mut JNIEnv, class: jclass) -> bool {
    let natives: [(&CStr, &CStr, *mut c_void); 16] = [
        (
            c"nativeStart",
            c"(Ldev/viso/VisoActivity;FI)V",
            native_start as *mut c_void,
        ),
        (c"nativeDestroy", c"()V", native_destroy as *mut c_void),
        (
            c"nativeSurfaceChanged",
            c"(Landroid/view/Surface;II)V",
            native_surface_changed as *mut c_void,
        ),
        (
            c"nativeSurfaceDestroyed",
            c"()V",
            native_surface_destroyed as *mut c_void,
        ),
        (c"nativeRedraw", c"()V", native_redraw as *mut c_void),
        (c"nativeLifecycle", c"(Z)V", native_lifecycle as *mut c_void),
        (c"nativeFocus", c"(Z)V", native_focus as *mut c_void),
        (c"nativeInsets", c"(IIIII)V", native_insets as *mut c_void),
        (c"nativeConfig", c"(FI)V", native_config as *mut c_void),
        (c"nativeLowMemory", c"()V", native_low_memory as *mut c_void),
        (
            c"nativeTouch",
            c"(II[I[F[III)V",
            native_touch as *mut c_void,
        ),
        (c"nativeScroll", c"(FFFFI)V", native_scroll as *mut c_void),
        (
            c"nativeKey",
            c"(IZZILjava/lang/String;)V",
            native_key as *mut c_void,
        ),
        (
            c"nativePreedit",
            c"(Ljava/lang/String;I)V",
            native_preedit as *mut c_void,
        ),
        (
            c"nativeCommit",
            c"(Ljava/lang/String;)V",
            native_commit as *mut c_void,
        ),
        (c"nativeEdit", c"(I)V", native_edit as *mut c_void),
    ];
    let methods: Vec<JNINativeMethod> = natives
        .iter()
        .map(|(name, sig, f)| JNINativeMethod {
            name: name.as_ptr().cast_mut(),
            signature: sig.as_ptr().cast_mut(),
            fnPtr: *f,
        })
        .collect();
    // SAFETY: the caller's contract; each function pointer has the
    // `(JNIEnv*, jclass, args…)` signature its descriptor declares.
    unsafe {
        env_call!(
            env,
            RegisterNatives,
            class,
            methods.as_ptr(),
            methods.len() as jint
        ) == JNI_OK
            && check(env)
    }
}

// The natives. Each runs on the UI thread with a valid `env`.

extern "system" fn native_start(
    env: *mut JNIEnv,
    _class: jclass,
    activity: jobject,
    density: jfloat,
    appearance: jint,
) {
    let Some(java) = JAVA.get() else { return };
    // SAFETY: `activity` is a live local reference; the global reference
    // made from it replaces (and deletes) the previous activity's.
    unsafe {
        let global = env_call!(env, NewGlobalRef, activity);
        let mut slot = java.activity.lock().unwrap_or_else(|e| e.into_inner());
        if !slot.0.is_null() {
            env_call!(env, DeleteGlobalRef, slot.0);
        }
        slot.0 = global;
    }
    let first = LAUNCH
        .set(Launch {
            density: f64::from(density),
            appearance,
        })
        .is_ok();
    if !first {
        send(Msg::Config {
            density: f64::from(density),
            appearance,
        });
        return;
    }
    redirect_output_to_logcat();
    let spawned = std::thread::Builder::new()
        .name("viso-main".into())
        .stack_size(8 << 20)
        .spawn(super::loop_thread);
    if spawned.is_err() {
        log_warning("could not start the app thread");
    }
}

extern "system" fn native_destroy(_env: *mut JNIEnv, _class: jclass) {
    send_and_wait(Msg::Destroy, Duration::from_secs(2));
}

extern "system" fn native_surface_changed(
    env: *mut JNIEnv,
    _class: jclass,
    surface: jobject,
    width: jint,
    height: jint,
) {
    // SAFETY: `surface` is a live `android.view.Surface`; the returned
    // window carries a reference the loop thread releases.
    let window = unsafe { ndk::ANativeWindow_fromSurface(env, surface) };
    if window.is_null() {
        return;
    }
    let size = (width.max(0) as u32, height.max(0) as u32);
    send(Msg::Surface(Surface { window, size }));
}

extern "system" fn native_surface_destroyed(_env: *mut JNIEnv, _class: jclass) {
    send_and_wait(Msg::SurfaceDestroyed, Duration::from_secs(2));
}

extern "system" fn native_redraw(_env: *mut JNIEnv, _class: jclass) {
    send(Msg::Redraw);
}

extern "system" fn native_lifecycle(_env: *mut JNIEnv, _class: jclass, visible: jboolean) {
    send(Msg::Visible(visible));
}

extern "system" fn native_focus(_env: *mut JNIEnv, _class: jclass, focused: jboolean) {
    send(Msg::Focus(focused));
}

extern "system" fn native_insets(
    _env: *mut JNIEnv,
    _class: jclass,
    top: jint,
    left: jint,
    bottom: jint,
    right: jint,
    ime: jint,
) {
    send(Msg::Insets {
        bars: [top, left, bottom, right],
        ime,
    });
}

extern "system" fn native_config(
    _env: *mut JNIEnv,
    _class: jclass,
    density: jfloat,
    appearance: jint,
) {
    send(Msg::Config {
        density: f64::from(density),
        appearance,
    });
}

extern "system" fn native_low_memory(_env: *mut JNIEnv, _class: jclass) {
    send(Msg::LowMemory);
}

extern "system" fn native_touch(
    env: *mut JNIEnv,
    _class: jclass,
    action: jint,
    action_index: jint,
    ids: jintArray,
    samples: jfloatArray,
    tools: jintArray,
    buttons: jint,
    meta: jint,
) {
    // SAFETY: the arrays are live Java arrays of the lengths read here; each
    // region read stays within them (`samples` holds three floats a
    // pointer).
    let touch = unsafe {
        let count = env_call!(env, GetArrayLength, ids).max(0);
        if env_call!(env, GetArrayLength, samples) < count * 3
            || env_call!(env, GetArrayLength, tools) < count
        {
            return;
        }
        let n = count as usize;
        let mut touch = Touch {
            action,
            action_index: action_index.max(0) as usize,
            ids: vec![0; n],
            samples: vec![0.0; n * 3],
            tools: vec![0; n],
            buttons,
            meta,
        };
        env_call!(
            env,
            GetIntArrayRegion,
            ids,
            0,
            count,
            touch.ids.as_mut_ptr()
        );
        env_call!(
            env,
            GetFloatArrayRegion,
            samples,
            0,
            count * 3,
            touch.samples.as_mut_ptr()
        );
        env_call!(
            env,
            GetIntArrayRegion,
            tools,
            0,
            count,
            touch.tools.as_mut_ptr()
        );
        touch
    };
    send(Msg::Touch(touch));
}

extern "system" fn native_scroll(
    _env: *mut JNIEnv,
    _class: jclass,
    x: jfloat,
    y: jfloat,
    dx: jfloat,
    dy: jfloat,
    meta: jint,
) {
    send(Msg::Scroll {
        at: [x, y],
        delta: [dx, dy],
        meta,
    });
}

extern "system" fn native_key(
    env: *mut JNIEnv,
    _class: jclass,
    code: jint,
    pressed: jboolean,
    repeat: jboolean,
    meta: jint,
    text: jstring,
) {
    // SAFETY: `text` is a live string or null.
    let text = unsafe { read_string(env, text) };
    send(Msg::Key {
        code,
        pressed,
        repeat,
        meta,
        text,
    });
}

extern "system" fn native_preedit(env: *mut JNIEnv, _class: jclass, text: jstring, caret: jint) {
    // SAFETY: `text` is a live string or null.
    let text = unsafe { read_string(env, text) }.unwrap_or_default();
    send(Msg::Preedit {
        text,
        caret_utf16: caret.max(0) as usize,
    });
}

extern "system" fn native_commit(env: *mut JNIEnv, _class: jclass, text: jstring) {
    // SAFETY: `text` is a live string or null.
    if let Some(text) = unsafe { read_string(env, text) } {
        send(Msg::Commit(text));
    }
}

extern "system" fn native_edit(_env: *mut JNIEnv, _class: jclass, action: jint) {
    send(Msg::Edit(action));
}
