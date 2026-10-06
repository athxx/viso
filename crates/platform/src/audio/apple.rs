//! Audio output through an output Audio Unit: the default output unit on
//! macOS, RemoteIO on iOS. The unit renders float32 non-interleaved stereo
//! at the device's rate on CoreAudio's IO thread, calling the render
//! callback with a scratch block allocated once at start.

use std::ffi::c_void;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr;

use super::{AudioError, AudioFormat, AudioRender};

type OsStatus = i32;
type AudioUnit = *mut c_void;

#[repr(C)]
struct AudioComponentDescription {
    component_type: u32,
    component_sub_type: u32,
    component_manufacturer: u32,
    component_flags: u32,
    component_flags_mask: u32,
}

#[repr(C)]
#[derive(Default)]
struct AudioStreamBasicDescription {
    sample_rate: f64,
    format_id: u32,
    format_flags: u32,
    bytes_per_packet: u32,
    frames_per_packet: u32,
    bytes_per_frame: u32,
    channels_per_frame: u32,
    bits_per_channel: u32,
    reserved: u32,
}

#[repr(C)]
struct AudioBuffer {
    number_channels: u32,
    data_byte_size: u32,
    data: *mut c_void,
}

/// `AudioBufferList`: its buffers follow the count, `number_buffers` of them.
#[repr(C)]
struct AudioBufferList {
    number_buffers: u32,
    buffers: [AudioBuffer; 1],
}

type RenderProc = unsafe extern "C" fn(
    ref_con: *mut c_void,
    flags: *mut u32,
    time: *const c_void,
    bus: u32,
    frames: u32,
    data: *mut AudioBufferList,
) -> OsStatus;

#[repr(C)]
struct RenderCallback {
    proc_: RenderProc,
    ref_con: *mut c_void,
}

#[link(name = "AudioToolbox", kind = "framework")]
unsafe extern "C" {
    fn AudioComponentFindNext(
        component: *mut c_void,
        desc: *const AudioComponentDescription,
    ) -> *mut c_void;
    fn AudioComponentInstanceNew(component: *mut c_void, instance: *mut AudioUnit) -> OsStatus;
    fn AudioComponentInstanceDispose(instance: AudioUnit) -> OsStatus;
    fn AudioUnitInitialize(unit: AudioUnit) -> OsStatus;
    fn AudioUnitUninitialize(unit: AudioUnit) -> OsStatus;
    fn AudioUnitSetProperty(
        unit: AudioUnit,
        id: u32,
        scope: u32,
        element: u32,
        data: *const c_void,
        size: u32,
    ) -> OsStatus;
    fn AudioUnitGetProperty(
        unit: AudioUnit,
        id: u32,
        scope: u32,
        element: u32,
        data: *mut c_void,
        size: *mut u32,
    ) -> OsStatus;
    fn AudioOutputUnitStart(unit: AudioUnit) -> OsStatus;
    fn AudioOutputUnitStop(unit: AudioUnit) -> OsStatus;
}

const fn four_cc(code: &[u8; 4]) -> u32 {
    u32::from_be_bytes(*code)
}

const TYPE_OUTPUT: u32 = four_cc(b"auou");
#[cfg(target_os = "macos")]
const SUB_TYPE_OUTPUT: u32 = four_cc(b"def ");
#[cfg(not(target_os = "macos"))]
const SUB_TYPE_OUTPUT: u32 = four_cc(b"rioc");
const MANUFACTURER_APPLE: u32 = four_cc(b"appl");
const FORMAT_LINEAR_PCM: u32 = four_cc(b"lpcm");
/// Float, packed, non-interleaved.
const FORMAT_FLAGS: u32 = 1 | 8 | 32;

const PROPERTY_STREAM_FORMAT: u32 = 8;
const PROPERTY_MAXIMUM_FRAMES: u32 = 14;
const PROPERTY_RENDER_CALLBACK: u32 = 23;
const SCOPE_GLOBAL: u32 = 0;
const SCOPE_INPUT: u32 = 1;
const SCOPE_OUTPUT: u32 = 2;

const CHANNELS: usize = 2;

