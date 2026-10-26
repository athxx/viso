//! Windows: Media Foundation's media engine, decoding in hardware through a
//! Direct3D 11 device of its own, each frame drawn into a shared texture a
//! GPU backend on the same adapter reads where it lies, or handed over as
//! BGRA rows.
//!
//! The engine runs on a thread of its own in the multithreaded apartment:
//! called from the loop's thread — a single-threaded apartment — it can stall
//! the message pump while it buffers. The player posts the thread what is
//! asked of it and reads back, without a lock, what the engine reports and
//! the latest picture. The thread wakes for each request and each of the
//! engine's notifications, and while the video plays, once a frame
//! interval: it asks the engine for a new frame and draws it into one of a
//! few shared textures — one no backend holds and the player has not handed
//! out — signalling a shared fence once it is written. Rows are read back
//! through a staging texture, into a buffer the player hands back once it
//! has shown the next, only for a picture wanted while paused and once a
//! caller has asked for rows; without shared textures every frame is.
//!
//! An extension's thread draws its live pictures the same way
//! ([`SharedCanvas`]), into the same shared textures.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{CloseHandle, GENERIC_ALL, HANDLE, HMODULE, RECT};
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE, D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_UNKNOWN, D3D_DRIVER_TYPE_WARP,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_CPU_ACCESS_READ,
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_FENCE_FLAG_SHARED,
    D3D11_MAP_READ, D3D11_MAPPED_SUBRESOURCE, D3D11_RESOURCE_MISC_SHARED,
    D3D11_RESOURCE_MISC_SHARED_NTHANDLE, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC,
    D3D11_USAGE_DEFAULT, D3D11_USAGE_STAGING, D3D11CreateDevice, ID3D11Device, ID3D11Device5,
    ID3D11DeviceContext, ID3D11DeviceContext4, ID3D11Fence, ID3D11Multithread, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE, DXGI_GPU_PREFERENCE_HIGH_PERFORMANCE,
    DXGI_SHARED_RESOURCE_READ, DXGI_SHARED_RESOURCE_WRITE, IDXGIAdapter, IDXGIAdapter1,
    IDXGIFactory6, IDXGIResource1,
};
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
use windows::core::{BSTR, Interface, PCWSTR, implement};

use viso_handle::{ExternalHolds, ExternalImage};

use super::{Frame, PixelLayout, Spilled, VideoError, VideoEvent, VideoInfo, VideoSource};

/// How long the thread looks for a frame wanted while paused.
const DUE_FOR: Duration = Duration::from_secs(2);

/// The most shared textures a player draws into: one shown, one in a frame
/// in flight, one being written, one to spare.
const SLOTS: usize = 4;

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

/// What a player's thread takes requests on and hands back on.
struct Lines {
    asked: Receiver<Ask>,
    tell: Sender<VideoEvent>,
    position: Arc<AtomicU64>,
}

/// A picture: in a shared texture at a fence value, read back in BGRA rows
/// `width * 4` bytes long, or both. `rows` is empty where it was not read.
struct Shot {
    rows: Vec<u8>,
    slot: Option<(Arc<Slot>, u64)>,
    width: u32,
    height: u32,
    time: Duration,
}

/// The picture of a shot: named in its shared texture, or as its rows.
#[derive(Clone, Copy)]
pub(crate) struct Picture<'a> {
    shot: &'a Shot,
    /// Set when rows are asked for and the shot has none: the next frames
    /// are read back.
    wants_rows: &'a AtomicBool,
}

impl std::fmt::Debug for Picture<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Picture")
    }
}

