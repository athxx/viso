//! Audio output through AAudio (`libaaudio`, loaded at runtime, Android 8+):
//! a shared low-latency float32 stereo stream at the device's rate, its
//! data callback on AAudio's realtime thread. A start opens a fresh stream
//! with the callback; a stop closes it, after which no callback runs.

use std::ffi::{CStr, c_void};
use std::ptr;

use super::{AudioError, AudioFormat, AudioRender};

type Builder = *mut c_void;
type Stream = *mut c_void;
type DataCallback = unsafe extern "C" fn(Stream, *mut c_void, *mut c_void, i32) -> i32;

const FORMAT_PCM_FLOAT: i32 = 2;
const SHARING_MODE_SHARED: i32 = 1;
const PERFORMANCE_MODE_LOW_LATENCY: i32 = 12;
const CALLBACK_CONTINUE: i32 = 0;
const CHANNELS: usize = 2;

/// The `libaaudio` entry points used.
struct AAudio {
    create_builder: unsafe extern "C" fn(*mut Builder) -> i32,
    set_format: unsafe extern "C" fn(Builder, i32),
    set_channel_count: unsafe extern "C" fn(Builder, i32),
    set_sample_rate: unsafe extern "C" fn(Builder, i32),
    set_sharing_mode: unsafe extern "C" fn(Builder, i32),
    set_performance_mode: unsafe extern "C" fn(Builder, i32),
    set_data_callback: unsafe extern "C" fn(Builder, Option<DataCallback>, *mut c_void),
    open_stream: unsafe extern "C" fn(Builder, *mut Stream) -> i32,
    delete_builder: unsafe extern "C" fn(Builder) -> i32,
    sample_rate: unsafe extern "C" fn(Stream) -> i32,
    capacity: unsafe extern "C" fn(Stream) -> i32,
    request_start: unsafe extern "C" fn(Stream) -> i32,
    close: unsafe extern "C" fn(Stream) -> i32,
}

fn aaudio() -> Result<&'static AAudio, AudioError> {
    static AAUDIO: std::sync::OnceLock<Option<AAudio>> = std::sync::OnceLock::new();
    AAUDIO
        .get_or_init(load)
        .as_ref()
        .ok_or_else(|| AudioError::Device("libaaudio.so is not available (Android 8+)".into()))
}

fn load() -> Option<AAudio> {
    // SAFETY: `dlopen` and `dlsym` take NUL-terminated names; each symbol is
    // cast to the signature `aaudio/AAudio.h` declares for it. The library
    // stays loaded for the process.
    unsafe {
        let library = libc::dlopen(c"libaaudio.so".as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL);
        if library.is_null() {
            return None;
        }
        let symbol = |name: &CStr| {
            let found = libc::dlsym(library, name.as_ptr());
            (!found.is_null()).then_some(found)
        };
        // A symbol's address as the function pointer type of its field.
        fn cast<F: Copy>(found: *mut c_void) -> F {
            assert_eq!(size_of::<F>(), size_of::<*mut c_void>());
            // SAFETY: `F` is a function pointer type of the symbol's
            // signature, the size of an address (checked above).
            unsafe { std::mem::transmute_copy(&found) }
        }
        Some(AAudio {
            create_builder: cast(symbol(c"AAudio_createStreamBuilder")?),
            set_format: cast(symbol(c"AAudioStreamBuilder_setFormat")?),
            set_channel_count: cast(symbol(c"AAudioStreamBuilder_setChannelCount")?),
            set_sample_rate: cast(symbol(c"AAudioStreamBuilder_setSampleRate")?),
            set_sharing_mode: cast(symbol(c"AAudioStreamBuilder_setSharingMode")?),
            set_performance_mode: cast(symbol(c"AAudioStreamBuilder_setPerformanceMode")?),
            set_data_callback: cast(symbol(c"AAudioStreamBuilder_setDataCallback")?),
            open_stream: cast(symbol(c"AAudioStreamBuilder_openStream")?),
            delete_builder: cast(symbol(c"AAudioStreamBuilder_delete")?),
            sample_rate: cast(symbol(c"AAudioStream_getSampleRate")?),
            capacity: cast(symbol(c"AAudioStream_getBufferCapacityInFrames")?),
            request_start: cast(symbol(c"AAudioStream_requestStart")?),
            close: cast(symbol(c"AAudioStream_close")?),
        })
    }
}

