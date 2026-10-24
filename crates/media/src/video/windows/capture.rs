//! Cameras on Windows: a Media Foundation device source read through a
//! source reader that converts its pictures to BGRA on the canvas's device,
//! each copied into one of the shared textures a video's frames go to.
//!
//! The reader runs asynchronously: it posts each picture to the player's
//! thread as a request, so the thread never waits on the device and closes
//! at once. The camera is opened from where it is plugged in each time the
//! player plays, and shut down when it pauses, so the device's light goes
//! out; a picture still on its way from a reader shut down is dropped.

use std::sync::atomic::Ordering;
use std::sync::mpsc::{Receiver, Sender};
use std::time::Duration;

use windows::Win32::Devices::DeviceAndDriverInstallation::{
    CM_Get_DevNode_PropertyW, CM_Get_Device_Interface_PropertyW, CM_LOCATE_DEVNODE_NORMAL,
    CM_Locate_DevNodeW, CR_SUCCESS,
};
use windows::Win32::Devices::Properties::{
    DEVPKEY_Device_InstanceId, DEVPKEY_Device_PhysicalDeviceLocation, DEVPROPTYPE,
};
use windows::Win32::Foundation::E_ACCESSDENIED;
use windows::Win32::Graphics::Direct3D11::{ID3D11DeviceContext, ID3D11Texture2D};
use windows::Win32::Media::MediaFoundation::{
    IMF2DBuffer, IMFActivate, IMFAttributes, IMFDXGIBuffer, IMFDXGIDeviceManager, IMFMediaEvent,
    IMFMediaSource, IMFMediaType, IMFSample, IMFSourceReader, IMFSourceReaderCallback,
    IMFSourceReaderCallback_Impl, MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME,
    MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE, MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID,
    MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK, MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE,
    MF_MT_MAJOR_TYPE, MF_MT_SUBTYPE, MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS,
    MF_SOURCE_READER_ASYNC_CALLBACK, MF_SOURCE_READER_D3D_MANAGER,
    MF_SOURCE_READER_ENABLE_ADVANCED_VIDEO_PROCESSING, MF_SOURCE_READER_FIRST_VIDEO_STREAM,
    MF_SOURCE_READERF_CURRENTMEDIATYPECHANGED, MF_SOURCE_READERF_ENDOFSTREAM,
    MF_SOURCE_READERF_ERROR, MF_VERSION, MFCreateAttributes, MFCreateDeviceSource,
    MFCreateMediaType, MFCreateSourceReaderFromMediaSource, MFEnumDeviceSources, MFMediaType_Video,
    MFSTARTUP_LITE, MFShutdown, MFStartup, MFVideoFormat_ARGB32, MFVideoFormat_MJPG,
    MFVideoFormat_NV12, MFVideoFormat_YUY2,
};
use windows::Win32::System::Com::{
    COINIT_MULTITHREADED, CoInitializeEx, CoTaskMemFree, CoUninitialize,
};
use windows::core::{GUID, HRESULT, Interface, PCWSTR, PWSTR, implement};

use super::super::camera::{self, CameraFacing, CameraInfo, CameraMode, CameraRequest};
use super::super::{VideoError, VideoEvent, VideoInfo};
use super::{Ask, Canvas, Lines, Shot};

/// A picture the reader read, or why it could not, posted to the thread.
pub(super) struct Read {
    /// Which opening of the camera it came from.
    opening: u64,
    status: HRESULT,
    flags: u32,
    sample: Option<IMFSample>,
}

// SAFETY: Media Foundation's samples live in the multithreaded apartment,
// which both the reader's thread and the player's are in; the sample is
// only read on the thread it is posted to.
unsafe impl Send for Read {}

/// The reader's callback, posting what it reads to the player's thread.
#[implement(IMFSourceReaderCallback)]
struct Reads {
    asks: Sender<Ask>,
    opening: u64,
}

impl IMFSourceReaderCallback_Impl for Reads_Impl {
    fn OnReadSample(
        &self,
        status: HRESULT,
        _: u32,
        flags: u32,
        _: i64,
        sample: windows::core::Ref<IMFSample>,
    ) -> windows::core::Result<()> {
        let _ = self.asks.send(Ask::Captured(Read {
            opening: self.opening,
            status,
            flags,
            sample: sample.cloned(),
        }));
        Ok(())
    }

    fn OnFlush(&self, _: u32) -> windows::core::Result<()> {
        Ok(())
    }

    fn OnEvent(&self, _: u32, _: windows::core::Ref<IMFMediaEvent>) -> windows::core::Result<()> {
        Ok(())
    }
}

