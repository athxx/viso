//! Android: the system's `MediaPlayer`, which decodes in hardware, plays
//! the sound, keeps it and the picture in step and reads files, content
//! URIs, HTTP and HLS, driven through the `dev.viso.media.VisoVideo` Java
//! class; its pictures drawn into an `AImageReader` and turned into RGBA
//! rows when taken.
//!
//! The Java class comes from the APK, loaded through the activity the app's
//! framework hands over once ([`attach`]); until then a player opens
//! nothing, as `Unsupported`. What the player reports arrives on the Java
//! main thread and is posted to the player's channel. Bytes are read where
//! they lie, through a direct `ByteBuffer`, never written out.

use std::ffi::{CStr, c_int, c_void};
use std::ptr;
use std::sync::OnceLock;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use jni_sys::{
    JNI_OK, JNI_VERSION_1_6, JNIEnv, JNINativeMethod, JavaVM, jboolean, jclass, jfloat, jint,
    jlong, jmethodID, jobject, jvalue,
};

use super::yuv::{self, Matrix, Plane};
use super::{Frame, VideoError, VideoEvent, VideoInfo, VideoSource, file_uri};

pub(crate) use super::no_picture::Picture;

/// Call a `JNIEnv` function.
macro_rules! env_call {
    ($env:expr, $f:ident $(, $arg:expr)* $(,)?) => {
        ((**$env).v1_6.$f)($env $(, $arg)*)
    };
}

type AImageReader = c_void;
type AImage = c_void;
type ANativeWindow = c_void;

#[repr(C)]
#[derive(Default)]
struct AImageCropRect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

const AIMAGE_FORMAT_YUV_420_888: i32 = 0x23;
const AMEDIA_OK: c_int = 0;

#[link(name = "mediandk")]
unsafe extern "C" {
    fn AImageReader_new(
        width: i32,
        height: i32,
        format: i32,
        max_images: i32,
        reader: *mut *mut AImageReader,
    ) -> c_int;
    fn AImageReader_delete(reader: *mut AImageReader);
    fn AImageReader_getWindow(reader: *mut AImageReader, window: *mut *mut ANativeWindow) -> c_int;
    fn AImageReader_acquireLatestImage(reader: *mut AImageReader, image: *mut *mut AImage)
    -> c_int;
    fn AImage_delete(image: *mut AImage);
    fn AImage_getWidth(image: *const AImage, width: *mut i32) -> c_int;
    fn AImage_getHeight(image: *const AImage, height: *mut i32) -> c_int;
    fn AImage_getCropRect(image: *const AImage, rect: *mut AImageCropRect) -> c_int;
    fn AImage_getPlaneData(
        image: *const AImage,
        plane: c_int,
        data: *mut *mut u8,
        length: *mut c_int,
    ) -> c_int;
    fn AImage_getPlaneRowStride(image: *const AImage, plane: c_int, stride: *mut i32) -> c_int;
    fn AImage_getPlanePixelStride(image: *const AImage, plane: c_int, stride: *mut i32) -> c_int;
}

#[link(name = "android")]
unsafe extern "C" {
    fn ANativeWindow_toSurface(env: *mut JNIEnv, window: *mut ANativeWindow) -> jobject;
}

/// What `VisoVideo` reports, by kind.
const PREPARED: jint = 0;
const COMPLETED: jint = 1;
const ERROR: jint = 2;
const BUFFERING: jint = 3;
const SEEKED: jint = 4;
const SIZE: jint = 5;

/// One report, as the Java side sent it.
struct Report {
    kind: jint,
    a: jint,
    b: jint,
    c: jint,
    d: jlong,
}

/// The VM, the class and its methods, and the application context, all
/// valid on every thread for as long as the process runs.
struct Java {
    vm: *mut JavaVM,
    class: jclass,
    context: jobject,
    new: jmethodID,
    set_surface: jmethodID,
    play: jmethodID,
    pause: jmethodID,
    seek: jmethodID,
    set_rate: jmethodID,
    position: jmethodID,
    set_volume: jmethodID,
    set_looping: jmethodID,
    close: jmethodID,
}

// SAFETY: the VM, global references and method ids are not tied to the
// thread that made them, and none is deleted.
unsafe impl Send for Java {}
// SAFETY: as above; the values are never written after they are set.
unsafe impl Sync for Java {}

static JAVA: OnceLock<Java> = OnceLock::new();

