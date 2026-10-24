//! Cameras on macOS and iOS: an `AVCaptureSession` from the camera's input
//! into a video data output, which hands each picture to a delegate on a
//! serial queue as a BGRA pixel buffer backed by an IOSurface, as the movie
//! player's are, for a GPU to sample where it lies. The delegate keeps the
//! newest for the loop's thread to take. The session starts and stops on
//! that queue, so the loop never waits on the device.
//!
//! Access is asked for at opening where the user has not answered yet; until
//! granted the session is left unconfigured. The app's bundle names why it
//! captures (`NSCameraUsageDescription`), as the OS requires.

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use block2::RcBlock;
use dispatch2::{DispatchQueue, DispatchRetained};
use objc2::rc::Retained;
use objc2::runtime::{Bool, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{AnyThread, DefinedClass, define_class, msg_send};
use objc2_av_foundation::{
    AVAuthorizationStatus, AVCaptureConnection, AVCaptureDevice, AVCaptureDeviceFormat,
    AVCaptureDeviceInput, AVCaptureDevicePosition, AVCaptureDeviceTypeBuiltInWideAngleCamera,
    AVCaptureOutput, AVCaptureSession, AVCaptureSessionPresetInputPriority,
    AVCaptureVideoDataOutput, AVCaptureVideoDataOutputSampleBufferDelegate, AVMediaTypeVideo,
};
use objc2_core_foundation::CFRetained;
use objc2_core_media::{CMSampleBuffer, CMTime, CMVideoFormatDescriptionGetDimensions};
use objc2_core_video::{CVPixelBuffer, CVPixelBufferGetHeight, CVPixelBufferGetWidth};

use super::super::camera::{self, CameraFacing, CameraInfo, CameraMode, CameraRequest};
use super::super::{Frame, VideoError, VideoEvent, VideoInfo};
use super::{Picture, bgra_attributes};

/// Whether the user let the app capture: not answered yet, or answered.
const ASKED: u8 = 0;
const GRANTED: u8 = 1;
const REFUSED: u8 = 2;

/// A picture the output handed over, kept until a newer one comes.
struct Held(CFRetained<CVPixelBuffer>);

// SAFETY: a pixel buffer's references are counted atomically, and the
// output writes no texel of one it handed over; one thread at a time reads
// it.
unsafe impl Send for Held {}

/// What the delegate shares with the player: the newest picture, and how
/// many came.
#[derive(Default)]
struct Shared {
    latest: Mutex<(Option<Held>, u64)>,
}

define_class!(
    // SAFETY: `NSObject` has no subclassing requirements, and the class has
    // no `Drop` of its own.
    #[unsafe(super(NSObject))]
    #[name = "VisoCameraDelegate"]
    #[ivars = Arc<Shared>]
    struct Delegate;

    // SAFETY: `NSObjectProtocol` has no requirements of its own.
    unsafe impl NSObjectProtocol for Delegate {}

    // SAFETY: the method has the signature the protocol declares.
    unsafe impl AVCaptureVideoDataOutputSampleBufferDelegate for Delegate {
        #[unsafe(method(captureOutput:didOutputSampleBuffer:fromConnection:))]
        fn did_output(
            &self,
            _output: &AVCaptureOutput,
            sample: &CMSampleBuffer,
            _connection: &AVCaptureConnection,
        ) {
            // SAFETY: the output's sample buffer, alive for the call; its
            // image buffer is retained to be kept past it.
            let Some(buffer) = (unsafe { sample.image_buffer() }) else {
                return;
            };
            if let Ok(mut latest) = self.ivars().latest.lock() {
                latest.0 = Some(Held(buffer));
                latest.1 += 1;
            }
        }
    }
);

impl Delegate {
    fn new(shared: Arc<Shared>) -> Retained<Delegate> {
        let this = Delegate::alloc().set_ivars(shared);
        // SAFETY: `NSObject`'s `init` on the allocated instance.
        unsafe { msg_send![super(this), init] }
    }
}

/// A session, started and stopped on the capture queue, and its camera.
struct Session(
    Retained<AVCaptureSession>,
    Option<Retained<AVCaptureDevice>>,
);

// SAFETY: a session's `startRunning` and `stopRunning` are meant to be
// called off the main thread, and the queue is serial, so they never run at
// once.
unsafe impl Send for Session {}

impl Session {
    fn run(self) {
        // SAFETY: the capture's session and camera, alive while held here.
        // The camera is held locked while the session starts, which would
        // otherwise put it back in the session's own format.
        unsafe {
            let locked = self
                .1
                .as_ref()
                .filter(|device| device.lockForConfiguration().is_ok());
            self.0.startRunning();
            if let Some(device) = locked {
                device.unlockForConfiguration();
            }
        }
    }

    fn halt(self) {
        // SAFETY: as in `run`.
        unsafe { self.0.stopRunning() }
    }
}

pub(crate) struct Capture {
    request: CameraRequest,
    session: Retained<AVCaptureSession>,
    output: Retained<AVCaptureVideoDataOutput>,
    delegate: Retained<Delegate>,
    queue: DispatchRetained<DispatchQueue>,
    shared: Arc<Shared>,
    /// The user's answer, written by the access request's block.
    access: Arc<AtomicU8>,
    /// The camera configured into the session, once access is granted,
    /// and which way it faces.
    configured: Option<CameraFacing>,
    device: Option<Retained<AVCaptureDevice>>,
    /// The rate the camera was set to capture at.
    rate: f64,
    running: bool,
    ready: bool,
    failed: bool,
    wants_play: bool,
    /// The picture last returned, and which it was.
    held: Option<Held>,
    seen: u64,
}

impl Capture {
    pub(crate) fn open(request: &CameraRequest) -> Result<Capture, VideoError> {
        let media = video_type()?;
        // SAFETY: class methods asking for and reading the app's access to
        // cameras; the block outlives the call, as AVFoundation copies it.
        let access = unsafe {
            match AVCaptureDevice::authorizationStatusForMediaType(media) {
                AVAuthorizationStatus::Authorized => Arc::new(AtomicU8::new(GRANTED)),
                AVAuthorizationStatus::NotDetermined => {
                    let access = Arc::new(AtomicU8::new(ASKED));
                    let answer = Arc::clone(&access);
                    let block = RcBlock::new(move |granted: Bool| {
                        let to = if granted.as_bool() { GRANTED } else { REFUSED };
                        answer.store(to, Ordering::Release);
                    });
                    AVCaptureDevice::requestAccessForMediaType_completionHandler(media, &block);
                    access
                }
                _ => return Err(VideoError::Denied),
            }
        };
        let shared = Arc::new(Shared::default());
        let delegate = Delegate::new(Arc::clone(&shared));
        // SAFETY: a new session and output, configured before the session
        // runs.
        let (session, output) =
            unsafe { (AVCaptureSession::new(), AVCaptureVideoDataOutput::new()) };
        let mut capture = Capture {
            request: request.clone(),
            session,
            output,
            delegate,
            queue: DispatchQueue::new("dev.viso.camera", None),
            shared,
            access,
            configured: None,
            device: None,
            rate: 0.0,
            running: false,
            ready: false,
            failed: false,
            wants_play: false,
            held: None,
            seen: 0,
        };
        if capture.access.load(Ordering::Acquire) == GRANTED {
            capture.configure()?;
        }
        Ok(capture)
    }

    /// Puts the camera the request chooses into the session, in its mode
    /// nearest the request, and starts it: it captures until its first
    /// picture, and then while it plays.
    fn configure(&mut self) -> Result<(), VideoError> {
        let devices = video_devices();
        let infos: Vec<CameraInfo> = devices.iter().map(|d| info_of(d)).collect();
        let info = camera::chosen(&infos, &self.request.camera)
            .ok_or_else(|| VideoError::Open("no such camera".to_owned()))?;
        let at = infos.iter().position(|i| i == info).unwrap_or(0);
        let device = &devices[at];
        // SAFETY: the session is configured between `beginConfiguration`
        // and `commitConfiguration` on this thread, before it runs; the
        // device is locked around the change of its format and rate, each
        // within what the format says it takes.
        unsafe {
            let input = AVCaptureDeviceInput::deviceInputWithDevice_error(device)
                .map_err(|e| VideoError::Open(e.localizedDescription().to_string()))?;
            let session = &self.session;
            session.beginConfiguration();
            if !session.canAddInput(&input) || !session.canAddOutput(&self.output) {
                session.commitConfiguration();
                return Err(VideoError::Open("the camera is in use".to_owned()));
            }
            session.addInput(&input);
            self.output.setVideoSettings(Some(&bgra_attributes()));
            self.output.setAlwaysDiscardsLateVideoFrames(true);
            let delegate = ProtocolObject::from_ref(&*self.delegate);
            self.output
                .setSampleBufferDelegate_queue(Some(delegate), Some(&self.queue));
            session.addOutput(&self.output);
            self.rate = choose_mode(device, &self.request);
            // The format chosen stands: the session presets none of its own.
            if session.canSetSessionPreset(AVCaptureSessionPresetInputPriority) {
                session.setSessionPreset(AVCaptureSessionPresetInputPriority);
            }
            #[cfg(target_os = "ios")]
            upright(&self.output);
            session.commitConfiguration();
        }
        self.configured = Some(info.facing);
        self.device = Some(device.clone());
        self.start();
        Ok(())
    }

    fn start(&mut self) {
        if self.running || self.configured.is_none() {
            return;
        }
        self.running = true;
        let session = Session(self.session.clone(), self.device.clone());
        self.queue.exec_async(move || session.run());
    }

    fn stop(&mut self) {
        if !self.running {
            return;
        }
        self.running = false;
        let session = Session(self.session.clone(), None);
        self.queue.exec_async(move || session.halt());
    }

    /// The newest picture, and how many came.
    fn latest(&self) -> (Option<CFRetained<CVPixelBuffer>>, u64) {
        match self.shared.latest.lock() {
            Ok(latest) => (latest.0.as_ref().map(|held| held.0.clone()), latest.1),
            Err(_) => (None, 0),
        }
    }

    pub(crate) fn poll(&mut self) -> Option<VideoEvent> {
        if self.failed {
            return None;
        }
        if self.configured.is_none() {
            match self.access.load(Ordering::Acquire) {
                GRANTED => {
                    if let Err(error) = self.configure() {
                        self.failed = true;
                        return Some(VideoEvent::Failed(error));
                    }
                }
                REFUSED => {
                    self.failed = true;
                    return Some(VideoEvent::Failed(VideoError::Denied));
                }
                _ => return None,
            }
        }
        if self.ready {
            return None;
        }
        let (Some(buffer), _) = self.latest() else {
            return None;
        };
        self.ready = true;
        if !self.wants_play {
            self.stop();
        }
        Some(VideoEvent::Ready(VideoInfo {
            width: CVPixelBufferGetWidth(&buffer) as u32,
            height: CVPixelBufferGetHeight(&buffer) as u32,
            duration: None,
            frame_rate: (self.rate > 0.0).then_some(self.rate as f32),
            audio: false,
            seekable: false,
            facing: self.configured,
        }))
    }

    pub(crate) fn frame(&mut self) -> Option<Frame<'_>> {
        if !self.ready {
            return None;
        }
        let (buffer, count) = self.latest();
        if count == self.seen {
            return None;
        }
        self.seen = count;
        self.held = buffer.map(Held);
        let buffer = &self.held.as_ref()?.0;
        Some(Frame::native_picture(
            CVPixelBufferGetWidth(buffer) as u32,
            CVPixelBufferGetHeight(buffer) as u32,
            Duration::ZERO,
            Picture(buffer),
        ))
    }

    pub(crate) fn play(&mut self) {
        self.wants_play = true;
        if self.ready {
            self.start();
        }
    }

    pub(crate) fn pause(&mut self) {
        self.wants_play = false;
        if self.ready {
            self.stop();
        }
    }

    pub(crate) fn is_playing(&self) -> bool {
        self.wants_play
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.stop();
        // SAFETY: the output lets go of the delegate on the thread that set
        // it; a picture already on its way only reaches the shared slot.
        unsafe {
            self.output.setSampleBufferDelegate_queue(None, None);
        }
    }
}

