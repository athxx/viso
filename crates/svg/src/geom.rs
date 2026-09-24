//! Geometry primitives: points, rects, affine transforms and path data.
//!
//! [`PathData`] is the canonical outline form every SVG shape lowers to — only
//! `MoveTo`/`LineTo`/`QuadTo`/`CubicTo`/`Close`, with absolute coordinates.
//! Arcs, `H`/`V`, smooth curves and basic shapes are all converted at parse time,
//! so the renderer and downstream consumers only ever see five commands.

/// A 2D point / vector.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

impl Point {
    #[inline]
    pub const fn new(x: f32, y: f32) -> Self {
        Point { x, y }
    }

    #[inline]
    pub fn length(self) -> f32 {
        (self.x * self.x + self.y * self.y).sqrt()
    }

    #[inline]
    pub fn dot(self, o: Point) -> f32 {
        self.x * o.x + self.y * o.y
    }

    #[inline]
    pub fn cross(self, o: Point) -> f32 {
        self.x * o.y - self.y * o.x
    }

    /// Unit vector in the same direction, or `None` for a (near) zero vector.
    #[inline]
    pub fn normalized(self) -> Option<Point> {
        let len = self.length();
        if len > 1e-9 && len.is_finite() {
            Some(Point::new(self.x / len, self.y / len))
        } else {
            None
        }
    }

    #[inline]
    pub fn lerp(self, o: Point, t: f32) -> Point {
        Point::new(self.x + (o.x - self.x) * t, self.y + (o.y - self.y) * t)
    }

    #[inline]
    pub fn is_finite(self) -> bool {
        self.x.is_finite() && self.y.is_finite()
    }
}

impl std::ops::Add for Point {
    type Output = Point;
    #[inline]
    fn add(self, o: Point) -> Point {
        Point::new(self.x + o.x, self.y + o.y)
    }
}

impl std::ops::Sub for Point {
    type Output = Point;
    #[inline]
    fn sub(self, o: Point) -> Point {
        Point::new(self.x - o.x, self.y - o.y)
    }
}

impl std::ops::Mul<f32> for Point {
    type Output = Point;
    #[inline]
    fn mul(self, s: f32) -> Point {
        Point::new(self.x * s, self.y * s)
    }
}

impl std::ops::Neg for Point {
    type Output = Point;
    #[inline]
    fn neg(self) -> Point {
        Point::new(-self.x, -self.y)
    }
}

/// An axis-aligned rectangle (`x`, `y` = top-left).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    #[inline]
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Rect { x, y, w, h }
    }

    #[inline]
    pub fn from_ltrb(l: f32, t: f32, r: f32, b: f32) -> Self {
        Rect::new(l, t, r - l, b - t)
    }

    #[inline]
    pub fn right(&self) -> f32 {
        self.x + self.w
    }

    #[inline]
    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }

    /// Positive, finite width and height.
    #[inline]
    pub fn is_valid(&self) -> bool {
        self.w > 0.0 && self.h > 0.0 && self.w.is_finite() && self.h.is_finite()
    }

    /// Smallest rect containing both.
    pub fn union(&self, o: &Rect) -> Rect {
        Rect::from_ltrb(
            self.x.min(o.x),
            self.y.min(o.y),
            self.right().max(o.right()),
            self.bottom().max(o.bottom()),
        )
    }

    /// Bounding box of this rect's four corners under `ts`.
    pub fn transform(&self, ts: &Transform) -> Rect {
        let pts = [
            ts.apply(Point::new(self.x, self.y)),
            ts.apply(Point::new(self.right(), self.y)),
            ts.apply(Point::new(self.x, self.bottom())),
            ts.apply(Point::new(self.right(), self.bottom())),
        ];
        let mut b = Bounds::new();
        for p in pts {
            b.add(p);
        }
        b.rect().unwrap_or_default()
    }

    /// The rect as a closed path (`M L L L Z`).
    pub fn to_path(&self) -> PathData {
        let mut p = PathData::new();
        p.move_to(self.x, self.y);
        p.line_to(self.right(), self.y);
        p.line_to(self.right(), self.bottom());
        p.line_to(self.x, self.bottom());
        p.close();
        p
    }
}

