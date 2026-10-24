//! Linux and the BSDs: GStreamer's `playbin`, loaded at run time, its
//! pictures taken from an `appsink` as NV12 planes — in a dma-buf the GPU
//! reads where it lies, or in memory at hand it uploads — which the GPU
//! converts, or as BGRA rows once a copy is wanted.
//!
//! The libraries are opened on first use, so a program runs where GStreamer
//! is not installed and only its videos fail, as `Unsupported`. `playbin`
//! picks the demuxer and decoder (hardware where the system ranks one
//! first), hands decoded Y′CbCr over as NV12 — as decoders write it, or
//! reinterleaved, never converted to RGB on the CPU — and keeps sound and
//! picture in step on its own threads. A hardware decoder exporting linear
//! dma-bufs hands them over as they are; the sink takes buffers laid out as
//! their video metadata says, padded rows and all, so none is copied to fit.
//! A dma-buf a backend does not import is mapped, and the sink then asks
//! for memory at hand; a picture copied out as rows is converted here, and
//! the sink then asks for BGRA, which GStreamer's own converter writes
//! faster, from the next on; everything this side does is on the loop's thread and never
//! waits. The bus is read when polled; the sink holds the picture due now,
//! which is handed out mapped where GStreamer wrote it, with no copy, until
//! the next replaces it.

use std::cell::{Cell, RefCell};
use std::ffi::{CStr, CString, c_char, c_int, c_uint, c_ulong, c_void};
use std::ptr;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use viso_handle::{ExternalHolds, ExternalImage, Nv12, YuvMatrix};

use super::camera::{self, CameraFacing, CameraInfo, CameraRequest};
use super::{
    Frame, PixelLayout, Spilled, VideoError, VideoEvent, VideoInfo, VideoSource, file_uri, yuv,
};

type GstElement = c_void;
type GstBus = c_void;
type GstSample = c_void;
type GstBuffer = c_void;
type GstCaps = c_void;
type GstStructure = c_void;
type GstQuery = c_void;
type GstPad = c_void;
type GstEvent = c_void;
type GstMemory = c_void;
type GstDevice = c_void;
type GstDeviceMonitor = c_void;
type GstPadProbeInfo = c_void;

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

/// `GList`.
#[repr(C)]
struct List {
    data: *mut c_void,
    next: *mut List,
    prev: *mut List,
}

/// The head of `GstQuery`.
#[repr(C)]
struct Query {
    mini_object: MiniObject,
    kind: c_uint,
}

/// `GstMemory`.
#[repr(C)]
struct Memory {
    mini_object: MiniObject,
    allocator: *mut c_void,
    parent: *mut c_void,
    maxsize: usize,
    align: usize,
    offset: usize,
    size: usize,
}

/// The head of `GstVideoMeta`: where a buffer's planes lie.
#[repr(C)]
struct VideoMeta {
    flags: c_uint,
    info: *const c_void,
    buffer: *mut c_void,
    frame_flags: c_uint,
    format: c_int,
    id: c_int,
    width: c_uint,
    height: c_uint,
    n_planes: c_uint,
    offset: [usize; 4],
    stride: [c_int; 4],
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
const STATE_READY: c_uint = 2;
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

/// `GST_QUERY_ALLOCATION`: number 140, downstream and serialized.
const QUERY_ALLOCATION: c_uint = (140 << 8) | (1 << 1) | (1 << 2);
/// `GST_PAD_PROBE_TYPE_QUERY_DOWNSTREAM | GST_PAD_PROBE_TYPE_PUSH`: a query
/// on its way in, before the pad answers it.
const PROBE_QUERY_IN: c_uint = (1 << 9) | (1 << 12);
const PROBE_OK: c_int = 1;
const PROBE_HANDLED: c_int = 4;

/// What the sink takes: linear NV12 dma-bufs where the libraries to read
/// them are, NV12 or BGRA in memory at hand, BGRA alone once rows are
/// wanted.
const CAPS_DMA_BUF: &CStr = c"video/x-raw(memory:DMABuf),format=DMA_DRM,drm-format=NV12; \
    video/x-raw(memory:DMABuf),format=NV12; video/x-raw,format={ NV12, BGRA }";
const CAPS_AT_HAND: &CStr = c"video/x-raw,format={ NV12, BGRA }";
const CAPS_ROWS: &CStr = c"video/x-raw,format=BGRA";

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
    pad_add_probe: unsafe extern "C" fn(
        *mut GstPad,
        c_uint,
        unsafe extern "C" fn(*mut GstPad, *mut GstPadProbeInfo, *mut c_void) -> c_int,
        *mut c_void,
        *mut c_void,
    ) -> c_ulong,
    pad_probe_info_get_query: unsafe extern "C" fn(*mut GstPadProbeInfo) -> *mut GstQuery,
    query_add_allocation_meta: unsafe extern "C" fn(*mut GstQuery, usize, *const GstStructure),
    buffer_n_memory: unsafe extern "C" fn(*mut GstBuffer) -> c_uint,
    buffer_peek_memory: unsafe extern "C" fn(*mut GstBuffer, c_uint) -> *mut GstMemory,
    mini_object_get_qdata: unsafe extern "C" fn(*mut c_void, u32) -> *mut c_void,
    mini_object_set_qdata:
        unsafe extern "C" fn(*mut c_void, u32, *mut c_void, unsafe extern "C" fn(*mut c_void)),
    device_monitor_new: unsafe extern "C" fn() -> *mut GstDeviceMonitor,
    device_monitor_add_filter:
        unsafe extern "C" fn(*mut GstDeviceMonitor, *const c_char, *mut GstCaps) -> c_uint,
    device_monitor_get_devices: unsafe extern "C" fn(*mut GstDeviceMonitor) -> *mut List,
    device_get_display_name: unsafe extern "C" fn(*mut GstDevice) -> *mut c_char,
    device_get_properties: unsafe extern "C" fn(*mut GstDevice) -> *mut GstStructure,
    device_create_element: unsafe extern "C" fn(*mut GstDevice, *const c_char) -> *mut GstElement,
    structure_free: unsafe extern "C" fn(*mut GstStructure),
    pipeline_new: unsafe extern "C" fn(*const c_char) -> *mut GstElement,
    bin_add: unsafe extern "C" fn(*mut GstElement, *mut GstElement) -> c_int,
    pad_query_caps: unsafe extern "C" fn(*mut GstPad, *mut GstCaps) -> *mut GstCaps,
    caps_get_size: unsafe extern "C" fn(*mut GstCaps) -> c_uint,
    structure_copy: unsafe extern "C" fn(*mut GstStructure) -> *mut GstStructure,
    structure_get_name: unsafe extern "C" fn(*mut GstStructure) -> *const c_char,
    structure_fixate_field_nearest_int:
        unsafe extern "C" fn(*mut GstStructure, *const c_char, c_int) -> c_int,
    structure_fixate_field_nearest_fraction:
        unsafe extern "C" fn(*mut GstStructure, *const c_char, c_int, c_int) -> c_int,
    element_link: unsafe extern "C" fn(*mut GstElement, *mut GstElement) -> c_int,
    list_free: unsafe extern "C" fn(*mut List),
    /// The quark the holds of a dma-buf's memory are kept under on it.
    holds_quark: u32,
    /// What reads dma-bufs and buffers' video metadata, where installed.
    video: Option<VideoLibs>,
    app_sink_set_caps: unsafe extern "C" fn(*mut GstElement, *const GstCaps),
    app_sink_try_pull_sample: unsafe extern "C" fn(*mut GstElement, u64) -> *mut GstSample,
    app_sink_try_pull_preroll: unsafe extern "C" fn(*mut GstElement, u64) -> *mut GstSample,
    object_set: unsafe extern "C" fn(*mut c_void, *const c_char, ...),
    object_get: unsafe extern "C" fn(*mut c_void, *const c_char, ...),
    error_free: unsafe extern "C" fn(*mut GError),
    free: unsafe extern "C" fn(*mut c_void),
    main_context_iteration: unsafe extern "C" fn(*mut c_void, c_int) -> c_int,
}

