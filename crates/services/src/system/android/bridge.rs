//! The JNI bridge to `dev.viso.services.VisoServices`, the Java half of the
//! Android services (`crates/services/android/java`).
//!
//! The class ships in the app's dex, so it is loaded through the activity's
//! class loader the first time a service is used; an APK without it has no
//! services. Asynchronous calls pass a token and register the reply's
//! completer under it; Java answers through one of the `native*` callbacks,
//! from whichever thread finished the work.

use std::collections::HashMap;
use std::ffi::{CStr, c_void};
use std::path::PathBuf;
use std::ptr;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Mutex, OnceLock};

use jni_sys::{
    JNI_OK, JNIEnv, JNINativeMethod, jbyteArray, jclass, jint, jlong, jmethodID, jobject,
    jobjectArray, jsize, jstring, jvalue,
};

use crate::files::PickedFile;
use crate::permissions::PermissionState;
use crate::reply::{Completer, Reply, ServiceError, ServiceResult, reply};

/// Call a `JNIEnv` function.
macro_rules! env_call {
    ($env:expr, $f:ident $(, $arg:expr)* $(,)?) => {
        ((**$env).v1_6.$f)($env $(, $arg)*)
    };
}

const CLASS: &CStr = c"dev.viso.services.VisoServices";

/// The `VisoServices` static methods the native side calls.
#[derive(Clone, Copy)]
pub(super) enum Method {
    OpenDocument,
    CreateDocument,
    Share,
    Notify,
    Withdraw,
    PermissionStatus,
    RequestPermission,
    SecretGet,
    SecretSet,
    SecretRemove,
    Vibrate,
}

/// Name and signature of each [`Method`], in declaration order.
const METHODS: [(&CStr, &CStr); 11] = [
    (
        c"openDocument",
        c"(Landroid/app/Activity;J[Ljava/lang/String;Z)V",
    ),
    (
        c"createDocument",
        c"(Landroid/app/Activity;JLjava/lang/String;Ljava/lang/String;[B)V",
    ),
    (c"share", c"(Landroid/app/Activity;JLjava/lang/String;)V"),
    (
        c"notify",
        c"(Landroid/app/Activity;JLjava/lang/String;Ljava/lang/String;Ljava/lang/String;)V",
    ),
    (c"withdraw", c"(Landroid/app/Activity;Ljava/lang/String;)V"),
    (c"permissionStatus", c"(Landroid/app/Activity;)I"),
    (c"requestPermission", c"(Landroid/app/Activity;J)V"),
    (
        c"secretGet",
        c"(Landroid/app/Activity;JLjava/lang/String;)V",
    ),
    (
        c"secretSet",
        c"(Landroid/app/Activity;JLjava/lang/String;[B)V",
    ),
    (
        c"secretRemove",
        c"(Landroid/app/Activity;JLjava/lang/String;)V",
    ),
    (c"vibrate", c"(Landroid/app/Activity;I)I"),
];

/// The loaded class, a global reference, and its method ids.
struct Class {
    class: jclass,
    methods: [jmethodID; METHODS.len()],
}

// SAFETY: a global reference and method ids are valid on every thread for
// as long as the VM runs; the class is never deleted.
unsafe impl Send for Class {}
// SAFETY: as above; the values are immutable.
unsafe impl Sync for Class {}

/// The class once loaded, or `None` when the APK lacks it.
static CLASS_REF: OnceLock<Option<Class>> = OnceLock::new();

/// One call's access to Java: the thread's environment and the activity,
/// valid for the call.
pub(super) struct Java<'a> {
    env: *mut JNIEnv,
    activity: jobject,
    class: &'a Class,
}

