//! Windows: Media Foundation's media engine, decoding in hardware through a
//! Direct3D 11 device of its own, each frame handed over as BGRA rows.
//!
//! The engine runs on a thread of its own in the multithreaded apartment:
//! called from the loop's thread — a single-threaded apartment — it can stall
//! the message pump while it buffers. The player posts the thread what is
//! asked of it and reads back, without a lock, what the engine reports and
//! the latest picture. The thread wakes for each request and each of the
//! engine's notifications, and while the video plays, once a frame
//! interval: it asks the engine for a new frame, draws it into a render
//! target, and reads the target back through a staging texture into a
//! buffer the player hands back once it has shown the next.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HMODULE, RECT};
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE, D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_CPU_ACCESS_READ,
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_MAP_READ,
    D3D11_MAPPED_SUBRESOURCE, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
    D3D11_USAGE_STAGING, D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Multithread,
    ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Media::MediaFoundation::{
    CLSID_MFMediaEngineClassFactory, IMFAttributes, IMFDXGIDeviceManager, IMFMediaEngine,
    IMFMediaEngineClassFactory, IMFMediaEngineNotify, IMFMediaEngineNotify_Impl,
    MF_MEDIA_ENGINE_CALLBACK, MF_MEDIA_ENGINE_DXGI_MANAGER, MF_MEDIA_ENGINE_EVENT_CANPLAY,
    MF_MEDIA_ENGINE_EVENT_ENDED, MF_MEDIA_ENGINE_EVENT_ERROR, MF_MEDIA_ENGINE_EVENT_FORMATCHANGE,
    MF_MEDIA_ENGINE_EVENT_PLAYING, MF_MEDIA_ENGINE_EVENT_SEEKED, MF_MEDIA_ENGINE_EVENT_STALLED,
    MF_MEDIA_ENGINE_EVENT_WAITING, MF_MEDIA_ENGINE_VIDEO_OUTPUT_FORMAT, MF_VERSION, MFARGB,
    MFCreateAttributes, MFCreateDXGIDeviceManager, MFSTARTUP_FULL, MFShutdown, MFStartup,
};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize,
};
use windows::core::{BSTR, Interface, implement};

use super::{Frame, Spilled, VideoError, VideoEvent, VideoInfo, VideoSource};

pub(crate) use super::no_picture::Picture;

/// How long the thread looks for a frame wanted while paused.
const DUE_FOR: Duration = Duration::from_secs(2);

/// What the player asks of its thread.
enum Ask {
    Play,
    Pause,
    Seek(Duration),
    Rate(f32),
    Volume(f32),
    Muted(bool),
    Looping(bool),
    /// One of the engine's notifications.
    Heard(u32),
    Close,
}

/// A picture read back, in BGRA rows `width * 4` bytes long.
struct Shot {
    rows: Vec<u8>,
    width: u32,
    height: u32,
    time: Duration,
}

pub(crate) struct Player {
    asks: Sender<Ask>,
    events: Receiver<VideoEvent>,
    shots: Receiver<Shot>,
    /// Buffers shown and done with, for the thread to fill again.
    spent: Sender<Vec<u8>>,
    /// The picture last handed out, and whether the next call hands it out.
    shown: Option<Shot>,
    fresh: bool,
    /// Where the engine is, in nanoseconds, as the thread last read it.
    position: Arc<AtomicU64>,
    /// Whether it was asked to play and has not ended since.
    wants_play: bool,
    looping: bool,
    ended: bool,
    thread: Option<JoinHandle<()>>,
    /// The file bytes were written to, removed once the thread is gone.
    _spilled: Option<Spilled>,
}

