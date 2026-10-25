//! Android: the system's `MediaPlayer`, which decodes in hardware, plays
//! the sound, keeps it and the picture in step and reads files, content
//! URIs, HTTP and HLS, driven through the `dev.viso.media.VisoVideo` Java
//! class; its pictures drawn into an `AImageReader` whose hardware buffers
//! a GPU backend samples where they lie, and turned into RGBA rows only for
//! a caller that copies them.
//!
//! The Java class comes from the APK, loaded through the activity the app's
//! framework hands over ([`attach`]); until then a player opens nothing, as
//! `Unsupported`. What the player reports arrives on the Java main thread
//! and is posted to the player's channel. Bytes are read where they lie,
//! through a direct `ByteBuffer`, never written out.
//!
//! Extensions draw their live pictures into the same kind of reader
//! ([`ImageReader`]).

use std::cell::RefCell;
use std::ffi::{CStr, c_int, c_void};
use std::ptr;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use jni_sys::{
    JNI_OK, JNI_VERSION_1_6, JNIEnv, JNINativeMethod, JavaVM, jboolean, jclass, jfloat, jint,
    jlong, jmethodID, jobject, jvalue,
};

use viso_handle::{ExternalHolds, ExternalImage};

use super::yuv::{self, Plane};
use super::{Frame, PixelLayout, VideoError, VideoEvent, VideoInfo, VideoSource, file_uri};

/// Call a `JNIEnv` function.
macro_rules! env_call {
    ($env:expr, $f:ident $(, $arg:expr)* $(,)?) => {
        ((**$env).v1_6.$f)($env $(, $arg)*)
    };
}

type AImageReader = c_void;
type AImage = c_void;
type AHardwareBuffer = c_void;
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
/// `AHARDWAREBUFFER_USAGE_GPU_SAMPLED_IMAGE | AHARDWAREBUFFER_USAGE_CPU_READ_OFTEN`:
/// sampled by a GPU where it lies, read by the CPU for a caller that copies.
const USAGE: u64 = 0x100 | 0x3;
/// The images a reader lends at once: one shown, a few a GPU still reads,
/// one coming.
const IMAGES: i32 = 5;

#[link(name = "mediandk")]
unsafe extern "C" {
    fn AImageReader_newWithUsage(
        width: i32,
        height: i32,
        format: i32,
        usage: u64,
        max_images: i32,
        reader: *mut *mut AImageReader,
    ) -> c_int;
    fn AImage_getHardwareBuffer(image: *const AImage, buffer: *mut *mut AHardwareBuffer) -> c_int;
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

/// Takes what players need from the app's activity, once: the VM, the
/// player class through the activity's class loader, and the application
/// context.
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
    // freed by the frame popped before returning, those kept made global.
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
pub(super) extern "system" fn native_event(
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

/// An image taken from the reader, shown until the next.
struct Taken {
    image: *mut AImage,
    /// Its hardware buffer, or null where it has none.
    buffer: *mut AHardwareBuffer,
    holds: Arc<ExternalHolds>,
    /// The picture's region of the image, chroma starting on an even
    /// sample: left, top, width, height.
    crop: (u32, u32, u32, u32),
    /// The quarter turns clockwise that show it upright.
    turns: u8,
    time: Duration,
}

impl Taken {
    /// The picture's size as it shows.
    fn shown_size(&self) -> (u32, u32) {
        let (_, _, width, height) = self.crop;
        if self.turns % 2 == 1 {
            (height, width)
        } else {
            (width, height)
        }
    }
}

/// The picture of the image last taken: its hardware buffer, for a GPU to
/// sample, or its planes turned into RGBA rows.
#[derive(Clone, Copy)]
pub(crate) struct Picture<'a> {
    taken: &'a Taken,
    rows: &'a RefCell<Vec<u8>>,
    /// Where rows are turned upright, for a picture that needs it.
    turned: &'a RefCell<Vec<u8>>,
}

impl std::fmt::Debug for Picture<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Picture")
    }
}