fn check(status: OsStatus, what: &str) -> Result<(), AudioError> {
    if status == 0 {
        Ok(())
    } else {
        Err(AudioError::Device(format!("{what}: OSStatus {status}")))
    }
}

/// What the IO thread renders with.
struct Renderer {
    render: AudioRender,
    scratch: Box<[f32]>,
    channels: usize,
    max_frames: usize,
    /// Set once the render callback panicked; the output stays silent.
    failed: bool,
}

/// The open output unit and, while it renders, its renderer.
pub(super) struct Output {
    unit: AudioUnit,
    running: Option<Box<Renderer>>,
    initialized: bool,
}

// SAFETY: the unit is an opaque handle the Audio Unit API accepts from any
// thread; the renderer is `Send`, and it is touched by the IO thread only
// between `start` and `stop`, which stops the unit before the renderer is
// dropped or replaced.
unsafe impl Send for Output {}

impl Output {
    pub(super) fn open() -> Result<(Output, AudioFormat), AudioError> {
        let desc = AudioComponentDescription {
            component_type: TYPE_OUTPUT,
            component_sub_type: SUB_TYPE_OUTPUT,
            component_manufacturer: MANUFACTURER_APPLE,
            component_flags: 0,
            component_flags_mask: 0,
        };
        // SAFETY: `desc` is a valid description; a null component starts the
        // search.
        let component = unsafe { AudioComponentFindNext(ptr::null_mut(), &desc) };
        if component.is_null() {
            return Err(AudioError::Device("no output audio unit".into()));
        }
        let mut unit: AudioUnit = ptr::null_mut();
        // SAFETY: `component` was just found and `unit` receives the instance.
        check(
            unsafe { AudioComponentInstanceNew(component, &mut unit) },
            "creating the output unit",
        )?;
        let output = Output {
            unit,
            running: None,
            initialized: false,
        };
        let mut hardware = AudioStreamBasicDescription::default();
        let mut size = size_of::<AudioStreamBasicDescription>() as u32;
        // SAFETY: the unit is live and `hardware` is a stream description of
        // `size` bytes.
        check(
            unsafe {
                AudioUnitGetProperty(
                    unit,
                    PROPERTY_STREAM_FORMAT,
                    SCOPE_OUTPUT,
                    0,
                    (&raw mut hardware).cast(),
                    &mut size,
                )
            },
            "reading the device format",
        )?;
        let sample_rate = if hardware.sample_rate > 0.0 {
            hardware.sample_rate
        } else {
            48_000.0
        };
        let format = AudioStreamBasicDescription {
            sample_rate,
            format_id: FORMAT_LINEAR_PCM,
            format_flags: FORMAT_FLAGS,
            bytes_per_packet: 4,
            frames_per_packet: 1,
            bytes_per_frame: 4,
            channels_per_frame: CHANNELS as u32,
            bits_per_channel: 32,
            reserved: 0,
        };
        // SAFETY: as above; the unit copies the description.
        check(
            unsafe {
                AudioUnitSetProperty(
                    unit,
                    PROPERTY_STREAM_FORMAT,
                    SCOPE_INPUT,
                    0,
                    (&raw const format).cast(),
                    size_of::<AudioStreamBasicDescription>() as u32,
                )
            },
            "setting the render format",
        )?;
        let mut max_frames: u32 = 0;
        let mut size = size_of::<u32>() as u32;
        // SAFETY: as above, a `u32` property.
        check(
            unsafe {
                AudioUnitGetProperty(
                    unit,
                    PROPERTY_MAXIMUM_FRAMES,
                    SCOPE_GLOBAL,
                    0,
                    (&raw mut max_frames).cast(),
                    &mut size,
                )
            },
            "reading the block size",
        )?;
        let format = AudioFormat {
            sample_rate,
            channels: CHANNELS,
            max_frames: (max_frames as usize).max(1),
        };
        Ok((output, format))
    }

