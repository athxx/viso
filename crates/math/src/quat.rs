//! Unit quaternion for 3D rotation: [`Quat`].
//!
//! Stored as `#[repr(C)]` `(x, y, z, w)` `f32` — the imaginary part first, real
//! part last, matching the GPU-friendly memory order. Where the reference
//! framework exposes `Quat::multiply(a, b)` / `Quat::from_slerp(a, b, t)` as
//! associated functions, this crate makes them **methods** (`a.mul(b)`,
//! `a.slerp(b, t)`) for the same reason the vectors do.
//!
//! One reference bug is fixed here: its `from_slerp` reads the wrong `.w`
//! component when the two quaternions are already close (the linear-blend
//! fallback), pulling the interpolation toward the wrong endpoint. `slerp`
//! below uses the correct component.

use crate::mat::Mat4;
use crate::vec::{Vec3, Vec4};

/// A quaternion, stored `(x, y, z, w)` with the real part `w` last.
///
/// Rotation methods assume a unit quaternion; use [`Quat::normalize`] after
/// accumulating products if drift matters.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Quat {
    /// Imaginary `i` component.
    pub x: f32,
    /// Imaginary `j` component.
    pub y: f32,
    /// Imaginary `k` component.
    pub z: f32,
    /// Real component.
    pub w: f32,
}

impl Default for Quat {
    #[inline]
    fn default() -> Quat {
        Quat::IDENTITY
    }
}

impl Quat {
    /// The identity rotation.
    pub const IDENTITY: Quat = Quat {
        x: 0.0,
        y: 0.0,
        z: 0.0,
        w: 1.0,
    };

    /// Builds a quaternion from its components.
    #[inline]
    pub const fn new(x: f32, y: f32, z: f32, w: f32) -> Quat {
        Quat { x, y, z, w }
    }

    /// A rotation of `angle` radians about the given (assumed unit) `axis`.
    #[inline]
    pub fn from_axis_angle(axis: Vec3, angle: f32) -> Quat {
        let half = angle * 0.5;
        let (s, c) = half.sin_cos();
        Quat {
            x: axis.x * s,
            y: axis.y * s,
            z: axis.z * s,
            w: c,
        }
    }

    /// The Hamilton product `self * rhs` (apply `rhs` first, then `self`).
    ///
    /// A named method rather than an `impl Mul`: the crate deliberately keeps
    /// quaternion composition explicit (`a.mul(b)`), like the vector `dot`/
    /// `cross` methods, so it does not read as scalar multiplication.
    #[allow(clippy::should_implement_trait)]
    #[inline]
    pub fn mul(self, rhs: Quat) -> Quat {
        Quat {
            w: self.w * rhs.w - self.x * rhs.x - self.y * rhs.y - self.z * rhs.z,
            x: self.w * rhs.x + self.x * rhs.w + self.y * rhs.z - self.z * rhs.y,
            y: self.w * rhs.y - self.x * rhs.z + self.y * rhs.w + self.z * rhs.x,
            z: self.w * rhs.z + self.x * rhs.y - self.y * rhs.x + self.z * rhs.w,
        }
    }

    /// The conjugate / inverse of a unit quaternion (negated imaginary part).
    #[inline]
    pub fn conjugate(self) -> Quat {
        Quat {
            x: -self.x,
            y: -self.y,
            z: -self.z,
            w: self.w,
        }
    }

    /// The dot product of the two quaternions viewed as 4-vectors.
    #[inline]
    pub fn dot(self, rhs: Quat) -> f32 {
        self.x * rhs.x + self.y * rhs.y + self.z * rhs.z + self.w * rhs.w
    }

    /// The length (norm).
    #[inline]
    pub fn length(self) -> f32 {
        self.dot(self).sqrt()
    }

    /// The unit quaternion in the same direction. Returns [`Quat::IDENTITY`]
    /// for a zero-length input rather than producing `NaN`.
    #[inline]
    pub fn normalize(self) -> Quat {
        let l = self.length();
        if l == 0.0 {
            Quat::IDENTITY
        } else {
            let inv = 1.0 / l;
            Quat {
                x: self.x * inv,
                y: self.y * inv,
                z: self.z * inv,
                w: self.w * inv,
            }
        }
    }

    /// Rotates a vector by this (assumed unit) quaternion.
    #[inline]
    pub fn rotate(self, v: Vec3) -> Vec3 {
        // v' = v + 2 * q_xyz × (q_xyz × v + w * v)
        let u = Vec3::new(self.x, self.y, self.z);
        let t = u.cross(v) * 2.0;
        v + t * self.w + u.cross(t)
    }