/// Takes, once, what players need from the app's activity: the VM, the
/// player class through the activity's class loader, and the application
/// context. Later calls do nothing.
///
/// # Safety
///
/// `env` is the calling thread's `JNIEnv` and `activity` a live reference
/// to the app's activity.
pub(crate) unsafe fn attach(env: *mut JNIEnv, activity: jobject) {
    if JAVA.get().is_some() {
        return;
    }
    // SAFETY: the caller's contract; every local reference made here is
    // freed by the frame popped before returning, the two kept made global.
    unsafe {
        if env_call!(env, PushLocalFrame, 16) != JNI_OK {
            return;
        }
        let java = load(env, activity);
        clear(env);
        env_call!(env, PopLocalFrame, ptr::null_mut());
        if let Some(java) = java {
            let _ = JAVA.set(java);
        }
    }
}

/// # Safety
///
/// As [`attach`], inside a local frame.
unsafe fn load(env: *mut JNIEnv, activity: jobject) -> Option<Java> {
    // SAFETY: the caller's contract; each method is looked up with its
    // own descriptor and called with arguments of those types.
    unsafe {
        let mut vm: *mut JavaVM = ptr::null_mut();
        if env_call!(env, GetJavaVM, &mut vm) != JNI_OK {
            return None;
        }
        let activity_class = env_call!(env, GetObjectClass, activity);
        let get_loader = method(
            env,
            activity_class,
            c"getClassLoader",
            c"()Ljava/lang/ClassLoader;",
        )?;
        let get_context = method(
            env,
            activity_class,
            c"getApplicationContext",
            c"()Landroid/content/Context;",
        )?;
        let loader = env_call!(env, CallObjectMethodA, activity, get_loader, ptr::null());
        let context = env_call!(env, CallObjectMethodA, activity, get_context, ptr::null());
        if !clear(env) || loader.is_null() || context.is_null() {
            return None;
        }
        let loader_class = env_call!(env, GetObjectClass, loader);
        let load_class = method(
            env,
            loader_class,
            c"loadClass",
            c"(Ljava/lang/String;)Ljava/lang/Class;",
        )?;
        let name = env_call!(env, NewStringUTF, c"dev.viso.media.VisoVideo".as_ptr());
        let class = env_call!(
            env,
            CallObjectMethodA,
            loader,
            load_class,
            [jvalue { l: name }].as_ptr()
        );
        if !clear(env) || class.is_null() {
            return None;
        }
        let natives = [JNINativeMethod {
            name: c"nativeEvent".as_ptr().cast_mut(),
            signature: c"(JIIIIJ)V".as_ptr().cast_mut(),
            fnPtr: native_event as *mut c_void,
        }];
        if env_call!(env, RegisterNatives, class, natives.as_ptr(), 1) != JNI_OK {
            return None;
        }
        let java = Java {
            vm,
            new: method(
                env,
                class,
                c"<init>",
                c"(JLandroid/content/Context;Ljava/lang/String;Ljava/nio/ByteBuffer;)V",
            )?,
            set_surface: method(env, class, c"setSurface", c"(Landroid/view/Surface;)V")?,
            play: method(env, class, c"play", c"()V")?,
            pause: method(env, class, c"pause", c"()V")?,
            seek: method(env, class, c"seek", c"(J)V")?,
            set_rate: method(env, class, c"setRate", c"(F)V")?,
            position: method(env, class, c"position", c"()I")?,
            set_volume: method(env, class, c"setVolume", c"(FZ)V")?,
            set_looping: method(env, class, c"setLooping", c"(Z)V")?,
            close: method(env, class, c"close", c"()V")?,
            class: env_call!(env, NewGlobalRef, class),
            context: env_call!(env, NewGlobalRef, context),
        };
        Some(java)
    }
}

/// The method `name` of `class`, or `None` (its exception cleared).
///
/// # Safety
///
/// `env` is the calling thread's environment and `class` a live class.
unsafe fn method(
    env: *mut JNIEnv,
    class: jclass,
    name: &CStr,
    signature: &CStr,
) -> Option<jmethodID> {
    // SAFETY: the caller's contract.
    unsafe {
        let id = env_call!(env, GetMethodID, class, name.as_ptr(), signature.as_ptr());
        (clear(env) && !id.is_null()).then_some(id)
    }
}