impl Player {
    pub(crate) fn open(source: &VideoSource) -> Result<Player, VideoError> {
        let (url, spilled) = match source {
            VideoSource::File(path) => (file_url(path), None),
            VideoSource::Url(url) => (url.clone(), None),
            VideoSource::Bytes(bytes) => spill(bytes)?,
            VideoSource::Static(bytes) => spill(bytes)?,
        };
        let (asks, asked) = mpsc::channel();
        let (tell, events) = mpsc::channel();
        let (send_shot, shots) = mpsc::channel();
        let (spent, refill) = mpsc::channel();
        let (opened, open) = mpsc::channel();
        let position = Arc::new(AtomicU64::new(0));
        let at = Arc::clone(&position);
        let heard = asks.clone();
        let thread = std::thread::Builder::new()
            .name("viso-video".to_owned())
            .spawn(move || {
                // SAFETY: this thread enters the multithreaded apartment
                // once, before any COM call, and leaves it last.
                if unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_err() {
                    let _ = opened.send(Err(VideoError::Open("COM did not start".to_owned())));
                    return;
                }
                // SAFETY: Media Foundation starts on this thread and shuts
                // down on it once the engine is gone.
                let started = unsafe { MFStartup(MF_VERSION, MFSTARTUP_FULL) }.is_ok();
                if started {
                    match Engine::new(&url, heard) {
                        Ok(engine) => {
                            let _ = opened.send(Ok(()));
                            engine.run(asked, tell, send_shot, refill, at);
                        }
                        Err(error) => {
                            let _ = opened.send(Err(error));
                        }
                    }
                    // SAFETY: balances the start above, with no engine left.
                    let _ = unsafe { MFShutdown() };
                } else {
                    let _ = opened.send(Err(VideoError::Unsupported));
                }
                // SAFETY: balances the apartment entered above.
                unsafe { CoUninitialize() };
            })
            .map_err(|e| VideoError::Open(e.to_string()))?;
        match open.recv() {
            Ok(Ok(())) => Ok(Player {
                asks,
                events,
                shots,
                spent,
                shown: None,
                fresh: false,
                position,
                wants_play: false,
                looping: false,
                ended: false,
                thread: Some(thread),
                _spilled: spilled,
            }),
            Ok(Err(error)) => {
                let _ = thread.join();
                Err(error)
            }
            Err(_) => {
                let _ = thread.join();
                Err(VideoError::Open("the player's thread ended".to_owned()))
            }
        }
    }

    pub(crate) fn poll(&mut self) -> Option<VideoEvent> {
        let event = self.events.try_recv().ok()?;
        if event == VideoEvent::Ended {
            self.wants_play = false;
            self.ended = true;
        }
        Some(event)
    }

