//! Path -> R8 coverage rasterization for the clip/mask cold path (§14.4).
//!
//! The mask model's baseline realization is "always one-channel coverage": an
//! arbitrary vector path clip is rasterized to an 8-bit coverage bitmap, tight
//! to its ROI, and sampled as a single R8 texture. This is the same analytic
//! accumulation the text lane uses ([`viso_text::raster_a8`]) — the outline
//! comes from a [`PathCmd`] stream instead of a font, but the sink and the
//! `ab_glyph_rasterizer` accumulation are identical.
//!
//! Coverage is emitted ROI-local: the ROI's top-left maps to pixel (0, 0), and
//! every command point is translated by `-roi.origin` and scaled by the device
//! scale, so the bitmap is exactly `slot.w * slot.h` bytes with no padding. Fill
//! is **NonZero winding** — the `ab_glyph_rasterizer` accumulation is
//! NonZero-oriented, and a [`Primitive::Path`](crate::primitive::Path) carries
//! no fill rule, so every path fill is NonZero at this level. EvenOdd path clips
//! are rejected upstream (at plan time) and fall back to a bounding scissor
//! rather than being rasterized here with the wrong rule.

use ab_glyph_rasterizer::{Point, Rasterizer, point};

use crate::primitive::{PathCmd, Rect};

/// Walks a [`PathCmd`] stream into the rasterizer, mapping world-space command
/// points to the ROI's device-pixel image space (origin at the ROI top-left).
struct PathSink<'a> {
    rasterizer: &'a mut Rasterizer,
    /// Logical-to-device scale applied after translating by `-roi.origin`.
    scale: f32,
    /// ROI top-left in world (logical) space, subtracted so the image starts at
    /// pixel (0, 0).
    x_min: f32,
    y_min: f32,
    /// The subpath's current point and start, kept so `Close` draws the closing
    /// edge back to the start.
    last: Point,
    start: Point,
}

impl PathSink<'_> {
    /// Map a world-space (y-down) point to the ROI-local device-pixel point.
    fn map(&self, p: crate::primitive::Point) -> Point {
        point(
            (p.x - self.x_min) * self.scale,
            (p.y - self.y_min) * self.scale,
        )
    }

    /// Feed one command into the rasterizer, advancing the current point.
    fn push(&mut self, cmd: &PathCmd) {
        match *cmd {
            PathCmd::MoveTo(p) => {
                let p = self.map(p);
                self.last = p;
                self.start = p;
            }
            PathCmd::LineTo(p) => {
                let p = self.map(p);
                self.rasterizer.draw_line(self.last, p);
                self.last = p;
            }
            PathCmd::QuadTo(c, p) => {
                let c = self.map(c);
                let p = self.map(p);
                self.rasterizer.draw_quad(self.last, c, p);
                self.last = p;
            }
            PathCmd::CubicTo(c0, c1, p) => {
                let c0 = self.map(c0);
                let c1 = self.map(c1);
                let p = self.map(p);
                self.rasterizer.draw_cubic(self.last, c0, c1, p);
                self.last = p;
            }
            PathCmd::Close => {
                self.rasterizer.draw_line(self.last, self.start);
                self.last = self.start;
            }
        }
    }
}

/// The world-space bounding rect of a [`PathCmd`] stream, or a zero-size rect at
/// the origin when the stream has no on-curve points.
///
/// Control points are included so a curve that bows outside its endpoints is
/// still enclosed — a superset of the true ink bound, which is exactly what a
/// clip ROI wants (never clips coverage it should keep).
pub fn path_bounds(cmds: impl IntoIterator<Item = PathCmd>) -> Rect {
    let mut seen = false;
    let mut min_x = f32::INFINITY;
    let mut min_y = f32::INFINITY;
    let mut max_x = f32::NEG_INFINITY;
    let mut max_y = f32::NEG_INFINITY;
    let mut acc = |p: crate::primitive::Point| {
        seen = true;
        min_x = min_x.min(p.x);
        min_y = min_y.min(p.y);
        max_x = max_x.max(p.x);
        max_y = max_y.max(p.y);
    };
    for cmd in cmds {
        match cmd {
            PathCmd::MoveTo(p) | PathCmd::LineTo(p) => acc(p),
            PathCmd::QuadTo(c, p) => {
                acc(c);
                acc(p);
            }
            PathCmd::CubicTo(c0, c1, p) => {
                acc(c0);
                acc(c1);
                acc(p);
            }
            PathCmd::Close => {}
        }
    }
    if seen {
        Rect {
            x: min_x,
            y: min_y,
            w: max_x - min_x,
            h: max_y - min_y,
        }
    } else {
        Rect {
            x: 0.0,
            y: 0.0,
            w: 0.0,
            h: 0.0,
        }
    }
}

