//! The web: the browser's `<video>`, which fetches, decodes and plays the
//! sound itself; each picture it presents named for the GPU backend to copy
//! where it lies, or drawn into an offscreen canvas and read back as RGBA
//! rows where it cannot.
//!
//! The element lives in the page, hidden, so every browser keeps decoding
//! its pictures. Its state is read when polled, never waited on; the
//! browser says when it presents a new picture through
//! `requestVideoFrameCallback`, and only then is one handed out. Bytes play
//! from a blob URL, revoked when the player goes.
//!
//! A camera is the stream `getUserMedia` gives, shown by the same element:
//! asked for at opening, its tracks stopped while paused — which lets the
//! device go — and asked for again to play on. The browser asks the user the
//! first time; a refusal is [`VideoError::Denied`]. The cameras are listed
//! by `enumerateDevices`, which answers later: a listing answers what the
//! last one found, and names them only once the user let the page capture.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use js_sys::{Array, Function, Promise, Uint8Array, Uint8ClampedArray};
use wasm_bindgen::JsCast;
use wasm_bindgen::convert::IntoWasmAbi;
use wasm_bindgen::prelude::*;

use super::camera::{CameraChoice, CameraFacing, CameraInfo, CameraRequest};
use super::{Frame, PixelLayout, VideoError, VideoEvent, VideoInfo, VideoSource};

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(extends = js_sys::Object)]
    type Video;

    #[wasm_bindgen(js_namespace = document, js_name = createElement)]
    fn create_video(tag: &str) -> Video;
    #[wasm_bindgen(js_namespace = ["document", "body"], js_name = appendChild)]
    fn append_to_body(node: &Video);

    #[wasm_bindgen(method, js_name = setAttribute)]
    fn set_attribute(this: &Video, name: &str, value: &str);
    #[wasm_bindgen(method, js_name = removeAttribute)]
    fn remove_attribute(this: &Video, name: &str);
    #[wasm_bindgen(method)]
    fn remove(this: &Video);
    #[wasm_bindgen(method)]
    fn load(this: &Video);
    #[wasm_bindgen(method)]
    fn play(this: &Video) -> Promise;
    #[wasm_bindgen(method)]
    fn pause(this: &Video);
    #[wasm_bindgen(method, catch, js_name = requestVideoFrameCallback)]
    fn request_video_frame_callback(this: &Video, callback: &Function) -> Result<u32, JsValue>;

    #[wasm_bindgen(method, setter = src)]
    fn set_src(this: &Video, src: &str);
    #[wasm_bindgen(method, setter = srcObject)]
    fn set_src_object(this: &Video, stream: Option<&MediaStream>);
    #[wasm_bindgen(method, setter = crossOrigin)]
    fn set_cross_origin(this: &Video, value: &str);
    #[wasm_bindgen(method, setter = preload)]
    fn set_preload(this: &Video, value: &str);
    #[wasm_bindgen(method, setter = playsInline)]
    fn set_plays_inline(this: &Video, value: bool);
    #[wasm_bindgen(method, setter = muted)]
    fn set_muted(this: &Video, value: bool);
    #[wasm_bindgen(method, setter = loop)]
    fn set_loop(this: &Video, value: bool);
    #[wasm_bindgen(method, setter = volume)]
    fn set_volume(this: &Video, value: f64);
    #[wasm_bindgen(method, setter = playbackRate)]
    fn set_playback_rate(this: &Video, value: f64);
    #[wasm_bindgen(method, setter = currentTime)]
    fn set_current_time(this: &Video, value: f64);

    #[wasm_bindgen(method, getter = currentTime)]
    fn current_time(this: &Video) -> f64;
    #[wasm_bindgen(method, getter)]
    fn duration(this: &Video) -> f64;
    #[wasm_bindgen(method, getter = readyState)]
    fn ready_state(this: &Video) -> u32;
    #[wasm_bindgen(method, getter = videoWidth)]
    fn video_width(this: &Video) -> u32;
    #[wasm_bindgen(method, getter = videoHeight)]
    fn video_height(this: &Video) -> u32;
    #[wasm_bindgen(method, getter)]
    fn paused(this: &Video) -> bool;
    #[wasm_bindgen(method, getter)]
    fn ended(this: &Video) -> bool;
    #[wasm_bindgen(method, getter)]
    fn seeking(this: &Video) -> bool;
    #[wasm_bindgen(method, getter)]
    fn error(this: &Video) -> Option<MediaError>;
    #[wasm_bindgen(method, getter)]
    fn seekable(this: &Video) -> TimeRanges;

    type MediaError;
    #[wasm_bindgen(method, getter)]
    fn code(this: &MediaError) -> u32;
    #[wasm_bindgen(method, getter)]
    fn message(this: &MediaError) -> String;

    type TimeRanges;
    #[wasm_bindgen(method, getter)]
    fn length(this: &TimeRanges) -> u32;

    #[wasm_bindgen(js_name = OffscreenCanvas)]
    type Canvas;
    #[wasm_bindgen(constructor, js_class = OffscreenCanvas)]
    fn new(width: u32, height: u32) -> Canvas;
    #[wasm_bindgen(method, setter = width)]
    fn set_width(this: &Canvas, value: u32);
    #[wasm_bindgen(method, setter = height)]
    fn set_height(this: &Canvas, value: u32);
    #[wasm_bindgen(method, js_name = getContext)]
    fn get_context(this: &Canvas, kind: &str, options: &JsValue) -> Context;

    type Context;
    #[wasm_bindgen(method, js_name = drawImage)]
    fn draw_image(this: &Context, image: &Video, x: f64, y: f64);
    #[wasm_bindgen(method, catch, js_name = getImageData)]
    fn get_image_data(
        this: &Context,
        x: f64,
        y: f64,
        width: f64,
        height: f64,
    ) -> Result<ImageData, JsValue>;

    type ImageData;
    #[wasm_bindgen(method, getter)]
    fn data(this: &ImageData) -> Uint8ClampedArray;

    type MediaStream;
    #[wasm_bindgen(method, js_name = getTracks)]
    fn get_tracks(this: &MediaStream) -> Array;
    #[wasm_bindgen(method, js_name = getVideoTracks)]
    fn get_video_tracks(this: &MediaStream) -> Array;

    type MediaStreamTrack;
    #[wasm_bindgen(method)]
    fn stop(this: &MediaStreamTrack);
    #[wasm_bindgen(method, js_name = getSettings)]
    fn get_settings(this: &MediaStreamTrack) -> JsValue;

    type MediaDevices;
    #[wasm_bindgen(thread_local_v2, js_namespace = navigator, js_name = mediaDevices)]
    static MEDIA_DEVICES: Option<MediaDevices>;
    #[wasm_bindgen(method, catch, js_name = getUserMedia)]
    fn get_user_media(this: &MediaDevices, constraints: &JsValue) -> Result<Promise, JsValue>;
    #[wasm_bindgen(method, catch, js_name = enumerateDevices)]
    fn enumerate_devices(this: &MediaDevices) -> Result<Promise, JsValue>;

    type Blob;
    #[wasm_bindgen(constructor, catch)]
    fn new(parts: &Array) -> Result<Blob, JsValue>;
    #[wasm_bindgen(js_namespace = URL, js_name = createObjectURL)]
    fn create_object_url(blob: &Blob) -> String;
    #[wasm_bindgen(js_namespace = URL, js_name = revokeObjectURL)]
    fn revoke_object_url(url: &str);
}