/// The entry points of `libgstallocators` and `libgstvideo` used.
struct VideoLibs {
    is_dmabuf_memory: unsafe extern "C" fn(*mut GstMemory) -> c_int,
    dmabuf_memory_get_fd: unsafe extern "C" fn(*mut GstMemory) -> c_int,
    buffer_get_video_meta: unsafe extern "C" fn(*mut GstBuffer) -> *mut VideoMeta,
    /// `GstVideoMeta`'s API type.
    video_meta_api: usize,
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
        // SAFETY: the C signature of `g_quark_from_static_string`; the
        // string is static.
        let holds_quark = unsafe {
            let quark: unsafe extern "C" fn(*const c_char) -> u32 =
                glib.get(c"g_quark_from_static_string")?;
            quark(c"viso-holds".as_ptr())
        };
        // SAFETY: each symbol is given its C signature from the GStreamer,
        // GObject and GLib headers.
        let mut table = unsafe {
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
                pad_add_probe: gst.get(c"gst_pad_add_probe")?,
                pad_probe_info_get_query: gst.get(c"gst_pad_probe_info_get_query")?,
                query_add_allocation_meta: gst.get(c"gst_query_add_allocation_meta")?,
                buffer_n_memory: gst.get(c"gst_buffer_n_memory")?,
                buffer_peek_memory: gst.get(c"gst_buffer_peek_memory")?,
                mini_object_get_qdata: gst.get(c"gst_mini_object_get_qdata")?,
                mini_object_set_qdata: gst.get(c"gst_mini_object_set_qdata")?,
                device_monitor_new: gst.get(c"gst_device_monitor_new")?,
                device_monitor_add_filter: gst.get(c"gst_device_monitor_add_filter")?,
                device_monitor_get_devices: gst.get(c"gst_device_monitor_get_devices")?,
                device_get_display_name: gst.get(c"gst_device_get_display_name")?,
                device_get_properties: gst.get(c"gst_device_get_properties")?,
                device_create_element: gst.get(c"gst_device_create_element")?,
                structure_free: gst.get(c"gst_structure_free")?,
                pipeline_new: gst.get(c"gst_pipeline_new")?,
                bin_add: gst.get(c"gst_bin_add")?,
                pad_query_caps: gst.get(c"gst_pad_query_caps")?,
                caps_get_size: gst.get(c"gst_caps_get_size")?,
                structure_copy: gst.get(c"gst_structure_copy")?,
                structure_get_name: gst.get(c"gst_structure_get_name")?,
                structure_fixate_field_nearest_int: gst
                    .get(c"gst_structure_fixate_field_nearest_int")?,
                structure_fixate_field_nearest_fraction: gst
                    .get(c"gst_structure_fixate_field_nearest_fraction")?,
                element_link: gst.get(c"gst_element_link")?,
                list_free: glib.get(c"g_list_free")?,
                holds_quark,
                video: None,
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
        if !started {
            return None;
        }
        // SAFETY: each symbol is given its C signature from the GStreamer
        // headers; the API type is asked for once GStreamer is started.
        table.video = unsafe {
            (|| {
                let allocators = Library::open(c"libgstallocators-1.0.so.0")?;
                let video = Library::open(c"libgstvideo-1.0.so.0")?;
                let api: unsafe extern "C" fn() -> usize =
                    video.get(c"gst_video_meta_api_get_type")?;
                Some(VideoLibs {
                    is_dmabuf_memory: allocators.get(c"gst_is_dmabuf_memory")?,
                    dmabuf_memory_get_fd: allocators.get(c"gst_dmabuf_memory_get_fd")?,
                    buffer_get_video_meta: video.get(c"gst_buffer_get_video_meta")?,
                    video_meta_api: api(),
                })
            })()
        };
        Some(table)
    }
}

