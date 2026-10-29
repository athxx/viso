//! The embedded panel a build is configured for: its mode
//! (`VISO_KMS_MODE`), pixel format (`VISO_KMS_FORMAT`), rotation
//! (`VISO_KMS_ROTATION`) and scale (`VISO_SCALE`). A variable set when the
//! app runs wins; else the value baked in when the app was built (a device
//! target's `display`, which `viso build` sets); else the display's own. The
//! KMS backend applies them to the card, and desktop emulation shapes a
//! window like the panel.
//!
//! With `VISO_SCANOUT_CAPTURE` naming a file, each CPU-drawn frame either
//! presents is written there as a PNG, replacing the one before, so that a
//! board's frames can be compared pixel for pixel with the emulated panel's.

#![cfg_attr(test, allow(dead_code))]

use viso_handle::ScanoutFormat;

/// The variable `name` as the app runs, else as it was built (`built`, the
/// caller's `option_env!`).
pub(crate) fn setting(name: &str, built: Option<&'static str>) -> Option<String> {
    std::env::var(name)
        .ok()
        .or_else(|| built.map(str::to_owned))
}

/// Where presented CPU frames are written (`VISO_SCANOUT_CAPTURE`).
pub(crate) struct Capture(std::path::PathBuf);

impl Capture {
    /// The capture the environment asks for, if any.
    pub(crate) fn configured() -> Option<Capture> {
        std::env::var_os("VISO_SCANOUT_CAPTURE")
            .filter(|path| !path.is_empty())
            .map(|path| Capture(path.into()))
    }

    /// Writes the frame `pixels`, `width` pixels in `format` a row and
    /// `stride` bytes apart. The file is replaced whole, never left half
    /// written.
    pub(crate) fn frame(&self, format: ScanoutFormat, width: u32, pixels: &[u8], stride: usize) {
        let height = (pixels.len() / stride.max(1)) as u32;
        let png =
            crate::clipboard::png::encode(width, height, &rgba(format, width, pixels, stride));
        let part = self.0.with_extension("part");
        if std::fs::write(&part, png).is_ok() {
            let _ = std::fs::rename(&part, &self.0);
        }
    }
}

/// Opaque RGBA8 of `pixels` in `format`; an RGB565 channel widened by
/// repeating its high bits.
fn rgba(format: ScanoutFormat, width: u32, pixels: &[u8], stride: usize) -> Vec<u8> {
    let width = width as usize;
    let mut out = Vec::with_capacity(width * 4 * (pixels.len() / stride.max(1)));
    for row in pixels.chunks_exact(stride) {
        match format {
            ScanoutFormat::Xrgb8888 => {
                for px in row[..width * 4].as_chunks::<4>().0 {
                    out.extend_from_slice(&[px[2], px[1], px[0], 255]);
                }
            }
            ScanoutFormat::Rgb565 => {
                for px in row[..width * 2].as_chunks::<2>().0 {
                    let v = u16::from_le_bytes([px[0], px[1]]);
                    let (r, g, b) = ((v >> 11) as u8, ((v >> 5) & 63) as u8, (v & 31) as u8);
                    out.extend_from_slice(&[
                        (r << 3) | (r >> 2),
                        (g << 2) | (g >> 4),
                        (b << 3) | (b >> 2),
                        255,
                    ]);
                }
            }
        }
    }
    out
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
    fn a_frame_is_captured_as_opaque_rgba() {
        let xrgb = [1, 2, 3, 9, 4, 5, 6, 9, 0, 0, 0, 0];
        assert_eq!(
            rgba(ScanoutFormat::Xrgb8888, 2, &xrgb, 12),
            [3, 2, 1, 255, 6, 5, 4, 255]
        );
        let white_red = [0xff, 0xff, 0x00, 0xf8];
        assert_eq!(
            rgba(ScanoutFormat::Rgb565, 2, &white_red, 4),
            [255, 255, 255, 255, 255, 0, 0, 255]
        );
    }

    #[test]
    fn rotations_and_formats_parse_by_name() {
        assert_eq!(parse_turns("270"), Some(3));
        assert_eq!(parse_turns("45"), None);
        assert_eq!(parse_format("rgb565"), Some(ScanoutFormat::Rgb565));
        assert_eq!(parse_format("argb"), None);
    }
}