/// Clears a pending exception; whether there was none.
///
/// # Safety
///
/// `env` is the calling thread's environment.
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

/// The text of the pending exception, cleared.
///
/// # Safety
///
/// `env` is the calling thread's environment with an exception pending.
unsafe fn take_exception(env: *mut JNIEnv) -> String {
    // SAFETY: the caller's contract; `toString` is called on the exception
    // with its exception cleared first, and its result read and freed.
    unsafe {
        let thrown = env_call!(env, ExceptionOccurred);
        env_call!(env, ExceptionClear);
        if thrown.is_null() {
            return "MediaPlayer failed".to_owned();
        }
        let class = env_call!(env, GetObjectClass, thrown);
        let text = method(env, class, c"toString", c"()Ljava/lang/String;")
            .map(|to_string| env_call!(env, CallObjectMethodA, thrown, to_string, ptr::null()))
            .filter(|s| clear(env) && !s.is_null())
            .map(|s| {
                let chars = env_call!(env, GetStringUTFChars, s, ptr::null_mut());
                let text = CStr::from_ptr(chars).to_string_lossy().into_owned();
                env_call!(env, ReleaseStringUTFChars, s, chars);
                env_call!(env, DeleteLocalRef, s);
                text
            });
        env_call!(env, DeleteLocalRef, class);
        env_call!(env, DeleteLocalRef, thrown);
        text.unwrap_or_else(|| "MediaPlayer failed".to_owned())
    }
}

/// The calling thread's environment, attaching it to the VM first if it
/// is not.
fn env(java: &Java) -> Option<*mut JNIEnv> {
    let vm = java.vm;
    let mut env: *mut c_void = ptr::null_mut();
    // SAFETY: `vm` is the process's VM; both calls only write the
    // out-pointer. A thread attached here stays attached, as a daemon.
    unsafe {
        if ((**vm).v1_4.GetEnv)(vm, &mut env, JNI_VERSION_1_6) != JNI_OK
            && ((**vm).v1_4.AttachCurrentThreadAsDaemon)(vm, &mut env, ptr::null_mut()) != JNI_OK
        {
            return None;
        }
    }
    Some(env.cast())
}

/// `VisoVideo.nativeEvent`: a report, posted to the player it is for.
///
/// `mailbox` is the player's `Sender`, which outlives every call: the Java
/// side calls only until its `close` returns, before the sender is freed.
extern "system" fn native_event(
    _env: *mut JNIEnv,
    _class: jclass,
    mailbox: jlong,
    kind: jint,
    a: jint,
    b: jint,
    c: jint,
    d: jlong,
) {
    // SAFETY: see above; the sender is only read, and a `Sender` may be
    // used from any thread.
    let sender = unsafe { &*(mailbox as *const Sender<Report>) };
    let _ = sender.send(Report { kind, a, b, c, d });
}

/// The reader a player draws into, and the size it was made for.
struct Reader {
    reader: *mut AImageReader,
    width: i32,
    height: i32,
}

pub(crate) struct Player {
    java: &'static Java,
    /// The `VisoVideo`, a global reference.
    video: jobject,
    reports: Receiver<Report>,
    /// The sender the Java side posts to, freed once it is closed.
    mailbox: *mut Sender<Report>,
    /// What the direct buffer of the bytes points into, kept while it plays.
    _bytes: Option<std::sync::Arc<[u8]>>,
    reader: Option<Reader>,
    /// The picture last taken, as RGBA rows.
    rows: Vec<u8>,
    ready: bool,
    failed: bool,
    wants_play: bool,
    looping: bool,
    ended: bool,
    rate: f32,
    volume: f32,
    muted: bool,
    seek: Option<Duration>,
    sought: Option<Duration>,
    at: Duration,
}