/// Rasterize a [`PathCmd`] stream to ROI-local R8 coverage at `device_scale`.
///
/// `roi` is the world-space ROI (typically the path's own bounds for a
/// self-masked fill); `width`/`height` are the ROI's device-pixel extent (the
/// [`MaskSlot`](crate::mask::MaskSlot) size). Points are translated by
/// `-roi.origin` and scaled by `device_scale`, so the returned bitmap is
/// row-major `width * height` bytes, top row first, one byte per pixel `0..=255`
/// — NonZero winding. Returns an all-zero bitmap of the requested size when the
/// ROI is degenerate.
pub fn rasterize_path_coverage(
    cmds: impl IntoIterator<Item = PathCmd>,
    roi: Rect,
    device_scale: f32,
    width: u32,
    height: u32,
) -> Vec<u8> {
    let mut out = vec![0u8; (width as usize) * (height as usize)];
    if width == 0 || height == 0 {
        return out;
    }
    let mut rasterizer = Rasterizer::new(width as usize, height as usize);
    let mut sink = PathSink {
        rasterizer: &mut rasterizer,
        scale: device_scale,
        x_min: roi.x,
        y_min: roi.y,
        last: point(0.0, 0.0),
        start: point(0.0, 0.0),
    };
    for cmd in cmds {
        sink.push(&cmd);
    }
    rasterizer.for_each_pixel_2d(|x, y, cov| {
        out[(y * width + x) as usize] = (cov * 255.0).round().clamp(0.0, 255.0) as u8;
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitive::Point as P;

    /// A closed axis-aligned unit-ish square fills its interior to near-full
    /// coverage and leaves the outside empty — the NonZero baseline.
    #[test]
    fn filled_square_covers_interior() {
        let roi = Rect {
            x: 0.0,
            y: 0.0,
            w: 8.0,
            h: 8.0,
        };
        let cmds = [
            PathCmd::MoveTo(P::new(1.0, 1.0)),
            PathCmd::LineTo(P::new(7.0, 1.0)),
            PathCmd::LineTo(P::new(7.0, 7.0)),
            PathCmd::LineTo(P::new(1.0, 7.0)),
            PathCmd::Close,
        ];
        let cov = rasterize_path_coverage(cmds, roi, 1.0, 8, 8);
        assert_eq!(cov.len(), 64);
        // Interior pixel fully covered.
        assert_eq!(cov[(4 * 8 + 4) as usize], 255);
        // Corner outside the square stays empty.
        assert_eq!(cov[0], 0);
    }

    /// Bounds enclose every on- and off-curve point of the stream.
    #[test]
    fn bounds_enclose_all_points() {
        let cmds = [
            PathCmd::MoveTo(P::new(2.0, 3.0)),
            PathCmd::QuadTo(P::new(10.0, -1.0), P::new(6.0, 5.0)),
            PathCmd::Close,
        ];
        let b = path_bounds(cmds);
        assert_eq!(b.x, 2.0);
        assert_eq!(b.y, -1.0);
        assert_eq!(b.w, 8.0);
        assert_eq!(b.h, 6.0);
    }

    /// An empty stream yields a zero rect at the origin, and a zero-size raster
    /// request yields an empty bitmap.
    #[test]
    fn degenerate_inputs() {
        let b = path_bounds([]);
        assert_eq!((b.x, b.y, b.w, b.h), (0.0, 0.0, 0.0, 0.0));
        let cov = rasterize_path_coverage([], b, 1.0, 0, 0);
        assert!(cov.is_empty());
    }

    /// Device scale multiplies the ROI-local extent: a path drawn at scale 2
    /// fills a 2x-larger image region.
    #[test]
    fn device_scale_expands_coverage() {
        let roi = Rect {
            x: 0.0,
            y: 0.0,
            w: 4.0,
            h: 4.0,
        };
        let cmds = [
            PathCmd::MoveTo(P::new(0.0, 0.0)),
            PathCmd::LineTo(P::new(4.0, 0.0)),
            PathCmd::LineTo(P::new(4.0, 4.0)),
            PathCmd::LineTo(P::new(0.0, 4.0)),
            PathCmd::Close,
        ];
        let cov = rasterize_path_coverage(cmds, roi, 2.0, 8, 8);
        // At scale 2 the 4x4 ROI fills the whole 8x8 image.
        assert_eq!(cov[(4 * 8 + 4) as usize], 255);
    }
}
