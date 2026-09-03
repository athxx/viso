//! `f32` vectors: [`Vec2`], [`Vec3`], [`Vec4`].
//!
//! `f32` is the primary precision across the library — it matches the render/ui
//! types, is cache-friendly, and is what the GPU upload boundary wants anyway.
//! The accuracy-sensitive UI-layout path uses [`DVec2`](crate::DVec2) instead.
//!
//! Every type is `#[repr(C)]` and `Copy` with plain public fields, so its layout
//! is stable and pointer-width independent. `dot`, `cross`, `normalize`, and the
//! rest are **methods** (`a.dot(b)`, `a.cross(b)`), not associated functions.

use core::ops::{Add, AddAssign, Div, DivAssign, Mul, MulAssign, Neg, Sub, SubAssign};

/// A 2-component `f32` vector.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Vec2 {
    /// First component.
    pub x: f32,
    /// Second component.
    pub y: f32,
}

/// A 3-component `f32` vector.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Vec3 {
    /// First component.
    pub x: f32,
    /// Second component.
    pub y: f32,
    /// Third component.
    pub z: f32,
}

/// A 4-component `f32` vector.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Vec4 {
    /// First component.
    pub x: f32,
    /// Second component.
    pub y: f32,
    /// Third component.
    pub z: f32,
    /// Fourth component.
    pub w: f32,
}

/// Shorthand constructor for [`Vec2`].
#[inline]
pub const fn vec2(x: f32, y: f32) -> Vec2 {
    Vec2 { x, y }
}

/// Shorthand constructor for [`Vec3`].
#[inline]
pub const fn vec3(x: f32, y: f32, z: f32) -> Vec3 {
    Vec3 { x, y, z }
}

/// Shorthand constructor for [`Vec4`].
#[inline]
pub const fn vec4(x: f32, y: f32, z: f32, w: f32) -> Vec4 {
    Vec4 { x, y, z, w }
}

impl Vec2 {
    /// The zero vector.
    pub const ZERO: Vec2 = Vec2 { x: 0.0, y: 0.0 };
    /// The all-ones vector.
    pub const ONE: Vec2 = Vec2 { x: 1.0, y: 1.0 };
    /// The `+x` unit vector.
    pub const X: Vec2 = Vec2 { x: 1.0, y: 0.0 };
    /// The `+y` unit vector.
    pub const Y: Vec2 = Vec2 { x: 0.0, y: 1.0 };

    /// Builds a vector from its components.
    #[inline]
    pub const fn new(x: f32, y: f32) -> Vec2 {
        Vec2 { x, y }
    }

    /// Builds a vector with every component set to `v`.
    #[inline]
    pub const fn splat(v: f32) -> Vec2 {
        Vec2 { x: v, y: v }
    }

    /// The dot product `self · other`.
    #[inline]
    pub fn dot(self, other: Vec2) -> f32 {
        self.x * other.x + self.y * other.y
    }

    /// The 2D cross product (the `z` of the 3D cross), i.e. the signed area of
    /// the parallelogram spanned by the two vectors.
    #[inline]
    pub fn cross(self, other: Vec2) -> f32 {
        self.x * other.y - self.y * other.x
    }

    /// The Euclidean length.
    #[inline]
    pub fn length(self) -> f32 {
        self.length_squared().sqrt()
    }

    /// The squared length (no `sqrt`).
    #[inline]
    pub fn length_squared(self) -> f32 {
        self.x * self.x + self.y * self.y
    }

    /// The distance to `other`.
    #[inline]
    pub fn distance(self, other: Vec2) -> f32 {
        (self - other).length()
    }

    /// The unit vector in the same direction. Returns [`Vec2::ZERO`] for a
    /// zero-length input rather than producing `NaN`.
    #[inline]
    pub fn normalize(self) -> Vec2 {
        let l = self.length();
        if l == 0.0 { Vec2::ZERO } else { self / l }
    }

    /// Linear interpolation: `self` at `t == 0`, `other` at `t == 1`.
    #[inline]
    pub fn lerp(self, other: Vec2, t: f32) -> Vec2 {
        self + (other - self) * t
    }

