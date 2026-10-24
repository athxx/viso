//! The web: the browser's `<video>`, which fetches, decodes and plays the
//! sound itself; each picture it presents drawn into an offscreen canvas
//! and read back as RGBA rows.
//!
//! The element lives in the page, hidden, so every browser keeps decoding
//! its pictures. Its state is read when polled, never waited on; the
//! browser says when it presents a new picture through
//! `requestVideoFrameCallback`, and only then is one read. Bytes play from a
//! blob URL, revoked when the player goes.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use js_sys::{Array, Function, Promise, Uint8Array, Uint8ClampedArray};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;

use super::{Frame, VideoError, VideoEvent, VideoInfo, VideoSource};

pub(crate) use super::no_picture::Picture;

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

pub(crate) struct Player {
    video: Video,
    /// The blob URL the bytes play from, revoked when the player goes.
    blob: Option<String>,
    /// Where the canvas pictures are drawn into and read back from, made at
    /// the first.
    canvas: Option<(Canvas, Context, u32, u32)>,
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
    rows: Vec<u8>,
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
            VideoSource::File(path) => (path.to_string_lossy().into_owned(), None),
            VideoSource::Url(url) => (url.clone(), None),
            VideoSource::Bytes(bytes) => blob_url(bytes)?,
            VideoSource::Static(bytes) => blob_url(bytes)?,
        };
        video.set_attribute("style", HIDDEN);
        video.set_attribute("aria-hidden", "true");
        video.set_plays_inline(true);
        video.set_preload("auto");
        // A picture read back must not taint the canvas: a cross-origin
        // source is fetched with CORS.
        if blob.is_none() && src.contains("://") {
            video.set_cross_origin("anonymous");
        }
        video.set_src(&src);
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
            blob,
            canvas: None,
            presented,
            asked,
            on_presented,
            refused,
            on_refused,
            callbacks: true,
            rows: Vec::new(),
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
        if !self.ready {
            if state < HAVE_CURRENT_DATA {
                return None;
            }
            self.ready = true;
            self.due = true;
            if self.wants_play {
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
        let (canvas, context, w, h) = self.canvas.get_or_insert_with(|| {
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
        context.draw_image(video, 0.0, 0.0);
        let Ok(image) = context.get_image_data(0.0, 0.0, f64::from(width), f64::from(height))
        else {
            // A cross-origin source without CORS: its pictures cannot be
            // read.
            self.failed = true;
            return None;
        };
        let data = image.data();
        self.rows.resize(data.length() as usize, 0);
        data.copy_to(&mut self.rows);
        self.read_at = now;
        let time = if presented.is_nan() { now } else { presented };
        Some(Frame::rgba(
            width,
            height,
            &self.rows,
            width as usize * 4,
            Duration::from_secs_f64(time.max(0.0)),
        ))
    }

    fn start(&self) {
        let _ = self.video.play().catch(&self.on_refused);
    }

    pub(crate) fn play(&mut self) {
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
    }

    pub(crate) fn seek(&mut self, time: Duration) {
        self.ended = false;
        self.seeking = true;
        self.video.set_current_time(time.as_secs_f64());
    }

    pub(crate) fn set_rate(&mut self, rate: f32) {
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
        self.video.remove_attribute("src");
        self.video.load();
        self.video.remove();
        if let Some(url) = self.blob.take() {
            revoke_object_url(&url);
        }
    }
}

/// A blob URL over a copy of `bytes`.
fn blob_url(bytes: &[u8]) -> Result<(String, Option<String>), VideoError> {
    let parts = Array::of1(&Uint8Array::from(bytes));
    let blob = Blob::new(&parts).map_err(|e| VideoError::Open(format!("{e:?}")))?;
    let url = create_object_url(&blob);
    Ok((url.clone(), Some(url)))
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
