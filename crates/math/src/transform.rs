//! 2D and 3D transforms: [`Affine2`] and [`Transform3`].
//!
//! [`Affine2`] is the 2D affine transform the reference framework lacks — a 2×2
//! linear part plus a translation, exactly what scroll / zoom / UI transforms
//! need without paying for a full 3×3 or homogeneous `Mat4`. It composes and
//! inverts in closed form.
//!
//! [`Transform3`] is a rigid 3D pose — a unit [`Quat`] rotation plus a [`Vec3`]
//! translation (the reference's `Pose`). It has no scale or shear, so its
//! inverse is exact and cheap.

use crate::mat::Mat4;
use crate::quat::Quat;
use crate::vec::{Vec2, Vec3};

/// A 2D affine transform: a 2×2 linear map plus a translation.
///
/// Applies as `p' = M * p + t`, where `M` is column-major `[a, b, c, d]`
/// (`x' = a·x + c·y + tx`, `y' = b·x + d·y + ty`). The linear columns match
/// [`Mat2`](crate::Mat2) storage.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Affine2 {
    /// Column-major 2×2 linear part `[m00, m10, m01, m11]` (`c * 2 + r`).
    pub matrix: [f32; 4],
    /// Translation applied after the linear part.
    pub translation: Vec2,
}

impl Default for Affine2 {
    #[inline]
    fn default() -> Affine2 {
        Affine2::IDENTITY
    }
}

impl Affine2 {
    /// The identity transform.
    pub const IDENTITY: Affine2 = Affine2 {
        matrix: [1.0, 0.0, 0.0, 1.0],
        translation: Vec2::ZERO,
    };

    /// A pure translation.
    #[inline]
    pub const fn from_translation(t: Vec2) -> Affine2 {
        Affine2 {
            matrix: [1.0, 0.0, 0.0, 1.0],
            translation: t,
        }
    }

    /// A pure (possibly non-uniform) scale about the origin.
    #[inline]
    pub const fn from_scale(s: Vec2) -> Affine2 {
        Affine2 {
            matrix: [s.x, 0.0, 0.0, s.y],
            translation: Vec2::ZERO,
        }
    }

    /// A counter-clockwise rotation of `angle` radians about the origin.
    #[inline]
    pub fn from_rotation(angle: f32) -> Affine2 {
        let (s, c) = angle.sin_cos();
        Affine2 {
            matrix: [c, s, -s, c],
            translation: Vec2::ZERO,
        }
    }

    /// Transforms a point (linear part **and** translation).
    #[inline]
    pub fn transform_point(&self, p: Vec2) -> Vec2 {
        let m = &self.matrix;
        Vec2::new(
            m[0] * p.x + m[2] * p.y + self.translation.x,
            m[1] * p.x + m[3] * p.y + self.translation.y,
        )
    }

    /// Transforms a direction vector (linear part only, no translation).
    #[inline]
    pub fn transform_vector(&self, v: Vec2) -> Vec2 {
        let m = &self.matrix;
        Vec2::new(m[0] * v.x + m[2] * v.y, m[1] * v.x + m[3] * v.y)
    }

    /// The composite that applies `self` first, then `rhs`
    /// (`rhs.then(self)` order: `(self.then(rhs)).transform_point(p)` ==
    /// `rhs.transform_point(self.transform_point(p))`).
    pub fn then(&self, rhs: &Affine2) -> Affine2 {
        let a = &self.matrix;
        let b = &rhs.matrix;
        // Linear part: b * a (apply a first).
        let matrix = [
            b[0] * a[0] + b[2] * a[1],
            b[1] * a[0] + b[3] * a[1],
            b[0] * a[2] + b[2] * a[3],
            b[1] * a[2] + b[3] * a[3],
        ];
        // Translation: b * a.t + b.t.
        let translation = rhs.transform_point(self.translation);
        Affine2 {
            matrix,
            translation,
        }
    }

    /// The inverse transform, or [`Affine2::IDENTITY`] if the linear part is
    /// singular (determinant ≈ 0), matching the matrix guard.
    pub fn inverse(&self) -> Affine2 {
        let m = &self.matrix;
        let det = m[0] * m[3] - m[2] * m[1];
        if det == 0.0 {
            return Affine2::IDENTITY;
        }
        let idet = 1.0 / det;
        // Inverse of the 2×2 linear part.
        let inv = [m[3] * idet, -m[1] * idet, -m[2] * idet, m[0] * idet];
        // Inverse translation: -inv * t.
        let t = self.translation;
        let translation = Vec2::new(
            -(inv[0] * t.x + inv[2] * t.y),
            -(inv[1] * t.x + inv[3] * t.y),
        );
        Affine2 {
            matrix: inv,
            translation,
        }
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
        let degenerate = Affine2 {
            matrix: [0.0, 0.0, 0.0, 0.0],
            translation: Vec2::ZERO,
        };
        assert_eq!(degenerate.inverse(), Affine2::IDENTITY);
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
