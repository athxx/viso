//! Linux and the BSDs: GStreamer's `playbin`, loaded at run time, its
//! pictures taken from an `appsink` as NV12 planes, which the GPU converts,
//! or as BGRA rows once a copy is wanted.
//!
//! The libraries are opened on first use, so a program runs where GStreamer
//! is not installed and only its videos fail, as `Unsupported`. `playbin`
//! picks the demuxer and decoder (hardware where the system ranks one
//! first), hands decoded Y′CbCr over as NV12 — as decoders write it, or
//! reinterleaved, never converted to RGB on the CPU — and keeps sound and
//! picture in step on its own threads. A picture copied out as rows instead
//! is converted here, then the sink asks for BGRA, which GStreamer's own
//! converter writes faster, from the next on; everything this side does is on the loop's thread and never
//! waits. The bus is read when polled; the sink holds the picture due now,
//! which is handed out mapped where GStreamer wrote it, with no copy, until
//! the next replaces it.

use std::cell::Cell;
use std::ffi::{CStr, CString, c_char, c_int, c_uint, c_void};
use std::ptr;
use std::sync::OnceLock;
use std::time::Duration;

use viso_handle::{Nv12, YuvMatrix};

use super::{Frame, Spilled, VideoError, VideoEvent, VideoInfo, VideoSource, file_uri, yuv};

pub(crate) use super::no_picture::Picture;

type GstElement = c_void;
type GstBus = c_void;
type GstSample = c_void;
type GstBuffer = c_void;
type GstCaps = c_void;
type GstStructure = c_void;
type GstQuery = c_void;
type GstPad = c_void;
type GstEvent = c_void;

/// The head of `GstMiniObject`, the same in every 1.x release.
#[repr(C)]
struct MiniObject {
    kind: usize,
    refcount: c_int,
    lockstate: c_int,
    flags: c_uint,
    copy: *mut c_void,
    dispose: *mut c_void,
    free: *mut c_void,
    priv_uint: c_uint,
    priv_pointer: *mut c_void,
}

/// The head of `GstMessage`.
#[repr(C)]
struct Message {
    mini_object: MiniObject,
    kind: c_uint,
}

/// The head of `GstBuffer`.
#[repr(C)]
struct Buffer {
    mini_object: MiniObject,
    pool: *mut c_void,
    pts: u64,
}

#[repr(C)]
struct MapInfo {
    memory: *mut c_void,
    flags: c_uint,
    data: *mut u8,
    size: usize,
    maxsize: usize,
    user_data: [*mut c_void; 4],
    reserved: [*mut c_void; 4],
}

#[repr(C)]
struct GError {
    domain: u32,
    code: c_int,
    message: *const c_char,
}

const STATE_NULL: c_uint = 1;
const STATE_PAUSED: c_uint = 3;
const STATE_PLAYING: c_uint = 4;
const STATE_CHANGE_NO_PREROLL: c_int = 3;

const FORMAT_TIME: c_int = 3;
const SEEK_FLAG_FLUSH: c_uint = 1 << 0;
const SEEK_FLAG_ACCURATE: c_uint = 1 << 1;
const SEEK_FLAG_INSTANT_RATE_CHANGE: c_uint = 1 << 10;
const SEEK_TYPE_NONE: c_int = 0;
const SEEK_TYPE_SET: c_int = 1;

const MESSAGE_EOS: c_uint = 1 << 0;
const MESSAGE_ERROR: c_uint = 1 << 1;
const MESSAGE_BUFFERING: c_uint = 1 << 5;
const MESSAGE_ASYNC_DONE: c_uint = 1 << 21;

const MAP_READ: c_uint = 1 << 0;

/// `playbin`'s video, audio, software volume and deinterlacing, without the
/// subtitles it would otherwise draw over the picture.
const PLAY_FLAGS: c_int = 0x1 | 0x2 | 0x10 | 0x200;

const CLOCK_TIME_NONE: u64 = u64::MAX;