/// `readyState`: the picture at the current position is there.
const HAVE_CURRENT_DATA: u32 = 2;
/// `readyState`: enough is there to play on.
const HAVE_FUTURE_DATA: u32 = 3;

/// How the element is kept in the page without being seen.
const HIDDEN: &str = "position:fixed;left:0;top:0;width:1px;height:1px;\
     opacity:0;pointer-events:none;z-index:-1";

/// The picture the element shows now: named for a GPU backend to copy, or
/// read back through the offscreen canvas.
#[derive(Clone, Copy)]
pub(crate) struct Picture<'a> {
    video: &'a Video,
    back: &'a RefCell<Readback>,
}

impl std::fmt::Debug for Picture<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Picture")
    }
}

/// What a picture is read back through, made at the first read.
#[derive(Default)]
struct Readback {
    canvas: Option<(Canvas, Context, u32, u32)>,
    rows: Vec<u8>,
    /// A cross-origin source without CORS: its pictures cannot be read.
    tainted: bool,
}

impl Picture<'_> {
    /// The element, borrowed by the index of the value it is held in.
    pub(crate) fn external(&self, width: u32, height: u32) -> Option<viso_handle::ExternalImage> {
        let element: &JsValue = self.video.as_ref();
        Some(viso_handle::ExternalImage::WebVideo {
            element: element.into_abi(),
            width,
            height,
        })
    }

    /// Calls `f` with the picture's rows, drawn into the canvas and read
    /// back, and their stride; returns whether they could be read.
    pub(crate) fn read(&self, f: impl FnOnce(&[u8], usize)) -> bool {
        let mut back = self.back.borrow_mut();
        let back = &mut *back;
        let (width, height) = (self.video.video_width(), self.video.video_height());
        if back.tainted || width == 0 || height == 0 {
            return false;
        }
        let (canvas, context, w, h) = back.canvas.get_or_insert_with(|| {
            let canvas = Canvas::new(width, height);
            let options = js_sys::Object::new();
            let _ = js_sys::Reflect::set(
                &options,
                &JsValue::from_str("willReadFrequently"),
                &JsValue::TRUE,
            );
            let context = canvas.get_context("2d", &options);
            (canvas, context, width, height)
        });
        if (*w, *h) != (width, height) {
            canvas.set_width(width);
            canvas.set_height(height);
            (*w, *h) = (width, height);
        }
        context.draw_image(self.video, 0.0, 0.0);
        let Ok(image) = context.get_image_data(0.0, 0.0, f64::from(width), f64::from(height))
        else {
            back.tainted = true;
            return false;
        };
        let data = image.data();
        back.rows.resize(data.length() as usize, 0);
        data.copy_to(&mut back.rows);
        f(&back.rows, width as usize * 4);
        true
    }

    pub(crate) fn layout(&self) -> PixelLayout {
        PixelLayout::Rgba8
    }
}