/// A camera opened: its source and the reader of it.
struct Opened {
    source: IMFMediaSource,
    reader: IMFSourceReader,
}

impl Drop for Opened {
    fn drop(&mut self) {
        // SAFETY: the source is shut down once, on the thread that opened
        // it; the device goes with it.
        let _ = unsafe { self.source.Shutdown() };
    }
}

/// A camera, owned by the player's thread.
pub(super) struct Capture {
    request: CameraRequest,
    camera: CameraInfo,
    canvas: Canvas,
    manager: IMFDXGIDeviceManager,
    /// Where the reader posts what it reads.
    asks: Sender<Ask>,
    opened: Option<Opened>,
    /// Counts the camera's openings, telling a picture of the current one.
    opening: u64,
    /// The size of the reader's pictures, and the rate of the camera's mode.
    width: u32,
    height: u32,
    frame_rate: Option<f32>,
    ready: bool,
    wants_play: bool,
}

impl Capture {
    /// The camera `request` names, opened once the thread runs, to show its
    /// first picture.
    pub(super) fn new(request: &CameraRequest, asks: Sender<Ask>) -> Result<Capture, VideoError> {
        let open = |what: &str, e: windows::core::Error| VideoError::Open(format!("{what}: {e}"));
        let cameras = list();
        let camera = camera::chosen(&cameras, &request.camera)
            .ok_or_else(|| VideoError::Open("no such camera".to_owned()))?
            .clone();
        let canvas = Canvas::new().map_err(|e| open("Direct3D 11", e))?;
        let manager = canvas.manager().map_err(|e| open("DXGI", e))?;
        Ok(Capture {
            request: request.clone(),
            camera,
            canvas,
            manager,
            asks,
            opened: None,
            opening: 0,
            width: 0,
            height: 0,
            frame_rate: None,
            ready: false,
            wants_play: false,
        })
    }

    /// Takes what the player asks and the pictures the reader posts until
    /// the player closes.
    pub(super) fn run(mut self, lines: Lines) {
        let Lines {
            asked,
            tell,
            shots,
            refill,
            wants_rows,
            ..
        } = lines;
        if let Err(error) = self.start() {
            let _ = tell.send(VideoEvent::Failed(error));
            return;
        }
        while let Ok(ask) = asked.recv() {
            match ask {
                Ask::Close => break,
                Ask::Play => {
                    self.wants_play = true;
                    if self.opened.is_none()
                        && let Err(error) = self.start()
                    {
                        let _ = tell.send(VideoEvent::Failed(error));
                        break;
                    }
                }
                Ask::Pause => {
                    self.wants_play = false;
                    if self.ready {
                        self.opened = None;
                    }
                }
                Ask::Captured(read) => {
                    if read.opening != self.opening || self.opened.is_none() {
                        continue;
                    }
                    if let Err(error) = self.take(read, &tell, &shots, &refill, &wants_rows) {
                        self.opened = None;
                        let _ = tell.send(VideoEvent::Failed(error));
                        break;
                    }
                }
                // A camera is always now, at its own rate and silent.
                _ => {}
            }
        }
        self.opened = None;
    }