/// `AVMediaTypeVideo`.
fn video_type() -> Result<&'static objc2_av_foundation::AVMediaType, VideoError> {
    // SAFETY: AVFoundation's constant, alive for the program.
    unsafe { AVMediaTypeVideo }.ok_or(VideoError::Unsupported)
}

/// The cameras the OS has, the default first.
fn video_devices() -> Vec<Retained<AVCaptureDevice>> {
    let Ok(media) = video_type() else {
        return Vec::new();
    };
    // SAFETY: class methods listing the devices; the discovery session
    // would need the device types each OS version knows.
    #[allow(deprecated)]
    let (all, first) = unsafe {
        (
            AVCaptureDevice::devicesWithMediaType(media),
            AVCaptureDevice::defaultDeviceWithMediaType(media),
        )
    };
    let mut devices: Vec<_> = all.iter().collect();
    if let Some(first) = first
        && let Some(at) = devices.iter().position(|d| **d == *first)
    {
        devices[..=at].rotate_right(1);
    }
    devices
}

/// What `device` is, as listed.
fn info_of(device: &AVCaptureDevice) -> CameraInfo {
    // SAFETY: reads of a listed device's properties.
    unsafe {
        let facing = match device.position() {
            AVCaptureDevicePosition::Front => CameraFacing::Front,
            AVCaptureDevicePosition::Back => CameraFacing::Back,
            // A Mac's own camera says no position; it faces its user.
            _ if device
                .deviceType()
                .isEqualToString(AVCaptureDeviceTypeBuiltInWideAngleCamera) =>
            {
                CameraFacing::Front
            }
            _ => CameraFacing::External,
        };
        CameraInfo {
            id: device.uniqueID().to_string(),
            name: device.localizedName().to_string(),
            facing,
        }
    }
}