pub(crate) struct Player {
    video: Video,
    /// A camera's stream, for a player of one.
    capture: Option<Capture>,
    /// The blob URL the bytes play from, revoked when the player goes.
    blob: Option<String>,
    back: RefCell<Readback>,
    /// The media time of the picture the browser presented last and that
    /// is not yet read; NaN for none.
    presented: Rc<Cell<f64>>,
    /// Whether a presented-picture callback is asked for.
    asked: Rc<Cell<bool>>,
    on_presented: Closure<dyn FnMut(f64, JsValue)>,
    /// Set when the browser refuses to play, as without a user gesture.
    refused: Rc<Cell<bool>>,
    on_refused: Closure<dyn FnMut(JsValue)>,
    /// Whether the browser calls back when it presents a picture; without,
    /// a picture is read whenever the position moves.
    callbacks: bool,
    ready: bool,
    failed: bool,
    wants_play: bool,
    looping: bool,
    ended: bool,
    waiting: bool,
    seeking: bool,
    /// A picture is due though none was presented: the first, or a seek's.
    due: bool,
    /// The position the last picture was read at, without callbacks.
    read_at: f64,
}

impl Player {
    pub(crate) fn open(source: &VideoSource) -> Result<Player, VideoError> {
        let video = create_video("video");
        if !video.is_instance_of::<js_sys::Object>() {
            return Err(VideoError::Unsupported);
        }
        let (src, blob) = match source {
            VideoSource::File(path) => (Some(path.to_string_lossy().into_owned()), None),
            VideoSource::Url(url) => (Some(url.clone()), None),
            VideoSource::Bytes(bytes) => blob_url(bytes)?,
            VideoSource::Static(bytes) => blob_url(bytes)?,
            VideoSource::Camera(_) => (None, None),
        };
        let capture = match source {
            VideoSource::Camera(request) => Some(Capture::new(request)?),
            _ => None,
        };
        video.set_attribute("style", HIDDEN);
        video.set_attribute("aria-hidden", "true");
        video.set_plays_inline(true);
        video.set_preload("auto");
        if let Some(src) = &src {
            // A picture read back must not taint the canvas: a cross-origin
            // source is fetched with CORS.
            if blob.is_none() && src.contains("://") {
                video.set_cross_origin("anonymous");
            }
            video.set_src(src);
        } else {
            video.set_muted(true);
        }
        append_to_body(&video);
        let presented = Rc::new(Cell::new(f64::NAN));
        let asked = Rc::new(Cell::new(false));
        let on_presented = {
            let (presented, asked) = (Rc::clone(&presented), Rc::clone(&asked));
            Closure::<dyn FnMut(f64, JsValue)>::new(move |_now: f64, metadata: JsValue| {
                asked.set(false);
                let time = js_sys::Reflect::get(&metadata, &JsValue::from_str("mediaTime"))
                    .ok()
                    .and_then(|t| t.as_f64())
                    .unwrap_or(0.0);
                presented.set(time);
            })
        };
        let refused = Rc::new(Cell::new(false));
        let on_refused = {
            let refused = Rc::clone(&refused);
            Closure::<dyn FnMut(JsValue)>::new(move |_| refused.set(true))
        };
        Ok(Player {
            video,
            capture,
            blob,
            back: RefCell::default(),
            presented,
            asked,
            on_presented,
            refused,
            on_refused,
            callbacks: true,
            ready: false,
            failed: false,
            wants_play: false,
            looping: false,
            ended: false,
            waiting: false,
            seeking: false,
            due: false,
            read_at: f64::NAN,
        })
    }