    /// Takes one read: draws its picture, tells the camera ready at its
    /// first, and asks for the next.
    fn take(
        &mut self,
        read: Read,
        tell: &Sender<VideoEvent>,
        shots: &Sender<Shot>,
        refill: &Receiver<Vec<u8>>,
        wants_rows: &std::sync::atomic::AtomicBool,
    ) -> Result<(), VideoError> {
        if read.status.is_err() || read.flags & MF_SOURCE_READERF_ERROR.0 as u32 != 0 {
            return Err(failure(self.ready, windows::core::Error::from(read.status)));
        }
        if read.flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
            return Err(VideoError::Play("the camera went away".to_owned()));
        }
        if read.flags & MF_SOURCE_READERF_CURRENTMEDIATYPECHANGED.0 as u32 != 0 {
            self.measure();
        }
        if let Some(sample) = read.sample {
            let (width, height) = (self.width, self.height);
            let drawn = self.canvas.draw(
                width,
                height,
                wants_rows.load(Ordering::Relaxed),
                refill,
                |context, target| copy(context, target, &sample, width, height),
            );
            if let Some((rows, slot)) = drawn {
                if !self.ready {
                    self.ready = true;
                    let _ = tell.send(VideoEvent::Ready(VideoInfo {
                        width,
                        height,
                        duration: None,
                        frame_rate: self.frame_rate,
                        audio: false,
                        seekable: false,
                        facing: Some(self.camera.facing),
                    }));
                }
                let _ = shots.send(Shot {
                    rows,
                    slot,
                    width,
                    height,
                    time: Duration::ZERO,
                });
                if !self.wants_play {
                    // Opened only to show its first picture.
                    self.opened = None;
                    return Ok(());
                }
            }
        }
        self.read_next()
    }

    /// Opens the camera in its mode nearest the request, through a reader
    /// that converts to BGRA, and asks for its first picture.
    fn start(&mut self) -> Result<(), VideoError> {
        self.opening += 1;
        let ready = self.ready;
        let fail = |e: windows::core::Error| failure(ready, e);
        // SAFETY: plain Media Foundation calls on live objects of this
        // thread's apartment; the attributes outlive the calls they are
        // handed to, and the link is a nul-terminated wide string.
        unsafe {
            let device = attributes(2).map_err(fail)?;
            device
                .SetGUID(
                    &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE,
                    &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID,
                )
                .map_err(fail)?;
            let link = wide(&self.camera.id);
            device
                .SetString(
                    &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK,
                    PCWSTR(link.as_ptr()),
                )
                .map_err(fail)?;
            let source = MFCreateDeviceSource(&device).map_err(fail)?;
            self.frame_rate = choose_mode(&source, &self.request).map_err(fail)?;
            let reading = attributes(4).map_err(fail)?;
            let reads: IMFSourceReaderCallback = Reads {
                asks: self.asks.clone(),
                opening: self.opening,
            }
            .into();
            reading
                .SetUnknown(&MF_SOURCE_READER_ASYNC_CALLBACK, &reads)
                .map_err(fail)?;
            reading
                .SetUnknown(&MF_SOURCE_READER_D3D_MANAGER, &self.manager)
                .map_err(fail)?;
            reading
                .SetUINT32(&MF_SOURCE_READER_ENABLE_ADVANCED_VIDEO_PROCESSING, 1)
                .map_err(fail)?;
            reading
                .SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1)
                .map_err(fail)?;
            let reader = match MFCreateSourceReaderFromMediaSource(&source, &reading) {
                Ok(reader) => reader,
                Err(e) => {
                    let _ = source.Shutdown();
                    return Err(fail(e));
                }
            };
            let opened = Opened { source, reader };
            let stream = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
            let native = opened.reader.GetCurrentMediaType(stream).map_err(fail)?;
            let output = MFCreateMediaType().map_err(fail)?;
            output
                .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
                .map_err(fail)?;
            output
                .SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_ARGB32)
                .map_err(fail)?;
            if let Ok(size) = native.GetUINT64(&MF_MT_FRAME_SIZE) {
                output.SetUINT64(&MF_MT_FRAME_SIZE, size).map_err(fail)?;
            }
            opened
                .reader
                .SetCurrentMediaType(stream, None, &output)
                .map_err(fail)?;
            self.opened = Some(opened);
        }
        self.measure();
        self.read_next()
    }

    /// Reads the size of the reader's pictures.
    fn measure(&mut self) {
        let Some(opened) = &self.opened else {
            return;
        };
        // SAFETY: a read of the live reader's type on its thread.
        let size = unsafe {
            opened
                .reader
                .GetCurrentMediaType(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32)
                .and_then(|t| t.GetUINT64(&MF_MT_FRAME_SIZE))
        };
        if let Ok(size) = size {
            (self.width, self.height) = ((size >> 32) as u32, size as u32);
        }
    }

    /// Asks the reader for its next picture, posted once read.
    fn read_next(&self) -> Result<(), VideoError> {
        let Some(opened) = &self.opened else {
            return Ok(());
        };
        // SAFETY: an asynchronous reader takes no out-pointers; it posts
        // what it reads to its callback.
        unsafe {
            opened.reader.ReadSample(
                MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32,
                0,
                None,
                None,
                None,
                None,
            )
        }
        .map_err(|e| failure(self.ready, e))
    }
}

/// Why a camera could not open or go on: refused where the user did not
/// let the app use it.
fn failure(ready: bool, error: windows::core::Error) -> VideoError {
    if error.code() == E_ACCESSDENIED {
        VideoError::Denied
    } else if ready {
        VideoError::Play(error.message())
    } else {
        VideoError::Open(error.message())
    }
}

