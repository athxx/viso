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

use std::fmt;

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
/// [`started`](Self::start) and stops when dropped.
pub struct AudioOutput {
    #[cfg(any(
        target_vendor = "apple",
        target_os = "windows",
        target_os = "linux",
        target_os = "android"
    ))]
    device: imp::Output,
    format: AudioFormat,
}

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
            Ok(AudioOutput { device, format })
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
        #[cfg(any(
            target_vendor = "apple",
            target_os = "windows",
            target_os = "linux",
            target_os = "android"
        ))]
        {
            self.device.start(Box::new(render), self.format)
        }
        #[cfg(not(any(
            target_vendor = "apple",
            target_os = "windows",
            target_os = "linux",
            target_os = "android"
        )))]
        {
            let _ = render;
            Err(AudioError::Unsupported)
        }
    }

    /// Stops rendering; [`start`](Self::start) renders again.
    pub fn stop(&mut self) {
        #[cfg(any(
            target_vendor = "apple",
            target_os = "windows",
            target_os = "linux",
            target_os = "android"
        ))]
        self.device.stop();
    }
}

impl fmt::Debug for AudioOutput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AudioOutput")
            .field("format", &self.format)
            .finish()
    }
}