impl Picture<'_> {
    /// The image's hardware buffer, with the picture's region of it.
    pub(crate) fn external(&self, _: u32, _: u32) -> Option<ExternalImage> {
        let t = self.taken;
        let (x, y, width, height) = t.crop;
        (!t.buffer.is_null()).then_some(ExternalImage::AndroidHardwareBuffer {
            buffer: t.buffer,
            holds: Arc::as_ptr(&t.holds),
            x,
            y,
            width,
            height,
            turns: t.turns,
        })
    }

    /// Calls `f` with the picture turned into RGBA rows, upright, and their
    /// stride.
    pub(crate) fn read(&self, f: impl FnOnce(&[u8], usize)) -> bool {
        let mut rows = self.rows.borrow_mut();
        let (x, y, width, height) = self.taken.crop;
        // SAFETY: the image is held, not deleted, while its picture is lent.
        if !unsafe { to_rgba(self.taken.image, x, y, width, height, &mut rows) } {
            return false;
        }
        if self.taken.turns.is_multiple_of(4) {
            f(&rows, width as usize * 4);
            return true;
        }
        let mut turned = self.turned.borrow_mut();
        turn(
            &rows,
            width as usize,
            height as usize,
            self.taken.turns,
            &mut turned,
        );
        f(&turned, self.taken.shown_size().0 as usize * 4);
        true
    }

    pub(crate) fn layout(&self) -> PixelLayout {
        PixelLayout::Rgba8
    }
}

pub(crate) type Player = Movie;

/// The reader pictures are drawn into, and the images taken from it.
struct Images {
    reader: Option<Reader>,
    /// The image last taken.
    shown: Option<Taken>,
    /// Images shown before, each deleted once no GPU holds its buffer.
    spent: Vec<Taken>,
    /// The holds of each of the reader's buffers, which outlive the images
    /// lent over them.
    holds: Vec<(*mut AHardwareBuffer, Arc<ExternalHolds>)>,
    /// The picture last turned into rows, for a caller that copies, and
    /// those rows turned upright.
    rows: RefCell<Vec<u8>>,
    turned: RefCell<Vec<u8>>,
}

impl Images {
    fn new() -> Images {
        Images {
            reader: None,
            shown: None,
            spent: Vec::new(),
            holds: Vec::new(),
            rows: RefCell::default(),
            turned: RefCell::default(),
        }
    }

    /// Whether it reads `width` × `height` pictures already.
    fn reads(&self, width: jint, height: jint) -> bool {
        self.reader
            .as_ref()
            .is_some_and(|r| r.width == width && r.height == height)
    }

    /// A new reader of `width` × `height`, whose window, as a surface, is
    /// handed to `aim` — which points the producer at it — before the old
    /// reader goes. Whether it was made.
    ///
    /// # Safety
    ///
    /// `env` is the calling thread's environment.
    unsafe fn read_into(
        &mut self,
        env: *mut JNIEnv,
        width: jint,
        height: jint,
        aim: impl FnOnce(jobject),
    ) -> bool {
        let mut reader = ptr::null_mut();
        // SAFETY: a new reader, its window owned by it; the surface made
        // over the window is a local reference handed to the producer,
        // which keeps its own, then deleted. The old reader is deleted once
        // the producer draws into the new one.
        unsafe {
            if AImageReader_newWithUsage(
                width,
                height,
                AIMAGE_FORMAT_YUV_420_888,
                USAGE,
                IMAGES,
                &mut reader,
            ) != AMEDIA_OK
            {
                return false;
            }
            let mut window = ptr::null_mut();
            if AImageReader_getWindow(reader, &mut window) != AMEDIA_OK {
                AImageReader_delete(reader);
                return false;
            }
            let surface = ANativeWindow_toSurface(env, window);
            if surface.is_null() {
                clear(env);
                AImageReader_delete(reader);
                return false;
            }
            aim(surface);
            env_call!(env, DeleteLocalRef, surface);
            let old = self.reader.replace(Reader {
                reader,
                width,
                height,
            });
            if let Some(old) = old {
                self.let_go();
                AImageReader_delete(old.reader);
            }
        }
        true
    }

    /// Takes the reader's newest image, if it has one, shown until the next
    /// at `time`, turned `turns` quarter turns clockwise.
    fn take(&mut self, time: Duration, turns: u8) -> bool {
        // SAFETY: each spent image was acquired from the reader and not yet
        // deleted; one no GPU holds is deleted once.
        self.spent.retain(|t| {
            let held = t.holds.is_held();
            if !held {
                unsafe { AImage_delete(t.image) };
            }
            held
        });
        let Some(reader) = self.reader.as_ref().map(|r| r.reader) else {
            return false;
        };
        let mut image = ptr::null_mut();
        // SAFETY: the newest image of the reader, read into locals and kept
        // until it is deleted.
        let (crop, buffer) = unsafe {
            if AImageReader_acquireLatestImage(reader, &mut image) != AMEDIA_OK || image.is_null() {
                return false;
            }
            let Some(crop) = crop_of(image) else {
                AImage_delete(image);
                return false;
            };
            let mut buffer = ptr::null_mut();
            if AImage_getHardwareBuffer(image, &mut buffer) != AMEDIA_OK {
                buffer = ptr::null_mut();
            }
            (crop, buffer)
        };
        let holds = match self.holds.iter().find(|(b, _)| *b == buffer) {
            Some((_, holds)) => Arc::clone(holds),
            None => {
                let holds = ExternalHolds::new();
                self.holds.push((buffer, Arc::clone(&holds)));
                holds
            }
        };
        let taken = Taken {
            image,
            buffer,
            holds,
            crop,
            turns,
            time,
        };
        if let Some(old) = self.shown.replace(taken) {
            self.spent.push(old);
        }
        true
    }

