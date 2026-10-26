//! A content stream: the operators that draw a page, a form or a glyph.

use viso_render::{Corners, PathCmd, Point, Rect};

use crate::object::num;

/// How far a cubic's control points sit along the tangent for a quarter
/// circle.
const KAPPA: f32 = 0.552_284_8;

/// The operators written so far.
#[derive(Debug, Default)]
pub(crate) struct Content {
    pub(crate) buf: Vec<u8>,
}

impl Content {
    /// Appends the numbers `values` and the operator `op` on a line.
    pub(crate) fn op(&mut self, values: &[f32], op: &str) {
        for &v in values {
            num(&mut self.buf, v);
            self.buf.push(b' ');
        }
        self.buf.extend_from_slice(op.as_bytes());
        self.buf.push(b'\n');
    }

    /// Appends a resource name and the operator `op`: `/Name op`.
    pub(crate) fn named(&mut self, name: &str, op: &str) {
        self.buf.push(b'/');
        self.buf.extend_from_slice(name.as_bytes());
        self.buf.push(b' ');
        self.buf.extend_from_slice(op.as_bytes());
        self.buf.push(b'\n');
    }

    /// Appends `raw` as it is.
    pub(crate) fn raw(&mut self, raw: &[u8]) {
        self.buf.extend_from_slice(raw);
    }

    pub(crate) fn save(&mut self) {
        self.buf.extend_from_slice(b"q\n");
    }

    pub(crate) fn restore(&mut self) {
        self.buf.extend_from_slice(b"Q\n");
    }

    /// Concatenates the matrix `[a b c d e f]` onto the current one.
    pub(crate) fn transform(&mut self, m: [f32; 6]) {
        self.op(&m, "cm");
    }

    /// Sets the fill color to the sRGB `rgb`.
    pub(crate) fn fill_rgb(&mut self, rgb: [f32; 3]) {
        self.op(&rgb, "rg");
    }

    /// Sets the stroke color to the sRGB `rgb`.
    pub(crate) fn stroke_rgb(&mut self, rgb: [f32; 3]) {
        self.op(&rgb, "RG");
    }

    pub(crate) fn rect(&mut self, r: Rect) {
        self.op(&[r.x, r.y, r.w, r.h], "re");
    }

    /// Adds `r` rounded by `corners` (normalized against it) as a closed
    /// subpath, clockwise on screen.
    pub(crate) fn rrect(&mut self, r: Rect, corners: Corners) {
        let c = corners.normalized(r.w, r.h);
        if c.is_sharp() {
            self.rect(r);
            return;
        }
        let (x0, y0, x1, y1) = (r.x, r.y, r.x + r.w, r.y + r.h);
        let k = 1.0 - KAPPA;
        self.op(&[x0 + c.left_top, y0], "m");
        self.op(&[x1 - c.right_top, y0], "l");
        let t = c.right_top;
        if t > 0.0 {
            self.op(&[x1 - t * k, y0, x1, y0 + t * k, x1, y0 + t], "c");
        }
        self.op(&[x1, y1 - c.right_bottom], "l");
        let t = c.right_bottom;
        if t > 0.0 {
            self.op(&[x1, y1 - t * k, x1 - t * k, y1, x1 - t, y1], "c");
        }
        self.op(&[x0 + c.left_bottom, y1], "l");
        let t = c.left_bottom;
        if t > 0.0 {
            self.op(&[x0 + t * k, y1, x0, y1 - t * k, x0, y1 - t], "c");
        }
        self.op(&[x0, y0 + c.left_top], "l");
        let t = c.left_top;
        if t > 0.0 {
            self.op(&[x0, y0 + t * k, x0 + t * k, y0, x0 + t, y0], "c");
        }
        self.op(&[], "h");
    }

    /// Adds the ellipse inscribed in `r` as a closed subpath.
    pub(crate) fn ellipse(&mut self, r: Rect) {
        let (rx, ry) = (r.w * 0.5, r.h * 0.5);
        let (cx, cy) = (r.x + rx, r.y + ry);
        let (kx, ky) = (rx * KAPPA, ry * KAPPA);
        self.op(&[cx + rx, cy], "m");
        self.op(&[cx + rx, cy + ky, cx + kx, cy + ry, cx, cy + ry], "c");
        self.op(&[cx - kx, cy + ry, cx - rx, cy + ky, cx - rx, cy], "c");
        self.op(&[cx - rx, cy - ky, cx - kx, cy - ry, cx, cy - ry], "c");
        self.op(&[cx + kx, cy - ry, cx + rx, cy - ky, cx + rx, cy], "c");
        self.op(&[], "h");
    }

    /// Adds the outline `cmds`, quadratics raised to cubics.
    pub(crate) fn path(&mut self, cmds: &[PathCmd]) {
        let mut at = Point { x: 0.0, y: 0.0 };
        let mut start = at;
        for cmd in cmds {
            match *cmd {
                PathCmd::MoveTo(p) => {
                    self.op(&[p.x, p.y], "m");
                    at = p;
                    start = p;
                }
                PathCmd::LineTo(p) => {
                    self.op(&[p.x, p.y], "l");
                    at = p;
                }
                PathCmd::QuadTo(q, p) => {
                    let c1 = lerp(at, q, 2.0 / 3.0);
                    let c2 = lerp(p, q, 2.0 / 3.0);
                    self.op(&[c1.x, c1.y, c2.x, c2.y, p.x, p.y], "c");
                    at = p;
                }
                PathCmd::CubicTo(c1, c2, p) => {
                    self.op(&[c1.x, c1.y, c2.x, c2.y, p.x, p.y], "c");
                    at = p;
                }
                PathCmd::Close => {
                    self.op(&[], "h");
                    at = start;
                }
            }
        }
    }
}

fn lerp(a: Point, b: Point, t: f32) -> Point {
    Point {
        x: a.x + (b.x - a.x) * t,
        y: a.y + (b.y - a.y) * t,
    }
}

/// `r` grown by `by` on every side (shrunk for a negative `by`), never
/// below empty.
pub(crate) fn inset(r: Rect, by: f32) -> Rect {
    let w = (r.w - 2.0 * by).max(0.0);
    let h = (r.h - 2.0 * by).max(0.0);
    Rect::new(r.x + by.min(r.w * 0.5), r.y + by.min(r.h * 0.5), w, h)
}

/// `corners` shrunk by `by`, never below sharp.
pub(crate) fn shrink(corners: Corners, by: f32) -> Corners {
    Corners {
        left_top: (corners.left_top - by).max(0.0),
        right_top: (corners.right_top - by).max(0.0),
        right_bottom: (corners.right_bottom - by).max(0.0),
        left_bottom: (corners.left_bottom - by).max(0.0),
    }
}
