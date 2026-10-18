//! 2D and 3D transforms: [`Affine2`] and [`Transform3`].
//!
//! [`Affine2`] is the one 2D affine transform of the framework — UI transform
//! roots, scroll/zoom, SVG `transform`, path transforms and the GPU transform
//! buffer all use it. It is six `f32` in canvas/SVG `matrix(a b c d e f)` order,
//! composes and inverts in closed form.
//!
//! [`Transform3`] is a rigid 3D pose — a unit [`Quat`] rotation plus a [`Vec3`]
//! translation. It has no scale or shear, so its inverse is exact and cheap.

use crate::float::{sin_cos, sqrt, tan};
use crate::mat::Mat4;
use crate::quat::Quat;
use crate::vec::{Vec2, Vec3};

/// A 2D affine transform: `x' = a·x + c·y + e`, `y' = b·x + d·y + f`.
///
/// The field order is the canvas/SVG `matrix(a b c d e f)` order, and the
/// `#[repr(C)]` layout is the 24-byte transform-buffer entry.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Affine2 {
    /// Linear part, column 0 row 0.
    pub a: f32,
    /// Linear part, column 0 row 1.
    pub b: f32,
    /// Linear part, column 1 row 0.
    pub c: f32,
    /// Linear part, column 1 row 1.
    pub d: f32,
    /// Translation x.
    pub e: f32,
    /// Translation y.
    pub f: f32,
}

impl Default for Affine2 {
    #[inline]
    fn default() -> Affine2 {
        Affine2::IDENTITY
    }
}

impl Affine2 {
    /// The identity transform.
    pub const IDENTITY: Affine2 = Affine2::new(1.0, 0.0, 0.0, 1.0, 0.0, 0.0);

    /// A transform from its six coefficients.
    #[inline]
    pub const fn new(a: f32, b: f32, c: f32, d: f32, e: f32, f: f32) -> Affine2 {
        Affine2 { a, b, c, d, e, f }
    }

    /// A pure translation.
    #[inline]
    pub const fn from_translation(t: Vec2) -> Affine2 {
        Affine2::new(1.0, 0.0, 0.0, 1.0, t.x, t.y)
    }

    /// A pure translation by components.
    #[inline]
    pub const fn translate(tx: f32, ty: f32) -> Affine2 {
        Affine2::new(1.0, 0.0, 0.0, 1.0, tx, ty)
    }

    /// A pure (possibly non-uniform) scale about the origin.
    #[inline]
    pub const fn from_scale(s: Vec2) -> Affine2 {
        Affine2::new(s.x, 0.0, 0.0, s.y, 0.0, 0.0)
    }

    /// A scale about the origin by components.
    #[inline]
    pub const fn scale(sx: f32, sy: f32) -> Affine2 {
        Affine2::new(sx, 0.0, 0.0, sy, 0.0, 0.0)
    }

    /// A rotation of `angle` radians about the origin: counter-clockwise on a
    /// y-up plane, clockwise on a y-down canvas.
    #[inline]
    pub fn from_rotation(angle: f32) -> Affine2 {
        let (s, c) = sin_cos(angle);
        Affine2::new(c, s, -s, c, 0.0, 0.0)
    }

    /// A horizontal skew of `angle` radians.
    #[inline]
    pub fn skew_x(angle: f32) -> Affine2 {
        Affine2::new(1.0, 0.0, tan(angle), 1.0, 0.0, 0.0)
    }

    /// A vertical skew of `angle` radians.
    #[inline]
    pub fn skew_y(angle: f32) -> Affine2 {
        Affine2::new(1.0, tan(angle), 0.0, 1.0, 0.0, 0.0)
    }

    /// The translation part.
    #[inline]
    pub const fn translation(&self) -> Vec2 {
        Vec2::new(self.e, self.f)
    }

    /// Transforms a point (linear part **and** translation).
    #[inline]
    pub fn transform_point(&self, p: Vec2) -> Vec2 {
        Vec2::new(
            self.a * p.x + self.c * p.y + self.e,
            self.b * p.x + self.d * p.y + self.f,
        )
    }

    /// Transforms a direction vector (linear part only, no translation).
    #[inline]
    pub fn transform_vector(&self, v: Vec2) -> Vec2 {
        Vec2::new(self.a * v.x + self.c * v.y, self.b * v.x + self.d * v.y)
    }

    /// `self × other`: `other` applies to a point first, then `self` — the SVG
    /// `transform="A B"` and parent/child order (`world = parent.pre_concat(local)`).
    #[inline]
    pub fn pre_concat(&self, o: &Affine2) -> Affine2 {
        Affine2 {
            a: self.a * o.a + self.c * o.b,
            b: self.b * o.a + self.d * o.b,
            c: self.a * o.c + self.c * o.d,
            d: self.b * o.c + self.d * o.d,
            e: self.a * o.e + self.c * o.f + self.e,
            f: self.b * o.e + self.d * o.f + self.f,
        }
    }