/// Run `f` against Java, inside a local frame that frees every local
/// reference it makes: `Unsupported` without an activity (or off the app's
/// thread) or without the Java class.
pub(super) fn with_java<R>(f: impl FnOnce(&Java<'_>) -> ServiceResult<R>) -> ServiceResult<R> {
    viso_platform::backend::android::with_activity(|env, activity| {
        // SAFETY: `env` is this thread's environment and `activity` a live
        // local reference, both valid for this closure; the frame pushed
        // here is popped before it returns.
        unsafe {
            if env_call!(env, PushLocalFrame, 16) != JNI_OK {
                return Err(pending_exception(env));
            }
            let result = match class(env, activity) {
                Some(class) => f(&Java {
                    env,
                    activity,
                    class,
                }),
                None => Err(ServiceError::Unsupported),
            };
            env_call!(env, PopLocalFrame, ptr::null_mut());
            result
        }
    })
    .unwrap_or(Err(ServiceError::Unsupported))
}

/// The class, loaded on first use.
///
/// # Safety
/// `env` must be the calling thread's environment and `activity` live.
unsafe fn class(env: *mut JNIEnv, activity: jobject) -> Option<&'static Class> {
    if let Some(class) = CLASS_REF.get() {
        return class.as_ref();
    }
    // SAFETY: the caller's contract.
    let loaded = unsafe { load(env, activity) };
    CLASS_REF.get_or_init(|| loaded).as_ref()
}

/// # Safety
/// As for [`class`]; called inside a local frame.
unsafe fn load(env: *mut JNIEnv, activity: jobject) -> Option<Class> {
    // SAFETY: the caller's contract. Each id is looked up on the object's
    // own class with the signature the JDK declares, and every call is
    // checked for a pending exception before its result is used.
    unsafe {
        let activity_class = env_call!(env, GetObjectClass, activity);
        let get_loader = env_call!(
            env,
            GetMethodID,
            activity_class,
            c"getClassLoader".as_ptr(),
            c"()Ljava/lang/ClassLoader;".as_ptr()
        );
        if !clear(env) {
            return None;
        }
        let loader = env_call!(env, CallObjectMethodA, activity, get_loader, ptr::null());
        if !clear(env) || loader.is_null() {
            return None;
        }
        let loader_class = env_call!(env, GetObjectClass, loader);
        let load_class = env_call!(
            env,
            GetMethodID,
            loader_class,
            c"loadClass".as_ptr(),
            c"(Ljava/lang/String;)Ljava/lang/Class;".as_ptr()
        );
        let name = env_call!(env, NewStringUTF, CLASS.as_ptr());
        if !clear(env) {
            return None;
        }
        let class = env_call!(
            env,
            CallObjectMethodA,
            loader,
            load_class,
            [jvalue { l: name }].as_ptr()
        );
        // `ClassNotFoundException`: the APK was built without the class.
        if !clear(env) || class.is_null() {
            return None;
        }
        let mut methods = [ptr::null_mut(); METHODS.len()];
        for (id, (name, signature)) in methods.iter_mut().zip(METHODS) {
            *id = env_call!(
                env,
                GetStaticMethodID,
                class,
                name.as_ptr(),
                signature.as_ptr()
            );
            if !clear(env) || id.is_null() {
                return None;
            }
        }
        if !register_natives(env, class) {
            return None;
        }
        let class = env_call!(env, NewGlobalRef, class);
        (!class.is_null()).then_some(Class { class, methods })
    }
}

/// Clear a pending exception: whether there was none.
///
/// # Safety
/// `env` must be the calling thread's environment.
unsafe fn clear(env: *mut JNIEnv) -> bool {
    // SAFETY: the caller's contract.
    unsafe {
        if env_call!(env, ExceptionCheck) {
            env_call!(env, ExceptionClear);
            return false;
        }
    }
    true
}

/// The pending exception, cleared, as a failure carrying its description.
///
/// # Safety
/// `env` must be the calling thread's environment.
unsafe fn pending_exception(env: *mut JNIEnv) -> ServiceError {
    // SAFETY: the caller's contract; `toString` is `Object`'s, looked up on
    // the thrown object's class, and the exception is cleared before any
    // further call.
    unsafe {
        let thrown = env_call!(env, ExceptionOccurred);
        env_call!(env, ExceptionClear);
        if thrown.is_null() {
            return ServiceError::Failed("the Java call failed".into());
        }
        let thrown_class = env_call!(env, GetObjectClass, thrown);
        let to_string = env_call!(
            env,
            GetMethodID,
            thrown_class,
            c"toString".as_ptr(),
            c"()Ljava/lang/String;".as_ptr()
        );
        let text = if clear(env) {
            let text = env_call!(env, CallObjectMethodA, thrown, to_string, ptr::null());
            if clear(env) {
                read_string(env, text)
            } else {
                None
            }
        } else {
            None
        };
        env_call!(env, DeleteLocalRef, thrown);
        ServiceError::Failed(text.unwrap_or_else(|| "the Java call failed".into()))
    }
}