/// A picture handed out, until the next replaces it: mapped, or a dma-buf
/// mapped only once asked.
struct Shown {
    sample: *mut GstSample,
    buffer: *mut GstBuffer,
    map: RefCell<MapInfo>,
    mapped: Cell<bool>,
    /// The holds of the dma-buf it lies in.
    holds: Option<Arc<ExternalHolds>>,
}

impl Shown {
    /// The bytes of its buffer, mapped if they are not yet; `None` where it
    /// cannot be mapped.
    ///
    /// # Safety
    ///
    /// The buffer is the sample's, alive while it is; the bytes are valid
    /// until it is unmapped, which only the player's next picture does.
    unsafe fn bytes<'a>(&'a self, gst: &Gst) -> Option<&'a [u8]> {
        let mut map = self.map.borrow_mut();
        // SAFETY: the caller's contract; mapped once, for reading.
        unsafe {
            if !self.mapped.get() {
                if (gst.buffer_map)(self.buffer, &mut *map, MAP_READ) == 0 || map.data.is_null() {
                    return None;
                }
                self.mapped.set(true);
            }
            Some(std::slice::from_raw_parts(map.data, map.size))
        }
    }
}

/// An empty mapping, filled by `gst_buffer_map`.
fn no_map() -> MapInfo {
    MapInfo {
        memory: ptr::null_mut(),
        flags: 0,
        data: ptr::null_mut(),
        size: 0,
        maxsize: 0,
        user_data: [ptr::null_mut(); 4],
        reserved: [ptr::null_mut(); 4],
    }
}

/// A picture in a dma-buf, named for a GPU to read where it lies, mapped
/// only when asked for its planes or rows.
#[derive(Clone, Copy)]
pub(crate) struct Picture<'a> {
    gst: &'static Gst,
    shown: &'a Shown,
    image: ExternalImage,
    layout: Layout,
    colors: Colors,
    width: u32,
    height: u32,
    /// Set once its planes are asked for: the backend does not import it.
    mapped: &'a Cell<bool>,
    /// Set once it is copied out as rows.
    copied: &'a Cell<bool>,
    rows: &'a RefCell<Vec<u8>>,
}

impl std::fmt::Debug for Picture<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Picture")
            .field("image", &self.image)
            .finish()
    }
}

impl<'a> Picture<'a> {
    pub(crate) fn external(&self, _: u32, _: u32) -> Option<ExternalImage> {
        Some(self.image)
    }

    /// Its planes, mapped.
    pub(crate) fn planes(&self) -> Option<Nv12<'a>> {
        self.mapped.set(true);
        // SAFETY: the shown picture's buffer, valid for `'a`.
        let bytes = unsafe { self.shown.bytes(self.gst) }?;
        self.layout
            .planes(bytes, self.width, self.height, self.colors)
    }

    /// Calls `f` with its texels as RGBA rows, converted from its planes.
    pub(crate) fn read(&self, f: impl FnOnce(&[u8], usize)) -> bool {
        self.copied.set(true);
        let Some(planes) = self.planes() else {
            return false;
        };
        let mut rows = self.rows.borrow_mut();
        let (w, h) = (planes.width as usize, planes.height as usize);
        let plane = |bytes, row_stride, pixel_stride| yuv::Plane {
            bytes,
            row_stride,
            pixel_stride,
        };
        if !yuv::to_rgba(
            w,
            h,
            plane(planes.y, planes.y_stride, 1),
            plane(planes.uv, planes.uv_stride, 2),
            plane(planes.uv.get(1..).unwrap_or_default(), planes.uv_stride, 2),
            planes.matrix,
            planes.full_range,
            &mut rows,
        ) {
            return false;
        }
        f(&rows, w * 4);
        true
    }

    pub(crate) fn layout(&self) -> PixelLayout {
        PixelLayout::Rgba8
    }
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
    /// Set once a dma-buf's planes are asked for.
    mapped: Cell<bool>,
    /// The sink was asked for memory at hand alone.
    at_hand: bool,
    /// The sink was asked for BGRA alone.
    rows: bool,
    /// The rows a dma-buf's picture is converted into when copied.
    converted: RefCell<Vec<u8>>,
    /// Pictures in dma-bufs replaced while a backend still holds them, kept
    /// from their pool until it lets go.
    spent: Vec<(*mut GstSample, Arc<ExternalHolds>)>,
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
    /// A camera's, which way it faces: live, paused by letting the device
    /// go.
    camera: Option<CameraFacing>,
}

impl Player {
    pub(crate) fn open(source: &VideoSource) -> Result<Player, VideoError> {
        let gst = gst().ok_or(VideoError::Unsupported)?;
        let (uri, spilled) = match source {
            VideoSource::File(path) => (file_uri(path), None),
            VideoSource::Url(url) => (url.clone(), None),
            VideoSource::Bytes(bytes) => spill(bytes)?,
            VideoSource::Static(bytes) => spill(bytes)?,
            VideoSource::Camera(request) => return Player::open_camera(request),
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
            let Some(sink) = make_sink(gst) else {
                (gst.object_unref)(pipeline);
                return Err(VideoError::Unsupported);
            };
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
            Ok(Player::start(gst, pipeline, sink, spilled, None))
        }
    }

    /// Opens the camera `request` chooses, capturing once ready.
    fn open_camera(request: &CameraRequest) -> Result<Player, VideoError> {
        let gst = gst().ok_or(VideoError::Unsupported)?;
        let devices = Devices::list(gst);
        let infos: Vec<CameraInfo> = devices.0.iter().map(|(info, _)| info.clone()).collect();
        let info = camera::chosen(&infos, &request.camera)
            .ok_or_else(|| VideoError::Open("no such camera".to_owned()))?;
        let device = devices.0.iter().find(|(i, _)| i == info).map(|(_, d)| *d);
        // SAFETY: a device of the list, alive while it is; the element made
        // of it is the player's.
        let source = unsafe {
            device.map_or(ptr::null_mut(), |d| {
                (gst.device_create_element)(d, ptr::null())
            })
        };
        drop(devices);
        if source.is_null() {
            return Err(VideoError::Open("the camera has no source".to_owned()));
        }
        // SAFETY: the source was just made, handed over.
        unsafe { Player::live(gst, source, request, info.facing) }
    }

