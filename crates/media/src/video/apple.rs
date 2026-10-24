//! macOS and iOS: an `AVPlayer` whose item hands its pictures to an
//! `AVPlayerItemVideoOutput` as BGRA pixel buffers, read where they lie.

use std::ptr::NonNull;
use std::time::Duration;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{AnyThread, MainThreadMarker};
use objc2_av_foundation::{
    AVMediaTypeAudio, AVPlayer, AVPlayerItem, AVPlayerItemStatus, AVPlayerItemVideoOutput,
    AVPlayerTimeControlStatus,
};
use objc2_core_foundation::CFString;
use objc2_core_media::{CMTime, CMTimeFlags, kCMTimeZero};
use objc2_core_video::{
    CVPixelBuffer, CVPixelBufferGetBaseAddress, CVPixelBufferGetBytesPerRow,
    CVPixelBufferGetHeight, CVPixelBufferGetPixelFormatType, CVPixelBufferGetWidth,
    CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress,
    kCVPixelBufferIOSurfacePropertiesKey, kCVPixelBufferMetalCompatibilityKey,
    kCVPixelBufferPixelFormatTypeKey, kCVPixelFormatType_32BGRA,
};
use objc2_foundation::{NSDictionary, NSNumber, NSString, NSURL};

use super::{Frame, PixelLayout, Spilled, VideoError, VideoEvent, VideoInfo, VideoSource};

unsafe extern "C" {
    fn mach_absolute_time() -> u64;
}

/// Where the player is in opening.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    Loading,
    Ready,
    Failed,
}

pub(crate) struct Player {
    player: Retained<AVPlayer>,
    item: Retained<AVPlayerItem>,
    output: Retained<AVPlayerItemVideoOutput>,
    /// The file bytes were written to, removed with the player.
    _spilled: Option<Spilled>,
    status: Status,
    /// Whether it was asked to play and has not ended since.
    wants_play: bool,
    rate: f32,
    looping: bool,
    /// Whether it last played to its end.
    ended: bool,
    /// Whether it last waited for data.
    waiting: bool,
    /// The picture last returned, locked for reading until the next.
    held: Option<Retained<CVPixelBuffer>>,
    held_time: Duration,
}

impl Player {
    pub(crate) fn open(source: &VideoSource) -> Result<Player, VideoError> {
        let mtm = MainThreadMarker::new().ok_or(VideoError::WrongThread)?;
        let (url, spilled) = match source {
            VideoSource::File(path) => (file_url(path)?, None),
            VideoSource::Url(url) => {
                let url = NSURL::URLWithString(&NSString::from_str(url))
                    .ok_or_else(|| VideoError::Open(format!("`{url}` is not a URL")))?;
                (url, None)
            }
            VideoSource::Bytes(bytes) => spill(bytes)?,
            VideoSource::Static(bytes) => spill(bytes)?,
        };
        // SAFETY: each call is made on the main thread with live objects,
        // as AVFoundation documents them; the dictionary's keys are the
        // CoreVideo attribute names, toll-free bridged to `NSString`.
        unsafe {
            let item = AVPlayerItem::playerItemWithURL(&url, mtm);
            let output = AVPlayerItemVideoOutput::initWithPixelBufferAttributes(
                AVPlayerItemVideoOutput::alloc(),
                Some(&bgra_attributes()),
            );
            item.addOutput(&output);
            let player = AVPlayer::playerWithPlayerItem(Some(&item), mtm);
            Ok(Player {
                player,
                item,
                output,
                _spilled: spilled,
                status: Status::Loading,
                wants_play: false,
                rate: 1.0,
                looping: false,
                ended: false,
                waiting: false,
                held: None,
                held_time: Duration::ZERO,
            })
        }
    }

    pub(crate) fn poll(&mut self) -> Option<VideoEvent> {
        // SAFETY: reads and writes of the player's and item's properties on
        // the thread that made them.
        unsafe {
            match self.status {
                Status::Failed => None,
                Status::Loading => match self.item.status() {
                    AVPlayerItemStatus::ReadyToPlay => {
                        self.status = Status::Ready;
                        if self.wants_play {
                            self.player.setRate(self.rate);
                        } else {
                            // A paused player shows its first picture.
                            self.player.seekToTime_toleranceBefore_toleranceAfter(
                                kCMTimeZero,
                                kCMTimeZero,
                                kCMTimeZero,
                            );
                        }
                        Some(VideoEvent::Ready(self.info()))
                    }
                    AVPlayerItemStatus::Failed => {
                        self.status = Status::Failed;
                        Some(VideoEvent::Failed(VideoError::Open(self.error())))
                    }
                    _ => None,
                },
                Status::Ready => {
                    if self.item.status() == AVPlayerItemStatus::Failed {
                        self.status = Status::Failed;
                        return Some(VideoEvent::Failed(VideoError::Play(self.error())));
                    }
                    let control = self.player.timeControlStatus();
                    if self.wants_play
                        && control == AVPlayerTimeControlStatus::Paused
                        && self.at_end()
                    {
                        if self.looping {
                            self.player.seekToTime_toleranceBefore_toleranceAfter(
                                kCMTimeZero,
                                kCMTimeZero,
                                kCMTimeZero,
                            );
                            self.player.setRate(self.rate);
                            return None;
                        }
                        self.wants_play = false;
                        self.ended = true;
                        return Some(VideoEvent::Ended);
                    }
                    let waiting = self.wants_play
                        && control == AVPlayerTimeControlStatus::WaitingToPlayAtSpecifiedRate;
                    if waiting != self.waiting {
                        self.waiting = waiting;
                        return Some(VideoEvent::Waiting(waiting));
                    }
                    None
                }
            }
        }
    }

