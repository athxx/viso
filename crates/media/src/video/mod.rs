//! Video playback through the platform's player.

#[cfg(target_os = "android")]
mod android;
#[cfg(target_vendor = "apple")]
mod apple;
#[cfg(any(
    target_os = "linux",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly"
))]
mod gstreamer;
#[cfg(not(any(
    target_vendor = "apple",
    target_os = "android",
    target_os = "windows",
    all(target_arch = "wasm32", target_os = "unknown")
)))]
mod no_picture;
#[cfg(not(any(
    all(target_arch = "wasm32", target_os = "unknown"),
    target_os = "android",
    target_vendor = "apple",
    target_os = "windows",
    target_os = "linux",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly"
)))]
mod unsupported;
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
mod web;
#[cfg(target_os = "windows")]
mod windows;
#[cfg(any(target_os = "android", test))]
mod yuv;

#[cfg(target_os = "android")]
use android as imp;
#[cfg(target_vendor = "apple")]
use apple as imp;
#[cfg(any(
    target_os = "linux",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly"
))]
use gstreamer as imp;
#[cfg(not(any(
    all(target_arch = "wasm32", target_os = "unknown"),
    target_os = "android",
    target_vendor = "apple",
    target_os = "windows",
    target_os = "linux",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly"
)))]
use unsupported as imp;
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
use web as imp;
#[cfg(target_os = "windows")]
use windows as imp;

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// Where a video plays from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VideoSource {
    /// A file.
    File(PathBuf),
    /// A URL the platform's player fetches itself: `http(s)://`, an HLS
    /// playlist, `file://`.
    Url(String),
    /// The encoded bytes of a whole file, in memory.
    Bytes(Arc<[u8]>),
    /// The encoded bytes of a whole file, embedded in the program.
    Static(&'static [u8]),
}

/// What a player knows of its video once it is ready.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VideoInfo {
    /// The picture's width in pixels, as shown: turned as the file says.
    pub width: u32,
    /// The picture's height in pixels, as shown.
    pub height: u32,
    /// How long it plays; `None` for a live stream.
    pub duration: Option<Duration>,
    /// The frames it shows a second, when the file says.
    pub frame_rate: Option<f32>,
    /// Whether it has sound.
    pub audio: bool,
    /// Whether it can be sought.
    pub seekable: bool,
}

/// What changed about a player since it was last polled.
#[derive(Debug, Clone, PartialEq)]
pub enum VideoEvent {
    /// It opened and can play; its first frame follows.
    Ready(VideoInfo),
    /// It played to its end and stopped there. A looping player starts over
    /// instead.
    Ended,
    /// It could not open or could not go on playing.
    Failed(VideoError),
    /// It is waiting for data to play on (`true`), or plays again (`false`).
    Waiting(bool),
}

/// Why a video does not play.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VideoError {
    /// This target has no player.
    Unsupported,
    /// It must be opened on the thread that runs the loop.
    WrongThread,
    /// Its source could not be opened.
    Open(String),
    /// It opened, then could not go on playing.
    Play(String),
}

impl fmt::Display for VideoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VideoError::Unsupported => f.write_str("this target plays no video"),
            VideoError::WrongThread => f.write_str("a video opens on the loop's thread"),
            VideoError::Open(why) => write!(f, "the video could not be opened: {why}"),
            VideoError::Play(why) => write!(f, "the video stopped playing: {why}"),
        }
    }
}

impl std::error::Error for VideoError {}

/// How a frame's texels are laid out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelLayout {
    /// Four bytes a texel — blue, green, red, alpha — sRGB-encoded and
    /// opaque.
    Bgra8,
    /// Four bytes a texel — red, green, blue, alpha — sRGB-encoded and
    /// opaque.
    Rgba8,
}

/// A picture to show, valid until the player's next: texels at hand, or
/// memory a GPU samples in place ([`native`](Self::native)), read on the CPU
/// only when copied out.
#[derive(Debug, Clone, Copy)]
pub struct Frame<'a> {
    pub width: u32,
    pub height: u32,
    /// Where in the video it shows.
    pub time: Duration,
    texels: Texels<'a>,
}

