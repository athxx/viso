//! The embedded panel a build is configured for: its mode
//! (`VISO_KMS_MODE`), pixel format (`VISO_KMS_FORMAT`), rotation
//! (`VISO_KMS_ROTATION`) and scale (`VISO_SCALE`). A variable set when the
//! app runs wins; else the value baked in when the app was built (a device
//! target's `display`, which `viso build` sets); else the display's own. The
//! KMS backend applies them to the card, and desktop emulation shapes a
//! window like the panel.

#![cfg_attr(test, allow(dead_code))]

use viso_handle::ScanoutFormat;

/// The variable `name` as the app runs, else as it was built (`built`, the
/// caller's `option_env!`).
pub(crate) fn setting(name: &str, built: Option<&'static str>) -> Option<String> {
    std::env::var(name)
        .ok()
        .or_else(|| built.map(str::to_owned))
}

/// A mode: `1280x720`, or `1280x720@60` with its refresh rate.
pub(crate) fn parse_mode(text: &str) -> Option<((u16, u16), Option<u32>)> {
    let (size, rate) = text.trim().split_once('@').unwrap_or((text.trim(), ""));
    let (w, h) = size.split_once('x')?;
    let size = (w.parse().ok()?, h.parse().ok()?);
    if size.0 == 0 || size.1 == 0 {
        return None;
    }
    Some((size, rate.parse().ok()))
}

/// A rotation in degrees (`0`, `90`, `180`, `270`) as quarter turns.
pub(crate) fn parse_turns(text: &str) -> Option<u8> {
    match text.trim() {
        "0" => Some(0),
        "90" => Some(1),
        "180" => Some(2),
        "270" => Some(3),
        _ => None,
    }
}

/// A pixel format by its DRM name (`xrgb8888`, `rgb565`).
pub(crate) fn parse_format(text: &str) -> Option<ScanoutFormat> {
    match text.trim() {
        "xrgb8888" => Some(ScanoutFormat::Xrgb8888),
        "rgb565" => Some(ScanoutFormat::Rgb565),
        _ => None,
    }
}

/// `VISO_SCALE`, when it is a positive number.
pub(crate) fn scale() -> Option<f64> {
    std::env::var("VISO_SCALE")
        .ok()?
        .parse::<f64>()
        .ok()
        .filter(|s| s.is_finite() && *s > 0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mode_is_a_size_and_maybe_a_rate() {
        assert_eq!(parse_mode("1280x720"), Some(((1280, 720), None)));
        assert_eq!(parse_mode(" 800x480@60 "), Some(((800, 480), Some(60))));
        assert_eq!(parse_mode("0x480"), None);
        assert_eq!(parse_mode("800"), None);
        assert_eq!(parse_mode("800xabc"), None);
    }

    #[test]
    fn rotations_and_formats_parse_by_name() {
        assert_eq!(parse_turns("270"), Some(3));
        assert_eq!(parse_turns("45"), None);
        assert_eq!(parse_format("rgb565"), Some(ScanoutFormat::Rgb565));
        assert_eq!(parse_format("argb"), None);
    }
}