    pub(crate) fn frame(&mut self) -> Option<Frame<'_>> {
        if self.status != Status::Ready {
            return None;
        }
        // SAFETY: the output belongs to the player's item; a pixel buffer it
        // hands over is retained by `Retained`, locked for reading before
        // its base address is read and unlocked once the next replaces it.
        unsafe {
            let now = self
                .output
                .itemTimeForMachAbsoluteTime(mach_absolute_time() as i64);
            if !self.output.hasNewPixelBufferForItemTime(now) {
                return None;
            }
            let mut shown = now;
            let buffer = self
                .output
                .copyPixelBufferForItemTime_itemTimeForDisplay(now, &mut shown)?;
            if CVPixelBufferGetPixelFormatType(&buffer) != kCVPixelFormatType_32BGRA {
                return None;
            }
            self.release_held();
            if CVPixelBufferLockBaseAddress(&buffer, CVPixelBufferLockFlags::ReadOnly) != 0 {
                return None;
            }
            self.held_time = seconds(shown).unwrap_or(self.held_time);
            self.held = Some(buffer);
            let buffer = self.held.as_deref()?;
            let base = CVPixelBufferGetBaseAddress(buffer);
            let stride = CVPixelBufferGetBytesPerRow(buffer);
            let (width, height) = (
                CVPixelBufferGetWidth(buffer),
                CVPixelBufferGetHeight(buffer),
            );
            let base = NonNull::new(base.cast::<u8>())?;
            Some(Frame {
                width: width as u32,
                height: height as u32,
                layout: PixelLayout::Bgra8,
                rows: std::slice::from_raw_parts(base.as_ptr(), stride * height),
                stride,
                time: self.held_time,
            })
        }
    }

    pub(crate) fn play(&mut self) {
        if self.ended {
            self.ended = false;
            // SAFETY: a seek of the player on its thread.
            unsafe {
                self.player.seekToTime_toleranceBefore_toleranceAfter(
                    kCMTimeZero,
                    kCMTimeZero,
                    kCMTimeZero,
                );
            }
        }
        self.wants_play = true;
        if self.status == Status::Ready {
            // SAFETY: a rate change of the player on its thread.
            unsafe { self.player.setRate(self.rate) };
        }
    }

    pub(crate) fn pause(&mut self) {
        self.wants_play = false;
        self.waiting = false;
        // SAFETY: a pause of the player on its thread.
        unsafe { self.player.pause() };
    }

    pub(crate) fn seek(&mut self, time: Duration) {
        self.ended = false;
        // SAFETY: a seek of the player on its thread, exact to the frame.
        unsafe {
            let to = CMTime::with_seconds(time.as_secs_f64(), 600);
            self.player
                .seekToTime_toleranceBefore_toleranceAfter(to, kCMTimeZero, kCMTimeZero);
        }
    }

    pub(crate) fn set_rate(&mut self, rate: f32) {
        self.rate = rate;
        if self.wants_play && self.status == Status::Ready {
            // SAFETY: a rate change of the player on its thread.
            unsafe { self.player.setRate(rate) };
        }
    }

    pub(crate) fn set_volume(&mut self, volume: f32) {
        // SAFETY: a volume change of the player on its thread.
        unsafe { self.player.setVolume(volume) };
    }

    pub(crate) fn set_muted(&mut self, muted: bool) {
        // SAFETY: a mute of the player on its thread.
        unsafe { self.player.setMuted(muted) };
    }

    pub(crate) fn set_looping(&mut self, looping: bool) {
        self.looping = looping;
    }

    pub(crate) fn position(&self) -> Duration {
        // SAFETY: a read of the player's time on its thread.
        unsafe { seconds(self.player.currentTime()) }.unwrap_or_default()
    }

    pub(crate) fn is_playing(&self) -> bool {
        self.wants_play
    }

    /// What the item knows of its video once ready.
    ///
    /// # Safety
    ///
    /// On the item's thread, once it is ready.
    unsafe fn info(&self) -> VideoInfo {
        // SAFETY: reads of the ready item and its tracks, as the caller
        // promises.
        unsafe {
            let size = self.item.presentationSize();
            let mut frame_rate = None;
            let mut audio = false;
            for track in self.item.tracks().iter() {
                let Some(asset) = track.assetTrack() else {
                    continue;
                };
                let kind = asset.mediaType();
                if AVMediaTypeAudio.is_some_and(|a| *kind == *a) {
                    audio = true;
                } else if frame_rate.is_none() && asset.nominalFrameRate() > 0.0 {
                    frame_rate = Some(asset.nominalFrameRate());
                }
            }
            let duration = seconds(self.item.duration());
            VideoInfo {
                width: size.width.round() as u32,
                height: size.height.round() as u32,
                duration,
                frame_rate,
                audio,
                seekable: duration.is_some() && self.item.seekableTimeRanges().count() > 0,
            }
        }
    }

    /// Whether the player stands at its item's end.
    ///
    /// # Safety
    ///
    /// On the player's thread.
    unsafe fn at_end(&self) -> bool {
        // SAFETY: reads of the item's times, as the caller promises.
        let (now, end) = unsafe {
            (
                seconds(self.item.currentTime()),
                seconds(self.item.duration()),
            )
        };
        match (now, end) {
            (Some(now), Some(end)) => now + Duration::from_millis(1) >= end,
            _ => false,
        }
    }

    /// The item's error, described.
    ///
    /// # Safety
    ///
    /// On the item's thread.
    unsafe fn error(&self) -> String {
        // SAFETY: a read of the item's error, as the caller promises.
        unsafe { self.item.error() }
            .map(|e| e.localizedDescription().to_string())
            .unwrap_or_else(|| "the player failed".to_owned())
    }

    fn release_held(&mut self) {
        if let Some(buffer) = self.held.take() {
            // SAFETY: the held buffer was locked read-only when it was taken.
            unsafe {
                CVPixelBufferUnlockBaseAddress(&buffer, CVPixelBufferLockFlags::ReadOnly);
            }
        }
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.release_held();
        // SAFETY: the player stops and its output leaves the item on the
        // thread that made them.
        unsafe {
            self.player.pause();
            self.item.removeOutput(&self.output);
            self.player.replaceCurrentItemWithPlayerItem(None);
        }
    }
}