impl Picture<'_> {
    /// The shared texture the picture is in, and the fence value it is
    /// written by.
    pub(crate) fn external(&self, width: u32, height: u32) -> Option<ExternalImage> {
        let (slot, value) = self.shot.slot.as_ref()?;
        Some(ExternalImage::DxgiShared {
            texture: slot.handle.0,
            fence: slot.fence.handle.0,
            value: *value,
            holds: Arc::as_ptr(&slot.holds),
            width,
            height,
        })
    }

    /// Calls `f` with the picture's rows and their stride, where they were
    /// read back.
    pub(crate) fn read(&self, f: impl FnOnce(&[u8], usize)) -> bool {
        if self.shot.rows.is_empty() {
            self.wants_rows.store(true, Ordering::Relaxed);
            return false;
        }
        f(&self.shot.rows, self.shot.width as usize * 4);
        true
    }

    pub(crate) fn layout(&self) -> PixelLayout {
        PixelLayout::Bgra8
    }
}

/// A shared texture frames are drawn into.
struct Slot {
    texture: ID3D11Texture2D,
    /// An NT handle to `texture`, closed when the slot goes.
    handle: HANDLE,
    /// The backends holding it: it is not written while any does.
    holds: Arc<ExternalHolds>,
    fence: Arc<SharedFence>,
}

/// The fence the thread signals once a frame is written into a slot.
struct SharedFence {
    fence: ID3D11Fence,
    /// An NT handle to `fence`, closed when it goes.
    handle: HANDLE,
}

// SAFETY: the device the texture and fence are made on is multithread
// protected; off the thread that writes them, only their handles are read
// and their references released.
unsafe impl Send for Slot {}
// SAFETY: as for `Send`: nothing but reads of immutable fields is shared.
unsafe impl Sync for Slot {}
// SAFETY: as for `Slot`.
unsafe impl Send for SharedFence {}
// SAFETY: as for `Slot`.
unsafe impl Sync for SharedFence {}

impl Drop for Slot {
    fn drop(&mut self) {
        // SAFETY: the slot owns the handle, closed once.
        let _ = unsafe { CloseHandle(self.handle) };
    }
}

impl Drop for SharedFence {
    fn drop(&mut self) {
        // SAFETY: the fence owns the handle, closed once.
        let _ = unsafe { CloseHandle(self.handle) };
    }
}

