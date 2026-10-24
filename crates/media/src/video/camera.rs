//! Cameras: which one a capture opens, and at what size and rate.
//!
//! A camera plays as a live video: [`VideoSource::Camera`](super::VideoSource)
//! opens it through the same [`VideoPlayer`](super::VideoPlayer), ready once
//! its first picture arrives, capturing while it plays and letting the device
//! go while paused. Its pictures come as a video's do, in the memory the
//! platform captured them into.

/// Which way a camera faces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CameraFacing {
    /// Toward the user: a phone's selfie camera, a laptop's.
    Front,
    /// Away from the user: a phone's main camera.
    Back,
    /// Plugged in, facing wherever it is put.
    External,
}

/// Which camera to open.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub enum CameraChoice {
    /// The platform's default: a laptop's own, a phone's front.
    #[default]
    Default,
    /// The first facing this way, else the default.
    Facing(CameraFacing),
    /// The one [`cameras`](super::cameras) names so.
    Id(String),
}

/// A camera to open, and the pictures wanted of it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CameraRequest {
    pub camera: CameraChoice,
    /// The picture's size wanted, in pixels: the camera's nearest is taken,
    /// at least as large where it has one.
    pub width: u32,
    pub height: u32,
    /// The pictures wanted a second: the nearest the camera keeps up at that
    /// size is taken.
    pub frame_rate: u32,
}

impl Default for CameraRequest {
    /// The default camera at 1280 × 720, 30 pictures a second: what a
    /// preview needs, without the cost of the sensor's largest.
    fn default() -> CameraRequest {
        CameraRequest {
            camera: CameraChoice::Default,
            width: 1280,
            height: 720,
            frame_rate: 30,
        }
    }
}

impl CameraRequest {
    /// The default size and rate from the camera facing `facing`.
    pub fn facing(facing: CameraFacing) -> CameraRequest {
        CameraRequest {
            camera: CameraChoice::Facing(facing),
            ..CameraRequest::default()
        }
    }
}

/// A camera the platform has.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CameraInfo {
    /// What [`CameraChoice::Id`] names it by; stable while it is plugged in.
    pub id: String,
    /// Its name, for people.
    pub name: String,
    pub facing: CameraFacing,
}