/// Running min/max accumulator for a bounding box.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Bounds {
    min: Point,
    max: Point,
}

impl Bounds {
    pub(crate) fn new() -> Self {
        Bounds {
            min: Point::new(f32::INFINITY, f32::INFINITY),
            max: Point::new(f32::NEG_INFINITY, f32::NEG_INFINITY),
        }
    }

    #[inline]
    pub(crate) fn add(&mut self, p: Point) {
        self.min.x = self.min.x.min(p.x);
        self.min.y = self.min.y.min(p.y);
        self.max.x = self.max.x.max(p.x);
        self.max.y = self.max.y.max(p.y);
    }

    /// The accumulated box; zero-width/height boxes are kept (a horizontal
    /// line still has a bounding box), empty accumulators return `None`.
    pub(crate) fn rect(&self) -> Option<Rect> {
        if self.min.x > self.max.x || !self.min.x.is_finite() || !self.max.y.is_finite() {
            return None;
        }
        Some(Rect::from_ltrb(
            self.min.x, self.min.y, self.max.x, self.max.y,
        ))
    }
}

/// A 2D affine transform, SVG `matrix(a b c d e f)` order:
/// `x' = a·x + c·y + e`, `y' = b·x + d·y + f`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Transform {
    pub a: f32,
    pub b: f32,
    pub c: f32,
    pub d: f32,
    pub e: f32,
    pub f: f32,
}

impl Default for Transform {
    fn default() -> Self {
        Transform::IDENTITY
    }
}

impl Transform {
    pub const IDENTITY: Transform = Transform {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: 1.0,
        e: 0.0,
        f: 0.0,
    };

    #[inline]
    pub const fn new(a: f32, b: f32, c: f32, d: f32, e: f32, f: f32) -> Self {
        Transform { a, b, c, d, e, f }
    }

    #[inline]
    pub const fn translate(tx: f32, ty: f32) -> Self {
        Transform::new(1.0, 0.0, 0.0, 1.0, tx, ty)
    }

    #[inline]
    pub const fn scale(sx: f32, sy: f32) -> Self {
        Transform::new(sx, 0.0, 0.0, sy, 0.0, 0.0)
    }

    /// Rotation by `deg` degrees (clockwise on a y-down canvas).
    pub fn rotate(deg: f32) -> Self {
        let (s, c) = deg.to_radians().sin_cos();
        Transform::new(c, s, -s, c, 0.0, 0.0)
    }

    pub fn skew_x(deg: f32) -> Self {
        Transform::new(1.0, 0.0, deg.to_radians().tan(), 1.0, 0.0, 0.0)
    }

    pub fn skew_y(deg: f32) -> Self {
        Transform::new(1.0, deg.to_radians().tan(), 0.0, 1.0, 0.0, 0.0)
    }

    /// `self × other`: `other` is applied to a point first, then `self`. This is
    /// how SVG composes `transform="A B"` and a parent/child chain
    /// (`abs = parent.pre_concat(local)`).
    pub fn pre_concat(&self, o: &Transform) -> Transform {
        Transform {
            a: self.a * o.a + self.c * o.b,
            b: self.b * o.a + self.d * o.b,
            c: self.a * o.c + self.c * o.d,
            d: self.b * o.c + self.d * o.d,
            e: self.a * o.e + self.c * o.f + self.e,
            f: self.b * o.e + self.d * o.f + self.f,
        }
    }

    #[inline]
    pub fn apply(&self, p: Point) -> Point {
        Point::new(
            self.a * p.x + self.c * p.y + self.e,
            self.b * p.x + self.d * p.y + self.f,
        )
    }

    /// Apply only the linear part (no translation) — for direction vectors.
    #[inline]
    pub fn apply_vector(&self, p: Point) -> Point {
        Point::new(self.a * p.x + self.c * p.y, self.b * p.x + self.d * p.y)
    }

    #[inline]
    pub fn determinant(&self) -> f32 {
        self.a * self.d - self.b * self.c
    }

    pub fn is_identity(&self) -> bool {
        *self == Transform::IDENTITY
    }