/// Where a frame's texels lie.
#[derive(Debug, Clone, Copy)]
enum Texels<'a> {
    /// At hand, in `layout`, row after row from the top, each `stride` bytes
    /// apart.
    Rows {
        layout: PixelLayout,
        rows: &'a [u8],
        stride: usize,
    },
    /// In a buffer of the platform's, read on the CPU only to be copied.
    #[cfg_attr(
        not(any(
            target_vendor = "apple",
            target_os = "android",
            target_os = "windows",
            all(target_arch = "wasm32", target_os = "unknown")
        )),
        allow(dead_code)
    )]
    Native(imp::Picture<'a>),
    /// In GPU memory alone, which the CPU does not read.
    External(viso_handle::ExternalImage),
}

impl<'a> Frame<'a> {
    /// A picture of `width` × `height` BGRA texels (`PixelLayout::Bgra8`),
    /// row after row from the top in `rows`, each `stride` bytes apart, shown
    /// at `time`.
    pub fn bgra(width: u32, height: u32, rows: &'a [u8], stride: usize, time: Duration) -> Self {
        Frame {
            width,
            height,
            time,
            texels: Texels::Rows {
                layout: PixelLayout::Bgra8,
                rows,
                stride,
            },
        }
    }

    /// A picture of `width` × `height` RGBA texels (`PixelLayout::Rgba8`),
    /// row after row from the top in `rows`, each `stride` bytes apart, shown
    /// at `time`.
    pub fn rgba(width: u32, height: u32, rows: &'a [u8], stride: usize, time: Duration) -> Self {
        Frame {
            width,
            height,
            time,
            texels: Texels::Rows {
                layout: PixelLayout::Rgba8,
                rows,
                stride,
            },
        }
    }

    /// A picture of `width` × `height` texels in GPU memory alone, as
    /// `image` names it, shown at `time`: only a backend that adopts it shows
    /// it.
    pub fn external(
        width: u32,
        height: u32,
        image: viso_handle::ExternalImage,
        time: Duration,
    ) -> Self {
        Frame {
            width,
            height,
            time,
            texels: Texels::External(image),
        }
    }

    #[cfg_attr(
        not(any(
            target_vendor = "apple",
            target_os = "android",
            target_os = "windows",
            all(target_arch = "wasm32", target_os = "unknown")
        )),
        allow(dead_code)
    )]
    pub(crate) fn native_picture(
        width: u32,
        height: u32,
        time: Duration,
        picture: imp::Picture<'a>,
    ) -> Self {
        Frame {
            width,
            height,
            time,
            texels: Texels::Native(picture),
        }
    }

    /// How its texels are laid out when copied.
    pub fn layout(&self) -> PixelLayout {
        match self.texels {
            Texels::Rows { layout, .. } => layout,
            Texels::Native(picture) => picture.layout(),
            Texels::External(_) => PixelLayout::Bgra8,
        }
    }

    /// The memory it lies in, for a GPU to sample in place; `None` for one
    /// only a copy shows.
    pub fn native(&self) -> Option<viso_handle::ExternalImage> {
        match self.texels {
            Texels::Rows { .. } => None,
            Texels::Native(picture) => picture.external(self.width, self.height),
            Texels::External(image) => Some(image),
        }
    }

    /// Writes its texels into `out`, tightly packed, in RGBA order. Returns
    /// whether they could be read: not for a picture in GPU memory alone.
    pub fn copy_rgba(&self, out: &mut Vec<u8>) -> bool {
        let (w, h) = (self.width as usize, self.height as usize);
        let copy = |layout: PixelLayout, rows: &[u8], stride: usize, out: &mut Vec<u8>| {
            out.clear();
            out.reserve(w * h * 4);
            for row in rows.chunks(stride).take(h) {
                let row = &row[..w * 4];
                match layout {
                    PixelLayout::Rgba8 => out.extend_from_slice(row),
                    PixelLayout::Bgra8 => {
                        for texel in row.as_chunks::<4>().0 {
                            out.extend_from_slice(&[texel[2], texel[1], texel[0], texel[3]]);
                        }
                    }
                }
            }
        };
        match self.texels {
            Texels::Rows {
                layout,
                rows,
                stride,
            } => {
                copy(layout, rows, stride, out);
                true
            }
            Texels::Native(picture) => {
                picture.read(|rows, stride| copy(picture.layout(), rows, stride, out))
            }
            Texels::External(_) => false,
        }
    }
}

