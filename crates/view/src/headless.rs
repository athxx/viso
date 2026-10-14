//! How a tool launches a headless build: the surface every window gets and
//! how many frames the run lasts, passed in the environment.
//!
//! A headless build reads them as it starts; a run without them gets the
//! default surface and lasts until the app settles.

/// The environment variable the surface is passed in (see
/// [`HeadlessSurface::to_env`]).
pub const SURFACE_ENV: &str = "VISO_HEADLESS_SURFACE";

/// The environment variable the frame bound is passed in, a count of frames:
/// the run ends after them even if the app keeps asking for more.
pub const FRAMES_ENV: &str = "VISO_HEADLESS_FRAMES";

/// The largest edge of a surface, in logical pixels.
pub const MAX_EDGE: u32 = 16384;

/// The largest scale of a surface.
pub const MAX_SCALE: f64 = 8.0;

/// Every window's surface in a headless run: its size in logical pixels and
/// its device pixels per logical pixel.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HeadlessSurface {
    pub width: u32,
    pub height: u32,
    pub scale: f64,
}

impl Default for HeadlessSurface {
    /// 1280 × 720 at 1x.
    fn default() -> Self {
        HeadlessSurface {
            width: 1280,
            height: 720,
            scale: 1.0,
        }
    }
}

impl HeadlessSurface {
    /// Whether every part is in range: each edge 1 to [`MAX_EDGE`], the
    /// scale above 0 and at most [`MAX_SCALE`].
    pub fn is_valid(&self) -> bool {
        (1..=MAX_EDGE).contains(&self.width)
            && (1..=MAX_EDGE).contains(&self.height)
            && self.scale > 0.0
            && self.scale <= MAX_SCALE
    }

    /// The surface as [`SURFACE_ENV`] spells it: `<width>x<height>@<scale>`.
    pub fn to_env(self) -> String {
        format!("{}x{}@{}", self.width, self.height, self.scale)
    }

    /// The surface `value` spells, `None` unless it is valid.
    pub fn from_env(value: &str) -> Option<HeadlessSurface> {
        let (size, scale) = value.trim().split_once('@')?;
        let (width, height) = size.split_once('x')?;
        let surface = HeadlessSurface {
            width: width.parse().ok()?,
            height: height.parse().ok()?,
            scale: scale.parse().ok()?,
        };
        surface.is_valid().then_some(surface)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_surface_round_trips_through_the_environment() {
        for surface in [
            HeadlessSurface::default(),
            HeadlessSurface {
                width: 390,
                height: 844,
                scale: 3.0,
            },
            HeadlessSurface {
                width: 412,
                height: 915,
                scale: 2.625,
            },
        ] {
            assert_eq!(HeadlessSurface::from_env(&surface.to_env()), Some(surface));
        }
        assert_eq!(HeadlessSurface::default().to_env(), "1280x720@1");
        for bad in [
            "",
            "1280x720",
            "1280@1",
            "0x720@1",
            "1280x720@0",
            "1280x720@9",
            "1280x99999@1",
            "1280x720@NaN",
            "-1x720@1",
        ] {
            assert_eq!(HeadlessSurface::from_env(bad), None, "{bad:?}");
        }
    }
}
