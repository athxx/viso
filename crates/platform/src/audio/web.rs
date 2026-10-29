//! Audio output on a threaded web build: an `AudioContext` at the device's
//! rate whose AudioWorklet runs the module on its own thread. The glue's
//! threads part opens the context, loads the glue itself into the worklet
//! and adds a stereo node to the destination; the worklet's processor
//! instantiates the module over the page's memory and calls
//! [`__viso_audio`] for each 128-frame block, copying the samples it
//! returns into its outputs.
//!
//! A stop suspends the context once no block renders, a start resumes it
//! (once the page has had a user gesture, as browsers require). The
//! processor holds its own reference to the device: a dropped output resumes
//! the context for one silent block, in which the processor lets the device
//! go and ends its thread, and the context closes after it (or after a
//! second, when it does not run again).

use std::cell::UnsafeCell;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use wasm_bindgen::prelude::{JsValue, wasm_bindgen};

use super::{AudioError, AudioFormat, AudioRender};

/// The frames of a Web Audio render quantum.
const QUANTUM: usize = 128;
const CHANNELS: usize = 2;

/// Set while a block renders.
const BUSY: u32 = 1;
/// Set while nothing renders: the processor outputs silence.
const STOPPED: u32 = 2;
/// Set once the output is gone: the processor lets the device go.
const CLOSED: u32 = 4;

/// What [`__viso_audio`] answers when the block is silent.
const SILENT: u32 = 0;
/// What [`__viso_audio`] answers once it let the device go.
const ENDED: u32 = 1;

#[wasm_bindgen]
extern "C" {
    /// The glue's audio output: its context and worklet node.
    type WebAudio;

    #[wasm_bindgen(catch, js_name = visoAudioOpen)]
    fn open(module: &JsValue, memory: &JsValue, device: u32) -> Result<WebAudio, JsValue>;
    #[wasm_bindgen(method, getter, js_name = sampleRate)]
    fn sample_rate(this: &WebAudio) -> f64;
    #[wasm_bindgen(method)]
    fn start(this: &WebAudio);
    #[wasm_bindgen(method)]
    fn stop(this: &WebAudio);
    /// Runs the context until the processor ends, then closes it.
    #[wasm_bindgen(method)]
    fn end(this: &WebAudio);
}

/// What the page and the worklet's processor share.
///
/// The worklet reaches `render` and `block` only between setting [`BUSY`]
/// over a state without [`STOPPED`] and clearing it; the page touches
/// `render` only once it set [`STOPPED`] and saw [`BUSY`] clear.
struct Device {
    state: AtomicU32,
    render: UnsafeCell<Option<AudioRender>>,
    block: UnsafeCell<[f32; QUANTUM * CHANNELS]>,
}

// SAFETY: the state's handshake above gives `render` and `block` one user at
// a time, and the render is `Send`.
unsafe impl Sync for Device {}

pub(super) struct Output {
    device: Arc<Device>,
    audio: WebAudio,
}

impl Output {
    pub(super) fn open() -> Result<(Output, AudioFormat), AudioError> {
        let device = Arc::new(Device {
            state: AtomicU32::new(STOPPED),
            render: UnsafeCell::new(None),
            block: UnsafeCell::new([0.0; QUANTUM * CHANNELS]),
        });
        // The processor's reference, let go in `__viso_audio`.
        let shared = Arc::into_raw(Arc::clone(&device));
        let audio = match open(
            &wasm_bindgen::module(),
            &wasm_bindgen::memory(),
            shared as u32,
        ) {
            Ok(audio) => audio,
            Err(error) => {
                // SAFETY: no processor was made to hold it.
                drop(unsafe { Arc::from_raw(shared) });
                return Err(AudioError::Device(format!("{error:?}")));
            }
        };
        let format = AudioFormat {
            sample_rate: audio.sample_rate(),
            channels: CHANNELS,
            max_frames: QUANTUM,
        };
        Ok((Output { device, audio }, format))
    }

    pub(super) fn start(
        &mut self,
        render: AudioRender,
        _format: AudioFormat,
    ) -> Result<(), AudioError> {
        self.stop();
        // SAFETY: stopped, so the processor does not touch it.
        unsafe { *self.device.render.get() = Some(render) };
        self.device.state.fetch_and(!STOPPED, Ordering::Release);
        self.audio.start();
        Ok(())
    }

    pub(super) fn stop(&mut self) {
        if self.halt() {
            self.audio.stop();
        }
    }

    /// Sets [`STOPPED`] and waits out a rendering block, then drops the
    /// render; whether it was rendering.
    fn halt(&self) -> bool {
        let was = self.device.state.fetch_or(STOPPED, Ordering::AcqRel);
        // At most one block: the page may not wait, so it spins.
        while self.device.state.load(Ordering::Acquire) & BUSY != 0 {
            std::hint::spin_loop();
        }
        // SAFETY: stopped with no block rendering, so the processor no longer
        // touches it.
        drop(unsafe { (*self.device.render.get()).take() });
        was & STOPPED == 0
    }
}

impl Drop for Output {
    fn drop(&mut self) {
        self.halt();
        self.device.state.fetch_or(CLOSED, Ordering::Release);
        self.audio.end();
    }
}

/// The worklet's block: renders `frames` frames of `channels` channels for
/// the device at `device`, answering where the samples are, channel-major;
/// [`SILENT`] for silence, [`ENDED`] once the output is gone, after which the
/// processor ends.
#[wasm_bindgen]
pub fn __viso_audio(device: u32, frames: u32, channels: u32) -> u32 {
    let shared = device as usize as *const Device;
    // SAFETY: the processor holds a reference to the device until this lets
    // it go.
    let device = unsafe { &*shared };
    let state = device.state.fetch_or(BUSY, Ordering::Acquire);
    if state & CLOSED != 0 {
        device.state.fetch_and(!BUSY, Ordering::Release);
        // SAFETY: the processor's reference, which it no longer uses.
        drop(unsafe { Arc::from_raw(shared) });
        return ENDED;
    }
    let (frames, channels) = (frames as usize, channels as usize);
    let at = if state & STOPPED != 0 || frames * channels > QUANTUM * CHANNELS {
        SILENT
    } else {
        // SAFETY: busy over a running state: the page does not touch either.
        let (render, block) = unsafe { (&mut *device.render.get(), &mut *device.block.get()) };
        match render {
            Some(render) => {
                let block = &mut block[..frames * channels];
                render(block);
                block.as_ptr() as u32
            }
            None => SILENT,
        }
    };
    device.state.fetch_and(!BUSY, Ordering::Release);
    at
}