/// Opens a stream at `rate` (`0` for the device's), rendering through
/// `renderer` when given.
fn open_stream(
    aaudio: &AAudio,
    rate: i32,
    renderer: Option<*mut Renderer>,
) -> Result<Stream, AudioError> {
    let mut builder: Builder = ptr::null_mut();
    // SAFETY: `builder` receives a new builder, configured, used once and
    // deleted; `renderer`, when given, outlives the stream (`stop` closes the
    // stream before dropping it).
    unsafe {
        let status = (aaudio.create_builder)(&mut builder);
        if status != 0 || builder.is_null() {
            return Err(AudioError::Device(format!(
                "creating a stream builder: {status}"
            )));
        }
        (aaudio.set_format)(builder, FORMAT_PCM_FLOAT);
        (aaudio.set_channel_count)(builder, CHANNELS as i32);
        if rate > 0 {
            (aaudio.set_sample_rate)(builder, rate);
        }
        (aaudio.set_sharing_mode)(builder, SHARING_MODE_SHARED);
        (aaudio.set_performance_mode)(builder, PERFORMANCE_MODE_LOW_LATENCY);
        if let Some(renderer) = renderer {
            (aaudio.set_data_callback)(builder, Some(render_block), renderer.cast());
        }
        let mut stream: Stream = ptr::null_mut();
        let status = (aaudio.open_stream)(builder, &mut stream);
        (aaudio.delete_builder)(builder);
        if status != 0 || stream.is_null() {
            return Err(AudioError::Device(format!("opening the stream: {status}")));
        }
        Ok(stream)
    }
}

/// What the callback renders with.
struct Renderer {
    render: AudioRender,
    block: Box<[f32]>,
    max_frames: usize,
    failed: bool,
}

pub(super) struct Output {
    rate: i32,
    running: Option<(Stream, Box<Renderer>)>,
}

// SAFETY: a stream handle may be used from any thread; the renderer is
// `Send` and touched by AAudio's thread only while its stream is open.
unsafe impl Send for Output {}

impl Output {
    pub(super) fn open() -> Result<(Output, AudioFormat), AudioError> {
        let aaudio = aaudio()?;
        let probe = open_stream(aaudio, 0, None)?;
        // SAFETY: the probe is open; it is closed right after.
        let (rate, capacity) = unsafe {
            let found = ((aaudio.sample_rate)(probe), (aaudio.capacity)(probe));
            (aaudio.close)(probe);
            found
        };
        let rate = if rate > 0 { rate } else { 48_000 };
        let format = AudioFormat {
            sample_rate: f64::from(rate),
            channels: CHANNELS,
            max_frames: usize::try_from(capacity).unwrap_or(0).max(256),
        };
        Ok((
            Output {
                rate,
                running: None,
            },
            format,
        ))
    }

    pub(super) fn start(
        &mut self,
        render: AudioRender,
        format: AudioFormat,
    ) -> Result<(), AudioError> {
        self.stop();
        let aaudio = aaudio()?;
        let mut renderer = Box::new(Renderer {
            render,
            block: vec![0.0; format.channels * format.max_frames].into(),
            max_frames: format.max_frames,
            failed: false,
        });
        let stream = open_stream(aaudio, self.rate, Some(&raw mut *renderer))?;
        self.running = Some((stream, renderer));
        // SAFETY: the stream is open.
        let status = unsafe { (aaudio.request_start)(stream) };
        if status != 0 {
            self.stop();
            return Err(AudioError::Device(format!("starting the stream: {status}")));
        }
        Ok(())
    }

    pub(super) fn stop(&mut self) {
        if let Some((stream, renderer)) = self.running.take()
            && let Ok(aaudio) = aaudio()
        {
            // SAFETY: the stream is open; once closed, its callback no longer
            // runs, so the renderer may go.
            unsafe {
                (aaudio.close)(stream);
            }
            drop(renderer);
        }
    }
}

impl Drop for Output {
    fn drop(&mut self) {
        self.stop();
    }
}

/// The stream's data callback, on AAudio's thread: `frames` interleaved
/// stereo frames into `data`.
unsafe extern "C" fn render_block(
    _stream: Stream,
    user: *mut c_void,
    data: *mut c_void,
    frames: i32,
) -> i32 {
    // SAFETY: `user` is the renderer `start` installed, alive while the
    // stream is open, and only this thread touches it then.
    let renderer = unsafe { &mut *user.cast::<Renderer>() };
    let frames = usize::try_from(frames).unwrap_or(0);
    // SAFETY: AAudio passes room for `frames` frames of `CHANNELS` floats.
    let out = unsafe { std::slice::from_raw_parts_mut(data.cast::<f32>(), frames * CHANNELS) };
    if frames > renderer.max_frames || renderer.failed {
        out.fill(0.0);
        return CALLBACK_CONTINUE;
    }
    let block = &mut renderer.block[..CHANNELS * frames];
    let render = &mut renderer.render;
    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| render(block))).is_err() {
        renderer.failed = true;
        out.fill(0.0);
        return CALLBACK_CONTINUE;
    }
    for frame in 0..frames {
        for channel in 0..CHANNELS {
            out[frame * CHANNELS + channel] = block[channel * frames + frame];
        }
    }
    CALLBACK_CONTINUE
}