    /// A player of what the live `source` captures, in the mode of its
    /// nearest what `request` wants: raw where the source has that mode so,
    /// else JPEG decoded; converted to what the sink takes.
    ///
    /// # Safety
    ///
    /// `source` is a floating or owned source element the player takes.
    unsafe fn live(
        gst: &'static Gst,
        source: *mut GstElement,
        request: &CameraRequest,
        facing: CameraFacing,
    ) -> Result<Player, VideoError> {
        // SAFETY: the caller's source.
        let modes = unsafe { modes_of(gst, source, request) };
        let caps = match preferred(&modes, request).map(|i| &modes[i]) {
            Some(m) => format!(
                "{},width={},height={},framerate={}/{}",
                if m.jpeg { "image/jpeg" } else { "video/x-raw" },
                m.mode.width,
                m.mode.height,
                m.rate.0,
                m.rate.1
            ),
            // A source that says nothing of its modes negotiates its own.
            None => "video/x-raw".to_owned(),
        };
        let jpeg = caps.starts_with("image/jpeg");
        let caps = CString::new(caps).map_err(|_| VideoError::Unsupported)?;
        // SAFETY: objects made here, each added to the pipeline, which then
        // owns them; the caps are released once set.
        unsafe {
            let make = |name: &CStr| (gst.element_factory_make)(name.as_ptr(), ptr::null());
            let pipeline = (gst.pipeline_new)(ptr::null());
            let filter = make(c"capsfilter");
            let decoder = if jpeg {
                make(c"jpegdec")
            } else {
                ptr::null_mut()
            };
            let convert = make(c"videoconvert");
            let sink = make_sink(gst);
            let made = !pipeline.is_null()
                && !filter.is_null()
                && !convert.is_null()
                && (decoder.is_null() != jpeg)
                && sink.is_some();
            let Some(sink) = sink.filter(|_| made) else {
                for element in [pipeline, source, filter, decoder, convert] {
                    if !element.is_null() {
                        (gst.object_unref)(element);
                    }
                }
                if let Some(sink) = sink {
                    (gst.object_unref)(sink);
                    (gst.object_unref)(sink);
                }
                return Err(VideoError::Unsupported);
            };
            let wanted = (gst.caps_from_string)(caps.as_ptr());
            (gst.object_set)(filter, c"caps".as_ptr(), wanted, ptr::null::<c_char>());
            (gst.mini_object_unref)(wanted);
            let chain: Vec<_> = [source, filter, decoder, convert, sink]
                .into_iter()
                .filter(|e| !e.is_null())
                .collect();
            for &element in &chain {
                (gst.bin_add)(pipeline, element);
            }
            if chain
                .windows(2)
                .any(|pair| (gst.element_link)(pair[0], pair[1]) == 0)
            {
                (gst.object_unref)(sink);
                (gst.object_unref)(pipeline);
                return Err(VideoError::Open(
                    "the camera's pictures cannot be shown".to_owned(),
                ));
            }
            Ok(Player::start(gst, pipeline, sink, None, Some(facing)))
        }
    }

    /// The player of `pipeline`, whose sink is `sink` (its own reference
    /// held), started paused: a live pipeline plays at once, as it shows
    /// nothing until it does.
    ///
    /// # Safety
    ///
    /// `pipeline` is a pipeline the player owns, `sink` the appsink in it.
    unsafe fn start(
        gst: &'static Gst,
        pipeline: *mut GstElement,
        sink: *mut GstElement,
        spilled: Option<Spilled>,
        camera: Option<CameraFacing>,
    ) -> Player {
        // SAFETY: the caller's pipeline; the bus is the player's reference.
        let (bus, live) = unsafe {
            let bus = (gst.element_get_bus)(pipeline);
            let live = (gst.element_set_state)(pipeline, STATE_PAUSED) == STATE_CHANGE_NO_PREROLL;
            if live {
                (gst.element_set_state)(pipeline, STATE_PLAYING);
            }
            (bus, live)
        };
        Player {
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
            mapped: Cell::new(false),
            at_hand: gst.video.is_none(),
            rows: false,
            converted: RefCell::default(),
            spent: Vec::new(),
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
            camera,
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
        if self.camera.is_none() {
            // SAFETY: `playbin`'s `n-audio` is an int, read into a local;
            // the list ends in NULL.
            unsafe {
                (gst.object_get)(
                    self.pipeline,
                    c"n-audio".as_ptr(),
                    &mut audio as *mut c_int,
                    ptr::null::<c_char>(),
                );
            }
        }
        if let Some(time) = self.seek.take() {
            self.seek_to(time);
        } else if self.rate != 1.0 {
            self.change_rate();
        }
        if self.live && !self.wants_play {
            self.hold();
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
            facing: self.camera,
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
            self.rows = true;
            self.at_hand = true;
            self.want(CAPS_ROWS);
        } else if self.mapped.get() && !self.at_hand {
            self.at_hand = true;
            self.want(CAPS_AT_HAND);
        }
        self.let_go_spent();
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
            let (width, height) = (self.width, self.height);
            let Some(layout) = self.layout_of(buffer) else {
                (gst.mini_object_unref)(sample);
                return None;
            };
            let pts = (*buffer.cast::<Buffer>()).pts;
            let time = if pts == CLOCK_TIME_NONE {
                self.at
            } else {
                Duration::from_nanos(pts)
            };
            // A dma-buf: named for the GPU, mapped only once asked.
            if let Some(colors) = self.planes
                && let Some((image, holds)) = self.dma_buf(buffer, &layout, colors)
            {
                self.release_shown();
                let shown = self.shown.insert(Shown {
                    sample,
                    buffer,
                    map: RefCell::new(no_map()),
                    mapped: Cell::new(false),
                    holds: Some(holds),
                });
                let picture = Picture {
                    gst,
                    shown,
                    image,
                    layout,
                    colors,
                    width,
                    height,
                    mapped: &self.mapped,
                    copied: &self.copied,
                    rows: &self.converted,
                };
                return Some(Frame::native_picture(width, height, time, picture));
            }
            let shown = Shown {
                sample,
                buffer,
                map: RefCell::new(no_map()),
                mapped: Cell::new(false),
                holds: None,
            };
            if width == 0 || shown.bytes(gst).is_none_or(|b| b.len() < layout.size) {
                if shown.mapped.get() {
                    (gst.buffer_unmap)(buffer, &mut *shown.map.borrow_mut());
                }
                (gst.mini_object_unref)(sample);
                return None;
            }
            self.release_shown();
            let shown = self.shown.insert(shown);
            let bytes = shown.bytes(gst)?;
            let Some(colors) = self.planes else {
                return Some(Frame::bgra(width, height, bytes, layout.strides[0], time));
            };
            let picture = layout.planes(bytes, width, height, colors)?;
            Some(Frame::nv12_watched(picture, &self.copied, time))
        }
    }