/// Puts the source's stream in the mode nearest what `request` wants,
/// uncompressed where the camera has that mode so; returns its rate.
///
/// # Safety
///
/// On the thread that made the live `source`.
unsafe fn choose_mode(
    source: &IMFMediaSource,
    request: &CameraRequest,
) -> windows::core::Result<Option<f32>> {
    // SAFETY: plain calls on the live source and what it hands over; the
    // descriptor and selection are locals the call fills.
    unsafe {
        let presentation = source.CreatePresentationDescriptor()?;
        let mut selected = windows::core::BOOL(0);
        let mut stream = None;
        presentation.GetStreamDescriptorByIndex(0, &mut selected, &mut stream)?;
        let handler = stream
            .ok_or_else(windows::core::Error::empty)?
            .GetMediaTypeHandler()?;
        let types: Vec<(IMFMediaType, CameraMode, u8)> = (0..handler.GetMediaTypeCount()?)
            .filter_map(|i| handler.GetMediaTypeByIndex(i).ok())
            .filter_map(|t| {
                let size = t.GetUINT64(&MF_MT_FRAME_SIZE).ok()?;
                let rate = t.GetUINT64(&MF_MT_FRAME_RATE).ok()?;
                let (num, den) = ((rate >> 32) as u32, rate as u32);
                let mode = CameraMode {
                    width: (size >> 32) as u32,
                    height: size as u32,
                    frame_rate: f64::from(num) / f64::from(den.max(1)),
                };
                let rank = rank(t.GetGUID(&MF_MT_SUBTYPE).unwrap_or_default());
                Some((t, mode, rank))
            })
            .collect();
        let modes: Vec<CameraMode> = types.iter().map(|(_, m, _)| *m).collect();
        let Some(index) = camera::nearest_mode(&modes, request) else {
            return Ok(None);
        };
        let mode = modes[index];
        let Some((chosen, _, _)) = types
            .iter()
            .filter(|(_, m, _)| *m == mode)
            .min_by_key(|(_, _, rank)| *rank)
        else {
            return Ok(None);
        };
        handler.SetCurrentMediaType(chosen)?;
        Ok(Some(mode.frame_rate as f32))
    }
}

/// How much a picture of `subtype` costs to bring to BGRA: planar 4:2:0
/// least, then packed 4:2:2, then the rest, JPEG most.
fn rank(subtype: GUID) -> u8 {
    if subtype == MFVideoFormat_NV12 {
        0
    } else if subtype == MFVideoFormat_YUY2 {
        1
    } else if subtype == MFVideoFormat_MJPG {
        3
    } else {
        2
    }
}

/// Copies the BGRA picture of `sample` into `target`: from the texture it
/// lies in on the canvas's device, else up from the rows it was read into.
fn copy(
    context: &ID3D11DeviceContext,
    target: &ID3D11Texture2D,
    sample: &IMFSample,
    width: u32,
    height: u32,
) -> Option<()> {
    // SAFETY: the sample's buffer is the reader's, alive while the sample
    // is; its texture was made on the canvas's device, of the target's size
    // and format; its rows are locked around the upload, `pitch` bytes
    // apart from the top row for its height.
    unsafe {
        let buffer = sample.GetBufferByIndex(0).ok()?;
        if let Ok(dxgi) = buffer.cast::<IMFDXGIBuffer>() {
            let mut texture: *mut std::ffi::c_void = std::ptr::null_mut();
            dxgi.GetResource(&ID3D11Texture2D::IID, &mut texture).ok()?;
            let texture = ID3D11Texture2D::from_raw(texture);
            let index = dxgi.GetSubresourceIndex().ok()?;
            context.CopySubresourceRegion(target, 0, 0, 0, 0, &texture, index, None);
            return Some(());
        }
        let rows = buffer.cast::<IMF2DBuffer>().ok()?;
        let mut top = std::ptr::null_mut();
        let mut pitch = 0i32;
        rows.Lock2D(&mut top, &mut pitch).ok()?;
        if pitch > 0 {
            context.UpdateSubresource(target, 0, None, top.cast(), pitch as u32, 0);
        } else {
            // Bottom-up rows: laid top-down before the upload.
            let row = width as usize * 4;
            let mut flipped = Vec::with_capacity(row * height as usize);
            for y in 0..height as isize {
                let line = top.offset(y * pitch as isize).cast_const();
                flipped.extend_from_slice(std::slice::from_raw_parts(line, row));
            }
            context.UpdateSubresource(target, 0, None, flipped.as_ptr().cast(), row as u32, 0);
        }
        let _ = rows.Unlock2D();
        Some(())
    }
}

