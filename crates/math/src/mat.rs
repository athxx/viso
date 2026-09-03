//! Column-major matrices: [`Mat2`], [`Mat3`], [`Mat4`].
//!
//! All three use a **uniform flat storage story** — a plain `[f32; N]` in
//! column-major order — rather than the reference framework's mix of
//! column-vector `Mat3` and flat `Mat4`. Column `c`, row `r` lives at index
//! `c * dim + r`. Column-major with the `M * v` convention is what the GPU
//! backends expect, so the upload path stays a memcpy.
//!
//! For [`Mat4`], the translation column is indices 12, 13, 14 (the standard
//! OpenGL/Metal layout). `transform_point` treats the input as `w = 1`.
//!
//! The public storage is the scalar `[f32; N]`; SIMD kernels (see
//! [`simd`](crate::simd)) accelerate the hot `Mat4` paths internally and must
//! match this layout bit-for-bit.

use crate::vec::{Vec2, Vec3, Vec4};

/// A 2×2 column-major matrix, stored as `[f32; 4]`.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Mat2 {
    /// Column-major elements: index `c * 2 + r` is column `c`, row `r`.
    pub v: [f32; 4],
}

/// A 3×3 column-major matrix, stored as `[f32; 9]`.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Mat3 {
    /// Column-major elements: index `c * 3 + r` is column `c`, row `r`.
    pub v: [f32; 9],
}

/// A 4×4 column-major matrix, stored as `[f32; 16]`.
///
/// The translation column is indices 12, 13, 14.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Mat4 {
    /// Column-major elements: index `c * 4 + r` is column `c`, row `r`.
    pub v: [f32; 16],
}

impl Default for Mat2 {
    #[inline]
    fn default() -> Mat2 {
        Mat2::IDENTITY
    }
}
impl Default for Mat3 {
    #[inline]
    fn default() -> Mat3 {
        Mat3::IDENTITY
    }
}
impl Default for Mat4 {
    #[inline]
    fn default() -> Mat4 {
        Mat4::IDENTITY
    }
}

impl Mat2 {
    /// The identity matrix.
    pub const IDENTITY: Mat2 = Mat2 {
        v: [1.0, 0.0, 0.0, 1.0],
    };

    /// Builds from a column-major array.
    #[inline]
    pub const fn from_cols_array(v: [f32; 4]) -> Mat2 {
        Mat2 { v }
    }

    /// A uniform scale on both axes.
    #[inline]
    pub const fn from_scale(s: Vec2) -> Mat2 {
        Mat2 {
            v: [s.x, 0.0, 0.0, s.y],
        }
    }

    /// A counter-clockwise rotation of `angle` radians.
    #[inline]
    pub fn from_angle(angle: f32) -> Mat2 {
        let (s, c) = angle.sin_cos();
        Mat2 { v: [c, s, -s, c] }
    }

    /// The transpose.
    #[inline]
    pub const fn transpose(&self) -> Mat2 {
        let a = &self.v;
        Mat2 {
            v: [a[0], a[2], a[1], a[3]],
        }
    }

    /// The matrix product `self * rhs`.
    #[inline]
    pub fn mul(&self, rhs: &Mat2) -> Mat2 {
        let a = &self.v;
        let b = &rhs.v;
        Mat2 {
            v: [
                a[0] * b[0] + a[2] * b[1],
                a[1] * b[0] + a[3] * b[1],
                a[0] * b[2] + a[2] * b[3],
                a[1] * b[2] + a[3] * b[3],
            ],
        }
    }

    /// Transforms a column vector: `self * v`.
    #[inline]
    pub fn transform_vec2(&self, v: Vec2) -> Vec2 {
        let m = &self.v;
        Vec2::new(m[0] * v.x + m[2] * v.y, m[1] * v.x + m[3] * v.y)
    }

    /// The determinant.
    #[inline]
    pub fn determinant(&self) -> f32 {
        let a = &self.v;
        a[0] * a[3] - a[2] * a[1]
    }
}