/// A video played through the platform's player. Opened, polled and steered
/// from the thread that runs the loop.
///
/// It opens paused, at the start; [`play`](Self::play) starts it once ready
/// or at once.
pub struct VideoPlayer {
    imp: imp::Player,
}

impl fmt::Debug for VideoPlayer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VideoPlayer")
            .field("position", &self.position())
            .field("playing", &self.is_playing())
            .finish()
    }
}

impl VideoPlayer {
    /// Opens `source`. It loads off the caller's thread; [`poll`](Self::poll)
    /// tells when it is ready or failed.
    pub fn open(source: &VideoSource) -> Result<VideoPlayer, VideoError> {
        imp::Player::open(source).map(|imp| VideoPlayer { imp })
    }

    /// What changed since the last poll, one event a call; `None` when
    /// nothing did.
    pub fn poll(&mut self) -> Option<VideoEvent> {
        self.imp.poll()
    }

    /// The picture to show now, when it is not the one this returned last.
    pub fn frame(&mut self) -> Option<Frame<'_>> {
        self.imp.frame()
    }

    /// Plays from where it is, at its rate; from the start once it ended.
    pub fn play(&mut self) {
        self.imp.play();
    }

    /// Holds it where it is.
    pub fn pause(&mut self) {
        self.imp.pause();
    }

    /// Moves it to `time`, clamped to its length, the picture there shown
    /// even while paused.
    pub fn seek(&mut self, time: Duration) {
        self.imp.seek(time);
    }

    /// Plays at `rate` times its speed (defaults to 1).
    pub fn set_rate(&mut self, rate: f32) {
        self.imp.set_rate(rate.clamp(0.0625, 16.0));
    }

    /// Plays its sound at `volume`, from 0 to 1 (defaults to 1).
    pub fn set_volume(&mut self, volume: f32) {
        self.imp.set_volume(volume.clamp(0.0, 1.0));
    }

    /// Silences it, or lets it sound.
    pub fn set_muted(&mut self, muted: bool) {
        self.imp.set_muted(muted);
    }

    /// Starts it over each time it ends, or lets it end.
    pub fn set_looping(&mut self, looping: bool) {
        self.imp.set_looping(looping);
    }

    /// Where it is.
    pub fn position(&self) -> Duration {
        self.imp.position()
    }

    /// Whether it plays, or will once ready: asked to, and not ended.
    pub fn is_playing(&self) -> bool {
        self.imp.is_playing()
    }
}

/// Android: gives the players what they need of the app's activity — the
/// Java VM, the player class from the APK, the application context. Players
/// open as `Unsupported` until it is called; calls after the first do
/// nothing. The app's framework calls it.
///
/// # Safety
///
/// `env` is the calling thread's `JNIEnv` and `activity` a live local or
/// global reference to the app's activity.
#[cfg(target_os = "android")]
pub unsafe fn attach_android(env: *mut std::ffi::c_void, activity: *mut std::ffi::c_void) {
    // SAFETY: the caller's contract.
    unsafe { android::attach(env.cast(), activity.cast()) }
}

/// The `file://` URI of `path`, made absolute, every byte outside the
/// unreserved set and `/` escaped.
#[cfg(all(unix, not(target_vendor = "apple")))]
fn file_uri(path: &Path) -> String {
    use std::fmt::Write;
    use std::os::unix::ffi::OsStrExt;
    let path = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let mut uri = String::from("file://");
    for &b in path.as_os_str().as_bytes() {
        if b.is_ascii_alphanumeric() || b"/-._~".contains(&b) {
            uri.push(b as char);
        } else {
            let _ = write!(uri, "%{b:02X}");
        }
    }
    uri
}