    pub(super) fn start(
        &mut self,
        render: AudioRender,
        format: AudioFormat,
    ) -> Result<(), AudioError> {
        self.stop();
        let mut renderer = Box::new(Renderer {
            render,
            scratch: vec![0.0; format.channels * format.max_frames].into(),
            channels: format.channels,
            max_frames: format.max_frames,
            failed: false,
        });
        let callback = RenderCallback {
            proc_: render_block,
            ref_con: (&raw mut *renderer).cast(),
        };
        // SAFETY: the unit is live and stopped, so no render runs while the
        // callback changes; the renderer outlives every render, as `stop`
        // stops the unit before dropping it.
        unsafe {
            check(
                AudioUnitSetProperty(
                    self.unit,
                    PROPERTY_RENDER_CALLBACK,
                    SCOPE_INPUT,
                    0,
                    (&raw const callback).cast(),
                    size_of::<RenderCallback>() as u32,
                ),
                "installing the render callback",
            )?;
            if !self.initialized {
                check(
                    AudioUnitInitialize(self.unit),
                    "initializing the output unit",
                )?;
                self.initialized = true;
            }
            self.running = Some(renderer);
            check(AudioOutputUnitStart(self.unit), "starting the output unit")
        }
    }

    pub(super) fn stop(&mut self) {
        if self.running.is_some() {
            // SAFETY: the unit is live; once this returns, the IO thread no
            // longer renders through the renderer dropped below.
            unsafe {
                AudioOutputUnitStop(self.unit);
            }
            self.running = None;
        }
    }
}

impl Drop for Output {
    fn drop(&mut self) {
        self.stop();
        // SAFETY: the unit is live and stopped; it is not used again.
        unsafe {
            if self.initialized {
                AudioUnitUninitialize(self.unit);
            }
            AudioComponentInstanceDispose(self.unit);
        }
    }
}

/// The unit's render callback, on CoreAudio's IO thread.
unsafe extern "C" fn render_block(
    ref_con: *mut c_void,
    _flags: *mut u32,
    _time: *const c_void,
    _bus: u32,
    frames: u32,
    data: *mut AudioBufferList,
) -> OsStatus {
    // SAFETY: `ref_con` is the renderer `start` installed, alive until the
    // unit stops, and only this thread touches it while the unit runs.
    let renderer = unsafe { &mut *ref_con.cast::<Renderer>() };
    let frames = frames as usize;
    let rendered = frames <= renderer.max_frames && !renderer.failed;
    let samples = &mut renderer.scratch[..renderer.channels * frames.min(renderer.max_frames)];
    if rendered {
        let render = &mut renderer.render;
        if catch_unwind(AssertUnwindSafe(|| render(samples))).is_err() {
            renderer.failed = true;
        }
    }
    if data.is_null() {
        return 0;
    }
    // SAFETY: CoreAudio passes a buffer list of `number_buffers` buffers,
    // each holding `frames` float samples of one channel.
    unsafe {
        let count = (*data).number_buffers as usize;
        let buffers = ptr::addr_of_mut!((*data).buffers).cast::<AudioBuffer>();
        for i in 0..count {
            let buffer = &mut *buffers.add(i);
            let out = std::slice::from_raw_parts_mut(buffer.data.cast::<f32>(), frames);
            if rendered && !renderer.failed {
                let channel = i.min(renderer.channels - 1);
                out.copy_from_slice(&renderer.scratch[channel * frames..(channel + 1) * frames]);
            } else {
                out.fill(0.0);
            }
        }
    }
    0
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, Instant};

    use super::super::AudioOutput;

    #[test]
    fn the_default_output_renders_blocks() {
        let Ok(mut output) = AudioOutput::open() else {
            // A machine without an output device has nothing to render to.
            return;
        };
        let format = output.format();
        assert_eq!(format.channels, 2);
        assert!(format.sample_rate >= 8_000.0, "{format:?}");
        let blocks = Arc::new(AtomicU64::new(0));
        let counted = Arc::clone(&blocks);
        output
            .start(move |samples| {
                // Silence: the test must not make a sound.
                samples.fill(0.0);
                counted.fetch_add(1, Ordering::Relaxed);
            })
            .expect("starts");
        let deadline = Instant::now() + Duration::from_secs(2);
        while blocks.load(Ordering::Relaxed) < 3 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        output.stop();
        let rendered = blocks.load(Ordering::Relaxed);
        assert!(rendered >= 3, "rendered {rendered} blocks");
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(blocks.load(Ordering::Relaxed), rendered, "stopped");
    }
}