    pub(crate) fn poll(&mut self) -> Option<VideoEvent> {
        if self.failed {
            return None;
        }
        if let Some(capture) = &mut self.capture {
            // Captured from while it opens, and while it plays.
            let wanted = !self.ready || self.wants_play;
            match capture.answered(&self.video, wanted) {
                Ok(true) => {
                    let _ = self.video.play().catch(&self.on_refused);
                }
                Ok(false) => {}
                Err(error) => {
                    self.failed = true;
                    return Some(VideoEvent::Failed(error));
                }
            }
        }
        let video = &self.video;
        if let Some(error) = video.error() {
            self.failed = true;
            let why = format!("<video> error {}: {}", error.code(), error.message());
            return Some(VideoEvent::Failed(if self.ready {
                VideoError::Play(why)
            } else {
                VideoError::Open(why)
            }));
        }
        let state = video.ready_state();
        if self.back.get_mut().tainted {
            self.failed = true;
            return Some(VideoEvent::Failed(VideoError::Play(
                "a cross-origin picture without CORS cannot be read".into(),
            )));
        }
        if !self.ready {
            if state < HAVE_CURRENT_DATA {
                return None;
            }
            self.ready = true;
            self.due = true;
            let facing = self.capture.as_ref().map(Capture::facing);
            if let Some(capture) = &mut self.capture {
                if !self.wants_play {
                    capture.release(&self.video);
                }
            } else if self.wants_play {
                self.start();
            }
            let length = video.duration();
            let duration =
                (length.is_finite() && length > 0.0).then(|| Duration::from_secs_f64(length));
            return Some(VideoEvent::Ready(VideoInfo {
                width: video.video_width(),
                height: video.video_height(),
                duration,
                frame_rate: None,
                audio: has_audio(video),
                seekable: duration.is_some() && video.seekable().length() > 0,
                facing,
            }));
        }
        if self.refused.replace(false) {
            // Refused to play, as without a user gesture: it stays paused.
            self.wants_play = false;
        }
        let seeking = video.seeking();
        if std::mem::replace(&mut self.seeking, seeking) && !seeking {
            self.due = true;
        }
        if self.wants_play && !self.looping && video.ended() && !self.ended {
            self.ended = true;
            self.wants_play = false;
            return Some(VideoEvent::Ended);
        }
        let waiting = self.wants_play && !video.paused() && state < HAVE_FUTURE_DATA;
        if waiting != self.waiting {
            self.waiting = waiting;
            return Some(VideoEvent::Waiting(waiting));
        }
        None
    }