    /// Finite and non-degenerate (invertible).
    pub fn is_valid(&self) -> bool {
        let det = self.determinant();
        det.is_finite() && det.abs() > 1e-12 && self.e.is_finite() && self.f.is_finite()
    }

    pub fn invert(&self) -> Option<Transform> {
        let det = self.determinant() as f64;
        if !det.is_finite() || det.abs() < 1e-14 {
            return None;
        }
        let (a, b, c, d, e, f) = (
            self.a as f64,
            self.b as f64,
            self.c as f64,
            self.d as f64,
            self.e as f64,
            self.f as f64,
        );
        let inv = 1.0 / det;
        Some(Transform {
            a: (d * inv) as f32,
            b: (-b * inv) as f32,
            c: (-c * inv) as f32,
            d: (a * inv) as f32,
            e: ((c * f - d * e) * inv) as f32,
            f: ((b * e - a * f) * inv) as f32,
        })
    }

    /// The largest stretch factor this transform applies to any direction (the
    /// larger singular value). Used to convert device-space tolerances into
    /// user space.
    pub fn max_scale(&self) -> f32 {
        let (a, b, c, d) = (self.a, self.b, self.c, self.d);
        let s1 = a * a + b * b + c * c + d * d;
        let s2 = ((a * a + b * b - c * c - d * d).powi(2) + 4.0 * (a * c + b * d).powi(2)).sqrt();
        ((s1 + s2) * 0.5).sqrt()
    }

    /// `sqrt(|det|)`: the geometric-mean scale, the factor SVG uses for
    /// non-directional lengths under a transform.
    pub fn mean_scale(&self) -> f32 {
        self.determinant().abs().sqrt()
    }

    /// True when the transform only scales and translates (no rotation/skew).
    pub fn is_scale_translate(&self) -> bool {
        self.b == 0.0 && self.c == 0.0
    }
}

/// One canonical path command (absolute coordinates).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Segment {
    MoveTo(Point),
    LineTo(Point),
    /// Control point, end point.
    QuadTo(Point, Point),
    /// Two control points, end point.
    CubicTo(Point, Point, Point),
    Close,
}

/// A vector outline as a flat list of canonical [`Segment`]s.
///
/// Every subpath begins with a `MoveTo`; the builder methods insert one when a
/// drawing command follows a `Close` (SVG's implicit moveto to the subpath
/// start).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PathData {
    segs: Vec<Segment>,
    start: Point,
    last: Point,
    /// A subpath is open for drawing (a `MoveTo` was emitted and not closed).
    open: bool,
}

impl PathData {
    pub fn new() -> Self {
        PathData::default()
    }

    #[inline]
    pub fn segments(&self) -> &[Segment] {
        &self.segs
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.segs.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.segs.is_empty()
    }

    /// The current point (end of the last command).
    #[inline]
    pub fn current(&self) -> Point {
        self.last
    }

    pub fn move_to(&mut self, x: f32, y: f32) {
        let p = Point::new(x, y);
        // Consecutive movetos collapse: only the last one starts a subpath.
        if let Some(Segment::MoveTo(last)) = self.segs.last_mut() {
            *last = p;
        } else {
            self.segs.push(Segment::MoveTo(p));
        }
        self.start = p;
        self.last = p;
        self.open = true;
    }

    fn ensure_open(&mut self) {
        if !self.open {
            let s = self.last;
            self.segs.push(Segment::MoveTo(s));
            self.start = s;
            self.open = true;
        }
    }

    pub fn line_to(&mut self, x: f32, y: f32) {
        self.ensure_open();
        let p = Point::new(x, y);
        self.segs.push(Segment::LineTo(p));
        self.last = p;
    }