impl Mat3 {
    /// The identity matrix.
    pub const IDENTITY: Mat3 = Mat3 {
        v: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0],
    };

    /// Builds from a column-major array.
    #[inline]
    pub const fn from_cols_array(v: [f32; 9]) -> Mat3 {
        Mat3 { v }
    }

    /// The transpose.
    #[inline]
    pub const fn transpose(&self) -> Mat3 {
        let a = &self.v;
        Mat3 {
            v: [a[0], a[3], a[6], a[1], a[4], a[7], a[2], a[5], a[8]],
        }
    }

    /// The matrix product `self * rhs`.
    #[inline]
    pub fn mul(&self, rhs: &Mat3) -> Mat3 {
        let a = &self.v;
        let b = &rhs.v;
        let mut out = [0.0f32; 9];
        for c in 0..3 {
            for r in 0..3 {
                out[c * 3 + r] =
                    a[r] * b[c * 3] + a[3 + r] * b[c * 3 + 1] + a[6 + r] * b[c * 3 + 2];
            }
        }
        Mat3 { v: out }
    }

    /// Transforms a column vector: `self * v`.
    #[inline]
    pub fn transform_vec3(&self, v: Vec3) -> Vec3 {
        let m = &self.v;
        Vec3::new(
            m[0] * v.x + m[3] * v.y + m[6] * v.z,
            m[1] * v.x + m[4] * v.y + m[7] * v.z,
            m[2] * v.x + m[5] * v.y + m[8] * v.z,
        )
    }

    /// The determinant.
    #[inline]
    pub fn determinant(&self) -> f32 {
        let a = &self.v;
        a[0] * (a[4] * a[8] - a[7] * a[5]) - a[3] * (a[1] * a[8] - a[7] * a[2])
            + a[6] * (a[1] * a[5] - a[4] * a[2])
    }
}

impl Mat4 {
    /// The identity matrix.
    pub const IDENTITY: Mat4 = Mat4 {
        v: [
            1.0, 0.0, 0.0, 0.0, //
            0.0, 1.0, 0.0, 0.0, //
            0.0, 0.0, 1.0, 0.0, //
            0.0, 0.0, 0.0, 1.0,
        ],
    };

    /// Builds from a column-major array.
    #[inline]
    pub const fn from_cols_array(v: [f32; 16]) -> Mat4 {
        Mat4 { v }
    }

    /// A pure translation.
    #[inline]
    pub const fn from_translation(t: Vec3) -> Mat4 {
        Mat4 {
            v: [
                1.0, 0.0, 0.0, 0.0, //
                0.0, 1.0, 0.0, 0.0, //
                0.0, 0.0, 1.0, 0.0, //
                t.x, t.y, t.z, 1.0,
            ],
        }
    }

    /// A pure scale.
    #[inline]
    pub const fn from_scale(s: Vec3) -> Mat4 {
        Mat4 {
            v: [
                s.x, 0.0, 0.0, 0.0, //
                0.0, s.y, 0.0, 0.0, //
                0.0, 0.0, s.z, 0.0, //
                0.0, 0.0, 0.0, 1.0,
            ],
        }
    }

    /// The transpose.
    #[inline]
    pub const fn transpose(&self) -> Mat4 {
        let a = &self.v;
        Mat4 {
            v: [
                a[0], a[4], a[8], a[12], //
                a[1], a[5], a[9], a[13], //
                a[2], a[6], a[10], a[14], //
                a[3], a[7], a[11], a[15],
            ],
        }
    }

    /// The matrix product `self * rhs`.
    ///
    /// This is the public scalar reference. The SIMD kernel in
    /// [`simd`](crate::simd) computes the same result bit-for-bit.
    #[inline]
    pub fn mul(&self, rhs: &Mat4) -> Mat4 {
        crate::simd::mat4_mul(&self.v, &rhs.v)
    }

    /// Transforms a homogeneous point (`w = 1`), returning the divided-through
    /// 3D result. For an affine (non-projective) matrix `w` stays 1.
    #[inline]
    pub fn transform_point(&self, p: Vec3) -> Vec3 {
        let out = self.transform_vec4(Vec4::new(p.x, p.y, p.z, 1.0));
        if out.w == 0.0 || out.w == 1.0 {
            Vec3::new(out.x, out.y, out.z)
        } else {
            Vec3::new(out.x / out.w, out.y / out.w, out.z / out.w)
        }
    }

    /// Transforms a homogeneous 4-vector: `self * v`.
    #[inline]
    pub fn transform_vec4(&self, v: Vec4) -> Vec4 {
        let m = &self.v;
        Vec4::new(
            m[0] * v.x + m[4] * v.y + m[8] * v.z + m[12] * v.w,
            m[1] * v.x + m[5] * v.y + m[9] * v.z + m[13] * v.w,
            m[2] * v.x + m[6] * v.y + m[10] * v.z + m[14] * v.w,
            m[3] * v.x + m[7] * v.y + m[11] * v.z + m[15] * v.w,
        )
    }