impl Player {
    pub(crate) fn open(source: &VideoSource) -> Result<Player, VideoError> {
        let java = JAVA.get().ok_or(VideoError::Unsupported)?;
        let env = env(java).ok_or(VideoError::WrongThread)?;
        // A URI the player reads, or bytes it reads in place, kept alive
        // while it does where they are shared.
        let (uri, bytes, kept) = match source {
            VideoSource::File(path) => (Some(file_uri(path)), None, None),
            VideoSource::Url(url) => (Some(url.clone()), None, None),
            VideoSource::Bytes(bytes) => (None, Some(&bytes[..]), Some(bytes.clone())),
            VideoSource::Static(bytes) => (None, Some(*bytes), None),
        };
        let (tell, reports) = mpsc::channel();
        let mailbox = Box::into_raw(Box::new(tell));
        // SAFETY: JNI calls on this thread's environment, inside a local
        // frame popped before returning; the constructor is called with the
        // types its descriptor names. The direct buffer points into bytes
        // the player keeps for as long as the Java side reads them.
        let video = unsafe {
            if env_call!(env, PushLocalFrame, 8) != JNI_OK {
                drop(Box::from_raw(mailbox));
                return Err(VideoError::Open("out of JNI references".to_owned()));
            }
            let juri = match &uri {
                Some(uri) => new_string(env, uri),
                None => ptr::null_mut(),
            };
            let buffer = match bytes {
                Some(bytes) => env_call!(
                    env,
                    NewDirectByteBuffer,
                    bytes.as_ptr().cast_mut().cast(),
                    bytes.len() as jlong
                ),
                None => ptr::null_mut(),
            };
            let args = [
                jvalue {
                    j: mailbox as jlong,
                },
                jvalue { l: java.context },
                jvalue { l: juri },
                jvalue { l: buffer },
            ];
            let local = env_call!(env, NewObjectA, java.class, java.new, args.as_ptr());
            let result = if env_call!(env, ExceptionCheck) || local.is_null() {
                Err(VideoError::Open(take_exception(env)))
            } else {
                Ok(env_call!(env, NewGlobalRef, local))
            };
            env_call!(env, PopLocalFrame, ptr::null_mut());
            match result {
                Ok(video) => video,
                Err(error) => {
                    // No report is made of a player that never opened.
                    drop(Box::from_raw(mailbox));
                    return Err(error);
                }
            }
        };
        Ok(Player {
            java,
            video,
            reports,
            mailbox,
            _bytes: kept,
            reader: None,
            rows: Vec::new(),
            ready: false,
            failed: false,
            wants_play: false,
            looping: false,
            ended: false,
            rate: 1.0,
            volume: 1.0,
            muted: false,
            seek: None,
            sought: None,
            at: Duration::ZERO,
        })
    }

    /// Calls the `void` method `method` of the player with `args`.
    fn call(&self, method: jmethodID, args: &[jvalue]) {
        let Some(env) = env(self.java) else {
            return;
        };
        // SAFETY: a call of one of `VisoVideo`'s methods, with the
        // arguments its descriptor names; an exception is cleared.
        unsafe {
            env_call!(env, CallVoidMethodA, self.video, method, args.as_ptr());
            clear(env);
        }
    }

    pub(crate) fn poll(&mut self) -> Option<VideoEvent> {
        while !self.failed {
            let report = self.reports.try_recv().ok()?;
            let event = match report.kind {
                PREPARED => Some(self.prepared(&report)),
                COMPLETED if !self.looping => {
                    self.ended = true;
                    self.wants_play = false;
                    self.at = self.position_now();
                    Some(VideoEvent::Ended)
                }
                ERROR => {
                    self.failed = true;
                    let why = format!("MediaPlayer error {}/{}", report.a, report.b);
                    Some(VideoEvent::Failed(if self.ready {
                        VideoError::Play(why)
                    } else {
                        VideoError::Open(why)
                    }))
                }
                BUFFERING => Some(VideoEvent::Waiting(report.a != 0)),
                SEEKED => {
                    self.sought = None;
                    None
                }
                SIZE => {
                    self.draw_into(report.a, report.b);
                    None
                }
                _ => None,
            };
            if event.is_some() {
                return event;
            }
        }
        None
    }

    fn prepared(&mut self, report: &Report) -> VideoEvent {
        self.ready = true;
        self.draw_into(report.a, report.b);
        if self.rate != 1.0 {
            self.call(self.java.set_rate, &[jvalue { f: self.rate }]);
        }
        let first = self.seek.take();
        if self.wants_play {
            if let Some(time) = first {
                self.seek_to(time);
            }
            self.call(self.java.play, &[]);
        } else {
            // A prepared player draws nothing until it plays or seeks.
            self.seek_to(first.unwrap_or(Duration::ZERO));
        }
        let duration = (report.d > 0).then(|| Duration::from_millis(report.d as u64));
        VideoEvent::Ready(VideoInfo {
            width: report.a.max(0) as u32,
            height: report.b.max(0) as u32,
            duration,
            frame_rate: None,
            audio: report.c != 0,
            seekable: duration.is_some(),
        })
    }

