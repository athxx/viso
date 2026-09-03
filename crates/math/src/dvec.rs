//! `f64` vector for the accuracy-sensitive UI-layout path: [`DVec2`].
//!
//! `f32` is the library's primary precision (see [`vec`](crate::vec)), but the
//! UI-layout path accumulates large canvas coordinates (scroll offsets, DPI
//! scaling over a big virtual surface) where `f32`'s ~7 significant digits let
//! sub-pixel error creep in and shimmer. [`DVec2`] carries that path in `f64`;
//! cheap explicit conversions bridge to [`Vec2`] at the GPU/upload boundary,
//! which is `f32`-native.
//!
//! Only the 2D point type earns the `f64` treatment — matrices, quaternions,
//! and 3D geometry stay `f32`. Keep the `f64` island small and convert at the
//! edges.

use crate::vec::Vec2;
use core::ops::{Add, AddAssign, Div, DivAssign, Mul, MulAssign, Neg, Sub, SubAssign};

/// A 2-component `f64` vector for the accuracy-sensitive UI-layout path.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct DVec2 {
    /// First component.
    pub x: f64,
    /// Second component.
    pub y: f64,
}

/// Shorthand constructor for [`DVec2`].
#[inline]
pub const fn dvec2(x: f64, y: f64) -> DVec2 {
    DVec2 { x, y }
}

impl DVec2 {
    /// The zero vector.
    pub const ZERO: DVec2 = DVec2 { x: 0.0, y: 0.0 };
    /// The all-ones vector.
    pub const ONE: DVec2 = DVec2 { x: 1.0, y: 1.0 };

    /// Builds a vector from its components.
    #[inline]
    pub const fn new(x: f64, y: f64) -> DVec2 {
        DVec2 { x, y }
    }

    /// Builds a vector with every component set to `v`.
    #[inline]
    pub const fn splat(v: f64) -> DVec2 {
        DVec2 { x: v, y: v }
    }

    /// The dot product `self · other`.
    #[inline]
    pub fn dot(self, other: DVec2) -> f64 {
        self.x * other.x + self.y * other.y
    }

    /// The 2D cross product (signed parallelogram area).
    #[inline]
    pub fn cross(self, other: DVec2) -> f64 {
        self.x * other.y - self.y * other.x
    }

    /// The Euclidean length.
    #[inline]
    pub fn length(self) -> f64 {
        self.length_squared().sqrt()
    }

    /// The squared length (no `sqrt`).
    #[inline]
    pub fn length_squared(self) -> f64 {
        self.x * self.x + self.y * self.y
    }

    /// The distance to `other`.
    #[inline]
    pub fn distance(self, other: DVec2) -> f64 {
        (self - other).length()
    }

    /// The unit vector in the same direction. Returns [`DVec2::ZERO`] for a
    /// zero-length input rather than producing `NaN`.
    #[inline]
    pub fn normalize(self) -> DVec2 {
        let l = self.length();
        if l == 0.0 { DVec2::ZERO } else { self / l }
    }

    /// Linear interpolation: `self` at `t == 0`, `other` at `t == 1`.
    #[inline]
    pub fn lerp(self, other: DVec2, t: f64) -> DVec2 {
        self + (other - self) * t
    }

    /// Component-wise minimum.
    #[inline]
    pub fn min(self, other: DVec2) -> DVec2 {
        DVec2::new(self.x.min(other.x), self.y.min(other.y))
    }

    /// Component-wise maximum.
    #[inline]
    pub fn max(self, other: DVec2) -> DVec2 {
        DVec2::new(self.x.max(other.x), self.y.max(other.y))
    }

    /// Rounds each component to the nearest device pixel at scale `dpi_factor`.
    ///
    /// This is the shimmer fix the `f64` path exists for: snapping an
    /// accumulated logical coordinate to the pixel grid before it crosses to the
    /// `f32` render side keeps a slowly-scrolling surface crisp instead of
    /// letting rounding noise drift. `dpi_factor` is device-pixels per logical
    /// pixel (e.g. `2.0` on a 2× display). A non-positive factor is a no-op.
    #[inline]
    pub fn dpi_snap(self, dpi_factor: f64) -> DVec2 {
        if dpi_factor <= 0.0 {
            return self;
        }
        DVec2::new(
            (self.x * dpi_factor).round() / dpi_factor,
            (self.y * dpi_factor).round() / dpi_factor,
        )
    }

    /// Narrows to an `f32` [`Vec2`] for the render/GPU boundary. Explicit
    /// because it is lossy — call it deliberately at the edge.
    #[inline]
    pub fn to_vec2(self) -> Vec2 {
        Vec2::new(self.x as f32, self.y as f32)
    }
}