    pub fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        self.ensure_open();
        let p = Point::new(x, y);
        self.segs.push(Segment::QuadTo(Point::new(x1, y1), p));
        self.last = p;
    }

    pub fn cubic_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        self.ensure_open();
        let p = Point::new(x, y);
        self.segs
            .push(Segment::CubicTo(Point::new(x1, y1), Point::new(x2, y2), p));
        self.last = p;
    }

    pub fn close(&mut self) {
        if self.open {
            self.segs.push(Segment::Close);
            self.open = false;
            self.last = self.start;
        }
    }

    /// SVG elliptical arc (`A rx ry rotation large-arc sweep x y`) from the
    /// current point, converted to at most four cubic Béziers per 360°
    /// (SVG 1.1 Appendix F.6 endpoint→center conversion with radii
    /// correction). A zero radius degrades to a straight line; a zero-length arc
    /// is omitted.
    #[allow(clippy::too_many_arguments)] // mirrors the `A` command's parameters
    pub fn arc_to(
        &mut self,
        rx: f32,
        ry: f32,
        x_axis_rotation: f32,
        large_arc: bool,
        sweep: bool,
        x: f32,
        y: f32,
    ) {
        self.ensure_open();
        let p0 = self.last;
        let p1 = Point::new(x, y);
        if p0 == p1 {
            return;
        }
        let (mut rx, mut ry) = (rx.abs() as f64, ry.abs() as f64);
        if rx < 1e-9 || ry < 1e-9 {
            self.line_to(x, y);
            return;
        }
        let phi = (x_axis_rotation as f64).to_radians();
        let (sin_phi, cos_phi) = phi.sin_cos();
        let (x0, y0, x1, y1) = (p0.x as f64, p0.y as f64, p1.x as f64, p1.y as f64);
        let dx2 = (x0 - x1) / 2.0;
        let dy2 = (y0 - y1) / 2.0;
        let x1p = cos_phi * dx2 + sin_phi * dy2;
        let y1p = -sin_phi * dx2 + cos_phi * dy2;

        // Radii correction (F.6.6).
        let lambda = (x1p * x1p) / (rx * rx) + (y1p * y1p) / (ry * ry);
        if lambda > 1.0 {
            let s = lambda.sqrt();
            rx *= s;
            ry *= s;
        }

        let rx2 = rx * rx;
        let ry2 = ry * ry;
        let num = rx2 * ry2 - rx2 * y1p * y1p - ry2 * x1p * x1p;
        let den = rx2 * y1p * y1p + ry2 * x1p * x1p;
        let mut coef = if den > 0.0 {
            (num / den).max(0.0).sqrt()
        } else {
            0.0
        };
        if large_arc == sweep {
            coef = -coef;
        }
        let cxp = coef * (rx * y1p / ry);
        let cyp = coef * -(ry * x1p / rx);
        let cx = cos_phi * cxp - sin_phi * cyp + (x0 + x1) / 2.0;
        let cy = sin_phi * cxp + cos_phi * cyp + (y0 + y1) / 2.0;

        let angle = |ux: f64, uy: f64, vx: f64, vy: f64| -> f64 {
            let dot = ux * vx + uy * vy;
            let len = (ux * ux + uy * uy).sqrt() * (vx * vx + vy * vy).sqrt();
            let mut a = (dot / len).clamp(-1.0, 1.0).acos();
            if ux * vy - uy * vx < 0.0 {
                a = -a;
            }
            a
        };
        let ux = (x1p - cxp) / rx;
        let uy = (y1p - cyp) / ry;
        let vx = (-x1p - cxp) / rx;
        let vy = (-y1p - cyp) / ry;
        let theta1 = angle(1.0, 0.0, ux, uy);
        let mut dtheta = angle(ux, uy, vx, vy);
        if !sweep && dtheta > 0.0 {
            dtheta -= std::f64::consts::TAU;
        } else if sweep && dtheta < 0.0 {
            dtheta += std::f64::consts::TAU;
        }
        if !dtheta.is_finite() {
            self.line_to(x, y);
            return;
        }

        let n = (dtheta.abs() / std::f64::consts::FRAC_PI_2 - 1e-7)
            .ceil()
            .max(1.0) as usize;
        let step = dtheta / n as f64;
        let k = 4.0 / 3.0 * (step / 4.0).tan();
        let map = |ex: f64, ey: f64| -> (f64, f64) {
            (
                cx + rx * cos_phi * ex - ry * sin_phi * ey,
                cy + rx * sin_phi * ex + ry * cos_phi * ey,
            )
        };
        let mut t = theta1;
        for i in 0..n {
            let (s0, c0) = t.sin_cos();
            let t1 = t + step;
            let (s1, c1) = t1.sin_cos();
            let (c1x, c1y) = map(c0 - k * s0, s0 + k * c0);
            let (c2x, c2y) = map(c1 + k * s1, s1 - k * c1);
            let (ex, ey) = if i + 1 == n {
                (x as f64, y as f64)
            } else {
                map(c1, s1)
            };
            self.cubic_to(
                c1x as f32, c1y as f32, c2x as f32, c2y as f32, ex as f32, ey as f32,
            );
            t = t1;
        }
    }

    /// Append an axis-aligned ellipse centered on `(cx, cy)` as four cubics
    /// (clockwise on a y-down canvas, starting at the rightmost point).
    pub fn push_ellipse(&mut self, cx: f32, cy: f32, rx: f32, ry: f32) {
        self.move_to(cx + rx, cy);
        self.arc_to(rx, ry, 0.0, false, true, cx, cy + ry);
        self.arc_to(rx, ry, 0.0, false, true, cx - rx, cy);
        self.arc_to(rx, ry, 0.0, false, true, cx, cy - ry);
        self.arc_to(rx, ry, 0.0, false, true, cx + rx, cy);
        self.close();
    }

    /// Append a rounded rect (radii already clamped to half the size).
    pub fn push_rounded_rect(&mut self, r: Rect, rx: f32, ry: f32) {
        if rx <= 0.0 || ry <= 0.0 {
            self.move_to(r.x, r.y);
            self.line_to(r.right(), r.y);
            self.line_to(r.right(), r.bottom());
            self.line_to(r.x, r.bottom());
            self.close();
            return;
        }
        self.move_to(r.x + rx, r.y);
        self.line_to(r.right() - rx, r.y);
        self.arc_to(rx, ry, 0.0, false, true, r.right(), r.y + ry);
        self.line_to(r.right(), r.bottom() - ry);
        self.arc_to(rx, ry, 0.0, false, true, r.right() - rx, r.bottom());
        self.line_to(r.x + rx, r.bottom());
        self.arc_to(rx, ry, 0.0, false, true, r.x, r.bottom() - ry);
        self.line_to(r.x, r.y + ry);
        self.arc_to(rx, ry, 0.0, false, true, r.x + rx, r.y);
        self.close();
    }

    /// A copy with every point mapped through `ts`.
    pub fn transformed(&self, ts: &Transform) -> PathData {
        let segs = self
            .segs
            .iter()
            .map(|s| match *s {
                Segment::MoveTo(p) => Segment::MoveTo(ts.apply(p)),
                Segment::LineTo(p) => Segment::LineTo(ts.apply(p)),
                Segment::QuadTo(c, p) => Segment::QuadTo(ts.apply(c), ts.apply(p)),
                Segment::CubicTo(c0, c1, p) => {
                    Segment::CubicTo(ts.apply(c0), ts.apply(c1), ts.apply(p))
                }
                Segment::Close => Segment::Close,
            })
            .collect();
        PathData {
            segs,
            start: ts.apply(self.start),
            last: ts.apply(self.last),
            open: self.open,
        }
    }

    /// Exact geometric bounds of the outline (curve extrema, not control
    /// hulls). `None` for a path with no points.
    pub fn bounds(&self) -> Option<Rect> {
        self.bounds_with(&Transform::IDENTITY)
    }

    /// Exact bounds of the outline after mapping through `ts`.
    pub fn bounds_with(&self, ts: &Transform) -> Option<Rect> {
        let mut b = Bounds::new();
        let mut last = Point::default();
        for s in &self.segs {
            match *s {
                Segment::MoveTo(p) | Segment::LineTo(p) => {
                    let p = ts.apply(p);
                    b.add(p);
                    last = p;
                }
                Segment::QuadTo(c, p) => {
                    let (c, p) = (ts.apply(c), ts.apply(p));
                    b.add(p);
                    for t in quad_extrema(last, c, p).into_iter().flatten() {
                        b.add(eval_quad(last, c, p, t));
                    }
                    last = p;
                }
                Segment::CubicTo(c0, c1, p) => {
                    let (c0, c1, p) = (ts.apply(c0), ts.apply(c1), ts.apply(p));
                    b.add(p);
                    for t in cubic_extrema(last, c0, c1, p).into_iter().flatten() {
                        b.add(eval_cubic(last, c0, c1, p, t));
                    }
                    last = p;
                }
                Segment::Close => {}
            }
        }
        b.rect()
    }
}