    /// Points the player at a reader `width` × `height`, unless it draws
    /// into one of that size already, or the size is none.
    fn draw_into(&mut self, width: jint, height: jint) {
        if width <= 0
            || height <= 0
            || self
                .reader
                .as_ref()
                .is_some_and(|r| r.width == width && r.height == height)
        {
            return;
        }
        let Some(env) = env(self.java) else {
            return;
        };
        let mut reader = ptr::null_mut();
        // SAFETY: a new reader, its window owned by it; the surface made
        // over the window is a local reference handed to the player, which
        // keeps its own, then deleted. The old reader is deleted once the
        // player draws into the new one.
        unsafe {
            if AImageReader_new(width, height, AIMAGE_FORMAT_YUV_420_888, 3, &mut reader)
                != AMEDIA_OK
            {
                return;
            }
            let mut window = ptr::null_mut();
            if AImageReader_getWindow(reader, &mut window) != AMEDIA_OK {
                AImageReader_delete(reader);
                return;
            }
            let surface = ANativeWindow_toSurface(env, window);
            if surface.is_null() {
                clear(env);
                AImageReader_delete(reader);
                return;
            }
            self.call(self.java.set_surface, &[jvalue { l: surface }]);
            env_call!(env, DeleteLocalRef, surface);
            if let Some(old) = self.reader.replace(Reader {
                reader,
                width,
                height,
            }) {
                AImageReader_delete(old.reader);
            }
        }
    }

