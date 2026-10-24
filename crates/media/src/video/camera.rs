//! Cameras: which one a capture opens, and at what size and rate.
//!
//! A camera plays as a live video: [`VideoSource::Camera`](super::VideoSource)
//! opens it through the same [`VideoPlayer`](super::VideoPlayer), ready once
//! its first picture arrives, capturing while it plays and letting the device
//! go while paused. Its pictures come as a video's do, in the memory the
//! platform captured them into.

// What the targets that capture share; one without a camera uses none of it.
#![cfg_attr(
    not(any(
        target_os = "linux",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly"
    )),
    allow(dead_code)
)]

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

/// One way a camera captures: a size, and the most pictures a second it
/// keeps up at it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct CameraMode {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) frame_rate: f64,
}

/// The index of the mode of `modes` nearest what `request` wants: the
/// smallest at least as large as asked, else the largest; at that size, the
/// rate nearest at or above the one asked, else the fastest.
pub(crate) fn nearest_mode(modes: &[CameraMode], request: &CameraRequest) -> Option<usize> {
    let wanted = (request.width.max(1), request.height.max(1));
    let covers = |m: &CameraMode| m.width >= wanted.0 && m.height >= wanted.1;
    let area = |m: &CameraMode| u64::from(m.width) * u64::from(m.height);
    let size = if modes.iter().any(covers) {
        modes.iter().filter(|m| covers(m)).min_by_key(|m| area(m))
    } else {
        modes.iter().max_by_key(|m| area(m))
    }?;
    let (w, h) = (size.width, size.height);
    let rate = f64::from(request.frame_rate.max(1));
    let at_size = || {
        modes
            .iter()
            .enumerate()
            .filter(move |(_, m)| (m.width, m.height) == (w, h))
    };
    let enough = at_size()
        .filter(|(_, m)| m.frame_rate + 0.5 >= rate)
        .min_by(|a, b| a.1.frame_rate.total_cmp(&b.1.frame_rate));
    enough
        .or_else(|| at_size().max_by(|a, b| a.1.frame_rate.total_cmp(&b.1.frame_rate)))
        .map(|(i, _)| i)
}

/// The camera of `cameras` that `choice` names: the default is the first
/// facing the user, else the first.
pub(crate) fn chosen<'a>(
    cameras: &'a [CameraInfo],
    choice: &CameraChoice,
) -> Option<&'a CameraInfo> {
    let facing = |f: CameraFacing| cameras.iter().find(move |c| c.facing == f);
    match choice {
        CameraChoice::Id(id) => cameras.iter().find(|c| &c.id == id),
        CameraChoice::Facing(f) => facing(*f).or_else(|| chosen(cameras, &CameraChoice::Default)),
        CameraChoice::Default => facing(CameraFacing::Front).or_else(|| cameras.first()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mode(width: u32, height: u32, frame_rate: f64) -> CameraMode {
        CameraMode {
            width,
            height,
            frame_rate,
        }
    }

    #[test]
    fn the_nearest_mode_covers_the_size_asked_at_the_rate_asked() {
        let modes = [
            mode(640, 480, 30.0),
            mode(1280, 720, 10.0),
            mode(1280, 720, 30.0),
            mode(1280, 720, 60.0),
            mode(1920, 1080, 30.0),
            mode(3840, 2160, 15.0),
        ];
        let request = CameraRequest::default();
        assert_eq!(nearest_mode(&modes, &request), Some(2));
        let fast = CameraRequest {
            frame_rate: 50,
            ..request.clone()
        };
        assert_eq!(nearest_mode(&modes, &fast), Some(3));
        let faster = CameraRequest {
            frame_rate: 120,
            ..request.clone()
        };
        assert_eq!(
            nearest_mode(&modes, &faster),
            Some(3),
            "the fastest at that size"
        );
        let large = CameraRequest {
            width: 1600,
            height: 900,
            ..request.clone()
        };
        assert_eq!(nearest_mode(&modes, &large), Some(4));
        let huge = CameraRequest {
            width: 8000,
            height: 6000,
            ..request
        };
        assert_eq!(nearest_mode(&modes, &huge), Some(5), "the largest it has");
        assert_eq!(nearest_mode(&[], &CameraRequest::default()), None);
    }

    #[test]
    fn a_choice_names_a_camera_and_the_default_faces_the_user() {
        let camera = |id: &str, facing| CameraInfo {
            id: id.into(),
            name: id.into(),
            facing,
        };
        let cameras = [
            camera("usb", CameraFacing::External),
            camera("back", CameraFacing::Back),
            camera("front", CameraFacing::Front),
        ];
        let id = |c: Option<&CameraInfo>| c.map(|c| c.id.clone());
        assert_eq!(
            id(chosen(&cameras, &CameraChoice::Default)),
            Some("front".into())
        );
        assert_eq!(
            id(chosen(&cameras, &CameraChoice::Facing(CameraFacing::Back))),
            Some("back".into())
        );
        assert_eq!(
            id(chosen(&cameras, &CameraChoice::Id("usb".into()))),
            Some("usb".into())
        );
        assert_eq!(chosen(&cameras, &CameraChoice::Id("gone".into())), None);
        assert_eq!(
            id(chosen(
                &cameras[..1],
                &CameraChoice::Facing(CameraFacing::Back)
            )),
            Some("usb".into()),
            "none facing so: the default"
        );
    }
}
