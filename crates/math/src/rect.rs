//! 2D UI geometry: [`Point`], [`Size`], [`Rect`], [`Insets`], and the `f64`
//! layout-accuracy variants [`DPoint`], [`DRect`].
//!
//! A [`Rect`] is a position plus a size, top-left origin. Hit-testing is
//! **half-open** — the near edges are inclusive and the far edges exclusive
//! (`[x, x + w)` / `[y, y + h)`) — so two rects sharing a seam do not both
//! claim a point on it. This matches the existing render `Rect` convention and
//! is what UI hit-testing wants; it is a deliberate divergence from the
//! reference framework's inclusive-both-edges `contains`.
//!
//! `f32` is the primary precision. [`DPoint`]/[`DRect`] mirror the layout-facing
//! subset in `f64` for the accuracy-sensitive path (see [`DVec2`](crate::DVec2)),
//! with `dpi_snap` to pin an accumulated coordinate to the device-pixel grid
//! before it crosses back to the `f32` render side.

use crate::dvec::DVec2;
use crate::vec::Vec2;

/// A 2D position, top-left origin, in `f32`.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Point {
    /// Horizontal coordinate.
    pub x: f32,
    /// Vertical coordinate.
    pub y: f32,
}

/// A 2D extent (width × height) in `f32`.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Size {
    /// Width.
    pub w: f32,
    /// Height.
    pub h: f32,
}

/// An axis-aligned rectangle: a top-left [`Point`] plus a [`Size`].
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Rect {
    /// Top-left corner.
    pub origin: Point,
    /// Width and height.
    pub size: Size,
}

/// Per-edge insets (padding/margins), in `f32`.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Insets {
    /// Top edge.
    pub top: f32,
    /// Right edge.
    pub right: f32,
    /// Bottom edge.
    pub bottom: f32,
    /// Left edge.
    pub left: f32,
}

impl Point {
    /// The origin `(0, 0)`.
    pub const ZERO: Point = Point { x: 0.0, y: 0.0 };

    /// Builds a point.
    #[inline]
    pub const fn new(x: f32, y: f32) -> Point {
        Point { x, y }
    }

    /// The point as a [`Vec2`].
    #[inline]
    pub const fn to_vec2(self) -> Vec2 {
        Vec2::new(self.x, self.y)
    }
}

impl Size {
    /// The zero size.
    pub const ZERO: Size = Size { w: 0.0, h: 0.0 };

    /// Builds a size.
    #[inline]
    pub const fn new(w: f32, h: f32) -> Size {
        Size { w, h }
    }

    /// The area `w * h`.
    #[inline]
    pub fn area(self) -> f32 {
        self.w * self.h
    }

    /// The size as a [`Vec2`] (`x = w`, `y = h`).
    #[inline]
    pub const fn to_vec2(self) -> Vec2 {
        Vec2::new(self.w, self.h)
    }
}

impl Insets {
    /// Zero on every edge.
    pub const ZERO: Insets = Insets {
        top: 0.0,
        right: 0.0,
        bottom: 0.0,
        left: 0.0,
    };

    /// Builds insets from the four edges.
    #[inline]
    pub const fn new(top: f32, right: f32, bottom: f32, left: f32) -> Insets {
        Insets {
            top,
            right,
            bottom,
            left,
        }
    }

    /// The same inset on all four edges.
    #[inline]
    pub const fn all(v: f32) -> Insets {
        Insets::new(v, v, v, v)
    }

    /// The combined horizontal inset (`left + right`).
    #[inline]
    pub fn horizontal(self) -> f32 {
        self.left + self.right
    }

    /// The combined vertical inset (`top + bottom`).
    #[inline]
    pub fn vertical(self) -> f32 {
        self.top + self.bottom
    }
}

impl Rect {
    /// An effectively unbounded rect — the identity for [`intersect`](Self::intersect):
    /// intersecting any finite rect with `INFINITE` yields that rect back. Its
    /// origin is far negative and its extent huge, chosen so `x + w` stays finite
    /// (no overflow) while covering every realistic coordinate.
    pub const INFINITE: Rect = Rect {
        origin: Point {
            x: -f32::MAX / 4.0,
            y: -f32::MAX / 4.0,
        },
        size: Size {
            w: f32::MAX / 2.0,
            h: f32::MAX / 2.0,
        },
    };