    /// The inverse. Returns [`Mat4::IDENTITY`] when the matrix is singular
    /// (determinant ≈ 0), matching the reference's non-panicking guard.
    pub fn invert(&self) -> Mat4 {
        let a = &self.v;
        let (a00, a01, a02, a03) = (a[0], a[1], a[2], a[3]);
        let (a10, a11, a12, a13) = (a[4], a[5], a[6], a[7]);
        let (a20, a21, a22, a23) = (a[8], a[9], a[10], a[11]);
        let (a30, a31, a32, a33) = (a[12], a[13], a[14], a[15]);

        let b00 = a00 * a11 - a01 * a10;
        let b01 = a00 * a12 - a02 * a10;
        let b02 = a00 * a13 - a03 * a10;
        let b03 = a01 * a12 - a02 * a11;
        let b04 = a01 * a13 - a03 * a11;
        let b05 = a02 * a13 - a03 * a12;
        let b06 = a20 * a31 - a21 * a30;
        let b07 = a20 * a32 - a22 * a30;
        let b08 = a20 * a33 - a23 * a30;
        let b09 = a21 * a32 - a22 * a31;
        let b10 = a21 * a33 - a23 * a31;
        let b11 = a22 * a33 - a23 * a32;

        let det = b00 * b11 - b01 * b10 + b02 * b09 + b03 * b08 - b04 * b07 + b05 * b06;
        if det == 0.0 {
            return Mat4::IDENTITY;
        }
        let idet = 1.0 / det;
        Mat4 {
            v: [
                (a11 * b11 - a12 * b10 + a13 * b09) * idet,
                (a02 * b10 - a01 * b11 - a03 * b09) * idet,
                (a31 * b05 - a32 * b04 + a33 * b03) * idet,
                (a22 * b04 - a21 * b05 - a23 * b03) * idet,
                (a12 * b08 - a10 * b11 - a13 * b07) * idet,
                (a00 * b11 - a02 * b08 + a03 * b07) * idet,
                (a32 * b02 - a30 * b05 - a33 * b01) * idet,
                (a20 * b05 - a22 * b02 + a23 * b01) * idet,
                (a10 * b10 - a11 * b08 + a13 * b06) * idet,
                (a01 * b08 - a00 * b10 - a03 * b06) * idet,
                (a30 * b04 - a31 * b02 + a33 * b00) * idet,
                (a21 * b02 - a20 * b04 - a23 * b00) * idet,
                (a11 * b07 - a10 * b09 - a12 * b06) * idet,
                (a00 * b09 - a01 * b07 + a02 * b06) * idet,
                (a31 * b01 - a30 * b03 - a32 * b00) * idet,
                (a20 * b03 - a21 * b01 + a22 * b00) * idet,
            ],
        }
    }

    /// The determinant.
    pub fn determinant(&self) -> f32 {
        let a = &self.v;
        let (a00, a01, a02, a03) = (a[0], a[1], a[2], a[3]);
        let (a10, a11, a12, a13) = (a[4], a[5], a[6], a[7]);
        let (a20, a21, a22, a23) = (a[8], a[9], a[10], a[11]);
        let (a30, a31, a32, a33) = (a[12], a[13], a[14], a[15]);

        let b00 = a00 * a11 - a01 * a10;
        let b01 = a00 * a12 - a02 * a10;
        let b02 = a00 * a13 - a03 * a10;
        let b03 = a01 * a12 - a02 * a11;
        let b04 = a01 * a13 - a03 * a11;
        let b05 = a02 * a13 - a03 * a12;
        let b06 = a20 * a31 - a21 * a30;
        let b07 = a20 * a32 - a22 * a30;
        let b08 = a20 * a33 - a23 * a30;
        let b09 = a21 * a32 - a22 * a31;
        let b10 = a21 * a33 - a23 * a31;
        let b11 = a22 * a33 - a23 * a32;
        b00 * b11 - b01 * b10 + b02 * b09 + b03 * b08 - b04 * b07 + b05 * b06
    }

    /// A right-handed perspective projection. `fov_y` is the vertical field of
    /// view in **radians**; `aspect` is width / height. Maps to the OpenGL-style
    /// `[-1, 1]` clip depth range.
    pub fn perspective(fov_y: f32, aspect: f32, near: f32, far: f32) -> Mat4 {
        let f = 1.0 / (fov_y * 0.5).tan();
        let nf = 1.0 / (near - far);
        Mat4 {
            v: [
                f / aspect,
                0.0,
                0.0,
                0.0,
                0.0,
                f,
                0.0,
                0.0,
                0.0,
                0.0,
                (far + near) * nf,
                -1.0,
                0.0,
                0.0,
                (2.0 * far * near) * nf,
                0.0,
            ],
        }
    }

    /// An orthographic projection mapping the box `[left, right] × [bottom, top]
    /// × [near, far]` to the `[-1, 1]` clip cube.
    pub fn ortho(left: f32, right: f32, bottom: f32, top: f32, near: f32, far: f32) -> Mat4 {
        let lr = 1.0 / (left - right);
        let bt = 1.0 / (bottom - top);
        let nf = 1.0 / (near - far);
        Mat4 {
            v: [
                -2.0 * lr,
                0.0,
                0.0,
                0.0,
                0.0,
                -2.0 * bt,
                0.0,
                0.0,
                0.0,
                0.0,
                2.0 * nf,
                0.0,
                (left + right) * lr,
                (top + bottom) * bt,
                (far + near) * nf,
                1.0,
            ],
        }
    }