    /// The image last taken, as a frame.
    fn shown(&self) -> Option<Frame<'_>> {
        let taken = self.shown.as_ref()?;
        let (width, height) = taken.shown_size();
        Some(Frame::native_picture(
            width,
            height,
            taken.time,
            Picture {
                taken,
                rows: &self.rows,
                turned: &self.turned,
            },
        ))
    }

    /// Deletes every image taken from the reader, and forgets its buffers.
    fn let_go(&mut self) {
        for t in self.shown.take().into_iter().chain(self.spent.drain(..)) {
            // SAFETY: each was acquired from the reader and is deleted once;
            // a GPU still reading its buffer holds a reference of its own.
            unsafe { AImage_delete(t.image) };
        }
        self.holds.clear();
    }
}

impl Drop for Images {
    fn drop(&mut self) {
        self.let_go();
        if let Some(reader) = self.reader.take() {
            // SAFETY: the reader nothing draws into any more, deleted once.
            unsafe { AImageReader_delete(reader.reader) };
        }
    }
}

/// A movie: the system's player, drawing into a reader of the picture's
/// size.
pub(crate) struct Movie {
    java: &'static Java,
    /// The `VisoVideo`, a global reference.
    video: jobject,
    reports: Receiver<Report>,
    /// The sender the Java side posts to, freed once it is closed.
    mailbox: *mut Sender<Report>,
    /// What the direct buffer of the bytes points into, kept while it plays.
    _bytes: Option<std::sync::Arc<[u8]>>,
    images: Images,
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

impl Movie {
    pub(crate) fn open(source: &VideoSource) -> Result<Movie, VideoError> {
        let java = JAVA.get().ok_or(VideoError::Unsupported)?;
        let env = env(java).ok_or(VideoError::WrongThread)?;
        // A URI the player reads, or bytes it reads in place, kept alive
        // while it does where they are shared.
        let (uri, bytes, kept) = match source {
            VideoSource::File(path) => (Some(file_uri(path)), None, None),
            VideoSource::Url(url) => (Some(url.clone()), None, None),
            VideoSource::Bytes(bytes) => (None, Some(&bytes[..]), Some(bytes.clone())),
            VideoSource::Static(bytes) => (None, Some(*bytes), None),
            VideoSource::Camera(_) => return Err(VideoError::Unsupported),
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
        Ok(Movie {
            java,
            video,
            reports,
            mailbox,
            _bytes: kept,
            images: Images::new(),
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
        call(self.java, self.video, method, args);
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
            facing: None,
        })
    }

    /// Points the player at a reader `width` × `height`, unless it draws
    /// into one of that size already, or the size is none.
    fn draw_into(&mut self, width: jint, height: jint) {
        if width <= 0 || height <= 0 || self.images.reads(width, height) {
            return;
        }
        let Some(env) = env(self.java) else {
            return;
        };
        let (java, video) = (self.java, self.video);
        // SAFETY: this thread's environment; the surface is handed to the
        // player's `setSurface`, the type its descriptor names.
        unsafe {
            self.images.read_into(env, width, height, |surface| {
                call(java, video, java.set_surface, &[jvalue { l: surface }]);
            });
        }
    }

    pub(crate) fn frame(&mut self) -> Option<Frame<'_>> {
        // An image's own stamp is when it shows, on the system clock; where
        // in the video it lies is the player's position.
        let time = self.position();
        if !self.images.take(time, 0) {
            return None;
        }
        self.images.shown()
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

impl Drop for Movie {
    fn drop(&mut self) {
        // Closing releases the player, after which nothing reports to the
        // mailbox and the reader has no producer.
        self.call(self.java.close, &[]);
        if let Some(env) = env(self.java) {
            // SAFETY: the player's global reference, deleted once.
            unsafe { env_call!(env, DeleteGlobalRef, self.video) };
        }
        // SAFETY: the mailbox the Java side no longer reports to, freed
        // once; the images go after.
        unsafe { drop(Box::from_raw(self.mailbox)) };
    }
}

/// Calls the `void` method `method` of `object` with `args`, clearing an
/// exception it throws.
fn call(java: &Java, object: jobject, method: jmethodID, args: &[jvalue]) {
    let Some(env) = env(java) else {
        return;
    };
    // SAFETY: callers name a method of the object's class and hand the
    // arguments its descriptor names.
    unsafe {
        env_call!(env, CallVoidMethodA, object, method, args.as_ptr());
        clear(env);
    }
}

/// `rows`, `width` × `height` RGBA texels, turned `turns` quarter turns
/// clockwise into `out`.
fn turn(rows: &[u8], width: usize, height: usize, turns: u8, out: &mut Vec<u8>) {
    out.clear();
    out.resize(rows.len(), 0);
    let shown_width = if turns % 2 == 1 { height } else { width };
    for y in 0..height {
        for x in 0..width {
            let (tx, ty) = match turns % 4 {
                1 => (height - 1 - y, x),
                2 => (width - 1 - x, height - 1 - y),
                3 => (y, width - 1 - x),
                _ => (x, y),
            };
            let from = (y * width + x) * 4;
            let to = (ty * shown_width + tx) * 4;
            out[to..to + 4].copy_from_slice(&rows[from..from + 4]);
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

/// The picture's region of `image` — its crop, else all of it — with chroma
/// starting on an even sample: left, top, width, height.
///
/// # Safety
///
/// `image` is an image acquired from a reader, not deleted.
unsafe fn crop_of(image: *mut AImage) -> Option<(u32, u32, u32, u32)> {
    let (mut w, mut h) = (0, 0);
    let mut crop = AImageCropRect::default();
    // SAFETY: the caller's contract; reads of the image into locals.
    let cropped = unsafe {
        AImage_getWidth(image, &mut w);
        AImage_getHeight(image, &mut h);
        AImage_getCropRect(image, &mut crop) == AMEDIA_OK
    } && crop.left >= 0
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
    let (left, top) = (crop.left & !1, crop.top & !1);
    let (width, height) = (crop.right - left, crop.bottom - top);
    (width > 0 && height > 0).then_some((left as u32, top as u32, width as u32, height as u32))
}

/// Turns the `width` × `height` region at (`left`, `top`) of `image` into
/// RGBA rows in `rows`; `false` where its planes cannot be read.
///
/// # Safety
///
/// `image` is an image acquired from a reader, not deleted.
unsafe fn to_rgba(
    image: *mut AImage,
    left: u32,
    top: u32,
    width: u32,
    height: u32,
    rows: &mut Vec<u8>,
) -> bool {
    let plane = |index: c_int, x: usize, y: usize| -> Option<Plane<'static>> {
        let (mut data, mut length, mut row, mut pixel) = (ptr::null_mut(), 0, 0, 0);
        // SAFETY: the caller's contract; the plane's bytes are its length
        // long, borrowed while the image is held.
        unsafe {
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
        }
    };
    let (left, top) = (left as usize, top as usize);
    let (Some(y), Some(u), Some(v)) = (
        plane(0, left, top),
        plane(1, left / 2, top / 2),
        plane(2, left / 2, top / 2),
    ) else {
        return false;
    };
    let matrix = yuv::matrix_for_height(height);
    yuv::to_rgba(
        width as usize,
        height as usize,
        y,
        u,
        v,
        matrix,
        false,
        rows,
    )
}

/// An `AImageReader` an extension's producer draws YUV pictures into — a
/// camera's capture session, a virtual display — each lent to the GPU as
/// its hardware buffer, sampled in place and turned upright in the same
/// pass; turned into RGBA rows only for a caller that copies.
pub struct ImageReader(Images);

impl Default for ImageReader {
    fn default() -> ImageReader {
        ImageReader::new()
    }
}

impl ImageReader {
    /// A reader of nothing yet.
    pub fn new() -> ImageReader {
        ImageReader(Images::new())
    }

    /// A new reader of `width` × `height` pictures (`YUV_420_888`, sampled
    /// by the GPU and read by the CPU), whose `android.view.Surface`, a
    /// local reference valid for the call, is handed to `aim` to point the
    /// producer at; the old reader goes after. Whether it was made.
    ///
    /// # Safety
    ///
    /// `env` is the calling thread's `JNIEnv`.
    pub unsafe fn read_into(
        &mut self,
        env: *mut c_void,
        width: i32,
        height: i32,
        aim: impl FnOnce(*mut c_void),
    ) -> bool {
        // SAFETY: the caller's contract.
        unsafe {
            self.0
                .read_into(env.cast(), width, height, |surface| aim(surface.cast()))
        }
    }

    /// Takes the newest picture drawn, if one came since, shown at `time`
    /// turned `turns` quarter turns clockwise. Whether one did.
    pub fn take(&mut self, time: Duration, turns: u8) -> bool {
        self.0.take(time, turns)
    }

    /// The picture taken last, valid until the next is taken.
    pub fn shown(&self) -> Option<Frame<'_>> {
        self.0.shown()
    }
}
