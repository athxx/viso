//! `viso-media` — media played through the OS.
//!
//! A [`VideoPlayer`] plays a file, a URL or bytes through the platform's own
//! player: macOS and iOS through AVFoundation, Windows through Media
//! Foundation's media engine, Linux and the BSDs through GStreamer (opened at
//! run time, so a system without it only fails to open videos), Android
//! through `MediaPlayer` (once the activity is handed over,
//! [`attach_android`]), the web through a hidden `<video>` element. The
//! platform decodes in
//! hardware, keeps sound and picture in step and drives the audio device; the
//! caller polls it from the thread that runs the loop for what changed
//! ([`VideoPlayer::poll`]) and for the picture to show now
//! ([`VideoPlayer::frame`]), and steers it with play, pause, seek, rate,
//! volume and looping. A target without a player opens nothing:
//! [`VideoError::Unsupported`].

#![forbid(unsafe_op_in_unsafe_fn)]

mod video;

#[cfg(target_os = "android")]
pub use video::attach_android;
pub use video::{
    CameraChoice, CameraFacing, CameraInfo, CameraRequest, Frame, PixelLayout, VideoError,
    VideoEvent, VideoInfo, VideoPlayer, VideoSource, cameras,
};
pub use viso_handle::{Nv12, YuvMatrix};
