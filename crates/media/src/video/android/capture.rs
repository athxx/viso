//! Cameras on Android: Camera2, driven through the `dev.viso.media.VisoCamera`
//! Java class, capturing into an `AImageReader` as a movie draws into one;
//! its hardware buffers a GPU backend samples where they lie, turned upright
//! as the sensor lies in the device and the display is turned.
//!
//! The camera opens at once to show its first picture, then closes unless it
//! plays; it asks the user's leave through the app's activity where the app
//! has none, which the APK declares (`android.permission.CAMERA`). What the
//! Java side reports arrives on the camera's thread and is posted to the
//! player's channel.

use std::ffi::CStr;
use std::ptr;
use std::sync::mpsc::{self, Receiver, Sender};

use jni_sys::{JNI_OK, JNIEnv, JNINativeMethod, jclass, jint, jmethodID, jobject, jvalue};

use super::super::camera::{self, CameraFacing, CameraInfo, CameraMode, CameraRequest};
use super::super::{Frame, VideoError, VideoEvent, VideoInfo};
use super::{
    ACTIVITY, ERROR, Images, JAVA, Java, Report, call, clear, env, method, native_event,
    new_string, take_exception,
};

/// The display turned: its rotation in degrees.
const ROTATION: jint = 6;

/// Why a camera could not go on, as the Java side tells.
const DENIED: jint = 1;
const BUSY: jint = 2;
const GONE: jint = 3;

/// The camera class and its methods, valid on every thread for as long as
/// the process runs.
pub(super) struct Methods {
    class: jclass,
    new: jmethodID,
    start: jmethodID,
    stop: jmethodID,
    close: jmethodID,
    list: jmethodID,
    modes: jmethodID,
}

impl Methods {
    /// The camera class through `loader`, its natives registered; `None`
    /// where the APK has none.
    ///
    /// # Safety
    ///
    /// `env` is the calling thread's environment, inside a local frame;
    /// `loader` a live class loader and `load_class` its `loadClass`.
    pub(super) unsafe fn load(
        env: *mut JNIEnv,
        loader: jobject,
        load_class: jmethodID,
    ) -> Option<Methods> {
        // SAFETY: the caller's contract; each method is looked up with its
        // own descriptor, and the class kept made global.
        unsafe {
            let name = env_call!(env, NewStringUTF, c"dev.viso.media.VisoCamera".as_ptr());
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
                fnPtr: native_event as *mut std::ffi::c_void,
            }];
            if env_call!(env, RegisterNatives, class, natives.as_ptr(), 1) != JNI_OK {
                clear(env);
                return None;
            }
            let fixed = |name: &CStr, signature: &CStr| {
                let id = env_call!(
                    env,
                    GetStaticMethodID,
                    class,
                    name.as_ptr(),
                    signature.as_ptr()
                );
                (clear(env) && !id.is_null()).then_some(id)
            };
            Some(Methods {
                new: method(
                    env,
                    class,
                    c"<init>",
                    c"(JLandroid/content/Context;Ljava/lang/String;Landroid/view/Surface;I)V",
                )?,
                start: method(env, class, c"start", c"(Landroid/app/Activity;)V")?,
                stop: method(env, class, c"stop", c"()V")?,
                close: method(env, class, c"close", c"()V")?,
                list: fixed(c"list", c"(Landroid/content/Context;)[Ljava/lang/String;")?,
                modes: fixed(c"modes", c"(Landroid/content/Context;Ljava/lang/String;)[I")?,
                class: env_call!(env, NewGlobalRef, class),
            })
        }
    }
}

// SAFETY: the class is a global reference and the method ids are not tied
// to a thread; none is deleted.
unsafe impl Send for Methods {}
// SAFETY: as above; nothing is written after it is made.
unsafe impl Sync for Methods {}

/// A camera as the Java side lists it, and the clockwise degrees its sensor
/// lies turned in the device.
struct Listed {
    info: CameraInfo,
    sensor: u32,
}