    /// A right-handed view matrix looking from `eye` toward `center`, with the
    /// given `up` direction.
    pub fn look_at(eye: Vec3, center: Vec3, up: Vec3) -> Mat4 {
        let forward = (center - eye).normalize();
        let side = forward.cross(up).normalize();
        let up = side.cross(forward);
        Mat4 {
            v: [
                side.x,
                up.x,
                -forward.x,
                0.0,
                side.y,
                up.y,
                -forward.y,
                0.0,
                side.z,
                up.z,
                -forward.z,
                0.0,
                -side.dot(eye),
                -up.dot(eye),
                forward.dot(eye),
                1.0,
            ],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vec::{vec2, vec3, vec4};

    fn approx_mat4(a: &Mat4, b: &Mat4, eps: f32) -> bool {
        a.v.iter().zip(b.v.iter()).all(|(x, y)| (x - y).abs() < eps)
    }

    #[test]
    fn identity_is_multiplicative_unit() {
        let m = Mat4::from_translation(vec3(3.0, 4.0, 5.0));
        assert_eq!(m.mul(&Mat4::IDENTITY), m);
        assert_eq!(Mat4::IDENTITY.mul(&m), m);
    }

    #[test]
    fn translation_moves_a_point() {
        let m = Mat4::from_translation(vec3(1.0, 2.0, 3.0));
        assert_eq!(
            m.transform_point(vec3(10.0, 20.0, 30.0)),
            vec3(11.0, 22.0, 33.0)
        );
    }

    #[test]
    fn mat4_multiplication_is_associative() {
        let a = Mat4::from_translation(vec3(1.0, 0.0, 0.0));
        let b = Mat4::from_scale(vec3(2.0, 3.0, 4.0));
        let c = Mat4::from_translation(vec3(0.0, 5.0, 0.0));
        let lhs = a.mul(&b).mul(&c);
        let rhs = a.mul(&b.mul(&c));
        assert!(approx_mat4(&lhs, &rhs, 1e-5));
    }

    #[test]
    fn compose_order_matches_apply_order() {
        // (T * S) applied to a point should scale first, then translate.
        let t = Mat4::from_translation(vec3(10.0, 0.0, 0.0));
        let s = Mat4::from_scale(vec3(2.0, 1.0, 1.0));
        let ts = t.mul(&s);
        assert_eq!(
            ts.transform_point(vec3(3.0, 0.0, 0.0)),
            vec3(16.0, 0.0, 0.0)
        );
    }

    #[test]
    fn invert_round_trips_to_identity() {
        let m =
            Mat4::from_translation(vec3(1.0, 2.0, 3.0)).mul(&Mat4::from_scale(vec3(2.0, 4.0, 8.0)));
        let prod = m.mul(&m.invert());
        assert!(approx_mat4(&prod, &Mat4::IDENTITY, 1e-5));
    }

    #[test]
    fn singular_matrix_inverts_to_identity() {
        let zero = Mat4::from_cols_array([0.0; 16]);
        assert_eq!(zero.invert(), Mat4::IDENTITY);
    }

    #[test]
    fn transpose_is_involutive() {
        let m = Mat4::from_cols_array([
            1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0, 11.0, 12.0, 13.0, 14.0, 15.0, 16.0,
        ]);
        assert_eq!(m.transpose().transpose(), m);
    }

    #[test]
    fn mat2_rotation_turns_x_into_y() {
        let r = Mat2::from_angle(core::f32::consts::FRAC_PI_2);
        let out = r.transform_vec2(vec2(1.0, 0.0));
        assert!((out.x - 0.0).abs() < 1e-6);
        assert!((out.y - 1.0).abs() < 1e-6);
    }

    #[test]
    fn mat3_identity_and_mul() {
        let m = Mat3::from_cols_array([2.0, 0.0, 0.0, 0.0, 3.0, 0.0, 0.0, 0.0, 4.0]);
        assert_eq!(m.mul(&Mat3::IDENTITY), m);
        assert_eq!(m.transform_vec3(vec3(1.0, 1.0, 1.0)), vec3(2.0, 3.0, 4.0));
    }

    #[test]
    fn perspective_keeps_forward_axis_on_negative_z() {
        // A point straight ahead projects to the center of the screen.
        let p = Mat4::perspective(core::f32::consts::FRAC_PI_2, 1.0, 0.1, 100.0);
        let clip = p.transform_vec4(vec4(0.0, 0.0, -1.0, 1.0));
        assert!(clip.x.abs() < 1e-6 && clip.y.abs() < 1e-6);
    }
}