impl Java<'_> {
    /// The activity, the first argument of every method.
    pub(super) fn activity(&self) -> jvalue {
        jvalue { l: self.activity }
    }

    pub(super) fn string(&self, text: &str) -> ServiceResult<jvalue> {
        let units: Vec<u16> = text.encode_utf16().collect();
        let len = jsize::try_from(units.len()).map_err(|_| too_large())?;
        // SAFETY: `env` is this thread's environment; `NewString` copies
        // `units`.
        let s = unsafe { env_call!(self.env, NewString, units.as_ptr(), len) };
        self.made(s)
    }

    pub(super) fn bytes(&self, bytes: &[u8]) -> ServiceResult<jvalue> {
        let len = jsize::try_from(bytes.len()).map_err(|_| too_large())?;
        // SAFETY: `env` is this thread's environment; the region written is
        // the new array's whole length.
        unsafe {
            let array = env_call!(self.env, NewByteArray, len);
            if !array.is_null() {
                env_call!(
                    self.env,
                    SetByteArrayRegion,
                    array,
                    0,
                    len,
                    bytes.as_ptr().cast()
                );
            }
            self.made(array)
        }
    }

    pub(super) fn strings(&self, list: &[String]) -> ServiceResult<jvalue> {
        let len = jsize::try_from(list.len()).map_err(|_| too_large())?;
        // SAFETY: `env` is this thread's environment; `String` resolves
        // from any class loader, and each element is set within the array's
        // length before its local reference is deleted.
        unsafe {
            let string_class = env_call!(self.env, FindClass, c"java/lang/String".as_ptr());
            if string_class.is_null() {
                return Err(pending_exception(self.env));
            }
            let array = env_call!(self.env, NewObjectArray, len, string_class, ptr::null_mut());
            if array.is_null() {
                return Err(pending_exception(self.env));
            }
            for (i, text) in (0..len).zip(list) {
                let element = self.string(text)?;
                env_call!(self.env, SetObjectArrayElement, array, i, element.l);
                env_call!(self.env, DeleteLocalRef, element.l);
            }
            self.made(array)
        }
    }

    fn made(&self, object: jobject) -> ServiceResult<jvalue> {
        if object.is_null() {
            // SAFETY: `env` is this thread's environment.
            return Err(unsafe { pending_exception(self.env) });
        }
        Ok(jvalue { l: object })
    }

    /// Call a `void` method with `args`, which must match its signature.
    pub(super) fn call_void(&self, method: Method, args: &[jvalue]) -> ServiceResult<()> {
        let id = self.class.methods[method as usize];
        // SAFETY: `id` is a static method of `class`; every caller passes
        // the arguments `METHODS` declares for it.
        unsafe {
            env_call!(
                self.env,
                CallStaticVoidMethodA,
                self.class.class,
                id,
                args.as_ptr()
            );
            self.checked(())
        }
    }

    /// Call an `int` method with `args`, which must match its signature.
    pub(super) fn call_int(&self, method: Method, args: &[jvalue]) -> ServiceResult<jint> {
        let id = self.class.methods[method as usize];
        // SAFETY: as in `call_void`.
        unsafe {
            let value = env_call!(
                self.env,
                CallStaticIntMethodA,
                self.class.class,
                id,
                args.as_ptr()
            );
            self.checked(value)
        }
    }

    fn checked<T>(&self, value: T) -> ServiceResult<T> {
        // SAFETY: `env` is this thread's environment.
        unsafe {
            if env_call!(self.env, ExceptionCheck) {
                return Err(pending_exception(self.env));
            }
        }
        Ok(value)
    }
}