    /// Spherical linear interpolation from `self` (at `t == 0`) to `rhs` (at
    /// `t == 1`), taking the shorter arc.
    ///
    /// Unlike the reference, the near-parallel fallback uses the correct
    /// endpoint `w` component.
    pub fn slerp(self, rhs: Quat, t: f32) -> Quat {
        let mut cos_theta = self.dot(rhs);
        // Take the shorter arc by flipping one quaternion if needed.
        let mut end = rhs;
        if cos_theta < 0.0 {
            end = Quat::new(-rhs.x, -rhs.y, -rhs.z, -rhs.w);
            cos_theta = -cos_theta;
        }

        // Nearly parallel: fall back to normalized linear blend to avoid a
        // division by ~0. Reference bug fixed: blend `end.w`, not `rhs.w`.
        if cos_theta > 0.9995 {
            return Quat::new(
                self.x + (end.x - self.x) * t,
                self.y + (end.y - self.y) * t,
                self.z + (end.z - self.z) * t,
                self.w + (end.w - self.w) * t,
            )
            .normalize();
        }

        let theta = cos_theta.clamp(-1.0, 1.0).acos();
        let sin_theta = theta.sin();
        let scale0 = ((1.0 - t) * theta).sin() / sin_theta;
        let scale1 = (t * theta).sin() / sin_theta;
        Quat::new(
            scale0 * self.x + scale1 * end.x,
            scale0 * self.y + scale1 * end.y,
            scale0 * self.z + scale1 * end.z,
            scale0 * self.w + scale1 * end.w,
        )
    }

    /// The equivalent rotation as a [`Mat4`] (upper-left 3×3 is the rotation,
    /// translation zero). Assumes a unit quaternion.
    pub fn to_mat4(self) -> Mat4 {
        let Quat { x, y, z, w } = self;
        let (x2, y2, z2) = (x + x, y + y, z + z);
        let (xx, xy, xz) = (x * x2, x * y2, x * z2);
        let (yy, yz, zz) = (y * y2, y * z2, z * z2);
        let (wx, wy, wz) = (w * x2, w * y2, w * z2);
        Mat4::from_cols_array([
            1.0 - (yy + zz),
            xy + wz,
            xz - wy,
            0.0,
            xy - wz,
            1.0 - (xx + zz),
            yz + wx,
            0.0,
            xz + wy,
            yz - wx,
            1.0 - (xx + yy),
            0.0,
            0.0,
            0.0,
            0.0,
            1.0,
        ])
    }

    /// The quaternion as a `(x, y, z, w)` [`Vec4`].
    #[inline]
    pub const fn to_vec4(self) -> Vec4 {
        Vec4::new(self.x, self.y, self.z, self.w)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vec::vec3;

    fn approx_vec3(a: Vec3, b: Vec3, eps: f32) -> bool {
        (a.x - b.x).abs() < eps && (a.y - b.y).abs() < eps && (a.z - b.z).abs() < eps
    }

    #[test]
    fn identity_rotates_nothing() {
        let v = vec3(1.0, 2.0, 3.0);
        assert_eq!(Quat::IDENTITY.rotate(v), v);
    }

    #[test]
    fn quarter_turn_about_z_maps_x_to_y() {
        let q = Quat::from_axis_angle(Vec3::Z, core::f32::consts::FRAC_PI_2);
        assert!(approx_vec3(
            q.rotate(vec3(1.0, 0.0, 0.0)),
            vec3(0.0, 1.0, 0.0),
            1e-6
        ));
    }

    #[test]
    fn rotate_agrees_with_matrix_form() {
        let q = Quat::from_axis_angle(vec3(1.0, 1.0, 0.0).normalize(), 0.7);
        let v = vec3(0.3, -1.2, 2.0);
        let via_quat = q.rotate(v);
        let via_mat = q.to_mat4().transform_point(v);
        assert!(approx_vec3(via_quat, via_mat, 1e-5));
    }

    #[test]
    fn product_of_rotations_composes() {
        // Two successive quarter-turns about z make a half-turn.
        let q = Quat::from_axis_angle(Vec3::Z, core::f32::consts::FRAC_PI_2);
        let half = q.mul(q);
        assert!(approx_vec3(
            half.rotate(vec3(1.0, 0.0, 0.0)),
            vec3(-1.0, 0.0, 0.0),
            1e-6
        ));
    }

    #[test]
    fn conjugate_undoes_rotation() {
        let q = Quat::from_axis_angle(vec3(0.0, 1.0, 0.0).normalize(), 1.1);
        let v = vec3(1.0, 2.0, 3.0);
        let round = q.conjugate().rotate(q.rotate(v));
        assert!(approx_vec3(round, v, 1e-5));
    }

    #[test]
    fn slerp_endpoints_are_exact() {
        let a = Quat::from_axis_angle(Vec3::Z, 0.2);
        let b = Quat::from_axis_angle(Vec3::Z, 1.3);
        assert!(a.slerp(b, 0.0).dot(a).abs() > 0.9999);
        assert!(a.slerp(b, 1.0).dot(b).abs() > 0.9999);
    }

    #[test]
    fn slerp_midpoint_is_half_angle() {
        let a = Quat::IDENTITY;
        let b = Quat::from_axis_angle(Vec3::Z, core::f32::consts::FRAC_PI_2);
        let mid = a.slerp(b, 0.5);
        // Midpoint should be a rotation of PI/4 about z.
        let expect = Quat::from_axis_angle(Vec3::Z, core::f32::consts::FRAC_PI_4);
        assert!(mid.dot(expect).abs() > 0.9999);
    }

    #[test]
    fn slerp_near_parallel_uses_correct_endpoint() {
        // Two nearly-identical quaternions hit the linear-blend fallback; the
        // result at t=1 must land on `b`, not drift toward `-b` (the reference
        // .w bug). dot with b stays ~1.
        let a = Quat::from_axis_angle(Vec3::Z, 0.10);
        let b = Quat::from_axis_angle(Vec3::Z, 0.1001);
        assert!(a.slerp(b, 1.0).dot(b) > 0.9999);
    }
}