    /// Component-wise minimum.
    #[inline]
    pub fn min(self, other: Vec2) -> Vec2 {
        Vec2::new(self.x.min(other.x), self.y.min(other.y))
    }

    /// Component-wise maximum.
    #[inline]
    pub fn max(self, other: Vec2) -> Vec2 {
        Vec2::new(self.x.max(other.x), self.y.max(other.y))
    }

    /// Extends to a [`Vec3`] with the given `z`.
    #[inline]
    pub const fn extend(self, z: f32) -> Vec3 {
        Vec3::new(self.x, self.y, z)
    }
}

impl Vec3 {
    /// The zero vector.
    pub const ZERO: Vec3 = Vec3 {
        x: 0.0,
        y: 0.0,
        z: 0.0,
    };
    /// The all-ones vector.
    pub const ONE: Vec3 = Vec3 {
        x: 1.0,
        y: 1.0,
        z: 1.0,
    };
    /// The `+x` unit vector.
    pub const X: Vec3 = Vec3 {
        x: 1.0,
        y: 0.0,
        z: 0.0,
    };
    /// The `+y` unit vector.
    pub const Y: Vec3 = Vec3 {
        x: 0.0,
        y: 1.0,
        z: 0.0,
    };
    /// The `+z` unit vector.
    pub const Z: Vec3 = Vec3 {
        x: 0.0,
        y: 0.0,
        z: 1.0,
    };

    /// Builds a vector from its components.
    #[inline]
    pub const fn new(x: f32, y: f32, z: f32) -> Vec3 {
        Vec3 { x, y, z }
    }

    /// Builds a vector with every component set to `v`.
    #[inline]
    pub const fn splat(v: f32) -> Vec3 {
        Vec3 { x: v, y: v, z: v }
    }

    /// The dot product `self · other`.
    #[inline]
    pub fn dot(self, other: Vec3) -> f32 {
        self.x * other.x + self.y * other.y + self.z * other.z
    }

    /// The cross product `self × other`.
    #[inline]
    pub fn cross(self, other: Vec3) -> Vec3 {
        Vec3::new(
            self.y * other.z - self.z * other.y,
            self.z * other.x - self.x * other.z,
            self.x * other.y - self.y * other.x,
        )
    }

    /// The Euclidean length.
    #[inline]
    pub fn length(self) -> f32 {
        self.length_squared().sqrt()
    }

    /// The squared length (no `sqrt`).
    #[inline]
    pub fn length_squared(self) -> f32 {
        self.x * self.x + self.y * self.y + self.z * self.z
    }

    /// The distance to `other`.
    #[inline]
    pub fn distance(self, other: Vec3) -> f32 {
        (self - other).length()
    }

    /// The unit vector in the same direction. Returns [`Vec3::ZERO`] for a
    /// zero-length input rather than producing `NaN`.
    #[inline]
    pub fn normalize(self) -> Vec3 {
        let l = self.length();
        if l == 0.0 { Vec3::ZERO } else { self / l }
    }

    /// Linear interpolation: `self` at `t == 0`, `other` at `t == 1`.
    #[inline]
    pub fn lerp(self, other: Vec3, t: f32) -> Vec3 {
        self + (other - self) * t
    }

    /// Component-wise minimum.
    #[inline]
    pub fn min(self, other: Vec3) -> Vec3 {
        Vec3::new(
            self.x.min(other.x),
            self.y.min(other.y),
            self.z.min(other.z),
        )
    }

    /// Component-wise maximum.
    #[inline]
    pub fn max(self, other: Vec3) -> Vec3 {
        Vec3::new(
            self.x.max(other.x),
            self.y.max(other.y),
            self.z.max(other.z),
        )
    }

    /// The `x` and `y` components as a [`Vec2`].
    #[inline]
    pub const fn truncate(self) -> Vec2 {
        Vec2::new(self.x, self.y)
    }

    /// Extends to a [`Vec4`] with the given `w`.
    #[inline]
    pub const fn extend(self, w: f32) -> Vec4 {
        Vec4::new(self.x, self.y, self.z, w)
    }
}

