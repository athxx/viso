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
//! Live pictures are a `MediaStream` an extension asks the browser for
//! ([`MediaStreams`]) — a camera's from `getUserMedia` — shown by the same
//! element: asked for at opening, its tracks stopped while paused, which
//! lets the device go, and asked for again to play on.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use js_sys::{Array, Function, Promise, Uint8Array, Uint8ClampedArray};
use wasm_bindgen::JsCast;
use wasm_bindgen::convert::IntoWasmAbi;
use wasm_bindgen::prelude::*;

use super::camera::{CameraFacing, CameraInfo};
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

    type MediaStreamTrack;
    #[wasm_bindgen(method)]
    fn stop(this: &MediaStreamTrack);

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
    /// The streams of a player of live pictures.
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
        let (src, blob) = match source {
            VideoSource::File(path) => (Some(path.to_string_lossy().into_owned()), None),
            VideoSource::Url(url) => (Some(url.clone()), None),
            VideoSource::Bytes(bytes) => blob_url(bytes)?,
            VideoSource::Static(bytes) => blob_url(bytes)?,
            VideoSource::Camera(_) => return Err(VideoError::Unsupported),
        };
        Player::with(src, blob, None)
    }

    /// A player of `src` (with the blob URL it is, to revoke), or of the
    /// streams `capture` asks for.
    fn with(
        src: Option<String>,
        blob: Option<String>,
        capture: Option<Capture>,
    ) -> Result<Player, VideoError> {
        let video = create_video("video");
        if !video.is_instance_of::<js_sys::Object>() {
            return Err(VideoError::Unsupported);
        }
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
            let facing = self.capture.as_ref().and_then(Capture::facing);
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

/// The web: where a player of live pictures gets its `MediaStream`s — a
/// camera's from `getUserMedia`, a screen's from `getDisplayMedia` — asked
/// for at opening and again each time it plays, as pausing stops the
/// stream's tracks.
pub trait MediaStreams {
    /// Asks the browser for a stream; the answer comes later.
    fn ask(&mut self);
    /// The browser's answer to the last ask, once it came: a `MediaStream`,
    /// or why there is none.
    fn answer(&mut self) -> Option<Result<JsValue, VideoError>>;
    /// Which way the camera `stream` captures faces, where it has one.
    fn facing(&self, stream: &JsValue) -> Option<CameraFacing>;
}

/// Plays the live pictures of the streams `streams` asks for, in a hidden
/// `<video>` as a video's are: ready on the first picture; paused, the
/// stream's tracks stopped, which lets the device go.
pub fn web_live(streams: Box<dyn MediaStreams>) -> Result<Box<dyn super::LiveSource>, VideoError> {
    let mut capture = Capture {
        streams,
        pending: false,
        stream: None,
    };
    capture.ask();
    Ok(Box::new(Live(Player::with(None, None, Some(capture))?)))
}

/// A player of live pictures.
struct Live(Player);

impl super::LiveSource for Live {
    fn poll(&mut self) -> Option<VideoEvent> {
        self.0.poll()
    }

    fn frame(&mut self) -> Option<Frame<'_>> {
        self.0.frame()
    }

    fn play(&mut self) {
        self.0.play();
    }

    fn pause(&mut self) {
        self.0.pause();
    }

    fn is_playing(&self) -> bool {
        self.0.is_playing()
    }
}

/// The streams a player of live pictures shows.
struct Capture {
    streams: Box<dyn MediaStreams>,
    /// Whether an answer is awaited.
    pending: bool,
    /// The stream the element shows.
    stream: Option<MediaStream>,
}

impl Capture {
    /// Asks for a stream, unless one is shown or asked for already.
    fn ask(&mut self) {
        if self.stream.is_none() && !self.pending {
            self.pending = true;
            self.streams.ask();
        }
    }

    /// Takes the browser's answer: a stream `video` shows while `wanted`,
    /// else lets go of at once. Returns whether `video` shows a new one.
    fn answered(&mut self, video: &Video, wanted: bool) -> Result<bool, VideoError> {
        let Some(answer) = self.streams.answer() else {
            return Ok(false);
        };
        self.pending = false;
        let stream: MediaStream = answer?.unchecked_into();
        if !wanted {
            stop(&stream);
            return Ok(false);
        }
        video.set_src_object(Some(&stream));
        self.stream = Some(stream);
        Ok(true)
    }

    /// Stops the stream's tracks, which lets the device go; an answer still
    /// awaited is let go of as it comes.
    fn release(&mut self, video: &Video) {
        if let Some(stream) = self.stream.take() {
            stop(&stream);
            video.set_src_object(None);
        }
    }

    /// Which way the camera of the stream shown faces.
    fn facing(&self) -> Option<CameraFacing> {
        self.streams.facing(self.stream.as_ref()?)
    }
}

/// Stops each of `stream`'s tracks.
fn stop(stream: &MediaStream) {
    for track in stream.get_tracks().iter() {
        track.unchecked_into::<MediaStreamTrack>().stop();
    }
}

/// The cameras the platform's players capture: none, an extension answers.
pub(crate) fn cameras() -> Vec<CameraInfo> {
    Vec::new()
}