/// The entry points used, out of the libraries opened once for the process
/// and never closed.
struct Gst {
    element_factory_make: unsafe extern "C" fn(*const c_char, *const c_char) -> *mut GstElement,
    element_set_state: unsafe extern "C" fn(*mut GstElement, c_uint) -> c_int,
    element_query_position: unsafe extern "C" fn(*mut GstElement, c_int, *mut i64) -> c_int,
    element_query_duration: unsafe extern "C" fn(*mut GstElement, c_int, *mut i64) -> c_int,
    element_query: unsafe extern "C" fn(*mut GstElement, *mut GstQuery) -> c_int,
    element_seek:
        unsafe extern "C" fn(*mut GstElement, f64, c_int, c_uint, c_int, i64, c_int, i64) -> c_int,
    element_get_bus: unsafe extern "C" fn(*mut GstElement) -> *mut GstBus,
    bus_pop_filtered: unsafe extern "C" fn(*mut GstBus, c_uint) -> *mut Message,
    message_parse_error: unsafe extern "C" fn(*mut Message, *mut *mut GError, *mut *mut c_char),
    message_parse_buffering: unsafe extern "C" fn(*mut Message, *mut c_int),
    query_new_seeking: unsafe extern "C" fn(c_int) -> *mut GstQuery,
    query_parse_seeking:
        unsafe extern "C" fn(*mut GstQuery, *mut c_int, *mut c_int, *mut i64, *mut i64),
    mini_object_unref: unsafe extern "C" fn(*mut c_void),
    object_ref: unsafe extern "C" fn(*mut c_void) -> *mut c_void,
    object_unref: unsafe extern "C" fn(*mut c_void),
    sample_get_buffer: unsafe extern "C" fn(*mut GstSample) -> *mut GstBuffer,
    sample_get_caps: unsafe extern "C" fn(*mut GstSample) -> *mut GstCaps,
    buffer_map: unsafe extern "C" fn(*mut GstBuffer, *mut MapInfo, c_uint) -> c_int,
    buffer_unmap: unsafe extern "C" fn(*mut GstBuffer, *mut MapInfo),
    caps_from_string: unsafe extern "C" fn(*const c_char) -> *mut GstCaps,
    caps_get_structure: unsafe extern "C" fn(*mut GstCaps, c_uint) -> *mut GstStructure,
    structure_get_int: unsafe extern "C" fn(*mut GstStructure, *const c_char, *mut c_int) -> c_int,
    structure_get_fraction:
        unsafe extern "C" fn(*mut GstStructure, *const c_char, *mut c_int, *mut c_int) -> c_int,
    structure_get_string: unsafe extern "C" fn(*mut GstStructure, *const c_char) -> *const c_char,
    element_get_static_pad: unsafe extern "C" fn(*mut GstElement, *const c_char) -> *mut GstPad,
    event_new_reconfigure: unsafe extern "C" fn() -> *mut GstEvent,
    pad_push_event: unsafe extern "C" fn(*mut GstPad, *mut GstEvent) -> c_int,
    app_sink_set_caps: unsafe extern "C" fn(*mut GstElement, *const GstCaps),
    app_sink_try_pull_sample: unsafe extern "C" fn(*mut GstElement, u64) -> *mut GstSample,
    app_sink_try_pull_preroll: unsafe extern "C" fn(*mut GstElement, u64) -> *mut GstSample,
    object_set: unsafe extern "C" fn(*mut c_void, *const c_char, ...),
    object_get: unsafe extern "C" fn(*mut c_void, *const c_char, ...),
    error_free: unsafe extern "C" fn(*mut GError),
    free: unsafe extern "C" fn(*mut c_void),
    main_context_iteration: unsafe extern "C" fn(*mut c_void, c_int) -> c_int,
}

// SAFETY: the table holds plain function pointers into libraries that stay
// loaded for the life of the process; GStreamer's calls are thread-safe.
unsafe impl Send for Gst {}
// SAFETY: as above; the table is never written after it is filled.
unsafe impl Sync for Gst {}

/// The process's GStreamer, opened and initialised on first use; `None`
/// where it is not installed or does not start.
fn gst() -> Option<&'static Gst> {
    static GST: OnceLock<Option<Gst>> = OnceLock::new();
    GST.get_or_init(Gst::load).as_ref()
}

/// An opened library's handle, never closed: the library stays loaded for
/// as long as the process runs.
struct Library(*mut c_void);