#[inline]
pub(crate) fn eval_quad(p0: Point, p1: Point, p2: Point, t: f32) -> Point {
    let mt = 1.0 - t;
    p0 * (mt * mt) + p1 * (2.0 * mt * t) + p2 * (t * t)
}

#[inline]
pub(crate) fn eval_cubic(p0: Point, p1: Point, p2: Point, p3: Point, t: f32) -> Point {
    let mt = 1.0 - t;
    p0 * (mt * mt * mt) + p1 * (3.0 * mt * mt * t) + p2 * (3.0 * mt * t * t) + p3 * (t * t * t)
}

/// Parameters in `(0, 1)` where a quadratic has an x or y extremum.
fn quad_extrema(p0: Point, p1: Point, p2: Point) -> [Option<f32>; 2] {
    let axis = |a: f32, b: f32, c: f32| {
        let den = a - 2.0 * b + c;
        if den.abs() < 1e-12 {
            return None;
        }
        let t = (a - b) / den;
        (t > 0.0 && t < 1.0).then_some(t)
    };
    [axis(p0.x, p1.x, p2.x), axis(p0.y, p1.y, p2.y)]
}

/// Parameters in `(0, 1)` where a cubic has an x or y extremum.
fn cubic_extrema(p0: Point, p1: Point, p2: Point, p3: Point) -> [Option<f32>; 4] {
    let axis = |a: f32, b: f32, c: f32, d: f32| -> [Option<f32>; 2] {
        // Derivative / 3: (b-a)(1-t)^2 + 2(c-b)(1-t)t + (d-c)t^2
        let qa = -a + 3.0 * b - 3.0 * c + d;
        let qb = 2.0 * (a - 2.0 * b + c);
        let qc = b - a;
        let ok = |t: f32| (t > 0.0 && t < 1.0).then_some(t);
        if qa.abs() < 1e-12 {
            if qb.abs() < 1e-12 {
                return [None, None];
            }
            return [ok(-qc / qb), None];
        }
        let disc = qb * qb - 4.0 * qa * qc;
        if disc < 0.0 {
            return [None, None];
        }
        let sq = disc.sqrt();
        [ok((-qb + sq) / (2.0 * qa)), ok((-qb - sq) / (2.0 * qa))]
    };
    let [a, b] = axis(p0.x, p1.x, p2.x, p3.x);
    let [c, d] = axis(p0.y, p1.y, p2.y, p3.y);
    [a, b, c, d]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transform_composition_order() {
        // translate(10,0) then scale(2): point (1,0) → scale → (2,0) → +10.
        let t = Transform::translate(10.0, 0.0).pre_concat(&Transform::scale(2.0, 2.0));
        assert_eq!(t.apply(Point::new(1.0, 0.0)), Point::new(12.0, 0.0));
        let inv = t.invert().unwrap();
        let p = inv.apply(Point::new(12.0, 0.0));
        assert!((p.x - 1.0).abs() < 1e-5 && p.y.abs() < 1e-5);
    }

    #[test]
    fn circle_bounds_are_exact() {
        let mut p = PathData::new();
        p.push_ellipse(50.0, 50.0, 10.0, 20.0);
        let b = p.bounds().unwrap();
        assert!((b.x - 40.0).abs() < 1e-3 && (b.w - 20.0).abs() < 1e-3);
        assert!((b.y - 30.0).abs() < 1e-3 && (b.h - 40.0).abs() < 1e-3);
    }

    #[test]
    fn implicit_moveto_after_close() {
        let mut p = PathData::new();
        p.move_to(1.0, 1.0);
        p.line_to(5.0, 1.0);
        p.close();
        p.line_to(3.0, 3.0);
        assert_eq!(p.segments()[3], Segment::MoveTo(Point::new(1.0, 1.0)));
    }
}