    pub(crate) fn frame(&mut self) -> Option<Frame<'_>> {
        if !self.ready || self.failed {
            return None;
        }
        let video = &self.video;
        let presented = self.presented.replace(f64::NAN);
        if self.callbacks && !self.asked.get() {
            match video.request_video_frame_callback(self.on_presented.as_ref().unchecked_ref()) {
                Ok(_) => self.asked.set(true),
                Err(_) => self.callbacks = false,
            }
        }
        let now = video.current_time();
        let fresh = if self.callbacks {
            !presented.is_nan()
        } else {
            now != self.read_at
        };
        if !fresh && !std::mem::take(&mut self.due) {
            return None;
        }
        self.due = false;
        let (width, height) = (video.video_width(), video.video_height());
        if width == 0 || height == 0 {
            return None;
        }
        self.read_at = now;
        let time = if presented.is_nan() { now } else { presented };
        Some(Frame::native_picture(
            width,
            height,
            Duration::from_secs_f64(time.max(0.0)),
            Picture {
                video: &self.video,
                back: &self.back,
            },
        ))
    }

    fn start(&self) {
        let _ = self.video.play().catch(&self.on_refused);
    }

    pub(crate) fn play(&mut self) {
        if let Some(capture) = &mut self.capture {
            self.wants_play = true;
            if self.ready {
                capture.ask();
            }
            return;
        }
        if std::mem::take(&mut self.ended) {
            self.seek(Duration::ZERO);
        }
        self.wants_play = true;
        if self.ready {
            self.start();
        }
    }

    pub(crate) fn pause(&mut self) {
        self.wants_play = false;
        self.video.pause();
        if self.ready
            && let Some(capture) = &mut self.capture
        {
            capture.release(&self.video);
        }
    }

    pub(crate) fn seek(&mut self, time: Duration) {
        if self.capture.is_some() {
            return;
        }
        self.ended = false;
        self.seeking = true;
        self.video.set_current_time(time.as_secs_f64());
    }

    pub(crate) fn set_rate(&mut self, rate: f32) {
        if self.capture.is_some() {
            return;
        }
        let rate = if rate.is_finite() && rate > 0.0 {
            rate
        } else {
            1.0
        };
        self.video.set_playback_rate(f64::from(rate));
    }

    pub(crate) fn set_volume(&mut self, volume: f32) {
        self.video.set_volume(f64::from(volume.clamp(0.0, 1.0)));
    }

    pub(crate) fn set_muted(&mut self, muted: bool) {
        self.video.set_muted(muted);
    }

    pub(crate) fn set_looping(&mut self, looping: bool) {
        self.looping = looping;
        self.video.set_loop(looping);
    }

    pub(crate) fn position(&self) -> Duration {
        if self.capture.is_some() {
            return Duration::ZERO;
        }
        Duration::from_secs_f64(self.video.current_time().max(0.0))
    }

    pub(crate) fn is_playing(&self) -> bool {
        self.wants_play && !self.ended
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        // Emptied and taken out of the page, the element frees its decoder.
        self.video.pause();
        if let Some(capture) = &mut self.capture {
            capture.release(&self.video);
        }
        self.video.remove_attribute("src");
        self.video.load();
        self.video.remove();
        if let Some(url) = self.blob.take() {
            revoke_object_url(&url);
        }
    }
}

/// A blob URL over a copy of `bytes`, to play and to revoke.
fn blob_url(bytes: &[u8]) -> Result<(Option<String>, Option<String>), VideoError> {
    let parts = Array::of1(&Uint8Array::from(bytes));
    let blob = Blob::new(&parts).map_err(|e| VideoError::Open(format!("{e:?}")))?;
    let url = create_object_url(&blob);
    Ok((Some(url.clone()), Some(url)))
}