    /// The composite that applies `self` first, then `rhs`:
    /// `self.then(rhs).transform_point(p) == rhs.transform_point(self.transform_point(p))`.
    #[inline]
    pub fn then(&self, rhs: &Affine2) -> Affine2 {
        rhs.pre_concat(self)
    }

    /// The determinant of the linear part.
    #[inline]
    pub fn determinant(&self) -> f32 {
        self.a * self.d - self.b * self.c
    }

    /// Whether this is exactly the identity.
    #[inline]
    pub fn is_identity(&self) -> bool {
        *self == Affine2::IDENTITY
    }

    /// Whether this is a pure translation (linear part exactly identity).
    #[inline]
    pub fn is_translation(&self) -> bool {
        self.a == 1.0 && self.b == 0.0 && self.c == 0.0 && self.d == 1.0
    }

    /// Whether it only scales and translates (no rotation or skew).
    #[inline]
    pub fn is_scale_translate(&self) -> bool {
        self.b == 0.0 && self.c == 0.0
    }

    /// Finite and invertible.
    pub fn is_valid(&self) -> bool {
        let det = self.determinant();
        det.is_finite() && det.abs() > 1e-12 && self.e.is_finite() && self.f.is_finite()
    }

    /// The inverse, or `None` when the linear part is singular or non-finite.
    /// Computed in `f64` so near-singular transforms keep their precision; it is
    /// a cold-path operation (hit-test setup, path tolerance), never per vertex.
    pub fn invert(&self) -> Option<Affine2> {
        let (a, b, c, d, e, f) = (
            self.a as f64,
            self.b as f64,
            self.c as f64,
            self.d as f64,
            self.e as f64,
            self.f as f64,
        );
        let det = a * d - b * c;
        if !det.is_finite() || det.abs() < 1e-14 {
            return None;
        }
        let inv = 1.0 / det;
        Some(Affine2 {
            a: (d * inv) as f32,
            b: (-b * inv) as f32,
            c: (-c * inv) as f32,
            d: (a * inv) as f32,
            e: ((c * f - d * e) * inv) as f32,
            f: ((b * e - a * f) * inv) as f32,
        })
    }

    /// [`invert`](Self::invert), falling back to the identity for a singular
    /// transform.
    #[inline]
    pub fn inverse(&self) -> Affine2 {
        self.invert().unwrap_or(Affine2::IDENTITY)
    }

    /// The largest stretch factor applied to any direction (the larger singular
    /// value) — converts device-space tolerances into local space.
    pub fn max_scale(&self) -> f32 {
        let (a, b, c, d) = (self.a, self.b, self.c, self.d);
        let s1 = a * a + b * b + c * c + d * d;
        let (p, q) = (a * a + b * b - c * c - d * d, a * c + b * d);
        let s2 = sqrt(p * p + 4.0 * (q * q));
        sqrt((s1 + s2) * 0.5)
    }

    /// `sqrt(|det|)`: the geometric-mean scale, used for non-directional
    /// lengths under a transform.
    #[inline]
    pub fn mean_scale(&self) -> f32 {
        sqrt(self.determinant().abs())
    }
}

/// A rigid 3D pose: a unit rotation plus a translation. No scale or shear.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Transform3 {
    /// Rotation, applied before the translation.
    pub rotation: Quat,
    /// Translation, applied after the rotation.
    pub translation: Vec3,
}

impl Default for Transform3 {
    #[inline]
    fn default() -> Transform3 {
        Transform3::IDENTITY
    }
}

impl Transform3 {
    /// The identity pose.
    pub const IDENTITY: Transform3 = Transform3 {
        rotation: Quat::IDENTITY,
        translation: Vec3::ZERO,
    };

    /// A pure translation.
    #[inline]
    pub const fn from_translation(t: Vec3) -> Transform3 {
        Transform3 {
            rotation: Quat::IDENTITY,
            translation: t,
        }
    }

    /// A pure rotation.
    #[inline]
    pub const fn from_rotation(rotation: Quat) -> Transform3 {
        Transform3 {
            rotation,
            translation: Vec3::ZERO,
        }
    }

    /// Transforms a point: rotate, then translate.
    #[inline]
    pub fn transform_point(&self, p: Vec3) -> Vec3 {
        self.rotation.rotate(p) + self.translation
    }

    /// Transforms a direction vector (rotation only, no translation).
    #[inline]
    pub fn transform_vector(&self, v: Vec3) -> Vec3 {
        self.rotation.rotate(v)
    }

    /// The composite that applies `self` first, then `rhs`:
    /// `(self.then(rhs)).transform_point(p)` ==
    /// `rhs.transform_point(self.transform_point(p))`.
    #[inline]
    pub fn then(&self, rhs: &Transform3) -> Transform3 {
        Transform3 {
            rotation: rhs.rotation.mul(self.rotation),
            translation: rhs.transform_point(self.translation),
        }
    }

    /// The inverse pose. Exact, since the transform is rigid.
    #[inline]
    pub fn inverse(&self) -> Transform3 {
        let inv_rot = self.rotation.conjugate();
        Transform3 {
            rotation: inv_rot,
            translation: inv_rot.rotate(self.translation) * -1.0,
        }
    }