impl Vec4 {
    /// The zero vector.
    pub const ZERO: Vec4 = Vec4 {
        x: 0.0,
        y: 0.0,
        z: 0.0,
        w: 0.0,
    };
    /// The all-ones vector.
    pub const ONE: Vec4 = Vec4 {
        x: 1.0,
        y: 1.0,
        z: 1.0,
        w: 1.0,
    };

    /// Builds a vector from its components.
    #[inline]
    pub const fn new(x: f32, y: f32, z: f32, w: f32) -> Vec4 {
        Vec4 { x, y, z, w }
    }

    /// Builds a vector with every component set to `v`.
    #[inline]
    pub const fn splat(v: f32) -> Vec4 {
        Vec4 {
            x: v,
            y: v,
            z: v,
            w: v,
        }
    }

    /// The dot product `self · other`.
    #[inline]
    pub fn dot(self, other: Vec4) -> f32 {
        self.x * other.x + self.y * other.y + self.z * other.z + self.w * other.w
    }

    /// The Euclidean length.
    #[inline]
    pub fn length(self) -> f32 {
        self.length_squared().sqrt()
    }

    /// The squared length (no `sqrt`).
    #[inline]
    pub fn length_squared(self) -> f32 {
        self.dot(self)
    }

    /// The unit vector in the same direction. Returns [`Vec4::ZERO`] for a
    /// zero-length input rather than producing `NaN`.
    #[inline]
    pub fn normalize(self) -> Vec4 {
        let l = self.length();
        if l == 0.0 { Vec4::ZERO } else { self / l }
    }

    /// Linear interpolation: `self` at `t == 0`, `other` at `t == 1`.
    #[inline]
    pub fn lerp(self, other: Vec4, t: f32) -> Vec4 {
        self + (other - self) * t
    }

    /// The `x`, `y`, and `z` components as a [`Vec3`].
    #[inline]
    pub const fn truncate(self) -> Vec3 {
        Vec3::new(self.x, self.y, self.z)
    }
}

// Operator overloads. Generated with a small macro so every vector type gets the
// same component-wise arithmetic and scalar scaling without hand-repeating it.
macro_rules! impl_vec_ops {
    ($ty:ident { $($field:ident),+ }) => {
        impl Add for $ty {
            type Output = $ty;
            #[inline]
            fn add(self, rhs: $ty) -> $ty {
                $ty { $($field: self.$field + rhs.$field),+ }
            }
        }
        impl Sub for $ty {
            type Output = $ty;
            #[inline]
            fn sub(self, rhs: $ty) -> $ty {
                $ty { $($field: self.$field - rhs.$field),+ }
            }
        }
        impl Neg for $ty {
            type Output = $ty;
            #[inline]
            fn neg(self) -> $ty {
                $ty { $($field: -self.$field),+ }
            }
        }
        // Component-wise multiply/divide (Hadamard).
        impl Mul<$ty> for $ty {
            type Output = $ty;
            #[inline]
            fn mul(self, rhs: $ty) -> $ty {
                $ty { $($field: self.$field * rhs.$field),+ }
            }
        }
        impl Div<$ty> for $ty {
            type Output = $ty;
            #[inline]
            fn div(self, rhs: $ty) -> $ty {
                $ty { $($field: self.$field / rhs.$field),+ }
            }
        }
        // Scalar multiply/divide.
        impl Mul<f32> for $ty {
            type Output = $ty;
            #[inline]
            fn mul(self, rhs: f32) -> $ty {
                $ty { $($field: self.$field * rhs),+ }
            }
        }
        impl Mul<$ty> for f32 {
            type Output = $ty;
            #[inline]
            fn mul(self, rhs: $ty) -> $ty {
                $ty { $($field: self * rhs.$field),+ }
            }
        }
        impl Div<f32> for $ty {
            type Output = $ty;
            #[inline]
            fn div(self, rhs: f32) -> $ty {
                $ty { $($field: self.$field / rhs),+ }
            }
        }
        impl AddAssign for $ty {
            #[inline]
            fn add_assign(&mut self, rhs: $ty) {
                $(self.$field += rhs.$field;)+
            }
        }
        impl SubAssign for $ty {
            #[inline]
            fn sub_assign(&mut self, rhs: $ty) {
                $(self.$field -= rhs.$field;)+
            }
        }
        impl MulAssign<f32> for $ty {
            #[inline]
            fn mul_assign(&mut self, rhs: f32) {
                $(self.$field *= rhs;)+
            }
        }
        impl DivAssign<f32> for $ty {
            #[inline]
            fn div_assign(&mut self, rhs: f32) {
                $(self.$field /= rhs;)+
            }
        }
    };
}