fn too_large() -> ServiceError {
    ServiceError::Failed("too large to pass to Java".into())
}

/// A reply Java answers later, by token.
pub(super) enum Pending {
    Files(Completer<Vec<PickedFile>>),
    Saved(Completer<Option<PathBuf>>),
    Done(Completer<()>),
    Bytes(Completer<Option<Vec<u8>>>),
    State(Completer<PermissionState>),
}

impl Pending {
    fn fail(self, error: ServiceError) {
        match self {
            Self::Files(c) => c.complete(Err(error)),
            Self::Saved(c) => c.complete(Err(error)),
            Self::Done(c) => c.complete(Err(error)),
            Self::Bytes(c) => c.complete(Err(error)),
            Self::State(c) => c.complete(Err(error)),
        }
    }
}

static PENDING: Mutex<Option<HashMap<jlong, Pending>>> = Mutex::new(None);
static NEXT_TOKEN: AtomicI64 = AtomicI64::new(1);

fn park(token: jlong, pending: Pending) {
    let mut map = PENDING.lock().unwrap_or_else(|e| e.into_inner());
    map.get_or_insert_with(HashMap::new).insert(token, pending);
}

fn take(token: jlong) -> Option<Pending> {
    let mut map = PENDING.lock().unwrap_or_else(|e| e.into_inner());
    map.as_mut()?.remove(&token)
}

/// Start an asynchronous call: `call` passes the token to Java, which
/// answers the returned reply through the callback matching `wrap`.
pub(super) fn ask<T>(
    wrap: fn(Completer<T>) -> Pending,
    call: impl FnOnce(&Java<'_>, jvalue) -> ServiceResult<()>,
) -> Reply<T> {
    let (completer, reply) = reply();
    let token = NEXT_TOKEN.fetch_add(1, Ordering::Relaxed);
    park(token, wrap(completer));
    if let Err(error) = with_java(|java| call(java, jvalue { j: token }))
        && let Some(pending) = take(token)
    {
        pending.fail(error);
    }
    reply
}

/// A Java status code as a result.
pub(super) fn outcome(status: jint, message: Option<String>) -> ServiceResult<()> {
    match status {
        0 => Ok(()),
        1 => Err(ServiceError::Cancelled),
        2 => Err(ServiceError::Denied),
        4 => Err(ServiceError::Unsupported),
        _ => Err(ServiceError::Failed(
            message.unwrap_or_else(|| "the system reported an error".into()),
        )),
    }
}

/// A Java permission state.
pub(super) fn state(state: jint) -> PermissionState {
    match state {
        0 => PermissionState::Granted,
        2 => PermissionState::Prompt,
        _ => PermissionState::Denied,
    }
}

/// # Safety
/// `env` must be the calling thread's environment and `class` the
/// `VisoServices` class.
unsafe fn register_natives(env: *mut JNIEnv, class: jclass) -> bool {
    let natives: [(&CStr, &CStr, *mut c_void); 4] = [
        (
            c"nativeFiles",
            c"(JILjava/lang/String;[Ljava/lang/String;[[B)V",
            native_files as *mut c_void,
        ),
        (
            c"nativeBytes",
            c"(JILjava/lang/String;[B)V",
            native_bytes as *mut c_void,
        ),
        (
            c"nativeDone",
            c"(JILjava/lang/String;)V",
            native_done as *mut c_void,
        ),
        (c"nativeState", c"(JII)V", native_state as *mut c_void),
    ];
    let methods = natives.map(|(name, signature, f)| JNINativeMethod {
        name: name.as_ptr().cast_mut(),
        signature: signature.as_ptr().cast_mut(),
        fnPtr: f,
    });
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
            && clear(env)
    }
}

