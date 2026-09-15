//! Pixel snapping and hairlines, in **device** pixels.
//!
//! Snapping decides whether and how a primitive's device-space coordinates are
//! pinned to the pixel grid before rasterization. A crisp 1px border wants its
//! edges on integer device rows/columns; a rotated or free-floating shape wants
//! no snapping at all so anti-aliasing can do its job. The policy is explicit
//! per primitive via [`PixelSnap`] rather than a global toggle, because the
//! right answer differs by primitive kind and by transform.
//!
//! This complements the logical-space `dpi_snap` on `DVec2`/`DRect`, which pins
//! an accumulated layout coordinate to the device grid *before* it crosses into
//! `f32` render space. [`PixelSnap`] here acts on the device-space `f32`
//! coordinates the renderer actually rasterizes.
//!
//! A [`Hairline`] is the separate problem of a line that must stay ~1 device
//! pixel wide regardless of device scale, rotation, or a non-integer transform —
//! a 1px rule at 2x is 2 device pixels, and under rotation its footprint is the
//! projected width, so the geometric width has to be derived from the device
//! scale rather than hardcoded.

/// How a primitive's device-space coordinates snap to the pixel grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PixelSnap {
    /// No snapping — rasterize at the exact sub-pixel position. The right choice
    /// for rotated / non-axis-aligned / freely animated geometry.
    #[default]
    None,
    /// Snap the origin (position) to the device grid; keep the size as-is. Keeps
    /// a shape from shimmering as it moves without changing its extent.
    Position,
    /// Snap the full bounds (origin **and** far edge) to the device grid, so
    /// both edges land on pixel boundaries. The choice for crisp filled rects /
    /// backgrounds.
    Bounds,
    /// Snap so a stroke straddles the grid correctly: the geometric edge is
    /// pinned to a half-pixel offset for odd device-pixel widths so the stroke
    /// covers whole pixels instead of two half-covered rows.
    Stroke,
}

/// Round one device-space coordinate to the nearest device pixel.
#[inline]
pub fn snap_device(v: f32) -> f32 {
    v.round()
}

/// Snap the origin of a device-space box, leaving the size unchanged.
#[inline]
pub fn snap_position(x: f32, y: f32) -> (f32, f32) {
    (x.round(), y.round())
}

/// Snap both edges of a device-space box: the near edge rounds down-or-nearest
/// and the far edge is re-derived so the extent stays on the grid.
#[inline]
pub fn snap_bounds(x: f32, y: f32, w: f32, h: f32) -> (f32, f32, f32, f32) {
    let x0 = x.round();
    let y0 = y.round();
    let x1 = (x + w).round();
    let y1 = (y + h).round();
    (x0, y0, x1 - x0, y1 - y0)
}

/// Snap a stroke's center coordinate for a given device-pixel stroke width so
/// the covered pixels are whole. Odd widths straddle a pixel center (`+0.5`),
/// even widths sit on a pixel boundary.
#[inline]
pub fn snap_stroke_center(center: f32, device_width: f32) -> f32 {
    let odd = (device_width.round() as i32) & 1 == 1;
    if odd {
        center.round() + 0.5
    } else {
        center.round()
    }
}

/// A line that must appear ~1 device pixel wide independent of device scale and
/// transform.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Hairline {
    /// Target width in **device** pixels (typically `1.0`).
    pub device_px: f32,
}

impl Hairline {
    /// A one-device-pixel hairline.
    pub const ONE: Hairline = Hairline { device_px: 1.0 };

    /// The geometric (pre-transform, logical) stroke width that yields
    /// [`device_px`](Hairline::device_px) device pixels at the given uniform
    /// `device_scale` (e.g. `2.0` on a 2x display): `device_px / device_scale`.
    #[inline]
    pub fn logical_width(self, device_scale: f32) -> f32 {
        if device_scale <= 0.0 {
            self.device_px
        } else {
            self.device_px / device_scale
        }
    }

    /// The geometric width needed so the *projected* device footprint is
    /// [`device_px`](Hairline::device_px) after a transform whose per-axis
    /// scales are `sx`/`sy` (in device pixels per geometric unit). Uses the
    /// larger axis scale so a rotated / anisotropically-scaled hairline never
    /// falls below one device pixel on its thinnest projection.
    #[inline]
    pub fn geometric_width(self, sx: f32, sy: f32) -> f32 {
        let s = sx.abs().max(sy.abs());
        if s <= 0.0 {
            self.device_px
        } else {
            self.device_px / s
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[inline]
    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-5
    }

    #[test]
    fn bounds_snap_keeps_extent_on_grid() {
        let (x, y, w, h) = snap_bounds(1.4, 2.6, 10.3, 4.4);
        assert_eq!((x, y), (1.0, 3.0));
        // Far edges 11.7 -> 12, 7.0 -> 7; extents re-derived.
        assert_eq!((w, h), (11.0, 4.0));
    }

    #[test]
    fn position_snap_leaves_size() {
        let (x, y) = snap_position(1.4, 2.6);
        assert_eq!((x, y), (1.0, 3.0));
    }

    #[test]
    fn stroke_center_odd_straddles_pixel() {
        assert!(close(snap_stroke_center(4.2, 1.0), 4.5));
        assert!(close(snap_stroke_center(4.2, 3.0), 4.5));
    }

    #[test]
    fn stroke_center_even_on_boundary() {
        assert!(close(snap_stroke_center(4.2, 2.0), 4.0));
        assert!(close(snap_stroke_center(4.7, 4.0), 5.0));
    }

    #[test]
    fn hairline_scales_with_device_scale() {
        for scale in [1.0f32, 1.5, 2.0, 2.75] {
            let w = Hairline::ONE.logical_width(scale);
            assert!(close(w * scale, 1.0), "scale={scale} w={w}");
        }
    }

    #[test]
    fn hairline_uses_max_axis_scale_under_rotation() {
        // 45-degree rotation at uniform scale 2: both axis projections ~1.414,
        // so geometric width stays 1/1.414 and projects back to ~1 device px.
        let s = 2.0 * std::f32::consts::FRAC_1_SQRT_2;
        let w = Hairline::ONE.geometric_width(s, s);
        assert!(close(w * s, 1.0));
    }

    #[test]
    fn hairline_non_positive_scale_is_noop() {
        assert_eq!(Hairline::ONE.logical_width(0.0), 1.0);
        assert_eq!(Hairline::ONE.geometric_width(0.0, 0.0), 1.0);
    }
}