/// A time in seconds; `None` for one invalid, indefinite or negative.
fn seconds(time: CMTime) -> Option<Duration> {
    if !time.flags.contains(CMTimeFlags::Valid) || time.flags.contains(CMTimeFlags::Indefinite) {
        return None;
    }
    // SAFETY: a pure conversion of a valid time.
    let s = unsafe { time.seconds() };
    (s.is_finite() && s >= 0.0).then(|| Duration::from_secs_f64(s))
}

/// `bytes` written to a file of their own, and its URL.
fn spill(bytes: &[u8]) -> Result<(Retained<NSURL>, Option<Spilled>), VideoError> {
    let spilled = Spilled::new(bytes).map_err(|e| VideoError::Open(e.to_string()))?;
    Ok((file_url(spilled.path())?, Some(spilled)))
}

fn file_url(path: &std::path::Path) -> Result<Retained<NSURL>, VideoError> {
    let path = path
        .to_str()
        .ok_or_else(|| VideoError::Open(format!("`{}` is not UTF-8", path.display())))?;
    Ok(NSURL::fileURLWithPath(&NSString::from_str(path)))
}

/// Pixel buffers in BGRA, backed by IOSurfaces Metal can sample.
fn bgra_attributes() -> Retained<NSDictionary<NSString, AnyObject>> {
    // SAFETY: CoreVideo's attribute names are constant `CFString`s,
    // toll-free bridged to `NSString`.
    let key = |name: &'static CFString| -> &'static NSString {
        unsafe { &*(name as *const CFString).cast::<NSString>() }
    };
    let format = NSNumber::new_u32(kCVPixelFormatType_32BGRA);
    let metal = NSNumber::new_bool(true);
    let surface = NSDictionary::<NSString, AnyObject>::new();
    // SAFETY: the statics are CoreVideo's, alive for the program.
    let keys = unsafe {
        [
            key(kCVPixelBufferPixelFormatTypeKey),
            key(kCVPixelBufferMetalCompatibilityKey),
            key(kCVPixelBufferIOSurfacePropertiesKey),
        ]
    };
    let values: [&AnyObject; 3] = [&format, &metal, &surface];
    NSDictionary::from_slices(&keys, &values)
}