impl Library {
    fn open(name: &CStr) -> Option<Library> {
        // SAFETY: `name` is a NUL-terminated library name; the handle is
        // kept for the life of the process.
        let handle = unsafe { libc::dlopen(name.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
        (!handle.is_null()).then_some(Library(handle))
    }

    /// The entry point `name`, as the function pointer type `F` the caller
    /// declares it to be.
    ///
    /// # Safety
    ///
    /// `F` is a function pointer type matching the symbol's C signature.
    unsafe fn get<F: Copy>(&self, name: &CStr) -> Option<F> {
        // SAFETY: a lookup in a handle `open` returned.
        let symbol = unsafe { libc::dlsym(self.0, name.as_ptr()) };
        if symbol.is_null() {
            return None;
        }
        debug_assert_eq!(size_of::<F>(), size_of::<*mut c_void>());
        // SAFETY: the caller names the symbol's own signature; a function
        // pointer is the size of the data pointer `dlsym` returns.
        Some(unsafe { std::mem::transmute_copy::<*mut c_void, F>(&symbol) })
    }
}

impl Gst {
    fn load() -> Option<Gst> {
        let gst = Library::open(c"libgstreamer-1.0.so.0")?;
        let app = Library::open(c"libgstapp-1.0.so.0")?;
        let gobject = Library::open(c"libgobject-2.0.so.0")?;
        let glib = Library::open(c"libglib-2.0.so.0")?;
        // SAFETY: each symbol is given its C signature from the GStreamer,
        // GObject and GLib headers.
        let table = unsafe {
            Gst {
                element_factory_make: gst.get(c"gst_element_factory_make")?,
                element_set_state: gst.get(c"gst_element_set_state")?,
                element_query_position: gst.get(c"gst_element_query_position")?,
                element_query_duration: gst.get(c"gst_element_query_duration")?,
                element_query: gst.get(c"gst_element_query")?,
                element_seek: gst.get(c"gst_element_seek")?,
                element_get_bus: gst.get(c"gst_element_get_bus")?,
                bus_pop_filtered: gst.get(c"gst_bus_pop_filtered")?,
                message_parse_error: gst.get(c"gst_message_parse_error")?,
                message_parse_buffering: gst.get(c"gst_message_parse_buffering")?,
                query_new_seeking: gst.get(c"gst_query_new_seeking")?,
                query_parse_seeking: gst.get(c"gst_query_parse_seeking")?,
                mini_object_unref: gst.get(c"gst_mini_object_unref")?,
                object_ref: gst.get(c"gst_object_ref")?,
                object_unref: gst.get(c"gst_object_unref")?,
                sample_get_buffer: gst.get(c"gst_sample_get_buffer")?,
                sample_get_caps: gst.get(c"gst_sample_get_caps")?,
                buffer_map: gst.get(c"gst_buffer_map")?,
                buffer_unmap: gst.get(c"gst_buffer_unmap")?,
                caps_from_string: gst.get(c"gst_caps_from_string")?,
                caps_get_structure: gst.get(c"gst_caps_get_structure")?,
                structure_get_int: gst.get(c"gst_structure_get_int")?,
                structure_get_fraction: gst.get(c"gst_structure_get_fraction")?,
                structure_get_string: gst.get(c"gst_structure_get_string")?,
                element_get_static_pad: gst.get(c"gst_element_get_static_pad")?,
                event_new_reconfigure: gst.get(c"gst_event_new_reconfigure")?,
                pad_push_event: gst.get(c"gst_pad_push_event")?,
                app_sink_set_caps: app.get(c"gst_app_sink_set_caps")?,
                app_sink_try_pull_sample: app.get(c"gst_app_sink_try_pull_sample")?,
                app_sink_try_pull_preroll: app.get(c"gst_app_sink_try_pull_preroll")?,
                object_set: gobject.get(c"g_object_set")?,
                object_get: gobject.get(c"g_object_get")?,
                error_free: glib.get(c"g_error_free")?,
                free: glib.get(c"g_free")?,
                main_context_iteration: glib.get(c"g_main_context_iteration")?,
            }
        };
        // SAFETY: the C signature of `gst_init_check`.
        let init: unsafe extern "C" fn(*mut c_int, *mut c_void, *mut *mut GError) -> c_int =
            unsafe { gst.get(c"gst_init_check")? };
        let mut error = ptr::null_mut();
        // SAFETY: no arguments to parse; an error, if any, is freed below.
        let started = unsafe { init(ptr::null_mut(), ptr::null_mut(), &mut error) } != 0;
        if !error.is_null() {
            // SAFETY: the error `gst_init_check` handed over.
            unsafe { (table.error_free)(error) };
        }
        started.then_some(table)
    }
}

/// A picture handed out, mapped until the next replaces it.
struct Shown {
    sample: *mut GstSample,
    buffer: *mut GstBuffer,
    map: MapInfo,
}

pub(crate) struct Player {
    gst: &'static Gst,
    pipeline: *mut GstElement,
    sink: *mut GstElement,
    bus: *mut GstBus,
    shown: Option<Shown>,
    /// The picture the player was ready with, not yet handed out.
    first: *mut GstSample,
    /// The caps last read, and the size and texels they gave.
    caps: *mut GstCaps,
    width: u32,
    height: u32,
    /// The pictures come as NV12 planes, encoded so; else as BGRA rows.
    planes: Option<Colors>,
    /// Set once a picture in planes is copied out as rows.
    copied: Cell<bool>,
    /// The sink was asked for BGRA alone.
    rows: bool,
    ready: bool,
    /// The source is live: it does not preroll, and plays to be ready.
    live: bool,
    failed: bool,
    wants_play: bool,
    looping: bool,
    ended: bool,
    /// Data is buffering; the pipeline is held paused until it is in.
    waiting: bool,
    rate: f32,
    /// A seek asked before the pipeline could take it.
    seek: Option<Duration>,
    /// Where a seek is going, until the pipeline is there.
    sought: Option<Duration>,
    /// Where the pipeline last said it was.
    at: Duration,
    /// The file bytes were written to, removed once the pipeline is gone.
    spilled: Option<Spilled>,
}

impl Player {
    pub(crate) fn open(source: &VideoSource) -> Result<Player, VideoError> {
        let gst = gst().ok_or(VideoError::Unsupported)?;
        let (uri, spilled) = match source {
            VideoSource::File(path) => (file_uri(path), None),
            VideoSource::Url(url) => (url.clone(), None),
            VideoSource::Bytes(bytes) => spill(bytes)?,
            VideoSource::Static(bytes) => spill(bytes)?,
        };
        let uri = CString::new(uri).map_err(|_| VideoError::Open("a NUL in the URL".to_owned()))?;
        // SAFETY: GStreamer calls on objects made here; each property is
        // set with the C type it is declared with (string, object pointer,
        // flags and booleans as int, caps as a pointer), the list ending in
        // NULL. The player owns the pipeline, its own reference to the sink
        // and the bus, released in `drop`.
        unsafe {
            let pipeline = (gst.element_factory_make)(c"playbin".as_ptr(), ptr::null());
            if pipeline.is_null() {
                return Err(VideoError::Unsupported);
            }
            let sink = (gst.element_factory_make)(c"appsink".as_ptr(), ptr::null());
            if sink.is_null() {
                (gst.object_unref)(pipeline);
                return Err(VideoError::Unsupported);
            }
            (gst.object_ref)(sink);
            let caps = (gst.caps_from_string)(c"video/x-raw,format={ NV12, BGRA }".as_ptr());
            (gst.app_sink_set_caps)(sink, caps);
            (gst.mini_object_unref)(caps);
            // The sink keeps the newest picture due, in step with the clock,
            // and ends without waiting for it to be taken: a video no node
            // shows is not pulled from.
            (gst.object_set)(
                sink,
                c"max-buffers".as_ptr(),
                1 as c_uint,
                c"drop".as_ptr(),
                1 as c_int,
                c"sync".as_ptr(),
                1 as c_int,
                c"qos".as_ptr(),
                1 as c_int,
                c"enable-last-sample".as_ptr(),
                0 as c_int,
                c"wait-on-eos".as_ptr(),
                0 as c_int,
                ptr::null::<c_char>(),
            );
            (gst.object_set)(
                pipeline,
                c"uri".as_ptr(),
                uri.as_ptr(),
                c"video-sink".as_ptr(),
                sink,
                c"flags".as_ptr(),
                PLAY_FLAGS,
                ptr::null::<c_char>(),
            );
            let bus = (gst.element_get_bus)(pipeline);
            let live = (gst.element_set_state)(pipeline, STATE_PAUSED) == STATE_CHANGE_NO_PREROLL;
            if live {
                // A live source shows nothing until it plays.
                (gst.element_set_state)(pipeline, STATE_PLAYING);
            }
            Ok(Player {
                gst,
                pipeline,
                sink,
                bus,
                shown: None,
                first: ptr::null_mut(),
                caps: ptr::null_mut(),
                width: 0,
                height: 0,
                planes: None,
                copied: Cell::new(false),
                rows: false,
                ready: false,
                live,
                failed: false,
                wants_play: false,
                looping: false,
                ended: false,
                waiting: false,
                rate: 1.0,
                seek: None,
                sought: None,
                at: Duration::ZERO,
                spilled,
            })
        }
    }

    pub(crate) fn poll(&mut self) -> Option<VideoEvent> {
        if self.failed {
            return None;
        }
        let gst = self.gst;
        // SAFETY: the default main context is iterated without blocking,
        // for the sources that dispatch through it; one that another thread
        // owns is left to it.
        unsafe { while (gst.main_context_iteration)(ptr::null_mut(), 0) != 0 {} }
        if self.live && !self.ready {
            // SAFETY: a pull from the player's sink, not waiting.
            let sample = unsafe { (gst.app_sink_try_pull_sample)(self.sink, 0) };
            if !sample.is_null() {
                return Some(self.become_ready(sample));
            }
        }
        loop {
            let mask = MESSAGE_EOS | MESSAGE_ERROR | MESSAGE_BUFFERING | MESSAGE_ASYNC_DONE;
            // SAFETY: a pop from the player's bus, not waiting; the message
            // is the caller's to release, its type read from its head.
            let message = unsafe { (gst.bus_pop_filtered)(self.bus, mask) };
            if message.is_null() {
                return None;
            }
            // SAFETY: as above.
            let kind = unsafe { (*message).kind };
            let event = match kind {
                MESSAGE_ASYNC_DONE => {
                    self.sought = None;
                    if self.ready || self.live {
                        None
                    } else {
                        // SAFETY: a pull of the prerolled picture, not
                        // waiting; a pipeline without video has none.
                        let sample = unsafe { (gst.app_sink_try_pull_preroll)(self.sink, 0) };
                        Some(self.become_ready(sample))
                    }
                }
                MESSAGE_EOS => self.end(),
                MESSAGE_BUFFERING => {
                    let mut percent = 100;
                    // SAFETY: a buffering message, parsed into a local.
                    unsafe { (gst.message_parse_buffering)(message, &mut percent) };
                    self.buffering(percent)
                }
                MESSAGE_ERROR => {
                    // SAFETY: an error message, parsed into its error.
                    let why = unsafe { error_text(gst, message) };
                    self.failed = true;
                    Some(VideoEvent::Failed(if self.ready {
                        VideoError::Play(why)
                    } else {
                        VideoError::Open(why)
                    }))
                }
                _ => None,
            };
            // SAFETY: the popped message, released once.
            unsafe { (gst.mini_object_unref)(message.cast()) };
            if event.is_some() {
                return event;
            }
        }
    }

    /// Ready with `sample`, the first picture (null without video): what
    /// is known of the video, and what was asked before now applied.
    fn become_ready(&mut self, sample: *mut GstSample) -> VideoEvent {
        let gst = self.gst;
        self.ready = true;
        self.first = sample;
        let mut frame_rate = None;
        if !sample.is_null() {
            // SAFETY: the sample's caps, borrowed for the reads.
            unsafe {
                let caps = (gst.sample_get_caps)(sample);
                self.read_caps(caps);
                let structure = (gst.caps_get_structure)(caps, 0);
                let (mut num, mut den) = (0, 0);
                if !structure.is_null()
                    && (gst.structure_get_fraction)(
                        structure,
                        c"framerate".as_ptr(),
                        &mut num,
                        &mut den,
                    ) != 0
                    && num > 0
                    && den > 0
                {
                    frame_rate = Some(num as f32 / den as f32);
                }
            }
        }
        let mut length = -1i64;
        // SAFETY: queries of the player's pipeline into locals; the query
        // object is released once.
        let (duration, seekable) = unsafe {
            (gst.element_query_duration)(self.pipeline, FORMAT_TIME, &mut length);
            let duration = (length > 0).then(|| Duration::from_nanos(length as u64));
            let query = (gst.query_new_seeking)(FORMAT_TIME);
            let mut seekable = 0;
            if !query.is_null() {
                if (gst.element_query)(self.pipeline, query) != 0 {
                    let (mut format, mut start, mut end) = (0, 0, 0);
                    (gst.query_parse_seeking)(
                        query,
                        &mut format,
                        &mut seekable,
                        &mut start,
                        &mut end,
                    );
                }
                (gst.mini_object_unref)(query);
            }
            (duration, seekable != 0 && duration.is_some())
        };
        let mut audio: c_int = 0;
        // SAFETY: `playbin`'s `n-audio` is an int, read into a local; the
        // list ends in NULL.
        unsafe {
            (gst.object_get)(
                self.pipeline,
                c"n-audio".as_ptr(),
                &mut audio as *mut c_int,
                ptr::null::<c_char>(),
            );
        }
        if let Some(time) = self.seek.take() {
            self.seek_to(time);
        } else if self.rate != 1.0 {
            self.change_rate();
        }
        if self.live && !self.wants_play {
            self.set_state(STATE_PAUSED);
        } else if self.wants_play && !self.live {
            self.set_state(STATE_PLAYING);
        }
        VideoEvent::Ready(VideoInfo {
            width: self.width,
            height: self.height,
            duration,
            frame_rate,
            audio: audio > 0,
            seekable,
        })
    }

    fn end(&mut self) -> Option<VideoEvent> {
        if self.looping {
            self.seek_to(Duration::ZERO);
            return None;
        }
        self.ended = true;
        self.at = self.position();
        self.wants_play = false;
        self.set_state(STATE_PAUSED);
        Some(VideoEvent::Ended)
    }

    fn buffering(&mut self, percent: c_int) -> Option<VideoEvent> {
        if self.live {
            return None;
        }
        if percent < 100 && !self.waiting {
            self.waiting = true;
            if self.wants_play {
                self.set_state(STATE_PAUSED);
            }
            Some(VideoEvent::Waiting(true))
        } else if percent >= 100 && self.waiting {
            self.waiting = false;
            if self.wants_play {
                self.set_state(STATE_PLAYING);
            }
            Some(VideoEvent::Waiting(false))
        } else {
            None
        }
    }

    pub(crate) fn frame(&mut self) -> Option<Frame<'_>> {
        if !self.ready || self.failed {
            return None;
        }
        if self.copied.get() && !self.rows {
            self.want_rows();
        }
        let gst = self.gst;
        let mut sample = std::mem::replace(&mut self.first, ptr::null_mut());
        // SAFETY: pulls from the player's sink, not waiting; every sample
        // but the newest is released, the newest owned until shown or
        // released.
        unsafe {
            let mut take = |next: *mut GstSample| {
                if next.is_null() {
                    return false;
                }
                if !sample.is_null() {
                    (gst.mini_object_unref)(sample);
                }
                sample = next;
                true
            };
            // Paused, the picture is the preroll, a seek's too, which a
            // pull of samples would pass over.
            if !self.wants_play || self.waiting {
                take((gst.app_sink_try_pull_preroll)(self.sink, 0));
            }
            while take((gst.app_sink_try_pull_sample)(self.sink, 0)) {}
        }
        if sample.is_null() {
            return None;
        }
        // SAFETY: the sample is owned here; its buffer and caps are
        // borrowed from it. The buffer is mapped for reading for as long as
        // the sample is kept, and unmapped before it is released.
        unsafe {
            let buffer = (gst.sample_get_buffer)(sample);
            if buffer.is_null() || self.shown.as_ref().is_some_and(|s| s.buffer == buffer) {
                // Nothing, or the picture already out.
                (gst.mini_object_unref)(sample);
                return None;
            }
            self.read_caps((gst.sample_get_caps)(sample));
            let mut map = MapInfo {
                memory: ptr::null_mut(),
                flags: 0,
                data: ptr::null_mut(),
                size: 0,
                maxsize: 0,
                user_data: [ptr::null_mut(); 4],
                reserved: [ptr::null_mut(); 4],
            };
            let (width, height) = (self.width as usize, self.height as usize);
            let layout = match self.planes {
                Some(_) => nv12_layout(width, height),
                None => Layout {
                    stride: width * 4,
                    chroma: 0,
                    size: width * 4 * height,
                },
            };
            if self.width == 0
                || (gst.buffer_map)(buffer, &mut map, MAP_READ) == 0
                || map.data.is_null()
                || map.size < layout.size
            {
                if !map.data.is_null() {
                    (gst.buffer_unmap)(buffer, &mut map);
                }
                (gst.mini_object_unref)(sample);
                return None;
            }
            self.release_shown();
            let pts = (*buffer.cast::<Buffer>()).pts;
            let time = if pts == CLOCK_TIME_NONE {
                self.at
            } else {
                Duration::from_nanos(pts)
            };
            let shown = self.shown.insert(Shown {
                sample,
                buffer,
                map,
            });
            let bytes = std::slice::from_raw_parts(shown.map.data, shown.map.size);
            let Some(colors) = self.planes else {
                return Some(Frame::bgra(
                    self.width,
                    self.height,
                    bytes,
                    layout.stride,
                    time,
                ));
            };
            let picture = Nv12 {
                width: self.width,
                height: self.height,
                y: bytes,
                y_stride: layout.stride,
                uv: &bytes[layout.chroma..],
                uv_stride: layout.stride,
                matrix: colors.matrix,
                full_range: colors.full_range,
                left_chroma: colors.left_chroma,
            };
            Some(Frame::nv12_watched(picture, &self.copied, time))
        }
    }

    /// The size `caps` give, read again only when they change.
    ///
    /// # Safety
    ///
    /// `caps` is null or caps a sample of this player's holds.
    unsafe fn read_caps(&mut self, caps: *mut GstCaps) {
        if caps.is_null() || caps == self.caps {
            return;
        }
        let gst = self.gst;
        // SAFETY: the caller's caps, read into locals.
        unsafe {
            let structure = (gst.caps_get_structure)(caps, 0);
            let (mut width, mut height) = (0, 0);
            if structure.is_null()
                || (gst.structure_get_int)(structure, c"width".as_ptr(), &mut width) == 0
                || (gst.structure_get_int)(structure, c"height".as_ptr(), &mut height) == 0
            {
                return;
            }
            let text = |name: &CStr| {
                let value = (gst.structure_get_string)(structure, name.as_ptr());
                (!value.is_null()).then(|| CStr::from_ptr(value).to_bytes())
            };
            self.caps = caps;
            self.width = width.max(0) as u32;
            self.height = height.max(0) as u32;
            self.planes = (text(c"format") == Some(b"NV12"))
                .then(|| colors(text(c"colorimetry"), text(c"chroma-site"), self.height));
        }
    }

    /// Asks the sink for BGRA rows alone, which GStreamer's converter
    /// writes from the next picture on.
    fn want_rows(&mut self) {
        self.rows = true;
        let gst = self.gst;
        // SAFETY: calls on the player's sink; the caps and the pad are
        // released once, the event handed to the pad, which takes it.
        unsafe {
            let caps = (gst.caps_from_string)(c"video/x-raw,format=BGRA".as_ptr());
            (gst.app_sink_set_caps)(self.sink, caps);
            (gst.mini_object_unref)(caps);
            let pad = (gst.element_get_static_pad)(self.sink, c"sink".as_ptr());
            if !pad.is_null() {
                (gst.pad_push_event)(pad, (gst.event_new_reconfigure)());
                (gst.object_unref)(pad);
            }
        }
    }

    fn release_shown(&mut self) {
        if let Some(mut shown) = self.shown.take() {
            // SAFETY: the picture's buffer is unmapped once, then its sample
            // released once.
            unsafe {
                (self.gst.buffer_unmap)(shown.buffer, &mut shown.map);
                (self.gst.mini_object_unref)(shown.sample);
            }
        }
    }

    pub(crate) fn play(&mut self) {
        if std::mem::take(&mut self.ended) {
            self.seek(Duration::ZERO);
        }
        self.wants_play = true;
        if self.ready && !self.waiting {
            self.set_state(STATE_PLAYING);
        }
    }

    pub(crate) fn pause(&mut self) {
        self.wants_play = false;
        if self.ready {
            self.at = self.position();
            self.set_state(STATE_PAUSED);
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
        if !self.first.is_null() {
            // SAFETY: the first picture, passed over, released once.
            unsafe { (self.gst.mini_object_unref)(self.first) };
            self.first = ptr::null_mut();
        }
        self.sought = Some(time);
        self.at = time;
        let to = i64::try_from(time.as_nanos()).unwrap_or(i64::MAX);
        // SAFETY: a seek of the player's pipeline.
        unsafe {
            (self.gst.element_seek)(
                self.pipeline,
                f64::from(self.rate),
                FORMAT_TIME,
                SEEK_FLAG_FLUSH | SEEK_FLAG_ACCURATE,
                SEEK_TYPE_SET,
                to,
                SEEK_TYPE_NONE,
                -1,
            );
        }
    }

    pub(crate) fn set_rate(&mut self, rate: f32) {
        let rate = if rate.is_finite() && rate > 0.0 {
            rate
        } else {
            1.0
        };
        if rate == self.rate {
            return;
        }
        self.rate = rate;
        if self.ready {
            self.change_rate();
        }
    }

    /// Plays on at the rate asked: in place where the pipeline can, else
    /// by a seek to where it is.
    fn change_rate(&mut self) {
        // SAFETY: a seek of the player's pipeline that only changes rate.
        let instant = unsafe {
            (self.gst.element_seek)(
                self.pipeline,
                f64::from(self.rate),
                FORMAT_TIME,
                SEEK_FLAG_INSTANT_RATE_CHANGE,
                SEEK_TYPE_NONE,
                -1,
                SEEK_TYPE_NONE,
                -1,
            )
        } != 0;
        if !instant {
            let at = self.position();
            self.seek_to(at);
        }
    }

    pub(crate) fn set_volume(&mut self, volume: f32) {
        // SAFETY: `playbin`'s `volume` is a double; the list ends in NULL.
        unsafe {
            (self.gst.object_set)(
                self.pipeline,
                c"volume".as_ptr(),
                f64::from(volume.clamp(0.0, 1.0)),
                ptr::null::<c_char>(),
            );
        }
    }

    pub(crate) fn set_muted(&mut self, muted: bool) {
        // SAFETY: `playbin`'s `mute` is a boolean; the list ends in NULL.
        unsafe {
            (self.gst.object_set)(
                self.pipeline,
                c"mute".as_ptr(),
                c_int::from(muted),
                ptr::null::<c_char>(),
            );
        }
    }

    pub(crate) fn set_looping(&mut self, looping: bool) {
        self.looping = looping;
    }

    pub(crate) fn position(&self) -> Duration {
        if let Some(time) = self.sought.or(self.seek) {
            return time;
        }
        if !self.ready || !self.wants_play {
            return self.at;
        }
        let mut at = -1i64;
        // SAFETY: a position query of the player's pipeline into a local.
        let known =
            unsafe { (self.gst.element_query_position)(self.pipeline, FORMAT_TIME, &mut at) } != 0;
        if known && at >= 0 {
            Duration::from_nanos(at as u64)
        } else {
            self.at
        }
    }

    pub(crate) fn is_playing(&self) -> bool {
        self.wants_play && !self.ended
    }

    fn set_state(&self, state: c_uint) {
        // SAFETY: a state change of the player's pipeline, which completes
        // on GStreamer's threads.
        unsafe { (self.gst.element_set_state)(self.pipeline, state) };
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.release_shown();
        let gst = self.gst;
        if !self.first.is_null() {
            // SAFETY: the first picture, never handed out, released once.
            unsafe { (gst.mini_object_unref)(self.first) };
        }
        // Stopping joins GStreamer's threads, which may wait on the network:
        // it happens on a thread of its own, which then frees the pipeline
        // and the file it read.
        let parts = Teardown {
            gst,
            pipeline: self.pipeline,
            sink: self.sink,
            bus: self.bus,
            spilled: self.spilled.take(),
        };
        let _ = std::thread::Builder::new()
            .name("viso-video-close".to_owned())
            .spawn(move || drop(parts));
    }
}

/// What a closed player leaves to free, freed when dropped: on a thread of
/// its own, or here where none starts.
struct Teardown {
    gst: &'static Gst,
    pipeline: *mut GstElement,
    sink: *mut GstElement,
    bus: *mut GstBus,
    /// Dropped after the pipeline stops reading it.
    #[allow(dead_code)]
    spilled: Option<Spilled>,
}

// SAFETY: the objects are GStreamer's, safe to stop and release from any
// thread, and nothing else holds them once the player is gone.
unsafe impl Send for Teardown {}

impl Drop for Teardown {
    fn drop(&mut self) {
        // SAFETY: the pipeline is stopped, then each reference the player
        // held released once.
        unsafe {
            (self.gst.element_set_state)(self.pipeline, STATE_NULL);
            (self.gst.object_unref)(self.bus);
            (self.gst.object_unref)(self.sink);
            (self.gst.object_unref)(self.pipeline);
        }
    }
}

/// The text of an error message.
///
/// # Safety
///
/// `message` is a live `GstMessage` of type error.
unsafe fn error_text(gst: &Gst, message: *mut Message) -> String {
    let mut error = ptr::null_mut::<GError>();
    let mut debug = ptr::null_mut::<c_char>();
    // SAFETY: the caller's error message; the error and the debug text it
    // hands over are freed once.
    unsafe {
        (gst.message_parse_error)(message, &mut error, &mut debug);
        let text = if error.is_null() || (*error).message.is_null() {
            "GStreamer error".to_owned()
        } else {
            CStr::from_ptr((*error).message)
                .to_string_lossy()
                .into_owned()
        };
        if !error.is_null() {
            (gst.error_free)(error);
        }
        if !debug.is_null() {
            (gst.free)(debug.cast());
        }
        text
    }
}

/// How a picture in NV12 planes was encoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Colors {
    matrix: YuvMatrix,
    full_range: bool,
    left_chroma: bool,
}

/// The encoding caps' `colorimetry` and `chroma-site` name, for a picture
/// `height` rows tall where they name none.
fn colors(colorimetry: Option<&[u8]>, site: Option<&[u8]>, height: u32) -> Colors {
    let mut out = Colors {
        matrix: yuv::matrix_for_height(height),
        full_range: false,
        // Unsaid, as H.264 and later codecs site it.
        left_chroma: !matches!(site, Some(b"jpeg" | b"none")),
    };
    match colorimetry {
        Some(b"bt601" | b"smpte170m") => out.matrix = YuvMatrix::Bt601,
        Some(b"bt709" | b"smpte240m") => out.matrix = YuvMatrix::Bt709,
        Some(b"bt2020" | b"bt2020-10" | b"bt2100-pq" | b"bt2100-hlg") => {
            out.matrix = YuvMatrix::Bt2020;
        }
        Some(b"jpeg") => {
            out.matrix = YuvMatrix::Bt601;
            out.full_range = true;
        }
        // Range, matrix, transfer and primaries by number.
        Some(fields) => {
            let mut parts = fields.split(|&b| b == b':');
            let (range, matrix) = (parts.next(), parts.next());
            out.full_range = range == Some(b"1");
            match matrix {
                Some(b"3" | b"5") => out.matrix = YuvMatrix::Bt709,
                Some(b"4") => out.matrix = YuvMatrix::Bt601,
                Some(b"6") => out.matrix = YuvMatrix::Bt2020,
                _ => {}
            }
        }
        None => {}
    }
    out
}

/// Where a picture's texels lie in its buffer: the bytes between rows, the
/// offset of the chroma plane, and the bytes the whole takes.
struct Layout {
    stride: usize,
    chroma: usize,
    size: usize,
}

/// The layout GStreamer gives an NV12 picture of `width` × `height` with no
/// metadata of its own: rows rounded up to four bytes, the chroma plane after
/// an even number of luma rows.
fn nv12_layout(width: usize, height: usize) -> Layout {
    let stride = width.next_multiple_of(4);
    let rows = height.next_multiple_of(2);
    let chroma = stride * rows;
    Layout {
        stride,
        chroma,
        size: chroma + stride * rows / 2,
    }
}

/// `bytes` in a file of their own, and its URI.
fn spill(bytes: &[u8]) -> Result<(String, Option<Spilled>), VideoError> {
    let spilled = Spilled::new(bytes).map_err(|e| VideoError::Open(e.to_string()))?;
    Ok((file_uri(spilled.path()), Some(spilled)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caps_name_how_planes_were_encoded() {
        let c = colors(Some(b"bt709"), Some(b"mpeg2"), 480);
        assert_eq!(
            c,
            Colors {
                matrix: YuvMatrix::Bt709,
                full_range: false,
                left_chroma: true,
            }
        );
        let c = colors(Some(b"1:4:0:0"), Some(b"jpeg"), 1080);
        assert_eq!(
            (c.matrix, c.full_range, c.left_chroma),
            (YuvMatrix::Bt601, true, false)
        );
        assert_eq!(
            colors(Some(b"bt2100-pq"), None, 480).matrix,
            YuvMatrix::Bt2020
        );
        assert_eq!(colors(None, None, 1080).matrix, YuvMatrix::Bt709);
        assert_eq!(colors(None, None, 480).matrix, YuvMatrix::Bt601);
    }

    #[test]
    fn an_nv12_buffer_without_metadata_lies_as_gstreamer_lays_it() {
        let l = nv12_layout(64, 48);
        assert_eq!((l.stride, l.chroma, l.size), (64, 64 * 48, 64 * 48 * 3 / 2));
        let l = nv12_layout(3, 3);
        assert_eq!((l.stride, l.chroma, l.size), (4, 16, 24));
    }
}