    /// Builds a rect from position and size components.
    #[inline]
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect {
            origin: Point { x, y },
            size: Size { w, h },
        }
    }

    /// Builds a rect from an [`origin`](Rect::origin) and [`size`](Rect::size).
    #[inline]
    pub const fn from_origin_size(origin: Point, size: Size) -> Rect {
        Rect { origin, size }
    }

    /// The right edge `x + w`.
    #[inline]
    pub fn max_x(self) -> f32 {
        self.origin.x + self.size.w
    }

    /// The bottom edge `y + h`.
    #[inline]
    pub fn max_y(self) -> f32 {
        self.origin.y + self.size.h
    }

    /// The center point.
    #[inline]
    pub fn center(self) -> Point {
        Point::new(
            self.origin.x + self.size.w * 0.5,
            self.origin.y + self.size.h * 0.5,
        )
    }

    /// Whether the point lies inside the rect, **half-open** (`[x, x+w)` /
    /// `[y, y+h)`): near edges inclusive, far edges exclusive, so a point on a
    /// shared seam belongs to exactly one of two tiling rects.
    #[inline]
    pub fn contains(self, p: Point) -> bool {
        p.x >= self.origin.x && p.x < self.max_x() && p.y >= self.origin.y && p.y < self.max_y()
    }

    /// Whether the two rects overlap. Strict: rects that merely touch on an edge
    /// do not count as intersecting.
    #[inline]
    pub fn intersects(self, other: Rect) -> bool {
        other.origin.x < self.max_x()
            && other.max_x() > self.origin.x
            && other.origin.y < self.max_y()
            && other.max_y() > self.origin.y
    }

    /// The overlapping region of two rects. A non-overlapping pair yields an
    /// empty rect (`w`/`h` clamped to 0).
    pub fn intersect(self, other: Rect) -> Rect {
        let x0 = self.origin.x.max(other.origin.x);
        let y0 = self.origin.y.max(other.origin.y);
        let x1 = self.max_x().min(other.max_x());
        let y1 = self.max_y().min(other.max_y());
        Rect::new(x0, y0, (x1 - x0).max(0.0), (y1 - y0).max(0.0))
    }

    /// The smallest rect containing both (the reference's `hull`).
    pub fn union(self, other: Rect) -> Rect {
        let x0 = self.origin.x.min(other.origin.x);
        let y0 = self.origin.y.min(other.origin.y);
        let x1 = self.max_x().max(other.max_x());
        let y1 = self.max_y().max(other.max_y());
        Rect::new(x0, y0, x1 - x0, y1 - y0)
    }

    /// A copy translated by `delta`.
    #[inline]
    pub fn translate(self, delta: Vec2) -> Rect {
        Rect::from_origin_size(
            Point::new(self.origin.x + delta.x, self.origin.y + delta.y),
            self.size,
        )
    }

    /// Shrinks the rect inward by `insets` (a negative inset grows it). The
    /// result is clamped to a non-negative size.
    pub fn inset(self, insets: Insets) -> Rect {
        let w = (self.size.w - insets.horizontal()).max(0.0);
        let h = (self.size.h - insets.vertical()).max(0.0);
        Rect::new(
            self.origin.x + insets.left,
            self.origin.y + insets.top,
            w,
            h,
        )
    }
}

/// A 2D position in `f64`, for the accuracy-sensitive layout path.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct DPoint {
    /// Horizontal coordinate.
    pub x: f64,
    /// Vertical coordinate.
    pub y: f64,
}

/// An axis-aligned rectangle in `f64`, for the accuracy-sensitive layout path.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct DRect {
    /// Top-left corner.
    pub origin: DPoint,
    /// Width and height, as a [`DVec2`] (`x = w`, `y = h`).
    pub size: DVec2,
}

impl DPoint {
    /// The origin `(0, 0)`.
    pub const ZERO: DPoint = DPoint { x: 0.0, y: 0.0 };

    /// Builds a point.
    #[inline]
    pub const fn new(x: f64, y: f64) -> DPoint {
        DPoint { x, y }
    }

    /// Narrows to an `f32` [`Point`] for the render boundary. Explicit because
    /// it is lossy.
    #[inline]
    pub fn to_point(self) -> Point {
        Point::new(self.x as f32, self.y as f32)
    }
}

impl DRect {
    /// Builds a rect from position and size components.
    #[inline]
    pub const fn new(x: f64, y: f64, w: f64, h: f64) -> DRect {
        DRect {
            origin: DPoint { x, y },
            size: DVec2 { x: w, y: h },
        }
    }

    /// The right edge `x + w`.
    #[inline]
    pub fn max_x(self) -> f64 {
        self.origin.x + self.size.x
    }

    /// The bottom edge `y + h`.
    #[inline]
    pub fn max_y(self) -> f64 {
        self.origin.y + self.size.y
    }

    /// Whether the point lies inside, half-open (matches [`Rect::contains`]).
    #[inline]
    pub fn contains(self, p: DPoint) -> bool {
        p.x >= self.origin.x && p.x < self.max_x() && p.y >= self.origin.y && p.y < self.max_y()
    }