/// A Rust string from a Java one (null is `None`).
///
/// # Safety
/// `env` must be the calling thread's environment and `s` a live string
/// reference or null.
unsafe fn read_string(env: *mut JNIEnv, s: jstring) -> Option<String> {
    if s.is_null() {
        return None;
    }
    // SAFETY: the caller's contract; the region read stays within the
    // string's length.
    unsafe {
        let len = env_call!(env, GetStringLength, s);
        let mut units = vec![0u16; usize::try_from(len).unwrap_or(0)];
        env_call!(env, GetStringRegion, s, 0, len, units.as_mut_ptr());
        Some(String::from_utf16_lossy(&units))
    }
}

/// The bytes of a Java `byte[]` (null is `None`).
///
/// # Safety
/// `env` must be the calling thread's environment and `array` a live byte
/// array or null.
unsafe fn read_bytes(env: *mut JNIEnv, array: jbyteArray) -> Option<Vec<u8>> {
    if array.is_null() {
        return None;
    }
    // SAFETY: the caller's contract; the region read is the array's length.
    unsafe {
        let len = env_call!(env, GetArrayLength, array);
        let mut bytes = vec![0u8; usize::try_from(len).unwrap_or(0)];
        env_call!(
            env,
            GetByteArrayRegion,
            array,
            0,
            len,
            bytes.as_mut_ptr().cast()
        );
        Some(bytes)
    }
}

/// The picked files, element by element.
///
/// # Safety
/// `env` must be the calling thread's environment; `names` a `String[]`
/// and `contents` a `byte[][]` of the same length.
unsafe fn read_files(
    env: *mut JNIEnv,
    names: jobjectArray,
    contents: jobjectArray,
) -> Vec<PickedFile> {
    if names.is_null() || contents.is_null() {
        return Vec::new();
    }
    // SAFETY: the caller's contract; each element is read within both
    // arrays' length and its local reference deleted after.
    unsafe {
        let len =
            env_call!(env, GetArrayLength, names).min(env_call!(env, GetArrayLength, contents));
        (0..len)
            .map(|i| {
                let name = env_call!(env, GetObjectArrayElement, names, i);
                let bytes = env_call!(env, GetObjectArrayElement, contents, i);
                let file = PickedFile {
                    name: read_string(env, name).unwrap_or_default(),
                    path: None,
                    contents: read_bytes(env, bytes).unwrap_or_default(),
                };
                env_call!(env, DeleteLocalRef, name);
                env_call!(env, DeleteLocalRef, bytes);
                file
            })
            .collect()
    }
}

// The callbacks. Each runs on a Java thread with a valid `env`.

extern "system" fn native_files(
    env: *mut JNIEnv,
    _class: jclass,
    token: jlong,
    status: jint,
    message: jstring,
    names: jobjectArray,
    contents: jobjectArray,
) {
    let Some(Pending::Files(completer)) = take(token) else {
        return;
    };
    // SAFETY: the arguments are the live references Java passed.
    let result = unsafe {
        outcome(status, read_string(env, message)).map(|()| read_files(env, names, contents))
    };
    completer.complete(result);
}

extern "system" fn native_bytes(
    env: *mut JNIEnv,
    _class: jclass,
    token: jlong,
    status: jint,
    message: jstring,
    value: jbyteArray,
) {
    let Some(Pending::Bytes(completer)) = take(token) else {
        return;
    };
    // SAFETY: the arguments are the live references Java passed.
    let result =
        unsafe { outcome(status, read_string(env, message)).map(|()| read_bytes(env, value)) };
    completer.complete(result);
}

extern "system" fn native_done(
    env: *mut JNIEnv,
    _class: jclass,
    token: jlong,
    status: jint,
    message: jstring,
) {
    // SAFETY: `message` is a live string or null.
    let result = outcome(status, unsafe { read_string(env, message) });
    match take(token) {
        Some(Pending::Done(completer)) => completer.complete(result),
        // A saved document has a content URI, not a path.
        Some(Pending::Saved(completer)) => completer.complete(result.map(|()| None)),
        _ => {}
    }
}

extern "system" fn native_state(
    _env: *mut JNIEnv,
    _class: jclass,
    token: jlong,
    status: jint,
    permission: jint,
) {
    if let Some(Pending::State(completer)) = take(token) {
        completer.complete(outcome(status, None).map(|()| state(permission)));
    }
}