// A widening `Vec2 -> DVec2` conversion is lossless, so it earns `From`; the
// narrowing direction is the explicit `to_vec2` / `From<DVec2>` below.
impl From<Vec2> for DVec2 {
    #[inline]
    fn from(v: Vec2) -> DVec2 {
        DVec2::new(v.x as f64, v.y as f64)
    }
}

impl From<DVec2> for Vec2 {
    #[inline]
    fn from(v: DVec2) -> Vec2 {
        v.to_vec2()
    }
}

impl Add for DVec2 {
    type Output = DVec2;
    #[inline]
    fn add(self, rhs: DVec2) -> DVec2 {
        DVec2::new(self.x + rhs.x, self.y + rhs.y)
    }
}
impl Sub for DVec2 {
    type Output = DVec2;
    #[inline]
    fn sub(self, rhs: DVec2) -> DVec2 {
        DVec2::new(self.x - rhs.x, self.y - rhs.y)
    }
}
impl Neg for DVec2 {
    type Output = DVec2;
    #[inline]
    fn neg(self) -> DVec2 {
        DVec2::new(-self.x, -self.y)
    }
}
impl Mul<DVec2> for DVec2 {
    type Output = DVec2;
    #[inline]
    fn mul(self, rhs: DVec2) -> DVec2 {
        DVec2::new(self.x * rhs.x, self.y * rhs.y)
    }
}
impl Div<DVec2> for DVec2 {
    type Output = DVec2;
    #[inline]
    fn div(self, rhs: DVec2) -> DVec2 {
        DVec2::new(self.x / rhs.x, self.y / rhs.y)
    }
}
impl Mul<f64> for DVec2 {
    type Output = DVec2;
    #[inline]
    fn mul(self, rhs: f64) -> DVec2 {
        DVec2::new(self.x * rhs, self.y * rhs)
    }
}
impl Mul<DVec2> for f64 {
    type Output = DVec2;
    #[inline]
    fn mul(self, rhs: DVec2) -> DVec2 {
        DVec2::new(self * rhs.x, self * rhs.y)
    }
}
impl Div<f64> for DVec2 {
    type Output = DVec2;
    #[inline]
    fn div(self, rhs: f64) -> DVec2 {
        DVec2::new(self.x / rhs, self.y / rhs)
    }
}
impl AddAssign for DVec2 {
    #[inline]
    fn add_assign(&mut self, rhs: DVec2) {
        self.x += rhs.x;
        self.y += rhs.y;
    }
}
impl SubAssign for DVec2 {
    #[inline]
    fn sub_assign(&mut self, rhs: DVec2) {
        self.x -= rhs.x;
        self.y -= rhs.y;
    }
}
impl MulAssign<f64> for DVec2 {
    #[inline]
    fn mul_assign(&mut self, rhs: f64) {
        self.x *= rhs;
        self.y *= rhs;
    }
}
impl DivAssign<f64> for DVec2 {
    #[inline]
    fn div_assign(&mut self, rhs: f64) {
        self.x /= rhs;
        self.y /= rhs;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vec::vec2;

    #[test]
    fn arithmetic_is_componentwise() {
        let a = dvec2(1.0, 2.0);
        let b = dvec2(4.0, 6.0);
        assert_eq!(a + b, dvec2(5.0, 8.0));
        assert_eq!(b - a, dvec2(3.0, 4.0));
        assert_eq!(a * 2.0, dvec2(2.0, 4.0));
        assert_eq!(2.0 * a, dvec2(2.0, 4.0));
    }

    #[test]
    fn normalize_zero_guard_returns_zero_not_nan() {
        let n = DVec2::ZERO.normalize();
        assert_eq!(n, DVec2::ZERO);
        assert!(!n.x.is_nan() && !n.y.is_nan());
    }

    #[test]
    fn vec2_widens_losslessly_and_narrows_back() {
        let v = vec2(1.5, -2.25); // exactly representable in both f32 and f64
        let d: DVec2 = v.into();
        assert_eq!(d, dvec2(1.5, -2.25));
        let back: Vec2 = d.into();
        assert_eq!(back, v);
    }

    #[test]
    fn dpi_snap_rounds_to_the_device_grid() {
        // At 2x, the grid step is 0.5 logical px.
        assert_eq!(dvec2(1.24, 1.26).dpi_snap(2.0), dvec2(1.0, 1.5));
        // Already on-grid values are unchanged.
        assert_eq!(dvec2(3.5, 4.0).dpi_snap(2.0), dvec2(3.5, 4.0));
        // Non-positive factor is a no-op.
        assert_eq!(dvec2(1.23, 4.56).dpi_snap(0.0), dvec2(1.23, 4.56));
    }

    #[test]
    fn dot_and_cross_match_hand_computation() {
        assert_eq!(dvec2(1.0, 2.0).dot(dvec2(3.0, 4.0)), 11.0);
        assert_eq!(dvec2(1.0, 0.0).cross(dvec2(0.0, 1.0)), 1.0);
    }
}