    pub(crate) fn frame(&mut self) -> Option<Frame<'_>> {
        let reader = self.reader.as_ref()?.reader;
        let mut image = ptr::null_mut();
        // SAFETY: the newest image of the player's reader, read while held
        // and deleted after: each plane's bytes are its length long.
        let (width, height, time) = unsafe {
            if AImageReader_acquireLatestImage(reader, &mut image) != AMEDIA_OK || image.is_null() {
                return None;
            }
            let taken = self.take(image);
            AImage_delete(image);
            taken?
        };
        Some(Frame::rgba(
            width,
            height,
            &self.rows,
            width as usize * 4,
            time,
        ))
    }

    /// Turns `image` into the RGBA rows of the picture its crop names.
    ///
    /// # Safety
    ///
    /// `image` is an image acquired from the player's reader, not deleted.
    unsafe fn take(&mut self, image: *mut AImage) -> Option<(u32, u32, Duration)> {
        // SAFETY: the caller's contract; reads of the image into locals,
        // its planes borrowed for as long as it is held.
        unsafe {
            let (mut w, mut h) = (0, 0);
            AImage_getWidth(image, &mut w);
            AImage_getHeight(image, &mut h);
            let mut crop = AImageCropRect::default();
            let cropped = AImage_getCropRect(image, &mut crop) == AMEDIA_OK
                && crop.left >= 0
                && crop.top >= 0
                && crop.right > crop.left
                && crop.bottom > crop.top
                && crop.right <= w
                && crop.bottom <= h;
            if !cropped {
                crop = AImageCropRect {
                    left: 0,
                    top: 0,
                    right: w,
                    bottom: h,
                };
            }
            // Chroma starts on an even sample.
            let (left, top) = ((crop.left & !1) as usize, (crop.top & !1) as usize);
            let (width, height) = (
                (crop.right as usize).saturating_sub(left),
                (crop.bottom as usize).saturating_sub(top),
            );
            if width == 0 || height == 0 {
                return None;
            }
            let plane = |index: c_int, x: usize, y: usize| -> Option<Plane<'static>> {
                let (mut data, mut length, mut row, mut pixel) = (ptr::null_mut(), 0, 0, 0);
                if AImage_getPlaneData(image, index, &mut data, &mut length) != AMEDIA_OK
                    || AImage_getPlaneRowStride(image, index, &mut row) != AMEDIA_OK
                    || AImage_getPlanePixelStride(image, index, &mut pixel) != AMEDIA_OK
                    || data.is_null()
                    || row <= 0
                    || pixel <= 0
                {
                    return None;
                }
                let (row, pixel) = (row as usize, pixel as usize);
                let start = y * row + x * pixel;
                let bytes = std::slice::from_raw_parts(data, length.max(0) as usize);
                Some(Plane {
                    bytes: bytes.get(start..)?,
                    row_stride: row,
                    pixel_stride: pixel,
                })
            };
            let y = plane(0, left, top)?;
            let u = plane(1, left / 2, top / 2)?;
            let v = plane(2, left / 2, top / 2)?;
            let matrix = Matrix::for_height(height as u32);
            if !yuv::to_rgba(width, height, y, u, v, matrix, &mut self.rows) {
                return None;
            }
            // An image's own stamp is when it shows, on the system clock;
            // where in the video it lies is the player's position.
            Some((width as u32, height as u32, self.position()))
        }
    }

    pub(crate) fn play(&mut self) {
        if std::mem::take(&mut self.ended) {
            self.seek(Duration::ZERO);
        }
        self.wants_play = true;
        if self.ready {
            self.call(self.java.play, &[]);
        }
    }

    pub(crate) fn pause(&mut self) {
        self.wants_play = false;
        if self.ready {
            self.at = self.position_now();
            self.call(self.java.pause, &[]);
        }
    }

    pub(crate) fn seek(&mut self, time: Duration) {
        self.ended = false;
        if self.ready {
            self.seek_to(time);
        } else {
            self.seek = Some(time);
        }
    }

    fn seek_to(&mut self, time: Duration) {
        self.sought = Some(time);
        self.at = time;
        let ms = i64::try_from(time.as_millis()).unwrap_or(i64::MAX);
        self.call(self.java.seek, &[jvalue { j: ms }]);
    }

    pub(crate) fn set_rate(&mut self, rate: f32) {
        let rate = if rate.is_finite() && rate > 0.0 {
            rate
        } else {
            1.0
        };
        self.rate = rate;
        if self.ready {
            self.call(self.java.set_rate, &[jvalue { f: rate as jfloat }]);
        }
    }

    pub(crate) fn set_volume(&mut self, volume: f32) {
        self.volume = volume.clamp(0.0, 1.0);
        self.apply_volume();
    }

    pub(crate) fn set_muted(&mut self, muted: bool) {
        self.muted = muted;
        self.apply_volume();
    }

    fn apply_volume(&self) {
        self.call(
            self.java.set_volume,
            &[
                jvalue { f: self.volume },
                jvalue {
                    z: jboolean::from(self.muted),
                },
            ],
        );
    }

    pub(crate) fn set_looping(&mut self, looping: bool) {
        self.looping = looping;
        self.call(
            self.java.set_looping,
            &[jvalue {
                z: jboolean::from(looping),
            }],
        );
    }

    pub(crate) fn position(&self) -> Duration {
        if let Some(time) = self.sought.or(self.seek) {
            return time;
        }
        if !self.ready || !self.wants_play {
            return self.at;
        }
        self.position_now()
    }

    /// Where the player says it is.
    fn position_now(&self) -> Duration {
        let Some(env) = env(self.java) else {
            return self.at;
        };
        // SAFETY: a call of `position()I`, its exception cleared.
        let ms = unsafe {
            let ms = env_call!(
                env,
                CallIntMethodA,
                self.video,
                self.java.position,
                ptr::null()
            );
            if !clear(env) {
                return self.at;
            }
            ms
        };
        Duration::from_millis(ms.max(0) as u64)
    }

    pub(crate) fn is_playing(&self) -> bool {
        self.wants_play && !self.ended
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        // Closing releases the player, after which nothing reports to the
        // mailbox and the reader has no producer.
        self.call(self.java.close, &[]);
        if let Some(env) = env(self.java) {
            // SAFETY: the player's global reference, deleted once.
            unsafe { env_call!(env, DeleteGlobalRef, self.video) };
        }
        // SAFETY: the mailbox the Java side no longer reports to, and the
        // reader nothing draws into, each freed once.
        unsafe {
            drop(Box::from_raw(self.mailbox));
            if let Some(reader) = self.reader.take() {
                AImageReader_delete(reader.reader);
            }
        }
    }
}

/// A Java string from `text`, as a local reference.
///
/// # Safety
///
/// `env` is the calling thread's environment.
unsafe fn new_string(env: *mut JNIEnv, text: &str) -> jobject {
    let units: Vec<u16> = text.encode_utf16().collect();
    // SAFETY: the caller's contract; `NewString` copies `units`.
    unsafe { env_call!(env, NewString, units.as_ptr(), units.len() as jint) }
}