/// The cameras the Java side lists.
///
/// # Safety
///
/// `env` is the calling thread's environment, inside a local frame.
unsafe fn list(env: *mut JNIEnv, java: &Java, methods: &Methods) -> Vec<Listed> {
    // SAFETY: the caller's contract; a call of the static `list`, its array
    // of strings read element by element.
    let entries: Vec<String> = unsafe {
        let array = env_call!(
            env,
            CallStaticObjectMethodA,
            methods.class,
            methods.list,
            [jvalue { l: java.context }].as_ptr()
        );
        if !clear(env) || array.is_null() {
            return Vec::new();
        }
        (0..env_call!(env, GetArrayLength, array))
            .filter_map(|i| {
                let entry = env_call!(env, GetObjectArrayElement, array, i);
                if entry.is_null() {
                    return None;
                }
                let chars = env_call!(env, GetStringUTFChars, entry, ptr::null_mut());
                let text = (!chars.is_null()).then(|| {
                    let text = CStr::from_ptr(chars).to_string_lossy().into_owned();
                    env_call!(env, ReleaseStringUTFChars, entry, chars);
                    text
                });
                env_call!(env, DeleteLocalRef, entry);
                text
            })
            .collect()
    };
    let mut listed: Vec<Listed> = Vec::new();
    for entry in entries {
        let Some((head, id)) = entry.split_once(':') else {
            continue;
        };
        let (facing, side) = match head.as_bytes().first() {
            Some(b'F') => (CameraFacing::Front, "Front"),
            Some(b'B') => (CameraFacing::Back, "Back"),
            _ => (CameraFacing::External, "External"),
        };
        let sensor = head[1..].parse().unwrap_or(0);
        let before = listed.iter().filter(|l| l.info.facing == facing).count();
        let name = if before == 0 {
            format!("{side} camera")
        } else {
            format!("{side} camera {}", before + 1)
        };
        listed.push(Listed {
            info: CameraInfo {
                id: id.to_owned(),
                name,
                facing,
            },
            sensor,
        });
    }
    listed
}

/// The cameras the Java side lists; none until the activity is handed over.
pub(crate) fn cameras() -> Vec<CameraInfo> {
    let Some(java) = JAVA.get() else {
        return Vec::new();
    };
    let (Some(methods), Some(env)) = (java.camera.as_ref(), env(java)) else {
        return Vec::new();
    };
    // SAFETY: this thread's environment, inside a local frame popped after.
    unsafe {
        if env_call!(env, PushLocalFrame, 16) != JNI_OK {
            return Vec::new();
        }
        let listed = list(env, java, methods);
        env_call!(env, PopLocalFrame, ptr::null_mut());
        listed.into_iter().map(|l| l.info).collect()
    }
}

/// The ways camera `id` captures, as the Java side tells.
///
/// # Safety
///
/// `env` is the calling thread's environment, inside a local frame.
unsafe fn modes(env: *mut JNIEnv, java: &Java, methods: &Methods, id: &str) -> Vec<CameraMode> {
    // SAFETY: the caller's contract; a call of the static `modes`, its int
    // array copied out.
    unsafe {
        let args = [
            jvalue { l: java.context },
            jvalue {
                l: new_string(env, id),
            },
        ];
        let array = env_call!(
            env,
            CallStaticObjectMethodA,
            methods.class,
            methods.modes,
            args.as_ptr()
        );
        if !clear(env) || array.is_null() {
            return Vec::new();
        }
        let length = env_call!(env, GetArrayLength, array).max(0) as usize;
        let mut ints = vec![0 as jint; length];
        env_call!(
            env,
            GetIntArrayRegion,
            array,
            0,
            length as jint,
            ints.as_mut_ptr()
        );
        if !clear(env) {
            return Vec::new();
        }
        let (modes, _) = ints.as_chunks::<3>();
        modes
            .iter()
            .filter(|&&[w, h, rate]| w > 0 && h > 0 && rate > 0)
            .map(|&[w, h, rate]| CameraMode {
                width: w as u32,
                height: h as u32,
                frame_rate: f64::from(rate),
            })
            .collect()
    }
}