impl_vec_ops!(Vec2 { x, y });
impl_vec_ops!(Vec3 { x, y, z });
impl_vec_ops!(Vec4 { x, y, z, w });

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arithmetic_is_componentwise() {
        let a = vec3(1.0, 2.0, 3.0);
        let b = vec3(4.0, 5.0, 6.0);
        assert_eq!(a + b, vec3(5.0, 7.0, 9.0));
        assert_eq!(b - a, vec3(3.0, 3.0, 3.0));
        assert_eq!(a * 2.0, vec3(2.0, 4.0, 6.0));
        assert_eq!(2.0 * a, vec3(2.0, 4.0, 6.0));
        assert_eq!(-a, vec3(-1.0, -2.0, -3.0));
    }

    #[test]
    fn dot_matches_hand_computation() {
        assert_eq!(vec2(1.0, 2.0).dot(vec2(3.0, 4.0)), 11.0);
        assert_eq!(vec4(1.0, 0.0, 0.0, 2.0).dot(vec4(0.0, 1.0, 0.0, 3.0)), 6.0);
    }

    #[test]
    fn cross_of_basis_vectors() {
        // x × y = z, right-handed.
        assert_eq!(Vec3::X.cross(Vec3::Y), Vec3::Z);
        assert_eq!(Vec3::Y.cross(Vec3::Z), Vec3::X);
        assert_eq!(Vec3::Z.cross(Vec3::X), Vec3::Y);
    }

    #[test]
    fn cross_is_perpendicular_to_operands() {
        let a = vec3(1.0, 2.0, 3.0);
        let b = vec3(-2.0, 0.5, 4.0);
        let c = a.cross(b);
        assert!(c.dot(a).abs() < 1e-5);
        assert!(c.dot(b).abs() < 1e-5);
    }

    #[test]
    fn vec2_cross_is_signed_area() {
        assert_eq!(Vec2::X.cross(Vec2::Y), 1.0);
        assert_eq!(Vec2::Y.cross(Vec2::X), -1.0);
    }

    #[test]
    fn normalize_produces_unit_length() {
        let n = vec3(3.0, 0.0, 4.0).normalize();
        assert!((n.length() - 1.0).abs() < 1e-6);
        assert_eq!(n, vec3(0.6, 0.0, 0.8));
    }

    #[test]
    fn normalize_zero_guard_returns_zero_not_nan() {
        let n = Vec2::ZERO.normalize();
        assert_eq!(n, Vec2::ZERO);
        assert!(!n.x.is_nan() && !n.y.is_nan());
    }

    #[test]
    fn lerp_endpoints_and_midpoint() {
        let a = vec2(0.0, 0.0);
        let b = vec2(10.0, 20.0);
        assert_eq!(a.lerp(b, 0.0), a);
        assert_eq!(a.lerp(b, 1.0), b);
        assert_eq!(a.lerp(b, 0.5), vec2(5.0, 10.0));
    }

    #[test]
    fn extend_and_truncate_round_trip() {
        let v = vec2(1.0, 2.0);
        assert_eq!(v.extend(3.0).extend(4.0), vec4(1.0, 2.0, 3.0, 4.0));
        assert_eq!(vec4(1.0, 2.0, 3.0, 4.0).truncate(), vec3(1.0, 2.0, 3.0));
        assert_eq!(vec3(1.0, 2.0, 3.0).truncate(), vec2(1.0, 2.0));
    }
}