    /// The equivalent [`Mat4`] (rotation in the upper-left 3×3, translation in
    /// column 3).
    pub fn to_mat4(&self) -> Mat4 {
        let mut m = self.rotation.to_mat4();
        m.v[12] = self.translation.x;
        m.v[13] = self.translation.y;
        m.v[14] = self.translation.z;
        m
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vec::{vec2, vec3};

    fn approx2(a: Vec2, b: Vec2, eps: f32) -> bool {
        (a.x - b.x).abs() < eps && (a.y - b.y).abs() < eps
    }
    fn approx3(a: Vec3, b: Vec3, eps: f32) -> bool {
        (a.x - b.x).abs() < eps && (a.y - b.y).abs() < eps && (a.z - b.z).abs() < eps
    }

    #[test]
    fn affine_translation_moves_point_not_vector() {
        let t = Affine2::from_translation(vec2(3.0, 4.0));
        assert_eq!(t.transform_point(vec2(1.0, 1.0)), vec2(4.0, 5.0));
        // A direction vector ignores translation.
        assert_eq!(t.transform_vector(vec2(1.0, 1.0)), vec2(1.0, 1.0));
    }

    #[test]
    fn affine_rotation_turns_x_into_y() {
        let r = Affine2::from_rotation(core::f32::consts::FRAC_PI_2);
        assert!(approx2(
            r.transform_point(vec2(1.0, 0.0)),
            vec2(0.0, 1.0),
            1e-6
        ));
    }

    #[test]
    fn affine_then_applies_self_first() {
        // Scale by 2, then translate by (10, 0).
        let s = Affine2::from_scale(vec2(2.0, 2.0));
        let t = Affine2::from_translation(vec2(10.0, 0.0));
        let st = s.then(&t);
        // point (3, 0) -> scale -> (6, 0) -> translate -> (16, 0)
        assert!(approx2(
            st.transform_point(vec2(3.0, 0.0)),
            vec2(16.0, 0.0),
            1e-6
        ));
        // Matches the manual order.
        let manual = t.transform_point(s.transform_point(vec2(3.0, 0.0)));
        assert!(approx2(st.transform_point(vec2(3.0, 0.0)), manual, 1e-6));
    }

    #[test]
    fn affine_inverse_round_trips() {
        let a = Affine2::from_rotation(0.6).then(&Affine2::from_translation(vec2(5.0, -2.0)));
        let p = vec2(1.7, -3.3);
        let round = a.inverse().transform_point(a.transform_point(p));
        assert!(approx2(round, p, 1e-5));
    }

    #[test]
    fn affine_singular_inverts_to_identity() {
        let degenerate = Affine2::new(0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
        assert_eq!(degenerate.inverse(), Affine2::IDENTITY);
        assert_eq!(degenerate.invert(), None);
    }

    #[test]
    fn pre_concat_is_then_reversed() {
        let s = Affine2::scale(2.0, 3.0);
        let t = Affine2::translate(5.0, -1.0);
        assert_eq!(t.pre_concat(&s), s.then(&t));
        assert_eq!(core::mem::size_of::<Affine2>(), 24);
    }

    #[test]
    fn transform3_rotate_then_translate() {
        let q = Quat::from_axis_angle(Vec3::Z, core::f32::consts::FRAC_PI_2);
        let tf = Transform3 {
            rotation: q,
            translation: vec3(10.0, 0.0, 0.0),
        };
        // (1,0,0) rotates to (0,1,0), then + (10,0,0) = (10,1,0)
        assert!(approx3(
            tf.transform_point(vec3(1.0, 0.0, 0.0)),
            vec3(10.0, 1.0, 0.0),
            1e-6
        ));
    }

    #[test]
    fn transform3_inverse_round_trips() {
        let tf = Transform3 {
            rotation: Quat::from_axis_angle(vec3(1.0, 1.0, 0.0).normalize(), 0.9),
            translation: vec3(3.0, -1.0, 2.0),
        };
        let p = vec3(0.5, 4.0, -2.0);
        let round = tf.inverse().transform_point(tf.transform_point(p));
        assert!(approx3(round, p, 1e-5));
    }

    #[test]
    fn transform3_to_mat4_agrees_with_transform_point() {
        let tf = Transform3 {
            rotation: Quat::from_axis_angle(Vec3::Y, 0.4),
            translation: vec3(1.0, 2.0, 3.0),
        };
        let p = vec3(2.0, -1.0, 0.5);
        assert!(approx3(
            tf.to_mat4().transform_point(p),
            tf.transform_point(p),
            1e-5
        ));
    }

    #[test]
    fn transform3_then_composes() {
        let a = Transform3::from_translation(vec3(1.0, 0.0, 0.0));
        let b =
            Transform3::from_rotation(Quat::from_axis_angle(Vec3::Z, core::f32::consts::FRAC_PI_2));
        let p = vec3(0.0, 0.0, 0.0);
        let via_then = a.then(&b).transform_point(p);
        let manual = b.transform_point(a.transform_point(p));
        assert!(approx3(via_then, manual, 1e-6));
    }
}
