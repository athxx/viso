//! Audio output: the default output device, rendering on the thread the OS
//! drives it from. The render callback fills each block of float samples
//! channel-major — each channel's frames in turn — and runs under realtime
//! rules: the backend calls it without allocating, locking or blocking, and
//! it must do the same. A panic in it silences the output for good.
//!
//! macOS and iOS render through an output Audio Unit (`AudioToolbox`),
//! Windows through WASAPI, Linux through ALSA and Android through AAudio
//! (both loaded at runtime); the web and the BSDs report
//! [`AudioError::Unsupported`].

#[cfg(target_os = "android")]
mod aaudio;
#[cfg(target_os = "linux")]
mod alsa;
#[cfg(target_vendor = "apple")]
mod apple;
#[cfg(target_os = "windows")]
mod wasapi;

#[cfg(target_os = "android")]
use aaudio as imp;
#[cfg(target_os = "linux")]
use alsa as imp;
#[cfg(target_vendor = "apple")]
use apple as imp;
#[cfg(target_os = "windows")]
use wasapi as imp;

use std::cell::UnsafeCell;
use std::fmt;
use std::sync::Arc;

/// What a device renders: its rate, its channels and the most frames one
/// block holds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AudioFormat {
    pub sample_rate: f64,
    pub channels: usize,
    pub max_frames: usize,
}

/// Why audio output is not available.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AudioError {
    /// This target has no audio output backend.
    Unsupported,
    /// The OS refused: what and its status.
    Device(String),
}

impl fmt::Display for AudioError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AudioError::Unsupported => f.write_str("audio output is not supported on this target"),
            AudioError::Device(why) => write!(f, "the audio device failed: {why}"),
        }
    }
}

impl std::error::Error for AudioError {}

/// What renders each block: `output` holds `output.len() / channels`
/// frames, channel-major.
pub type AudioRender = Box<dyn FnMut(&mut [f32]) + Send>;

/// The default output device, opened; it renders once
/// [`started`](Self::start), [`pause`](Self::pause)s and
/// [`resume`](Self::resume)s with what it renders kept, and stops when
/// dropped.
pub struct AudioOutput {
    #[cfg(any(
        target_vendor = "apple",
        target_os = "windows",
        target_os = "linux",
        target_os = "android"
    ))]
    device: imp::Output,
    format: AudioFormat,
    /// What renders, while started: kept across a pause.
    render: Option<Arc<RenderSlot>>,
    /// Whether the device renders it now.
    running: bool,
}

/// What renders, shared by the output and the device's callback.
///
/// The callback is the only user while the device runs and nothing uses it
/// while the device is stopped: the output stops the device (whose `stop`
/// returns once no render runs) before it starts it again or drops the
/// slot's last other owner.
#[cfg_attr(
    not(any(
        target_vendor = "apple",
        target_os = "windows",
        target_os = "linux",
        target_os = "android"
    )),
    expect(dead_code, reason = "no backend renders it")
)]
struct RenderSlot(UnsafeCell<AudioRender>);

// SAFETY: one thread at a time reaches the render, as above, and the render
// is `Send`.
unsafe impl Sync for RenderSlot {}

impl AudioOutput {
    /// Opens the default output device, stereo at its own rate.
    ///
    /// # Errors
    ///
    /// When the target has no backend or the device does not open.
    pub fn open() -> Result<AudioOutput, AudioError> {
        #[cfg(any(
            target_vendor = "apple",
            target_os = "windows",
            target_os = "linux",
            target_os = "android"
        ))]
        {
            let (device, format) = imp::Output::open()?;
            Ok(AudioOutput {
                device,
                format,
                render: None,
                running: false,
            })
        }
        #[cfg(not(any(
            target_vendor = "apple",
            target_os = "windows",
            target_os = "linux",
            target_os = "android"
        )))]
        {
            Err(AudioError::Unsupported)
        }
    }

    /// What the device renders.
    pub fn format(&self) -> AudioFormat {
        self.format
    }

    /// Starts rendering through `render`, replacing what rendered before.
    ///
    /// # Errors
    ///
    /// When the device does not start.
    pub fn start(
        &mut self,
        render: impl FnMut(&mut [f32]) + Send + 'static,
    ) -> Result<(), AudioError> {
        self.stop();
        self.render = Some(Arc::new(RenderSlot(UnsafeCell::new(Box::new(render)))));
        self.resume()
    }

    /// Stops rendering and keeps what renders, for [`resume`](Self::resume);
    /// whether it was rendering.
    pub fn pause(&mut self) -> bool {
        let was = self.running;
        if was {
            #[cfg(any(
                target_vendor = "apple",
                target_os = "windows",
                target_os = "linux",
                target_os = "android"
            ))]
            self.device.stop();
            self.running = false;
        }
        was
    }

    /// Renders again what rendered before a [`pause`](Self::pause); nothing
    /// when nothing was started or it renders already.
    ///
    /// # Errors
    ///
    /// When the device does not start.
    pub fn resume(&mut self) -> Result<(), AudioError> {
        let Some(slot) = self.render.as_ref().filter(|_| !self.running) else {
            return Ok(());
        };
        #[cfg(any(
            target_vendor = "apple",
            target_os = "windows",
            target_os = "linux",
            target_os = "android"
        ))]
        {
            let slot = Arc::clone(slot);
            let forward = move |block: &mut [f32]| {
                // SAFETY: the device is running, so its callback is the
                // render's only user (`RenderSlot`).
                let render = unsafe { &mut *slot.0.get() };
                render(block);
            };
            self.device.start(Box::new(forward), self.format)?;
            self.running = true;
            Ok(())
        }
        #[cfg(not(any(
            target_vendor = "apple",
            target_os = "windows",
            target_os = "linux",
            target_os = "android"
        )))]
        {
            let _ = slot;
            Err(AudioError::Unsupported)
        }
    }

    /// Whether it renders now: started and not paused.
    pub fn is_running(&self) -> bool {
        self.running
    }

    /// Stops rendering and drops what rendered; [`start`](Self::start)
    /// renders again.
    pub fn stop(&mut self) {
        self.pause();
        self.render = None;
    }
}

impl Drop for AudioOutput {
    fn drop(&mut self) {
        // The device stops before the render goes.
        self.pause();
    }
}

impl fmt::Debug for AudioOutput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AudioOutput")
            .field("format", &self.format)
            .finish()
    }
}