/// A camera capturing into a reader of its mode's size.
pub(crate) struct Capture {
    java: &'static Java,
    methods: &'static Methods,
    /// The `VisoCamera`, a global reference.
    camera: jobject,
    reports: Receiver<Report>,
    /// The sender the Java side posts to, freed once it is closed.
    mailbox: *mut Sender<Report>,
    images: Images,
    facing: CameraFacing,
    /// The clockwise degrees the sensor lies turned in the device, and the
    /// display's rotation.
    sensor: u32,
    display: u32,
    frame_rate: f32,
    ready: bool,
    /// Whether the picture taken last is not handed out yet.
    fresh: bool,
    failed: bool,
    wants_play: bool,
    /// Whether the Java side was asked to capture and not to stop since.
    capturing: bool,
}

impl Capture {
    pub(crate) fn open(request: &CameraRequest) -> Result<Capture, VideoError> {
        let java = JAVA.get().ok_or(VideoError::Unsupported)?;
        let methods = java.camera.as_ref().ok_or(VideoError::Unsupported)?;
        let env = env(java).ok_or(VideoError::WrongThread)?;
        let (tell, reports) = mpsc::channel();
        let mailbox = Box::into_raw(Box::new(tell));
        let mut images = Images::new();
        // SAFETY: JNI calls on this thread's environment, inside a local
        // frame popped before returning; the constructor is called with the
        // types its descriptor names.
        let opened = unsafe {
            if env_call!(env, PushLocalFrame, 32) != JNI_OK {
                drop(Box::from_raw(mailbox));
                return Err(VideoError::Open("out of JNI references".to_owned()));
            }
            let opened = open_in(env, java, methods, request, mailbox, &mut images);
            env_call!(env, PopLocalFrame, ptr::null_mut());
            opened
        };
        let (camera, facing, sensor, frame_rate) = match opened {
            Ok(opened) => opened,
            Err(error) => {
                // No report is made of a camera that never opened.
                // SAFETY: the mailbox nothing posts to, freed once.
                unsafe { drop(Box::from_raw(mailbox)) };
                return Err(error);
            }
        };
        let mut capture = Capture {
            java,
            methods,
            camera,
            reports,
            mailbox,
            images,
            facing,
            sensor,
            display: 0,
            frame_rate,
            ready: false,
            fresh: false,
            failed: false,
            wants_play: false,
            capturing: false,
        };
        capture.start();
        Ok(capture)
    }

    /// Asks the Java side to capture, with the user's leave.
    fn start(&mut self) {
        if self.capturing {
            return;
        }
        self.capturing = true;
        let activity = *ACTIVITY.lock().unwrap_or_else(|e| e.into_inner()) as jobject;
        call(
            self.java,
            self.camera,
            self.methods.start,
            &[jvalue { l: activity }],
        );
    }

    /// Asks the Java side to let the camera go.
    fn stop(&mut self) {
        if std::mem::take(&mut self.capturing) {
            call(self.java, self.camera, self.methods.stop, &[]);
        }
    }

    /// The quarter turns clockwise that show a picture upright now: the
    /// sensor's, less the display's for a camera facing away, plus it for
    /// one facing the user, whose turning the sensor sees mirrored.
    fn turns(&self) -> u8 {
        let degrees = match self.facing {
            CameraFacing::Front => self.sensor + self.display,
            _ => self.sensor + 360 - self.display % 360,
        };
        ((degrees / 90) % 4) as u8
    }