pub(crate) struct Player {
    asks: Sender<Ask>,
    events: Receiver<VideoEvent>,
    pictures: SharedPictures,
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
            VideoSource::Camera(_) => return Err(VideoError::Unsupported),
        };
        let (asks, asked) = mpsc::channel();
        let (tell, events) = mpsc::channel();
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
                    let lines = Lines {
                        asked,
                        tell,
                        position: at,
                    };
                    match Engine::new(&url, heard) {
                        Ok((engine, pictures)) => {
                            let _ = opened.send(Ok(pictures));
                            engine.run(lines);
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
            Ok(Ok(pictures)) => Ok(Player {
                asks,
                events,
                pictures,
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
        self.pictures.frame()
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

/// What a frame is drawn into and read back from, of one size: the slots
/// where textures are shared, else a render target of its own; and the
/// staging copy, made when first read back.
struct Targets {
    width: u32,
    height: u32,
    target: Option<ID3D11Texture2D>,
    slots: Vec<Arc<Slot>>,
    staging: Option<ID3D11Texture2D>,
}

/// What shares the thread's frames with another device.
struct Sharing {
    context: ID3D11DeviceContext4,
    fence: Arc<SharedFence>,
    /// The value last signalled.
    value: u64,
}

/// Where a thread draws its frames, on a device of its own: shared textures
/// a backend reads where the device shares a fence, else a render target of
/// its own; and the staging copy rows are read back through.
struct Canvas {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    /// `None` where the device shares no fence.
    sharing: Option<Sharing>,
    targets: Option<Targets>,
}

impl Canvas {
    /// A canvas on a device of its own, whose context the thread shares with
    /// Media Foundation.
    fn new() -> windows::core::Result<Canvas> {
        let (device, context) = device()?;
        if let Ok(threads) = device.cast::<ID3D11Multithread>() {
            // SAFETY: a plain COM call on the live device.
            let _ = unsafe { threads.SetMultithreadProtected(true) };
        }
        let sharing = sharing(&device, &context);
        Ok(Canvas {
            device,
            context,
            sharing,
            targets: None,
        })
    }

    /// A device manager Media Foundation decodes through on the canvas's
    /// device.
    fn manager(&self) -> windows::core::Result<IMFDXGIDeviceManager> {
        // SAFETY: plain calls on live objects; the token and manager are
        // locals the first call fills.
        unsafe {
            let mut token = 0u32;
            let mut manager = None;
            MFCreateDXGIDeviceManager(&mut token, &mut manager)?;
            let manager: IMFDXGIDeviceManager = manager.ok_or_else(windows::core::Error::empty)?;
            manager.ResetDevice(&self.device, token)?;
            Ok(manager)
        }
    }

    /// Draws a picture of `width` × `height` through `write`: into a slot no
    /// one holds where textures are shared, signalling the fence once it is
    /// written, else into a target of its own. Reads it back into a buffer
    /// handed back by the player, or a new one, where `read_back` or there is
    /// no slot; returns the rows, empty where not read, and the slot with its
    /// fence value.
    #[allow(clippy::type_complexity)]
    fn draw(
        &mut self,
        width: u32,
        height: u32,
        read_back: bool,
        refill: &Receiver<Vec<u8>>,
        write: impl FnOnce(&ID3D11DeviceContext, &ID3D11Texture2D) -> Option<()>,
    ) -> Option<(Vec<u8>, Option<(Arc<Slot>, u64)>)> {
        if width == 0 || height == 0 {
            return None;
        }
        if self
            .targets
            .as_ref()
            .is_none_or(|t| t.width != width || t.height != height)
        {
            self.targets = Some(Targets {
                width,
                height,
                target: None,
                slots: Vec::new(),
                staging: None,
            });
        }
        let t = self.targets.as_mut()?;
        let slot = match &self.sharing {
            Some(sharing) => {
                let free = t
                    .slots
                    .iter()
                    .find(|s| Arc::strong_count(s) == 1 && !s.holds.is_held())
                    .cloned();
                Some(match free {
                    Some(slot) => slot,
                    None if t.slots.len() < SLOTS => {
                        let slot =
                            Arc::new(slot(&self.device, &sharing.fence, width, height).ok()?);
                        t.slots.push(Arc::clone(&slot));
                        slot
                    }
                    // Every slot is shown or held: this frame is skipped,
                    // the next tries again.
                    None => return None,
                })
            }
            None => None,
        };
        let target = match &slot {
            Some(slot) => slot.texture.clone(),
            None => match &t.target {
                Some(target) => target.clone(),
                None => t
                    .target
                    .insert(texture(&self.device, width, height, false).ok()?)
                    .clone(),
            },
        };
        write(&self.context, &target)?;
        // SAFETY: the textures are this thread's, made on its device, and the
        // staging texture is mapped for reading only around the copy of its
        // rows, each `RowPitch` bytes apart for its height.
        unsafe {
            let slot = match (slot, &mut self.sharing) {
                (Some(slot), Some(sharing)) => {
                    sharing.value += 1;
                    sharing
                        .context
                        .Signal(&sharing.fence.fence, sharing.value)
                        .ok()?;
                    // Another device waits on the fence: the work signalling
                    // it goes to the GPU now.
                    self.context.Flush();
                    Some((slot, sharing.value))
                }
                _ => None,
            };
            let mut rows = Vec::new();
            if slot.is_none() || read_back {
                let staging = match &t.staging {
                    Some(staging) => staging.clone(),
                    None => t
                        .staging
                        .insert(staging(&self.device, width, height).ok()?)
                        .clone(),
                };
                self.context.CopyResource(&staging, &target);
                let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
                self.context
                    .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
                    .ok()?;
                let row = width as usize * 4;
                rows = refill.try_recv().unwrap_or_default();
                rows.clear();
                rows.reserve(row * height as usize);
                let base = mapped.pData.cast::<u8>().cast_const();
                for y in 0..height as usize {
                    let line =
                        std::slice::from_raw_parts(base.add(y * mapped.RowPitch as usize), row);
                    rows.extend_from_slice(line);
                }
                self.context.Unmap(&staging, 0);
            }
            Some((rows, slot))
        }
    }
}

/// Where an extension's thread draws its live pictures — a camera's, a
/// screen's — on a Direct3D 11 device of its own (on the adapter a GPU
/// backend picks, multithread protected): into a few shared textures the
/// backend reads where they lie, each signalled through a shared fence once
/// written, or into a target of its own where the device shares no fence.
/// Rows are read back through a staging texture only once a caller copied a
/// picture, or where nothing is shared. Its pictures are taken on the
/// loop's thread from the [`SharedPictures`] made with it.
pub struct SharedCanvas {
    canvas: Canvas,
    shots: Sender<Shot>,
    /// Buffers shown and done with, to fill again.
    refill: Receiver<Vec<u8>>,
    wants_rows: Arc<AtomicBool>,
}

impl SharedCanvas {
    /// A canvas on a device of its own, and where its pictures are taken.
    pub fn new() -> windows::core::Result<(SharedCanvas, SharedPictures)> {
        let canvas = Canvas::new()?;
        let (shots, taken) = mpsc::channel();
        let (spent, refill) = mpsc::channel();
        let wants_rows = Arc::new(AtomicBool::new(false));
        Ok((
            SharedCanvas {
                canvas,
                shots,
                refill,
                wants_rows: Arc::clone(&wants_rows),
            },
            SharedPictures {
                shots: taken,
                spent,
                shown: None,
                fresh: false,
                wants_rows,
            },
        ))
    }

    /// The device it draws on, which a decoder or capture produces onto.
    pub fn device(&self) -> &ID3D11Device {
        &self.canvas.device
    }

    /// A device manager Media Foundation decodes or converts through on its
    /// device.
    pub fn dxgi_manager(&self) -> windows::core::Result<IMFDXGIDeviceManager> {
        self.canvas.manager()
    }

    /// Draws a BGRA picture of `width` × `height`, shown at `time`, through
    /// `write` — which fills the texture it is handed, on the device's
    /// context — and hands it to the pictures. Whether it was drawn: not
    /// while every texture is still shown or held, nor where `write` fails.
    pub fn draw(
        &mut self,
        width: u32,
        height: u32,
        time: Duration,
        write: impl FnOnce(&ID3D11DeviceContext, &ID3D11Texture2D) -> Option<()>,
    ) -> bool {
        self.draw_read(width, height, time, false, write)
    }

    /// [`draw`](Self::draw), read back as rows too where `read_back`.
    fn draw_read(
        &mut self,
        width: u32,
        height: u32,
        time: Duration,
        read_back: bool,
        write: impl FnOnce(&ID3D11DeviceContext, &ID3D11Texture2D) -> Option<()>,
    ) -> bool {
        let read_back = read_back || self.wants_rows.load(Ordering::Relaxed);
        let Some((rows, slot)) = self
            .canvas
            .draw(width, height, read_back, &self.refill, write)
        else {
            return false;
        };
        let _ = self.shots.send(Shot {
            rows,
            slot,
            width,
            height,
            time,
        });
        true
    }

    /// Forgets the textures of the size drawn last: the next picture's are
    /// made anew.
    fn resize(&mut self) {
        self.canvas.targets = None;
    }
}

/// The pictures a [`SharedCanvas`] draws, taken on the loop's thread.
pub struct SharedPictures {
    shots: Receiver<Shot>,
    /// Buffers shown and done with, for the canvas to fill again.
    spent: Sender<Vec<u8>>,
    /// The picture last handed out, and whether the next call hands it out.
    shown: Option<Shot>,
    fresh: bool,
    /// Whether a caller has asked for rows a picture had none of.
    wants_rows: Arc<AtomicBool>,
}

impl SharedPictures {
    /// The picture drawn last, when it is not the one this returned last:
    /// in its shared texture, or as rows.
    pub fn frame(&mut self) -> Option<Frame<'_>> {
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
        if shot.slot.is_none() {
            return Some(Frame::bgra(
                shot.width,
                shot.height,
                &shot.rows,
                shot.width as usize * 4,
                shot.time,
            ));
        }
        Some(Frame::native_picture(
            shot.width,
            shot.height,
            shot.time,
            Picture {
                shot,
                wants_rows: &self.wants_rows,
            },
        ))
    }
}

/// The media engine and the canvas it decodes onto, owned by its thread.
struct Engine {
    engine: IMFMediaEngine,
    canvas: SharedCanvas,
    /// Kept for as long as the engine decodes through it.
    _manager: IMFDXGIDeviceManager,
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
    fn new(url: &str, heard: Sender<Ask>) -> Result<(Engine, SharedPictures), VideoError> {
        let open = |what: &str, e: windows::core::Error| VideoError::Open(format!("{what}: {e}"));
        let (canvas, pictures) = SharedCanvas::new().map_err(|e| open("Direct3D 11", e))?;
        let manager = canvas.dxgi_manager().map_err(|e| open("DXGI", e))?;
        // SAFETY: plain COM calls on live objects of this thread's
        // apartment; each out-pointer is a local the call fills.
        unsafe {
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
            let engine = Engine {
                engine,
                canvas,
                _manager: manager,
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
            };
            Ok((engine, pictures))
        }
    }

    /// Takes what the player asks and what the engine tells until the
    /// player closes, drawing each new frame.
    fn run(mut self, lines: Lines) {
        let Lines {
            asked,
            tell,
            position,
        } = lines;
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
                self.draw();
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
                self.canvas.resize();
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
                facing: None,
            }
        }
    }

    /// Draws the engine's frame if it has a new one; read back where rows
    /// are wanted or a paused picture is.
    fn draw(&mut self) -> Option<()> {
        // SAFETY: the engine is asked on its thread, and draws into a texture
        // of its canvas, made on the device it decodes on.
        let (pts, width, height) = unsafe {
            let pts = self.engine.OnVideoStreamTick().ok()?;
            if pts < 0 || self.last.is_some_and(|(seen, _)| seen == pts) && self.due.is_none() {
                return None;
            }
            let (mut width, mut height) = (0u32, 0u32);
            self.engine
                .GetNativeVideoSize(Some(&mut width), Some(&mut height))
                .ok()?;
            (pts, width, height)
        };
        let engine = &self.engine;
        // The engine's times are in hundreds of nanoseconds.
        let time = Duration::from_nanos(pts as u64 * 100);
        let drawn = self
            .canvas
            .draw_read(width, height, time, self.due.is_some(), |_, target| {
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
                // SAFETY: as above.
                unsafe { engine.TransferVideoFrame(target, None, &rect, Some(&black)) }.ok()
            });
        if !drawn {
            return None;
        }
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
        Some(())
    }
}

/// A Direct3D 11 device that decodes video: on the adapter a GPU backend
/// picks — the high-performance one, so the two can share textures — else
/// on any GPU, else in software.
fn device() -> windows::core::Result<(ID3D11Device, ID3D11DeviceContext)> {
    let flags = D3D11_CREATE_DEVICE_VIDEO_SUPPORT | D3D11_CREATE_DEVICE_BGRA_SUPPORT;
    let make = |adapter: Option<&IDXGIAdapter>, driver: D3D_DRIVER_TYPE| {
        let (mut device, mut context) = (None, None);
        // SAFETY: the call fills the two locals it is handed.
        unsafe {
            D3D11CreateDevice(
                adapter,
                driver,
                HMODULE::default(),
                flags,
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
    // SAFETY: adapter enumeration has no preconditions beyond live
    // arguments.
    let preferred = unsafe {
        CreateDXGIFactory1::<IDXGIFactory6>()
            .ok()
            .and_then(|factory| {
                (0..)
                    .map_while(|i| {
                        factory
                            .EnumAdapterByGpuPreference::<IDXGIAdapter1>(
                                i,
                                DXGI_GPU_PREFERENCE_HIGH_PERFORMANCE,
                            )
                            .ok()
                    })
                    .find(|adapter| {
                        adapter
                            .GetDesc1()
                            .is_ok_and(|d| d.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 == 0)
                    })
            })
            .and_then(|adapter| adapter.cast::<IDXGIAdapter>().ok())
    };
    preferred
        .ok_or_else(windows::core::Error::empty)
        .and_then(|adapter| make(Some(&adapter), D3D_DRIVER_TYPE_UNKNOWN))
        .or_else(|_| make(None, D3D_DRIVER_TYPE_HARDWARE))
        .or_else(|_| make(None, D3D_DRIVER_TYPE_WARP))
}

/// What shares frames with another device: a shared fence, where the device
/// makes one.
fn sharing(device: &ID3D11Device, context: &ID3D11DeviceContext) -> Option<Sharing> {
    // SAFETY: plain COM calls on live objects; the fence is a local the call
    // fills, and its handle is owned by the `SharedFence` made of it.
    unsafe {
        let device: ID3D11Device5 = device.cast().ok()?;
        let context: ID3D11DeviceContext4 = context.cast().ok()?;
        let mut fence: Option<ID3D11Fence> = None;
        device
            .CreateFence(0, D3D11_FENCE_FLAG_SHARED, &mut fence)
            .ok()?;
        let fence = fence?;
        let handle = fence
            .CreateSharedHandle(None, GENERIC_ALL.0, PCWSTR::null())
            .ok()?;
        Some(Sharing {
            context,
            fence: Arc::new(SharedFence { fence, handle }),
            value: 0,
        })
    }
}

/// A shared texture of `width` × `height` to draw frames into, signalled
/// through `fence`.
fn slot(
    device: &ID3D11Device,
    fence: &Arc<SharedFence>,
    width: u32,
    height: u32,
) -> windows::core::Result<Slot> {
    let texture = texture(device, width, height, true)?;
    // SAFETY: the texture was made shared through an NT handle, which the
    // slot owns.
    let handle = unsafe {
        texture.cast::<IDXGIResource1>()?.CreateSharedHandle(
            None,
            (DXGI_SHARED_RESOURCE_READ | DXGI_SHARED_RESOURCE_WRITE).0,
            PCWSTR::null(),
        )?
    };
    Ok(Slot {
        texture,
        handle,
        holds: ExternalHolds::new(),
        fence: Arc::clone(fence),
    })
}

/// A BGRA render target of `width` × `height`, shared through an NT handle
/// or not.
fn texture(
    device: &ID3D11Device,
    width: u32,
    height: u32,
    shared: bool,
) -> windows::core::Result<ID3D11Texture2D> {
    let desc = D3D11_TEXTURE2D_DESC {
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
        MiscFlags: if shared {
            (D3D11_RESOURCE_MISC_SHARED.0 | D3D11_RESOURCE_MISC_SHARED_NTHANDLE.0) as u32
        } else {
            0
        },
    };
    let mut texture = None;
    // SAFETY: the call reads the description it is handed and fills the
    // local it is handed.
    unsafe { device.CreateTexture2D(&desc, None, Some(&mut texture))? };
    texture.ok_or_else(windows::core::Error::empty)
}

/// A staging texture of `width` × `height` BGRA texels to read frames back
/// through.
fn staging(
    device: &ID3D11Device,
    width: u32,
    height: u32,
) -> windows::core::Result<ID3D11Texture2D> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_STAGING,
        BindFlags: 0,
        CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
        MiscFlags: 0,
    };
    let mut texture = None;
    // SAFETY: as for `texture`.
    unsafe { device.CreateTexture2D(&desc, None, Some(&mut texture))? };
    texture.ok_or_else(windows::core::Error::empty)
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
