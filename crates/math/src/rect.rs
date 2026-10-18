//! 2D UI geometry: [`Point`], [`Size`], [`Rect`], [`Insets`].
//!
//! A [`Rect`] is a top-left position plus an extent, stored flat
//! (`x, y, w, h`) so it is one 16-byte `#[repr(C)]` value everywhere it travels:
//! layout, hit testing, paint, damage and the GPU upload path. Hit-testing is
//! **half-open** — the near edges are inclusive and the far edges exclusive
//! (`[x, x + w)` / `[y, y + h)`) — so two rects sharing a seam do not both
//! claim a point on it.

use crate::transform::Affine2;
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

/// An axis-aligned rectangle: top-left corner plus extent.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Rect {
    /// Top-left corner x.
    pub x: f32,
    /// Top-left corner y.
    pub y: f32,
    /// Width.
    pub w: f32,
    /// Height.
    pub h: f32,
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
    /// The empty rect at the origin.
    pub const ZERO: Rect = Rect::new(0.0, 0.0, 0.0, 0.0);

    /// An effectively unbounded rect — the identity for [`intersect`](Self::intersect).
    /// Its origin is far negative and its extent huge, chosen so `x + w` stays
    /// finite (no overflow) while covering every realistic coordinate.
    pub const INFINITE: Rect = Rect::new(
        -f32::MAX / 4.0,
        -f32::MAX / 4.0,
        f32::MAX / 2.0,
        f32::MAX / 2.0,
    );

    /// Builds a rect from position and size components.
    #[inline]
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect { x, y, w, h }
    }

    /// Builds a rect from its left, top, right and bottom edges.
    #[inline]
    pub fn from_ltrb(l: f32, t: f32, r: f32, b: f32) -> Rect {
        Rect::new(l, t, r - l, b - t)
    }

    /// Builds a rect from an origin and a size.
    #[inline]
    pub const fn from_origin_size(origin: Point, size: Size) -> Rect {
        Rect::new(origin.x, origin.y, size.w, size.h)
    }

    /// The top-left corner.
    #[inline]
    pub const fn origin(self) -> Point {
        Point::new(self.x, self.y)
    }

    /// The extent.
    #[inline]
    pub const fn size(self) -> Size {
        Size::new(self.w, self.h)
    }

    /// The right edge `x + w`.
    #[inline]
    pub fn max_x(self) -> f32 {
        self.x + self.w
    }

    /// The bottom edge `y + h`.
    #[inline]
    pub fn max_y(self) -> f32 {
        self.y + self.h
    }

    /// The center point.
    #[inline]
    pub fn center(self) -> Point {
        Point::new(self.x + self.w * 0.5, self.y + self.h * 0.5)
    }

    /// Whether the extent is zero or negative on either axis.
    #[inline]
    pub fn is_empty(self) -> bool {
        self.w <= 0.0 || self.h <= 0.0
    }

    /// Whether the extent is positive and finite on both axes — a rect that can
    /// cover pixels.
    #[inline]
    pub fn has_area(self) -> bool {
        self.w > 0.0 && self.h > 0.0 && self.w.is_finite() && self.h.is_finite()
    }

    /// Whether every component is finite.
    #[inline]
    pub fn is_finite(self) -> bool {
        self.x.is_finite() && self.y.is_finite() && self.w.is_finite() && self.h.is_finite()
    }

    /// Whether `(px, py)` lies inside, **half-open** (`[x, x+w)` / `[y, y+h)`):
    /// a point on a shared seam belongs to exactly one of two tiling rects.
    #[inline]
    pub fn contains(self, px: f32, py: f32) -> bool {
        px >= self.x && px < self.x + self.w && py >= self.y && py < self.y + self.h
    }

    /// [`contains`](Self::contains) for a [`Point`].
    #[inline]
    pub fn contains_point(self, p: Point) -> bool {
        self.contains(p.x, p.y)
    }

    /// Whether the two rects overlap. Strict: rects that merely touch on an edge
    /// do not count as intersecting.
    #[inline]
    pub fn intersects(self, other: Rect) -> bool {
        other.x < self.max_x()
            && other.max_x() > self.x
            && other.y < self.max_y()
            && other.max_y() > self.y
    }

    /// The overlapping region of two rects. A non-overlapping pair yields an
    /// empty rect (`w`/`h` clamped to 0).
    #[inline]
    pub fn intersect(self, other: Rect) -> Rect {
        let x0 = self.x.max(other.x);
        let y0 = self.y.max(other.y);
        let x1 = self.max_x().min(other.max_x());
        let y1 = self.max_y().min(other.max_y());
        Rect::new(x0, y0, (x1 - x0).max(0.0), (y1 - y0).max(0.0))
    }

    /// The smallest rect containing both, degenerate operands included (the
    /// bounds of a horizontal line still widen a hull).
    #[inline]
    pub fn union(self, other: Rect) -> Rect {
        Rect::from_ltrb(
            self.x.min(other.x),
            self.y.min(other.y),
            self.max_x().max(other.max_x()),
            self.max_y().max(other.max_y()),
        )
    }

    /// The bounding box of two rects where an empty operand is the identity, so
    /// [`Rect::ZERO`] seeds a running accumulation: `ZERO.accumulate(a) == a`.
    #[inline]
    pub fn accumulate(self, other: Rect) -> Rect {
        if self.is_empty() {
            return other;
        }
        if other.is_empty() {
            return self;
        }
        self.union(other)
    }

    /// A copy translated by `delta`.
    #[inline]
    pub fn translate(self, delta: Vec2) -> Rect {
        Rect::new(self.x + delta.x, self.y + delta.y, self.w, self.h)
    }

    /// Shrinks the rect inward by `insets` (a negative inset grows it). The
    /// result is clamped to a non-negative size.
    #[inline]
    pub fn inset(self, insets: Insets) -> Rect {
        let w = (self.w - insets.horizontal()).max(0.0);
        let h = (self.h - insets.vertical()).max(0.0);
        Rect::new(self.x + insets.left, self.y + insets.top, w, h)
    }

    /// Grows the rect by `amount` on every side (a negative amount shrinks it);
    /// the extent is clamped at zero.
    #[inline]
    pub fn inflate(self, amount: f32) -> Rect {
        Rect::new(
            self.x - amount,
            self.y - amount,
            (self.w + amount * 2.0).max(0.0),
            (self.h + amount * 2.0).max(0.0),
        )
    }

    /// The axis-aligned bounding box of the four corners under `t`.
    pub fn transform_bounds(self, t: &Affine2) -> Rect {
        let corners = [
            t.transform_point(Vec2::new(self.x, self.y)),
            t.transform_point(Vec2::new(self.max_x(), self.y)),
            t.transform_point(Vec2::new(self.x, self.max_y())),
            t.transform_point(Vec2::new(self.max_x(), self.max_y())),
        ];
        let (mut x0, mut y0, mut x1, mut y1) =
            (corners[0].x, corners[0].y, corners[0].x, corners[0].y);
        for c in &corners[1..] {
            x0 = x0.min(c.x);
            y0 = y0.min(c.y);
            x1 = x1.max(c.x);
            y1 = y1.max(c.y);
        }
        Rect::from_ltrb(x0, y0, x1, y1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contains_is_half_open() {
        let r = Rect::new(0.0, 0.0, 10.0, 10.0);
        assert!(r.contains_point(Point::new(0.0, 0.0))); // near edge inclusive
        assert!(r.contains_point(Point::new(9.999, 9.999)));
        assert!(!r.contains_point(Point::new(10.0, 5.0))); // far edge exclusive
        assert!(!r.contains_point(Point::new(5.0, 10.0)));
        assert!(!r.contains_point(Point::new(-0.001, 5.0)));
    }

    #[test]
    fn seam_belongs_to_exactly_one_rect() {
        let left = Rect::new(0.0, 0.0, 10.0, 10.0);
        let right = Rect::new(10.0, 0.0, 10.0, 10.0);
        let seam = Point::new(10.0, 5.0);
        assert!(!left.contains_point(seam));
        assert!(right.contains_point(seam));
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
        assert_eq!(a.intersect(b).w, 0.0);
    }

    #[test]
    fn disjoint_intersection_is_empty() {
        let a = Rect::new(0.0, 0.0, 5.0, 5.0);
        let b = Rect::new(100.0, 100.0, 5.0, 5.0);
        let i = a.intersect(b);
        assert_eq!(i.size(), Size::ZERO);
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
        assert_eq!(r.inset(Insets::all(60.0)).size(), Size::ZERO);
    }

    #[test]
    fn negative_inset_grows() {
        let r = Rect::new(10.0, 10.0, 20.0, 20.0);
        assert_eq!(r.inset(Insets::all(-5.0)), Rect::new(5.0, 5.0, 30.0, 30.0));
    }

    #[test]
    fn accumulate_treats_empty_as_identity() {
        let a = Rect::new(3.0, 4.0, 5.0, 6.0);
        assert_eq!(Rect::ZERO.accumulate(a), a);
        assert_eq!(a.accumulate(Rect::ZERO), a);
        // A degenerate operand still widens a plain hull.
        let line = Rect::new(20.0, 4.0, 0.0, 6.0);
        assert_eq!(a.union(line), Rect::new(3.0, 4.0, 17.0, 6.0));
    }

    #[test]
    fn transform_bounds_covers_rotated_corners() {
        let r = Rect::new(0.0, 0.0, 2.0, 1.0);
        let b = r.transform_bounds(&Affine2::from_rotation(core::f32::consts::FRAC_PI_2));
        assert!((b.x - -1.0).abs() < 1e-5 && (b.w - 1.0).abs() < 1e-5 && (b.h - 2.0).abs() < 1e-5);
    }

    #[test]
    fn rect_is_sixteen_bytes() {
        assert_eq!(core::mem::size_of::<Rect>(), 16);
    }
}