/// Bytes written to a file of their own for a player that reads only
/// files, removed when dropped.
pub(crate) struct Spilled {
    pub(crate) path: PathBuf,
}

impl Spilled {
    /// `bytes` in a new file of the temporary directory, named for the
    /// container they are in.
    #[cfg_attr(
        not(any(
            target_vendor = "apple",
            windows,
            all(unix, not(target_os = "android"))
        )),
        allow(dead_code)
    )]
    pub(crate) fn new(bytes: &[u8]) -> std::io::Result<Spilled> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let name = format!(
            "viso-video-{}-{}.{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
            extension(bytes)
        );
        let path = std::env::temp_dir().join(name);
        std::fs::write(&path, bytes)?;
        Ok(Spilled { path })
    }

    #[cfg_attr(
        not(any(
            target_vendor = "apple",
            windows,
            all(unix, not(target_os = "android"))
        )),
        allow(dead_code)
    )]
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Spilled {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// The file extension of the container `bytes` are in, read from its
/// signature; `mp4` for one it does not know.
fn extension(bytes: &[u8]) -> &'static str {
    if bytes.starts_with(&[0x1a, 0x45, 0xdf, 0xa3]) {
        // Matroska and WebM share the EBML header; the doc type names which.
        let head = &bytes[..bytes.len().min(64)];
        return if head.windows(4).any(|w| w == b"webm") {
            "webm"
        } else {
            "mkv"
        };
    }
    if bytes.starts_with(b"OggS") {
        return "ogv";
    }
    if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"AVI " {
        return "avi";
    }
    if bytes.len() >= 12 && &bytes[4..8] == b"ftyp" && &bytes[8..12] == b"qt  " {
        return "mov";
    }
    "mp4"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(all(unix, not(target_vendor = "apple")))]
    #[test]
    fn a_path_becomes_an_escaped_file_uri() {
        assert_eq!(
            file_uri(Path::new("/tmp/a b/clip#1.mp4")),
            "file:///tmp/a%20b/clip%231.mp4"
        );
    }

    #[test]
    fn spilled_bytes_are_named_for_their_container_and_removed_when_dropped() {
        assert_eq!(extension(b"\x1a\x45\xdf\xa3\x01\x00webm"), "webm");
        assert_eq!(extension(b"OggS\0"), "ogv");
        assert_eq!(extension(b"\0\0\0\x14ftypqt  \0\0"), "mov");
        assert_eq!(extension(b"\0\0\0\x18ftypisom\0\0"), "mp4");
        let spilled = Spilled::new(b"\0\0\0\x18ftypisom").unwrap();
        let path = spilled.path.clone();
        assert_eq!(path.extension().unwrap(), "mp4");
        assert!(path.exists());
        drop(spilled);
        assert!(!path.exists());
    }

    #[test]
    fn an_rgba_frame_copies_out_tightly_packed_as_it_lies() {
        let rows = [1, 2, 3, 255, 0, 0, 0, 0, 4, 5, 6, 255, 0, 0, 0, 0];
        let frame = Frame::rgba(1, 2, &rows, 8, Duration::ZERO);
        assert_eq!(frame.layout(), PixelLayout::Rgba8);
        let mut out = Vec::new();
        assert!(frame.copy_rgba(&mut out));
        assert_eq!(out, [1, 2, 3, 255, 4, 5, 6, 255]);
    }

    #[test]
    fn a_bgra_frame_copies_out_tightly_packed_in_rgba_order() {
        // Two texels a row, rows padded to twelve bytes.
        let rows = [
            1, 2, 3, 255, 4, 5, 6, 255, 0, 0, 0, 0, //
            7, 8, 9, 255, 10, 11, 12, 255, 0, 0, 0, 0,
        ];
        let frame = Frame::bgra(2, 2, &rows, 12, Duration::ZERO);
        assert!(frame.native().is_none());
        let mut out = Vec::new();
        assert!(frame.copy_rgba(&mut out));
        assert_eq!(
            out,
            [3, 2, 1, 255, 6, 5, 4, 255, 9, 8, 7, 255, 12, 11, 10, 255]
        );
    }
}