/// Sets `device` to its format nearest `request`, at the rate nearest it
/// asks within what the format takes; returns that rate.
///
/// # Safety
///
/// `device` is in a session being configured.
unsafe fn choose_mode(device: &AVCaptureDevice, request: &CameraRequest) -> f64 {
    // SAFETY: reads of the device's formats and their ranges, and changes
    // within them under the device's lock, as the caller promises.
    unsafe {
        let formats: Vec<Retained<AVCaptureDeviceFormat>> = device.formats().iter().collect();
        let modes: Vec<CameraMode> = formats
            .iter()
            .map(|format| {
                let size = CMVideoFormatDescriptionGetDimensions(&format.formatDescription());
                let fastest = format
                    .videoSupportedFrameRateRanges()
                    .iter()
                    .map(|range| range.maxFrameRate())
                    .fold(0.0, f64::max);
                CameraMode {
                    width: size.width.max(0) as u32,
                    height: size.height.max(0) as u32,
                    frame_rate: fastest,
                }
            })
            .collect();
        let Some(at) = camera::nearest_mode(&modes, request) else {
            return 0.0;
        };
        if device.lockForConfiguration().is_err() {
            return 0.0;
        }
        let format = &formats[at];
        device.setActiveFormat(format);
        let wanted = f64::from(request.frame_rate.max(1));
        let ranges = format.videoSupportedFrameRateRanges();
        let range = ranges
            .iter()
            .find(|r| r.minFrameRate() <= wanted && wanted <= r.maxFrameRate())
            .or_else(|| {
                ranges
                    .iter()
                    .max_by(|a, b| a.maxFrameRate().total_cmp(&b.maxFrameRate()))
            });
        let rate = match range {
            Some(range) => {
                let (least, most) = (range.minFrameRate(), range.maxFrameRate());
                let (rate, duration) = if wanted >= most {
                    (most, range.minFrameDuration())
                } else if wanted <= least {
                    (least, range.maxFrameDuration())
                } else {
                    (wanted, CMTime::with_seconds(1.0 / wanted, 600))
                };
                device.setActiveVideoMinFrameDuration(duration);
                device.setActiveVideoMaxFrameDuration(duration);
                rate
            }
            None => 0.0,
        };
        device.unlockForConfiguration();
        rate
    }
}

/// Has the output deliver its pictures upright for a phone held upright,
/// rather than as the sensor lies.
///
/// # Safety
///
/// The output is in a session being configured.
#[cfg(target_os = "ios")]
unsafe fn upright(output: &AVCaptureVideoDataOutput) {
    // SAFETY: the output's connection, as the caller promises; the rotation
    // is set only where the connection takes it.
    unsafe {
        let Ok(media) = video_type() else {
            return;
        };
        let Some(connection) = output.connectionWithMediaType(media) else {
            return;
        };
        let rotates: bool =
            msg_send![&connection, respondsToSelector: objc2::sel!(setVideoRotationAngle:)];
        if rotates {
            if connection.isVideoRotationAngleSupported(90.0) {
                connection.setVideoRotationAngle(90.0);
            }
        } else {
            #[allow(deprecated)]
            if connection.isVideoOrientationSupported() {
                connection
                    .setVideoOrientation(objc2_av_foundation::AVCaptureVideoOrientation::Portrait);
            }
        }
    }
}

/// The cameras the OS has, the default first.
pub(crate) fn cameras() -> Vec<CameraInfo> {
    video_devices().iter().map(|d| info_of(d)).collect()
}