/// Whether the video has sound, where the browser says; taken to have it
/// where it does not.
fn has_audio(video: &Video) -> bool {
    let get = |name: &str| js_sys::Reflect::get(video, &JsValue::from_str(name)).ok();
    if let Some(tracks) = get("audioTracks").filter(|t| t.is_object())
        && let Some(length) = js_sys::Reflect::get(&tracks, &JsValue::from_str("length"))
            .ok()
            .and_then(|l| l.as_f64())
    {
        return length > 0.0;
    }
    get("mozHasAudio")
        .and_then(|has| has.as_bool())
        .unwrap_or(true)
}

/// A camera's stream, asked of the browser.
struct Capture {
    /// What `getUserMedia` is asked for.
    constraints: JsValue,
    /// Which way the camera asked for faces, where the request says.
    asked: Option<CameraFacing>,
    /// The browser's answer to the last ask, until taken.
    answer: Rc<RefCell<Option<Result<MediaStream, JsValue>>>>,
    on_stream: Closure<dyn FnMut(JsValue)>,
    on_refusal: Closure<dyn FnMut(JsValue)>,
    /// Whether an answer is awaited.
    pending: bool,
    /// The stream the element shows.
    stream: Option<MediaStream>,
}

impl Capture {
    /// The capture `request` asks for, asked for at once.
    fn new(request: &CameraRequest) -> Result<Capture, VideoError> {
        let set = |object: &js_sys::Object, key: &str, value: &JsValue| {
            let _ = js_sys::Reflect::set(object, &JsValue::from_str(key), value);
        };
        let ideal = |value: &JsValue| {
            let object = js_sys::Object::new();
            set(&object, "ideal", value);
            object
        };
        let video = js_sys::Object::new();
        set(&video, "width", &ideal(&request.width.max(1).into()));
        set(&video, "height", &ideal(&request.height.max(1).into()));
        set(
            &video,
            "frameRate",
            &ideal(&request.frame_rate.max(1).into()),
        );
        let asked = match &request.camera {
            CameraChoice::Default => None,
            CameraChoice::Facing(facing) => Some(*facing),
            CameraChoice::Id(id) => {
                let exact = js_sys::Object::new();
                set(&exact, "exact", &JsValue::from_str(id));
                set(&video, "deviceId", &exact);
                None
            }
        };
        let mode = match asked {
            Some(CameraFacing::Front) => Some("user"),
            Some(CameraFacing::Back) => Some("environment"),
            Some(CameraFacing::External) | None => None,
        };
        if let Some(mode) = mode {
            set(&video, "facingMode", &ideal(&JsValue::from_str(mode)));
        }
        let constraints = js_sys::Object::new();
        set(&constraints, "video", &video);
        set(&constraints, "audio", &JsValue::FALSE);
        let answer = Rc::new(RefCell::new(None));
        let on_stream = {
            let answer = Rc::clone(&answer);
            Closure::<dyn FnMut(JsValue)>::new(move |stream: JsValue| {
                *answer.borrow_mut() = Some(Ok(stream.unchecked_into()));
            })
        };
        let on_refusal = {
            let answer = Rc::clone(&answer);
            Closure::<dyn FnMut(JsValue)>::new(move |error: JsValue| {
                *answer.borrow_mut() = Some(Err(error));
            })
        };
        let mut capture = Capture {
            constraints: constraints.into(),
            asked,
            answer,
            on_stream,
            on_refusal,
            pending: false,
            stream: None,
        };
        if MEDIA_DEVICES.with(Option::is_none) {
            return Err(VideoError::Unsupported);
        }
        capture.ask();
        Ok(capture)
    }

    /// Asks for the stream, unless it is shown or asked for already.
    fn ask(&mut self) {
        if self.stream.is_some() || self.pending {
            return;
        }
        let asked = MEDIA_DEVICES.with(|devices| {
            let promise = devices.as_ref()?.get_user_media(&self.constraints).ok()?;
            let _ = promise.then2(&self.on_stream, &self.on_refusal);
            Some(())
        });
        self.pending = asked.is_some();
        if asked.is_none() {
            *self.answer.borrow_mut() = Some(Err(JsValue::from_str("NotFoundError")));
        }
    }