    /// Where the planes of `buffer`, a picture of the caps last read, lie:
    /// as its video metadata says, else as GStreamer lays them out without.
    ///
    /// # Safety
    ///
    /// `buffer` is a live buffer of a sample this player holds.
    unsafe fn layout_of(&self, buffer: *mut GstBuffer) -> Option<Layout> {
        let (width, height) = (self.width as usize, self.height as usize);
        let planes = if self.planes.is_some() { 2 } else { 1 };
        let default = if planes == 2 {
            nv12_layout(width, height)
        } else {
            Layout {
                offsets: [0, 0],
                strides: [width * 4, 0],
                size: width * 4 * height,
            }
        };
        let Some(video) = &self.gst.video else {
            return Some(default);
        };
        // SAFETY: the caller's buffer; its metadata, when there, is read in
        // place.
        let meta = unsafe { (video.buffer_get_video_meta)(buffer).as_ref() };
        let Some(meta) = meta else {
            return Some(default);
        };
        if (meta.n_planes as usize) < planes || meta.stride[..planes].iter().any(|&s| s <= 0) {
            return None;
        }
        let strides = [meta.stride[0] as usize, meta.stride[1].max(0) as usize];
        let offsets = [meta.offset[0], if planes == 2 { meta.offset[1] } else { 0 }];
        let size = if planes == 2 {
            (offsets[0] + strides[0] * height).max(offsets[1] + strides[1] * height.div_ceil(2))
        } else {
            offsets[0] + strides[0] * height
        };
        Some(Layout {
            offsets,
            strides,
            size,
        })
    }

    /// The dma-buf `buffer` lies in, as the external image a GPU reads where
    /// it lies, and the holds its memory keeps; `None` for a buffer in
    /// memory of another kind, or in more than one.
    ///
    /// # Safety
    ///
    /// `buffer` is a live buffer of a sample this player holds.
    unsafe fn dma_buf(
        &self,
        buffer: *mut GstBuffer,
        layout: &Layout,
        colors: Colors,
    ) -> Option<(ExternalImage, Arc<ExternalHolds>)> {
        let gst = self.gst;
        let video = gst.video.as_ref()?;
        // SAFETY: the caller's buffer and its one memory, borrowed; the
        // descriptor is the memory's own, borrowed while the sample is kept.
        unsafe {
            if (gst.buffer_n_memory)(buffer) != 1 {
                return None;
            }
            let memory = (gst.buffer_peek_memory)(buffer, 0);
            if memory.is_null() || (video.is_dmabuf_memory)(memory) == 0 {
                return None;
            }
            let fd = (video.dmabuf_memory_get_fd)(memory);
            if fd < 0 {
                return None;
            }
            let len = libc::lseek(fd, 0, libc::SEEK_END);
            if len <= 0 {
                return None;
            }
            let start = (*memory.cast::<Memory>()).offset;
            let holds = holds_of(gst, memory);
            let image = ExternalImage::DmaBuf {
                fd,
                len: len as u64,
                offsets: layout.offsets.map(|o| (start + o) as u64),
                strides: layout.strides.map(|s| s as u32),
                holds: Arc::as_ptr(&holds),
                matrix: colors.matrix,
                full_range: colors.full_range,
                left_chroma: colors.left_chroma,
                width: self.width,
                height: self.height,
            };
            Some((image, holds))
        }
    }