    pub(crate) fn poll(&mut self) -> Option<VideoEvent> {
        while !self.failed {
            let Ok(report) = self.reports.try_recv() else {
                break;
            };
            match report.kind {
                ERROR => {
                    self.failed = true;
                    self.capturing = false;
                    let why = match report.a {
                        DENIED => return Some(VideoEvent::Failed(VideoError::Denied)),
                        BUSY => "the camera is in use",
                        GONE => "the camera went away",
                        _ => "the camera failed",
                    };
                    return Some(VideoEvent::Failed(if self.ready {
                        VideoError::Play(why.to_owned())
                    } else {
                        VideoError::Open(why.to_owned())
                    }));
                }
                ROTATION => self.display = report.a.max(0) as u32 % 360,
                _ => {}
            }
        }
        if self.failed || self.ready || !self.images.take(Default::default(), self.turns()) {
            return None;
        }
        self.ready = true;
        self.fresh = true;
        if !self.wants_play {
            // Opened only to show its first picture.
            self.stop();
        }
        let frame = self.images.shown()?;
        Some(VideoEvent::Ready(VideoInfo {
            width: frame.width,
            height: frame.height,
            duration: None,
            frame_rate: Some(self.frame_rate),
            audio: false,
            seekable: false,
            facing: Some(self.facing),
        }))
    }

    pub(crate) fn frame(&mut self) -> Option<Frame<'_>> {
        if !self.ready {
            return None;
        }
        let turns = self.turns();
        if !std::mem::take(&mut self.fresh) && !self.images.take(Default::default(), turns) {
            return None;
        }
        self.images.shown()
    }

    pub(crate) fn play(&mut self) {
        self.wants_play = true;
        if !self.failed {
            self.start();
        }
    }

    pub(crate) fn pause(&mut self) {
        self.wants_play = false;
        if self.ready {
            self.stop();
        }
    }

    pub(crate) fn is_playing(&self) -> bool {
        self.wants_play && !self.failed
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        // Closing lets the camera go, after which nothing reports to the
        // mailbox; the reader's producer is gone once its session closes.
        call(self.java, self.camera, self.methods.close, &[]);
        if let Some(env) = env(self.java) {
            // SAFETY: the camera's global reference, deleted once.
            unsafe { env_call!(env, DeleteGlobalRef, self.camera) };
        }
        // SAFETY: the mailbox the Java side no longer reports to, freed
        // once; the images go after.
        unsafe { drop(Box::from_raw(self.mailbox)) };
    }
}

/// Picks the camera and mode `request` names, makes `images` read its
/// mode's size and the Java camera capture into it; the camera as a global
/// reference, which way it faces, its sensor's turn and its rate.
///
/// # Safety
///
/// `env` is the calling thread's environment, inside a local frame; the
/// mailbox outlives the Java camera's reports.
unsafe fn open_in(
    env: *mut JNIEnv,
    java: &Java,
    methods: &Methods,
    request: &CameraRequest,
    mailbox: *mut Sender<Report>,
    images: &mut Images,
) -> Result<(jobject, CameraFacing, u32, f32), VideoError> {
    // SAFETY: the caller's contract.
    unsafe {
        let listed = list(env, java, methods);
        let infos: Vec<CameraInfo> = listed.iter().map(|l| l.info.clone()).collect();
        let info = camera::chosen(&infos, &request.camera)
            .ok_or_else(|| VideoError::Open("no such camera".to_owned()))?;
        let sensor = listed
            .iter()
            .find(|l| l.info == *info)
            .map_or(0, |l| l.sensor);
        let modes = modes(env, java, methods, &info.id);
        let mode = camera::nearest_mode(&modes, request)
            .map(|i| modes[i])
            .ok_or_else(|| VideoError::Open("the camera has no mode".to_owned()))?;
        let id = new_string(env, &info.id);
        let mut made: Result<jobject, VideoError> =
            Err(VideoError::Open("no image reader".to_owned()));
        images.read_into(env, mode.width as jint, mode.height as jint, |surface| {
            let args = [
                jvalue {
                    j: mailbox as jni_sys::jlong,
                },
                jvalue { l: java.context },
                jvalue { l: id },
                jvalue { l: surface },
                jvalue {
                    i: mode.frame_rate.round() as jint,
                },
            ];
            let local = env_call!(env, NewObjectA, methods.class, methods.new, args.as_ptr());
            made = if env_call!(env, ExceptionCheck) || local.is_null() {
                Err(VideoError::Open(take_exception(env)))
            } else {
                Ok(env_call!(env, NewGlobalRef, local))
            };
        });
        Ok((made?, info.facing, sensor, mode.frame_rate as f32))
    }
}