    pub(crate) fn frame(&mut self) -> Option<Frame<'_>> {
        while let Ok(shot) = self.shots.try_recv() {
            if let Some(old) = self.shown.replace(shot) {
                let _ = self.spent.send(old.rows);
            }
            self.fresh = true;
        }
        if !std::mem::take(&mut self.fresh) {
            return None;
        }
        let shot = self.shown.as_ref()?;
        Some(Frame::bgra(
            shot.width,
            shot.height,
            &shot.rows,
            shot.width as usize * 4,
            shot.time,
        ))
    }

    pub(crate) fn play(&mut self) {
        if std::mem::take(&mut self.ended) {
            self.seek(Duration::ZERO);
        }
        self.wants_play = true;
        let _ = self.asks.send(Ask::Play);
    }

    pub(crate) fn pause(&mut self) {
        self.wants_play = false;
        let _ = self.asks.send(Ask::Pause);
    }

    pub(crate) fn seek(&mut self, time: Duration) {
        self.ended = false;
        self.position.store(nanos(time), Ordering::Relaxed);
        let _ = self.asks.send(Ask::Seek(time));
    }

    pub(crate) fn set_rate(&mut self, rate: f32) {
        let _ = self.asks.send(Ask::Rate(rate));
    }

    pub(crate) fn set_volume(&mut self, volume: f32) {
        let _ = self.asks.send(Ask::Volume(volume));
    }

    pub(crate) fn set_muted(&mut self, muted: bool) {
        let _ = self.asks.send(Ask::Muted(muted));
    }

    pub(crate) fn set_looping(&mut self, looping: bool) {
        self.looping = looping;
        let _ = self.asks.send(Ask::Looping(looping));
    }

    pub(crate) fn position(&self) -> Duration {
        Duration::from_nanos(self.position.load(Ordering::Relaxed))
    }

    pub(crate) fn is_playing(&self) -> bool {
        self.wants_play && !self.ended
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        let _ = self.asks.send(Ask::Close);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// The engine's notifications, posted to its thread.
#[implement(IMFMediaEngineNotify)]
struct Notify(Sender<Ask>);

impl IMFMediaEngineNotify_Impl for Notify_Impl {
    fn EventNotify(&self, event: u32, _: usize, _: u32) -> windows::core::Result<()> {
        let _ = self.0.send(Ask::Heard(event));
        Ok(())
    }
}

/// The render target a frame is drawn into and the staging copy it is read
/// back from, of one size.
struct Targets {
    width: u32,
    height: u32,
    target: ID3D11Texture2D,
    staging: ID3D11Texture2D,
}

/// The media engine and the device it decodes on, owned by its thread.
struct Engine {
    engine: IMFMediaEngine,
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    /// Kept for as long as the engine decodes through it.
    _manager: IMFDXGIDeviceManager,
    targets: Option<Targets>,
    ready: bool,
    wants_play: bool,
    looping: bool,
    waiting: bool,
    /// A seek asked before the engine could take it.
    seek: Option<Duration>,
    rate: f32,
    volume: f32,
    muted: bool,
    /// The time of the frame last drawn, and when it was drawn.
    last: Option<(i64, Instant)>,
    /// How long a frame shows, as the last two told.
    interval: Duration,
    /// Since when a frame is wanted though the video may be paused: the
    /// first, or one sought to. Given up after `DUE_FOR`, so a source that
    /// never shows one does not keep the thread polling.
    due: Option<Instant>,
}

impl Engine {
    fn new(url: &str, heard: Sender<Ask>) -> Result<Engine, VideoError> {
        let open = |what: &str, e: windows::core::Error| VideoError::Open(format!("{what}: {e}"));
        let (device, context) = device().map_err(|e| open("Direct3D 11", e))?;
        // SAFETY: plain COM calls on live objects of this thread's
        // apartment; each out-pointer is a local the call fills.
        unsafe {
            // The engine and this thread share the device's context.
            if let Ok(threads) = device.cast::<ID3D11Multithread>() {
                let _ = threads.SetMultithreadProtected(true);
            }
            let mut token = 0u32;
            let mut manager = None;
            MFCreateDXGIDeviceManager(&mut token, &mut manager).map_err(|e| open("DXGI", e))?;
            let manager: IMFDXGIDeviceManager =
                manager.ok_or_else(|| VideoError::Open("no DXGI manager".to_owned()))?;
            manager
                .ResetDevice(&device, token)
                .map_err(|e| open("DXGI", e))?;
            let mut attributes: Option<IMFAttributes> = None;
            MFCreateAttributes(&mut attributes, 3).map_err(|e| open("attributes", e))?;
            let attributes =
                attributes.ok_or_else(|| VideoError::Open("no attributes".to_owned()))?;
            let notify: IMFMediaEngineNotify = Notify(heard).into();
            attributes
                .SetUnknown(&MF_MEDIA_ENGINE_CALLBACK, &notify)
                .map_err(|e| open("attributes", e))?;
            attributes
                .SetUnknown(&MF_MEDIA_ENGINE_DXGI_MANAGER, &manager)
                .map_err(|e| open("attributes", e))?;
            attributes
                .SetUINT32(
                    &MF_MEDIA_ENGINE_VIDEO_OUTPUT_FORMAT,
                    DXGI_FORMAT_B8G8R8A8_UNORM.0 as u32,
                )
                .map_err(|e| open("attributes", e))?;
            let factory: IMFMediaEngineClassFactory =
                CoCreateInstance(&CLSID_MFMediaEngineClassFactory, None, CLSCTX_INPROC_SERVER)
                    .map_err(|_| VideoError::Unsupported)?;
            let engine = factory
                .CreateInstance(0, &attributes)
                .map_err(|e| open("media engine", e))?;
            engine
                .SetSource(&BSTR::from(url))
                .map_err(|e| open("source", e))?;
            Ok(Engine {
                engine,
                device,
                context,
                _manager: manager,
                targets: None,
                ready: false,
                wants_play: false,
                looping: false,
                waiting: false,
                seek: None,
                rate: 1.0,
                volume: 1.0,
                muted: false,
                last: None,
                interval: Duration::from_millis(16),
                due: None,
            })
        }
    }

    /// Takes what the player asks and what the engine tells until the
    /// player closes, drawing each new frame.
    fn run(
        mut self,
        asked: Receiver<Ask>,
        tell: Sender<VideoEvent>,
        shots: Sender<Shot>,
        refill: Receiver<Vec<u8>>,
        position: Arc<AtomicU64>,
    ) {
        loop {
            if self.due.is_some_and(|since| since.elapsed() > DUE_FOR) {
                self.due = None;
            }
            let wait = if self.ready && (self.playing() || self.due.is_some()) {
                self.next_frame_in()
            } else {
                Duration::MAX
            };
            let ask = match asked.recv_timeout(wait) {
                Ok(ask) => Some(ask),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => break,
            };
            match ask {
                Some(Ask::Close) => break,
                Some(ask) => {
                    if !self.take(ask, &tell) {
                        break;
                    }
                    // Take whatever else came meanwhile before drawing.
                    let mut closed = false;
                    while let Ok(ask) = asked.try_recv() {
                        if matches!(ask, Ask::Close) || !self.take(ask, &tell) {
                            closed = true;
                            break;
                        }
                    }
                    if closed {
                        break;
                    }
                }
                None => {}
            }
            if self.ready {
                // SAFETY: a read of the engine's clock on its thread.
                let now = unsafe { self.engine.GetCurrentTime() };
                if now.is_finite() && now >= 0.0 && self.seek.is_none() {
                    position.store(nanos(Duration::from_secs_f64(now)), Ordering::Relaxed);
                }
                if let Some(shot) = self.draw(&refill) {
                    let _ = shots.send(shot);
                }
            }
        }
        // SAFETY: the engine stops on the thread that made it, before its
        // device goes.
        let _ = unsafe { self.engine.Shutdown() };
    }

    fn playing(&self) -> bool {
        self.wants_play && !self.waiting
    }

    /// How long until the next frame is likely due: half a frame after the
    /// last was drawn, at least soon enough to catch a paused one.
    fn next_frame_in(&self) -> Duration {
        let half = self.interval / 2;
        match self.last {
            Some((_, at)) if self.due.is_none() => half
                .saturating_sub(at.elapsed())
                .max(Duration::from_millis(2)),
            _ => Duration::from_millis(4),
        }
    }

    /// Takes one request or notification; returns whether the thread goes
    /// on.
    fn take(&mut self, ask: Ask, tell: &Sender<VideoEvent>) -> bool {
        // SAFETY: calls on the engine on the thread that made it.
        unsafe {
            match ask {
                Ask::Play => {
                    self.wants_play = true;
                    if self.ready {
                        let _ = self.engine.Play();
                    }
                }
                Ask::Pause => {
                    self.wants_play = false;
                    if self.ready {
                        let _ = self.engine.Pause();
                    }
                    if std::mem::take(&mut self.waiting) {
                        return tell.send(VideoEvent::Waiting(false)).is_ok();
                    }
                }
                Ask::Seek(time) => {
                    if self.ready {
                        let _ = self.engine.SetCurrentTime(time.as_secs_f64());
                        self.due = Some(Instant::now());
                    } else {
                        self.seek = Some(time);
                    }
                }
                Ask::Rate(rate) => {
                    self.rate = rate;
                    if self.ready {
                        let _ = self.engine.SetPlaybackRate(f64::from(rate));
                    }
                }
                Ask::Volume(volume) => {
                    self.volume = volume;
                    if self.ready {
                        let _ = self.engine.SetVolume(f64::from(volume));
                    }
                }
                Ask::Muted(muted) => {
                    self.muted = muted;
                    if self.ready {
                        let _ = self.engine.SetMuted(muted);
                    }
                }
                Ask::Looping(looping) => {
                    self.looping = looping;
                    let _ = self.engine.SetLoop(looping);
                }
                Ask::Heard(event) => return self.hear(event, tell),
                Ask::Close => return false,
            }
        }
        true
    }

    /// Takes one of the engine's notifications.
    fn hear(&mut self, event: u32, tell: &Sender<VideoEvent>) -> bool {
        let is =
            |e: windows::Win32::Media::MediaFoundation::MF_MEDIA_ENGINE_EVENT| event == e.0 as u32;
        // SAFETY: calls on the engine on the thread that made it.
        unsafe {
            if is(MF_MEDIA_ENGINE_EVENT_CANPLAY) && !self.ready {
                self.ready = true;
                let _ = self.engine.SetPlaybackRate(f64::from(self.rate));
                let _ = self.engine.SetVolume(f64::from(self.volume));
                let _ = self.engine.SetMuted(self.muted);
                let _ = self.engine.SetLoop(self.looping);
                if let Some(time) = self.seek.take() {
                    let _ = self.engine.SetCurrentTime(time.as_secs_f64());
                }
                if self.wants_play {
                    let _ = self.engine.Play();
                }
                self.due = Some(Instant::now());
                let info = self.info();
                return tell.send(VideoEvent::Ready(info)).is_ok();
            }
            if is(MF_MEDIA_ENGINE_EVENT_ENDED) && !self.looping {
                self.wants_play = false;
                return tell.send(VideoEvent::Ended).is_ok();
            }
            if is(MF_MEDIA_ENGINE_EVENT_ERROR) {
                let code = self.engine.GetError().map_or(0, |e| e.GetErrorCode());
                let why = format!("Media Foundation error {code}");
                let error = if self.ready {
                    VideoError::Play(why)
                } else {
                    VideoError::Open(why)
                };
                let _ = tell.send(VideoEvent::Failed(error));
                return false;
            }
            if is(MF_MEDIA_ENGINE_EVENT_WAITING) || is(MF_MEDIA_ENGINE_EVENT_STALLED) {
                if self.wants_play && !self.waiting {
                    self.waiting = true;
                    return tell.send(VideoEvent::Waiting(true)).is_ok();
                }
            } else if is(MF_MEDIA_ENGINE_EVENT_PLAYING) {
                if std::mem::take(&mut self.waiting) {
                    return tell.send(VideoEvent::Waiting(false)).is_ok();
                }
            } else if is(MF_MEDIA_ENGINE_EVENT_SEEKED) {
                self.due = Some(Instant::now());
            } else if is(MF_MEDIA_ENGINE_EVENT_FORMATCHANGE) {
                self.targets = None;
                self.due = Some(Instant::now());
            }
        }
        true
    }

    /// What the engine knows of its video once it can play.
    fn info(&self) -> VideoInfo {
        let (mut width, mut height) = (0u32, 0u32);
        // SAFETY: reads of the engine on its thread into locals.
        unsafe {
            let _ = self
                .engine
                .GetNativeVideoSize(Some(&mut width), Some(&mut height));
            let length = self.engine.GetDuration();
            let duration =
                (length.is_finite() && length > 0.0).then(|| Duration::from_secs_f64(length));
            VideoInfo {
                width,
                height,
                duration,
                frame_rate: None,
                audio: self.engine.HasAudio().as_bool(),
                seekable: duration.is_some(),
            }
        }
    }

    /// Draws the engine's frame if it has a new one, and reads it back into
    /// a buffer handed back by the player or a new one.
    fn draw(&mut self, refill: &Receiver<Vec<u8>>) -> Option<Shot> {
        // SAFETY: the engine is asked on its thread; the textures are this
        // thread's, made on the engine's device, and the staging texture is
        // mapped for reading only around the copy of its rows, each
        // `RowPitch` bytes apart for its height.
        unsafe {
            let pts = self.engine.OnVideoStreamTick().ok()?;
            if pts < 0 || self.last.is_some_and(|(seen, _)| seen == pts) && self.due.is_none() {
                return None;
            }
            let (mut width, mut height) = (0u32, 0u32);
            self.engine
                .GetNativeVideoSize(Some(&mut width), Some(&mut height))
                .ok()?;
            if width == 0 || height == 0 {
                return None;
            }
            if self
                .targets
                .as_ref()
                .is_none_or(|t| t.width != width || t.height != height)
            {
                self.targets = Some(targets(&self.device, width, height).ok()?);
            }
            let t = self.targets.as_ref()?;
            let rect = RECT {
                left: 0,
                top: 0,
                right: width as i32,
                bottom: height as i32,
            };
            let black = MFARGB {
                rgbBlue: 0,
                rgbGreen: 0,
                rgbRed: 0,
                rgbAlpha: 255,
            };
            self.engine
                .TransferVideoFrame(&t.target, None, &rect, Some(&black))
                .ok()?;
            self.context.CopyResource(&t.staging, &t.target);
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            self.context
                .Map(&t.staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
                .ok()?;
            let row = width as usize * 4;
            let mut rows = refill.try_recv().unwrap_or_default();
            rows.clear();
            rows.reserve(row * height as usize);
            let base = mapped.pData.cast::<u8>().cast_const();
            for y in 0..height as usize {
                let line = std::slice::from_raw_parts(base.add(y * mapped.RowPitch as usize), row);
                rows.extend_from_slice(line);
            }
            self.context.Unmap(&t.staging, 0);
            let now = Instant::now();
            if let Some((seen, _)) = self.last
                && pts > seen
            {
                // The engine's times are in hundreds of nanoseconds.
                let step = Duration::from_nanos((pts - seen) as u64 * 100);
                self.interval = step.clamp(Duration::from_millis(4), Duration::from_millis(50));
            }
            self.last = Some((pts, now));
            self.due = None;
            Some(Shot {
                rows,
                width,
                height,
                time: Duration::from_nanos(pts as u64 * 100),
            })
        }
    }
}

/// A Direct3D 11 device that decodes video: on the GPU, else in software.
fn device() -> windows::core::Result<(ID3D11Device, ID3D11DeviceContext)> {
    let make = |driver: D3D_DRIVER_TYPE| {
        let (mut device, mut context) = (None, None);
        // SAFETY: the call fills the two locals it is handed.
        unsafe {
            D3D11CreateDevice(
                None,
                driver,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_VIDEO_SUPPORT | D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )?;
        }
        match (device, context) {
            (Some(device), Some(context)) => Ok((device, context)),
            _ => Err(windows::core::Error::empty()),
        }
    };
    make(D3D_DRIVER_TYPE_HARDWARE).or_else(|_| make(D3D_DRIVER_TYPE_WARP))
}

/// A BGRA render target of `width` × `height` and its staging copy.
fn targets(device: &ID3D11Device, width: u32, height: u32) -> windows::core::Result<Targets> {
    let mut desc = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let (mut target, mut staging) = (None, None);
    // SAFETY: each call reads the description it is handed and fills the
    // local it is handed.
    unsafe {
        device.CreateTexture2D(&desc, None, Some(&mut target))?;
        desc.Usage = D3D11_USAGE_STAGING;
        desc.BindFlags = 0;
        desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
        device.CreateTexture2D(&desc, None, Some(&mut staging))?;
    }
    match (target, staging) {
        (Some(target), Some(staging)) => Ok(Targets {
            width,
            height,
            target,
            staging,
        }),
        _ => Err(windows::core::Error::empty()),
    }
}

/// `bytes` in a file of their own, and its URL.
fn spill(bytes: &[u8]) -> Result<(String, Option<Spilled>), VideoError> {
    let spilled = Spilled::new(bytes).map_err(|e| VideoError::Open(e.to_string()))?;
    Ok((file_url(spilled.path()), Some(spilled)))
}

/// The `file:` URL of `path`, made absolute, its reserved bytes escaped.
fn file_url(path: &Path) -> String {
    let path = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let path = path.to_string_lossy().replace('\\', "/");
    let mut url = String::from("file:///");
    for c in path.trim_start_matches('/').chars() {
        match c {
            ' ' => url.push_str("%20"),
            '%' => url.push_str("%25"),
            '#' => url.push_str("%23"),
            '?' => url.push_str("%3F"),
            c => url.push(c),
        }
    }
    url
}

fn nanos(time: Duration) -> u64 {
    u64::try_from(time.as_nanos()).unwrap_or(u64::MAX)
}