/// Attributes with room for `count`.
fn attributes(count: u32) -> windows::core::Result<IMFAttributes> {
    let mut attributes = None;
    // SAFETY: the call fills the local it is handed.
    unsafe { MFCreateAttributes(&mut attributes, count)? };
    attributes.ok_or_else(windows::core::Error::empty)
}

/// `text` as a nul-terminated wide string.
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

/// The cameras Media Foundation lists, each named by its symbolic link.
pub(crate) fn cameras() -> Vec<CameraInfo> {
    // SAFETY: the calling thread enters the multithreaded apartment unless
    // it is in one already, and leaves only what it entered; Media
    // Foundation starts and shuts down around the listing.
    unsafe {
        let entered = CoInitializeEx(None, COINIT_MULTITHREADED).is_ok();
        let cameras = if MFStartup(MF_VERSION, MFSTARTUP_LITE).is_ok() {
            let cameras = list();
            let _ = MFShutdown();
            cameras
        } else {
            Vec::new()
        };
        if entered {
            CoUninitialize();
        }
        cameras
    }
}

/// The video capture devices, the default first, on a thread in an
/// apartment with Media Foundation started.
fn list() -> Vec<CameraInfo> {
    let Ok(attributes) = attributes(1) else {
        return Vec::new();
    };
    // SAFETY: the call fills the array and count it is handed; each
    // activation object of the array is taken over once, and the array
    // freed after.
    let activates = unsafe {
        if attributes
            .SetGUID(
                &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE,
                &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID,
            )
            .is_err()
        {
            return Vec::new();
        }
        let mut array: *mut Option<IMFActivate> = std::ptr::null_mut();
        let mut count = 0u32;
        if MFEnumDeviceSources(&attributes, &mut array, &mut count).is_err() || array.is_null() {
            return Vec::new();
        }
        let activates: Vec<IMFActivate> = (0..count as usize)
            .filter_map(|i| array.add(i).read())
            .collect();
        CoTaskMemFree(Some(array.cast_const().cast()));
        activates
    };
    activates
        .iter()
        .filter_map(|activate| {
            let name = string(activate, &MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME)?;
            let id = string(
                activate,
                &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK,
            )?;
            let facing = panel(&id).unwrap_or_else(|| camera::facing_by_name(&name));
            Some(CameraInfo { id, name, facing })
        })
        .collect()
}

/// The string attribute `key` of `activate`.
fn string(activate: &IMFActivate, key: &GUID) -> Option<String> {
    let mut text = PWSTR::null();
    let mut length = 0u32;
    // SAFETY: the call allocates the string it hands over, read once and
    // freed after.
    unsafe {
        activate
            .GetAllocatedString(key, &mut text, &mut length)
            .ok()?;
        let read = text.to_string().ok();
        CoTaskMemFree(Some(text.0.cast_const().cast()));
        read
    }
}

/// The panel the device behind the interface `link` is mounted in, as its
/// firmware's physical location tells: the screen's side, or the back.
fn panel(link: &str) -> Option<CameraFacing> {
    let link = wide(link);
    // SAFETY: each call reads the nul-terminated strings and key it is
    // handed and writes at most the size it is told into the buffer it is
    // handed, and the size it wrote into the local.
    unsafe {
        let mut kind = DEVPROPTYPE::default();
        let mut instance = [0u16; 512];
        let mut size = std::mem::size_of_val(&instance) as u32;
        if CM_Get_Device_Interface_PropertyW(
            PCWSTR(link.as_ptr()),
            &DEVPKEY_Device_InstanceId,
            &mut kind,
            Some(instance.as_mut_ptr().cast()),
            &mut size,
            0,
        ) != CR_SUCCESS
        {
            return None;
        }
        let mut node = 0u32;
        if CM_Locate_DevNodeW(
            &mut node,
            PCWSTR(instance.as_ptr()),
            CM_LOCATE_DEVNODE_NORMAL,
        ) != CR_SUCCESS
        {
            return None;
        }
        let mut location = [0u8; 64];
        let mut size = location.len() as u32;
        if CM_Get_DevNode_PropertyW(
            node,
            &DEVPKEY_Device_PhysicalDeviceLocation,
            &mut kind,
            Some(location.as_mut_ptr()),
            &mut size,
            0,
        ) != CR_SUCCESS
            || size < 9
        {
            return None;
        }
        // The ACPI physical location's panel, bits 67 to 69.
        match (location[8] >> 3) & 7 {
            4 => Some(CameraFacing::Front),
            5 => Some(CameraFacing::Back),
            _ => None,
        }
    }
}