    /// Takes the browser's answer: a stream `video` shows while `wanted`,
    /// else lets go of at once. Returns whether `video` shows a new one.
    fn answered(&mut self, video: &Video, wanted: bool) -> Result<bool, VideoError> {
        let Some(answer) = self.answer.borrow_mut().take() else {
            return Ok(false);
        };
        self.pending = false;
        let stream = answer.map_err(refusal)?;
        if !wanted {
            stop(&stream);
            return Ok(false);
        }
        video.set_src_object(Some(&stream));
        self.stream = Some(stream);
        Ok(true)
    }

    /// Stops the stream's tracks, which lets the camera go; an answer still
    /// awaited is let go of as it comes.
    fn release(&mut self, video: &Video) {
        if let Some(stream) = self.stream.take() {
            stop(&stream);
            video.set_src_object(None);
        }
    }

    /// Which way the camera shown faces: as its track says, else as asked,
    /// else toward the user, as a computer's own camera does.
    fn facing(&self) -> CameraFacing {
        let said = self.stream.as_ref().and_then(|stream| {
            let track: MediaStreamTrack = stream.get_video_tracks().get(0).dyn_into().ok()?;
            js_sys::Reflect::get(&track.get_settings(), &JsValue::from_str("facingMode"))
                .ok()?
                .as_string()
        });
        match said.as_deref() {
            Some("user") => CameraFacing::Front,
            Some("environment") => CameraFacing::Back,
            Some(_) => CameraFacing::External,
            None => self.asked.unwrap_or(CameraFacing::Front),
        }
    }
}

/// Stops each of `stream`'s tracks.
fn stop(stream: &MediaStream) {
    for track in stream.get_tracks().iter() {
        track.unchecked_into::<MediaStreamTrack>().stop();
    }
}

/// The error a refused `getUserMedia` stands for.
fn refusal(error: JsValue) -> VideoError {
    let name = js_sys::Reflect::get(&error, &JsValue::from_str("name"))
        .ok()
        .and_then(|name| name.as_string())
        .or_else(|| error.as_string())
        .unwrap_or_default();
    match name.as_str() {
        "NotAllowedError" | "SecurityError" => VideoError::Denied,
        "NotFoundError" | "OverconstrainedError" => VideoError::Open("no such camera".into()),
        _ => VideoError::Open(format!("the camera cannot be opened: {name}")),
    }
}

thread_local! {
    /// The cameras the last listing found, and whether one is under way.
    static LISTED: Rc<RefCell<(Vec<CameraInfo>, bool)>> = Rc::default();
}

/// The cameras the last listing found; a new one starts, its answer kept
/// for the next call.
pub(crate) fn cameras() -> Vec<CameraInfo> {
    let listed = LISTED.with(Rc::clone);
    if !std::mem::replace(&mut listed.borrow_mut().1, true) {
        let found = Rc::clone(&listed);
        let on_devices = Closure::once(move |devices: JsValue| {
            let devices: Array = devices.unchecked_into();
            let get = |device: &JsValue, key: &str| {
                js_sys::Reflect::get(device, &JsValue::from_str(key))
                    .ok()
                    .and_then(|v| v.as_string())
                    .unwrap_or_default()
            };
            let cameras = devices
                .iter()
                .filter(|device| get(device, "kind") == "videoinput")
                .map(|device| {
                    let name = get(&device, "label");
                    let lower = name.to_ascii_lowercase();
                    let facing = if lower.contains("back") || lower.contains("rear") {
                        CameraFacing::Back
                    } else if lower.contains("front")
                        || lower.contains("facetime")
                        || lower.contains("integrated")
                        || lower.contains("built-in")
                    {
                        CameraFacing::Front
                    } else {
                        CameraFacing::External
                    };
                    CameraInfo {
                        id: get(&device, "deviceId"),
                        name,
                        facing,
                    }
                })
                .collect();
            *found.borrow_mut() = (cameras, false);
        });
        let started = MEDIA_DEVICES.with(|devices| {
            let promise = devices.as_ref()?.enumerate_devices().ok()?;
            let _ = promise.then(&on_devices);
            Some(())
        });
        if started.is_some() {
            on_devices.forget();
        } else {
            listed.borrow_mut().1 = false;
        }
    }
    listed.borrow().0.clone()
}