    /// Releases the pictures in dma-bufs no backend holds any longer.
    fn let_go_spent(&mut self) {
        let gst = self.gst;
        self.spent.retain(|(sample, holds)| {
            let held = holds.is_held();
            if !held {
                // SAFETY: the sample was kept here, released once.
                unsafe { (gst.mini_object_unref)(*sample) };
            }
            held
        });
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
            let nv12 = match text(c"format") {
                Some(b"NV12") => true,
                // Linear NV12, as asked for.
                Some(b"DMA_DRM") => matches!(
                    text(c"drm-format"),
                    Some(b"NV12" | b"NV12:0x0000000000000000")
                ),
                _ => false,
            };
            self.planes =
                nv12.then(|| colors(text(c"colorimetry"), text(c"chroma-site"), self.height));
        }
    }

    /// Asks the sink for `wanted` alone, which GStreamer negotiates for the
    /// pictures after.
    fn want(&mut self, wanted: &CStr) {
        let gst = self.gst;
        // SAFETY: calls on the player's sink; the caps and the pad are
        // released once, the event handed to the pad, which takes it.
        unsafe {
            let caps = (gst.caps_from_string)(wanted.as_ptr());
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
        if let Some(shown) = self.shown.take() {
            // SAFETY: the picture's buffer is unmapped once, then its sample
            // released once, or kept while a backend holds its dma-buf.
            unsafe {
                if shown.mapped.get() {
                    (self.gst.buffer_unmap)(shown.buffer, &mut *shown.map.borrow_mut());
                }
                match shown.holds {
                    Some(holds) if holds.is_held() => self.spent.push((shown.sample, holds)),
                    _ => (self.gst.mini_object_unref)(shown.sample),
                }
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
            self.hold();
        }
    }

    /// Holds the pipeline where it is; a camera's lets the device go.
    fn hold(&mut self) {
        self.set_state(if self.camera.is_some() {
            STATE_NULL
        } else {
            STATE_PAUSED
        });
    }

    pub(crate) fn seek(&mut self, time: Duration) {
        if self.camera.is_some() {
            return;
        }
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
        if self.camera.is_some() {
            return;
        }
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
        if self.camera.is_some() {
            return;
        }
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
        if self.camera.is_some() {
            return;
        }
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
        for (sample, _) in self.spent.drain(..) {
            // SAFETY: each kept sample released once; a backend still
            // reading a dma-buf holds a duplicate of its descriptor.
            unsafe { (gst.mini_object_unref)(sample) };
        }
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

/// Where a picture's planes lie in its buffer — one of rows, or luma then
/// chroma — the bytes between their rows, and the bytes they take.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Layout {
    offsets: [usize; 2],
    strides: [usize; 2],
    size: usize,
}

impl Layout {
    /// The NV12 planes of the `width` × `height` picture in `bytes`, encoded
    /// as `colors` say; `None` where they do not fit.
    fn planes(self, bytes: &[u8], width: u32, height: u32, colors: Colors) -> Option<Nv12<'_>> {
        if bytes.len() < self.size {
            return None;
        }
        Some(Nv12 {
            width,
            height,
            y: bytes.get(self.offsets[0]..)?,
            y_stride: self.strides[0],
            uv: bytes.get(self.offsets[1]..)?,
            uv_stride: self.strides[1],
            matrix: colors.matrix,
            full_range: colors.full_range,
            left_chroma: colors.left_chroma,
        })
    }
}

/// The layout GStreamer gives an NV12 picture of `width` × `height` with no
/// metadata of its own: rows rounded up to four bytes, the chroma plane after
/// an even number of luma rows.
fn nv12_layout(width: usize, height: usize) -> Layout {
    let stride = width.next_multiple_of(4);
    let rows = height.next_multiple_of(2);
    let chroma = stride * rows;
    Layout {
        offsets: [0, chroma],
        strides: [stride, stride],
        size: chroma + stride * rows / 2,
    }
}

/// The holds kept on dma-buf `memory`, made at its first picture and freed
/// with it, so they name it alone for as long as anyone holds them.
///
/// # Safety
///
/// `memory` is a live `GstMemory`.
unsafe fn holds_of(gst: &Gst, memory: *mut GstMemory) -> Arc<ExternalHolds> {
    /// Drops the memory's own count of its holds.
    unsafe extern "C" fn release(holds: *mut c_void) {
        // SAFETY: the pointer `Arc::into_raw` gave when the holds were kept
        // on the memory, dropped once, as the memory goes.
        drop(unsafe { Arc::from_raw(holds.cast_const().cast::<ExternalHolds>()) });
    }
    // SAFETY: the caller's memory; the data kept on it is an `Arc` it owns
    // one count of, released with it.
    unsafe {
        let kept = (gst.mini_object_get_qdata)(memory, gst.holds_quark);
        if !kept.is_null() {
            return ExternalHolds::retain(kept.cast_const().cast());
        }
        let holds = ExternalHolds::new();
        let owned = Arc::into_raw(Arc::clone(&holds)).cast_mut().cast();
        (gst.mini_object_set_qdata)(memory, gst.holds_quark, owned, release);
        holds
    }
}

/// Answers the sink's allocation query: buffers laid out as their video
/// metadata says are taken as they are, so upstream need not copy padded
/// ones to fit.
unsafe extern "C" fn offer_video_meta(
    _: *mut GstPad,
    info: *mut GstPadProbeInfo,
    api: *mut c_void,
) -> c_int {
    let Some(gst) = gst() else {
        return PROBE_OK;
    };
    // SAFETY: the probe's info and the query it carries, live for the call.
    unsafe {
        let query = (gst.pad_probe_info_get_query)(info);
        if query.is_null() || (*query.cast::<Query>()).kind != QUERY_ALLOCATION {
            return PROBE_OK;
        }
        (gst.query_add_allocation_meta)(query, api as usize, ptr::null());
    }
    PROBE_HANDLED
}

/// An appsink taking what the players take, with a reference of the
/// caller's beside the one a bin it is added to takes.
///
/// # Safety
///
/// GStreamer is started.
unsafe fn make_sink(gst: &Gst) -> Option<*mut GstElement> {
    // SAFETY: calls on the sink made here; each property is set with the C
    // type it is declared with, the list ending in NULL.
    unsafe {
        let sink = (gst.element_factory_make)(c"appsink".as_ptr(), ptr::null());
        if sink.is_null() {
            return None;
        }
        (gst.object_ref)(sink);
        let wanted = if gst.video.is_some() {
            CAPS_DMA_BUF
        } else {
            CAPS_AT_HAND
        };
        let caps = (gst.caps_from_string)(wanted.as_ptr());
        (gst.app_sink_set_caps)(sink, caps);
        (gst.mini_object_unref)(caps);
        // Buffers laid out as their metadata says are taken as they are.
        if let Some(video) = &gst.video {
            let pad = (gst.element_get_static_pad)(sink, c"sink".as_ptr());
            if !pad.is_null() {
                (gst.pad_add_probe)(
                    pad,
                    PROBE_QUERY_IN,
                    offer_video_meta,
                    video.video_meta_api as *mut c_void,
                    ptr::null_mut(),
                );
                (gst.object_unref)(pad);
            }
        }
        // The sink keeps the newest picture due, in step with the clock,
        // and ends without waiting for it to be taken: a video no node shows
        // is not pulled from.
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
        Some(sink)
    }
}

/// The cameras GStreamer's device providers list — V4L2, PipeWire,
/// libcamera — and the devices, released when dropped.
struct Devices(Vec<(CameraInfo, *mut GstDevice)>, &'static Gst);

impl Devices {
    fn list(gst: &'static Gst) -> Devices {
        let mut out = Vec::new();
        // SAFETY: the monitor and the list are made here and released once;
        // each device's reference is kept, its name and properties read and
        // freed.
        unsafe {
            let monitor = (gst.device_monitor_new)();
            if monitor.is_null() {
                return Devices(out, gst);
            }
            (gst.device_monitor_add_filter)(monitor, c"Video/Source".as_ptr(), ptr::null_mut());
            let list = (gst.device_monitor_get_devices)(monitor);
            let mut at = list;
            while let Some(node) = at.as_ref() {
                let device = node.data;
                at = node.next;
                if device.is_null() {
                    continue;
                }
                let name = (gst.device_get_display_name)(device);
                let name_text = if name.is_null() {
                    String::new()
                } else {
                    let text = CStr::from_ptr(name).to_string_lossy().into_owned();
                    (gst.free)(name.cast());
                    text
                };
                let properties = (gst.device_get_properties)(device);
                let text = |key: &CStr| {
                    if properties.is_null() {
                        return None;
                    }
                    let value = (gst.structure_get_string)(properties, key.as_ptr());
                    (!value.is_null()).then(|| CStr::from_ptr(value).to_string_lossy().into_owned())
                };
                let id = text(c"device.path")
                    .or_else(|| text(c"api.v4l2.path"))
                    .or_else(|| text(c"object.serial"))
                    .unwrap_or_else(|| name_text.clone());
                let facing = facing_of(text(c"api.libcamera.location").as_deref(), &name_text);
                if !properties.is_null() {
                    (gst.structure_free)(properties);
                }
                out.push((
                    CameraInfo {
                        id,
                        name: name_text,
                        facing,
                    },
                    device,
                ));
            }
            (gst.list_free)(list);
            (gst.object_unref)(monitor);
        }
        Devices(out, gst)
    }
}

impl Drop for Devices {
    fn drop(&mut self) {
        for (_, device) in self.0.drain(..) {
            // SAFETY: the list's reference to each device, released once.
            unsafe { (self.1.object_unref)(device) };
        }
    }
}

/// A mode a camera source captures in, as its caps name it.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Captured {
    mode: camera::CameraMode,
    /// In JPEG rather than raw.
    jpeg: bool,
    /// The rate as the caps name it.
    rate: (c_int, c_int),
}

/// The modes `source` says it captures in once opened, each of its caps'
/// sizes at the rate nearest the one asked and at its fastest; none where it
/// says nothing.
///
/// # Safety
///
/// `source` is a live source element.
unsafe fn modes_of(gst: &Gst, source: *mut GstElement, request: &CameraRequest) -> Vec<Captured> {
    let mut out = Vec::new();
    // SAFETY: the source is opened to know its modes; its pad and caps are
    // references released once, each structure read through a copy fixated
    // and freed.
    unsafe {
        (gst.element_set_state)(source, STATE_READY);
        let pad = (gst.element_get_static_pad)(source, c"src".as_ptr());
        if pad.is_null() {
            return out;
        }
        let caps = (gst.pad_query_caps)(pad, ptr::null_mut());
        (gst.object_unref)(pad);
        if caps.is_null() {
            return out;
        }
        let side = |v: u32| c_int::try_from(v.max(1)).unwrap_or(c_int::MAX);
        let (w, h) = (side(request.width), side(request.height));
        for i in 0..(gst.caps_get_size)(caps) {
            let structure = (gst.caps_get_structure)(caps, i);
            let name = CStr::from_ptr((gst.structure_get_name)(structure));
            let jpeg = match name.to_bytes() {
                b"video/x-raw" => false,
                b"image/jpeg" => true,
                _ => continue,
            };
            for rate in [side(request.frame_rate), 1000] {
                let fixed = (gst.structure_copy)(structure);
                (gst.structure_fixate_field_nearest_int)(fixed, c"width".as_ptr(), w);
                (gst.structure_fixate_field_nearest_int)(fixed, c"height".as_ptr(), h);
                (gst.structure_fixate_field_nearest_fraction)(
                    fixed,
                    c"framerate".as_ptr(),
                    rate,
                    1,
                );
                let (mut width, mut height, mut n, mut d) = (0, 0, 0, 0);
                let read = (gst.structure_get_int)(fixed, c"width".as_ptr(), &mut width) != 0
                    && (gst.structure_get_int)(fixed, c"height".as_ptr(), &mut height) != 0
                    && (gst.structure_get_fraction)(fixed, c"framerate".as_ptr(), &mut n, &mut d)
                        != 0;
                (gst.structure_free)(fixed);
                if read && width > 0 && height > 0 && n > 0 && d > 0 {
                    let captured = Captured {
                        mode: camera::CameraMode {
                            width: width as u32,
                            height: height as u32,
                            frame_rate: f64::from(n) / f64::from(d),
                        },
                        jpeg,
                        rate: (n, d),
                    };
                    if !out.contains(&captured) {
                        out.push(captured);
                    }
                }
            }
        }
        (gst.mini_object_unref)(caps.cast());
    }
    out
}

/// The mode of `modes` nearest what `request` wants, raw rather than JPEG
/// where the source has it both ways: raw costs no decoding.
fn preferred(modes: &[Captured], request: &CameraRequest) -> Option<usize> {
    let plain: Vec<_> = modes.iter().map(|m| m.mode).collect();
    let near = camera::nearest_mode(&plain, request)?;
    let mode = plain[near];
    let same = |m: &Captured| {
        (m.mode.width, m.mode.height) == (mode.width, mode.height)
            && (m.mode.frame_rate - mode.frame_rate).abs() < 0.01
    };
    modes.iter().position(|m| same(m) && !m.jpeg).or(Some(near))
}

/// Which way a camera faces, as libcamera says, else as its name does: a
/// laptop's own is integrated in its lid, toward the user.
fn facing_of(location: Option<&str>, name: &str) -> CameraFacing {
    match location {
        Some("front") => CameraFacing::Front,
        Some("back") => CameraFacing::Back,
        Some(_) => CameraFacing::External,
        None => {
            let name = name.to_ascii_lowercase();
            if ["integrated", "built-in", "facetime", "internal"]
                .iter()
                .any(|word| name.contains(word))
            {
                CameraFacing::Front
            } else {
                CameraFacing::External
            }
        }
    }
}

/// The cameras GStreamer lists; none where it is not installed.
pub(crate) fn cameras() -> Vec<CameraInfo> {
    gst().map_or_else(Vec::new, |gst| {
        Devices::list(gst)
            .0
            .iter()
            .map(|(info, _)| info.clone())
            .collect()
    })
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
        assert_eq!(
            (l.offsets, l.strides, l.size),
            ([0, 64 * 48], [64, 64], 64 * 48 * 3 / 2)
        );
        let l = nv12_layout(3, 3);
        assert_eq!((l.offsets, l.strides, l.size), ([0, 16], [4, 4], 24));
        let colors = colors(None, None, 3);
        assert!(l.planes(&[0; 23], 3, 3, colors).is_none());
        let planes = l.planes(&[0; 24], 3, 3, colors).unwrap();
        assert_eq!((planes.y.len(), planes.uv.len()), (24, 8));
    }

    #[test]
    fn a_camera_takes_its_nearest_mode_raw_where_it_has_it_so() {
        let mode = |width, height, rate: c_int, jpeg| Captured {
            mode: camera::CameraMode {
                width,
                height,
                frame_rate: f64::from(rate),
            },
            jpeg,
            rate: (rate, 1),
        };
        let modes = [
            mode(1920, 1080, 30, true),
            mode(1920, 1080, 5, false),
            mode(1280, 720, 30, true),
            mode(1280, 720, 10, false),
            mode(1280, 720, 30, false),
        ];
        let request = CameraRequest::default();
        assert_eq!(preferred(&modes, &request), Some(4), "raw at the same mode");
        let large = CameraRequest {
            width: 1920,
            height: 1080,
            ..request
        };
        assert_eq!(
            preferred(&modes, &large),
            Some(0),
            "JPEG keeps up, raw does not"
        );
        assert_eq!(preferred(&[], &large), None);
    }

    #[test]
    fn a_camera_faces_where_libcamera_or_its_name_says() {
        assert_eq!(facing_of(Some("back"), "x"), CameraFacing::Back);
        assert_eq!(facing_of(Some("front"), "x"), CameraFacing::Front);
        assert_eq!(
            facing_of(None, "Integrated Camera: Integrated C"),
            CameraFacing::Front
        );
        assert_eq!(
            facing_of(None, "HD Pro Webcam C920"),
            CameraFacing::External
        );
    }

    /// A live source plays as a camera does: ready at its first picture, in
    /// the size asked, with no length; paused, it lets the source go, and
    /// plays on when asked again.
    #[test]
    fn a_live_source_plays_as_a_camera() {
        let Some(gst) = gst() else {
            eprintln!("no GStreamer; skipped");
            return;
        };
        // Listing cameras works where there are none.
        let _ = cameras();
        // SAFETY: a source made here, handed to the player.
        let mut player = unsafe {
            let source = (gst.element_factory_make)(c"videotestsrc".as_ptr(), ptr::null());
            if source.is_null() {
                eprintln!("no videotestsrc; skipped");
                return;
            }
            (gst.object_set)(
                source,
                c"is-live".as_ptr(),
                1 as c_int,
                ptr::null::<c_char>(),
            );
            let request = CameraRequest {
                width: 320,
                height: 240,
                frame_rate: 30,
                ..CameraRequest::default()
            };
            Player::live(gst, source, &request, CameraFacing::External).unwrap()
        };
        let until = |player: &mut Player, what: &dyn Fn(&mut Player) -> bool| {
            let start = std::time::Instant::now();
            while !what(player) {
                assert!(start.elapsed() < Duration::from_secs(5), "timed out");
                std::thread::sleep(Duration::from_millis(5));
            }
        };
        let ready = std::cell::RefCell::new(None);
        until(&mut player, &|p| {
            match p.poll() {
                Some(VideoEvent::Ready(info)) => *ready.borrow_mut() = Some(info),
                Some(VideoEvent::Failed(why)) => panic!("{why:?}"),
                _ => {}
            }
            ready.borrow().is_some()
        });
        let info = ready.take().unwrap();
        assert_eq!((info.width, info.height), (320, 240));
        assert_eq!(
            (info.duration, info.seekable, info.audio),
            (None, false, false)
        );
        assert_eq!(info.facing, Some(CameraFacing::External));
        player.play();
        until(&mut player, &|p| {
            p.frame().is_some_and(|f| (f.width, f.height) == (320, 240))
        });
        player.pause();
        player.seek(Duration::from_secs(1));
        player.set_rate(2.0);
        player.play();
        until(&mut player, &|p| p.frame().is_some());
    }

    /// The allocation query is the one the probe answers.
    #[test]
    fn the_allocation_query_is_known_by_its_number() {
        let Some(gst) = gst() else {
            eprintln!("no GStreamer; skipped");
            return;
        };
        let lib = Library::open(c"libgstreamer-1.0.so.0").unwrap();
        // SAFETY: the C signature of `gst_query_new_allocation`; the query
        // made is read and released once.
        unsafe {
            let new: unsafe extern "C" fn(*mut GstCaps, c_int) -> *mut GstQuery =
                lib.get(c"gst_query_new_allocation").unwrap();
            let query = new(ptr::null_mut(), 0);
            assert_eq!((*query.cast::<Query>()).kind, QUERY_ALLOCATION);
            (gst.mini_object_unref)(query);
        }
    }
}