    /// Snaps origin and size to the device-pixel grid at scale `dpi_factor`
    /// before crossing to the `f32` side, to avoid sub-pixel shimmer. See
    /// [`DVec2::dpi_snap`](crate::DVec2::dpi_snap). A non-positive factor is a
    /// no-op.
    pub fn dpi_snap(self, dpi_factor: f64) -> DRect {
        if dpi_factor <= 0.0 {
            return self;
        }
        let snap = |v: f64| (v * dpi_factor).round() / dpi_factor;
        DRect {
            origin: DPoint::new(snap(self.origin.x), snap(self.origin.y)),
            size: DVec2::new(snap(self.size.x), snap(self.size.y)),
        }
    }

    /// Narrows to an `f32` [`Rect`] for the render boundary. Explicit because it
    /// is lossy.
    #[inline]
    pub fn to_rect(self) -> Rect {
        Rect::new(
            self.origin.x as f32,
            self.origin.y as f32,
            self.size.x as f32,
            self.size.y as f32,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contains_is_half_open() {
        let r = Rect::new(0.0, 0.0, 10.0, 10.0);
        assert!(r.contains(Point::new(0.0, 0.0))); // near edge inclusive
        assert!(r.contains(Point::new(9.999, 9.999)));
        assert!(!r.contains(Point::new(10.0, 5.0))); // far edge exclusive
        assert!(!r.contains(Point::new(5.0, 10.0)));
        assert!(!r.contains(Point::new(-0.001, 5.0)));
    }

    #[test]
    fn seam_belongs_to_exactly_one_rect() {
        let left = Rect::new(0.0, 0.0, 10.0, 10.0);
        let right = Rect::new(10.0, 0.0, 10.0, 10.0);
        let seam = Point::new(10.0, 5.0);
        assert!(!left.contains(seam));
        assert!(right.contains(seam));
    }

    #[test]
    fn intersect_and_intersects_agree_on_overlap() {
        let a = Rect::new(0.0, 0.0, 10.0, 10.0);
        let b = Rect::new(5.0, 5.0, 10.0, 10.0);
        assert!(a.intersects(b));
        assert_eq!(a.intersect(b), Rect::new(5.0, 5.0, 5.0, 5.0));
    }

    #[test]
    fn touching_edges_do_not_intersect() {
        let a = Rect::new(0.0, 0.0, 10.0, 10.0);
        let b = Rect::new(10.0, 0.0, 10.0, 10.0);
        assert!(!a.intersects(b));
        // Intersection region collapses to zero width.
        assert_eq!(a.intersect(b).size.w, 0.0);
    }

    #[test]
    fn disjoint_intersection_is_empty() {
        let a = Rect::new(0.0, 0.0, 5.0, 5.0);
        let b = Rect::new(100.0, 100.0, 5.0, 5.0);
        let i = a.intersect(b);
        assert_eq!(i.size, Size::ZERO);
    }

    #[test]
    fn infinite_is_intersection_identity() {
        let r = Rect::new(3.0, 4.0, 20.0, 30.0);
        assert_eq!(Rect::INFINITE.intersect(r), r);
    }

    #[test]
    fn union_is_the_hull() {
        let a = Rect::new(0.0, 0.0, 5.0, 5.0);
        let b = Rect::new(10.0, 10.0, 5.0, 5.0);
        assert_eq!(a.union(b), Rect::new(0.0, 0.0, 15.0, 15.0));
    }

    #[test]
    fn inset_shrinks_and_clamps() {
        let r = Rect::new(0.0, 0.0, 100.0, 100.0);
        assert_eq!(
            r.inset(Insets::all(10.0)),
            Rect::new(10.0, 10.0, 80.0, 80.0)
        );
        // Over-inset clamps to zero, does not go negative.
        assert_eq!(r.inset(Insets::all(60.0)).size, Size::ZERO);
    }

    #[test]
    fn negative_inset_grows() {
        let r = Rect::new(10.0, 10.0, 20.0, 20.0);
        assert_eq!(r.inset(Insets::all(-5.0)), Rect::new(5.0, 5.0, 30.0, 30.0));
    }

    #[test]
    fn drect_dpi_snap_rounds_to_grid() {
        let r = DRect::new(1.24, 1.26, 3.1, 4.9);
        let snapped = r.dpi_snap(2.0); // grid step 0.5
        assert_eq!(snapped.origin, DPoint::new(1.0, 1.5));
        assert_eq!(snapped.size, DVec2::new(3.0, 5.0));
    }

    #[test]
    fn drect_narrows_to_rect() {
        let r = DRect::new(1.5, 2.5, 3.0, 4.0);
        assert_eq!(r.to_rect(), Rect::new(1.5, 2.5, 3.0, 4.0));
    }
}
