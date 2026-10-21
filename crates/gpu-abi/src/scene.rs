//! The interface of the 3D scene built-in ([`BuiltinShader::Scene`], ADR 0047):
//! the vertex, instance and uniform layouts the renderer writes, every backend's
//! scene program reads, and the CPU raster reproduces.
//!
//! A scene draw is a [`Geometry::InstancedMesh`]: vertex `i` of the mesh is
//! `vertices[i]` and instance `j` is `instances[j]`, both read by index in the
//! vertex stage; the uniform block is a uniform buffer the draw's bind group
//! holds. Positions are in a right-handed world, `y` up. The camera projects
//! with reversed Z (near at depth 1, far at 0), so the depth attachment clears
//! to `0.0` and a nearer fragment passes `Greater`.
//!
//! [`BuiltinShader::Scene`]: crate::BuiltinShader::Scene
//! [`Geometry::InstancedMesh`]: https://docs.rs/viso-gpu

use crate::instance::{AttrFormat, InstanceSchema, SchemaAttr};

/// The vertex attributes of a scene draw, [`SceneVertex`]'s fields: the
/// pipeline's `instance_schema` (its vertex-rate stream).
pub const VERTEX_SCHEMA: InstanceSchema = InstanceSchema {
    attributes: &[
        SchemaAttr {
            name: "position",
            format: AttrFormat::Float3,
        },
        SchemaAttr {
            name: "normal",
            format: AttrFormat::Float3,
        },
        SchemaAttr {
            name: "uv",
            format: AttrFormat::Float2,
        },
    ],
};

/// The instance attributes of a scene draw, [`SceneInstance`]'s fields
/// (`rows` as three `Float4`s): its instance-rate stream, at the locations
/// after the vertex attributes.
pub const INSTANCE_SCHEMA: InstanceSchema = InstanceSchema {
    attributes: &[
        SchemaAttr {
            name: "row0",
            format: AttrFormat::Float4,
        },
        SchemaAttr {
            name: "row1",
            format: AttrFormat::Float4,
        },
        SchemaAttr {
            name: "row2",
            format: AttrFormat::Float4,
        },
        SchemaAttr {
            name: "color",
            format: AttrFormat::Float4,
        },
    ],
};

/// One vertex of a scene mesh: position, normal and texture coordinate, in
/// the mesh's own space. 32 bytes.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
#[repr(C)]
pub struct SceneVertex {
    pub position: [f32; 3],
    pub normal: [f32; 3],
    pub uv: [f32; 2],
}

/// One instance of a scene mesh: the affine from the mesh's space to the
/// world, as three rows `[m00 m01 m02 tx]`, and its color, premultiplied in
/// the target's encoding. 64 bytes.
///
/// The normal is carried to the world through the cofactor of the affine's
/// linear part, so a non-uniform scale (a box's half extents) keeps faces
/// lit by their true orientation.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
#[repr(C)]
pub struct SceneInstance {
    pub rows: [[f32; 4]; 3],
    pub color: [f32; 4],
}

/// The uniform block of a scene draw. 160 bytes.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
#[repr(C)]
pub struct SceneUniforms {
    /// World to clip space, column-major.
    pub view_proj: [f32; 16],
    /// The camera's position; `w` unused.
    pub camera: [f32; 4],
    /// The unit direction toward the light; `w` its intensity.
    pub light_dir: [f32; 4],
    /// The light's color, linear.
    pub light_color: [f32; 4],
    /// The ambient light from above and from below, linear.
    pub sky: [f32; 4],
    pub ground: [f32; 4],
    /// The fog's color, linear, and in `w` its density per unit of distance
    /// (`0.0`: no fog).
    pub fog: [f32; 4],
}

macro_rules! as_bytes {
    ($ty:ty, $size:expr) => {
        impl $ty {
            /// Its size in bytes, as the shaders read it.
            pub const SIZE: usize = $size;

            /// Its bytes, little-endian `f32`s in field order.
            pub fn write_to(&self, out: &mut [u8]) {
                const _: () = assert!(core::mem::size_of::<$ty>() == $size);
                let floats: &[f32] = {
                    // SAFETY: the type is `repr(C)` of `f32` fields and arrays
                    // only (asserted size, no padding), so it is a valid
                    // `[f32; SIZE / 4]`.
                    unsafe {
                        core::slice::from_raw_parts((self as *const $ty).cast::<f32>(), $size / 4)
                    }
                };
                for (chunk, f) in out[..$size].chunks_exact_mut(4).zip(floats) {
                    chunk.copy_from_slice(&f.to_le_bytes());
                }
            }

            /// It, read back from `bytes` as [`write_to`](Self::write_to)
            /// wrote it.
            pub fn read_from(bytes: &[u8]) -> Self {
                let mut floats = [0.0f32; $size / 4];
                for (f, chunk) in floats.iter_mut().zip(bytes[..$size].chunks_exact(4)) {
                    *f = f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                }
                // SAFETY: as in `write_to`: every bit pattern of the floats is
                // a valid value of the type.
                unsafe { core::mem::transmute::<[f32; $size / 4], $ty>(floats) }
            }
        }
    };
}

as_bytes!(SceneVertex, 32);
as_bytes!(SceneInstance, 64);
as_bytes!(SceneUniforms, 160);

impl SceneInstance {
    /// `p`, a point of the mesh, in the world.
    #[inline]
    pub fn place(&self, p: [f32; 3]) -> [f32; 3] {
        let r = &self.rows;
        core::array::from_fn(|i| r[i][0] * p[0] + r[i][1] * p[1] + r[i][2] * p[2] + r[i][3])
    }

    /// `n`, a normal of the mesh, in the world, not normalized: the cofactor
    /// of the linear part applied to it.
    #[inline]
    pub fn turn_normal(&self, n: [f32; 3]) -> [f32; 3] {
        let r = &self.rows;
        let col = |j: usize| [r[0][j], r[1][j], r[2][j]];
        let (c0, c1, c2) = (col(0), col(1), col(2));
        let cross = |a: [f32; 3], b: [f32; 3]| {
            [
                a[1] * b[2] - a[2] * b[1],
                a[2] * b[0] - a[0] * b[2],
                a[0] * b[1] - a[1] * b[0],
            ]
        };
        let (k0, k1, k2) = (cross(c1, c2), cross(c2, c0), cross(c0, c1));
        core::array::from_fn(|i| k0[i] * n[0] + k1[i] * n[1] + k2[i] * n[2])
    }
}

impl SceneUniforms {
    /// `p`, a world point, in clip space.
    #[inline]
    pub fn clip(&self, p: [f32; 3]) -> [f32; 4] {
        let m = &self.view_proj;
        core::array::from_fn(|i| m[i] * p[0] + m[4 + i] * p[1] + m[8 + i] * p[2] + m[12 + i])
    }

    /// The color of a surface point at `world` with the unit normal `normal`
    /// and the instance color `color` (premultiplied, the target's encoding):
    /// hemispheric ambient, Lambert diffuse and Blinn-Phong specular from the
    /// light, then fog, all in linear light, encoded back for the target.
    ///
    /// The scene programs of every backend compute the same expression.
    pub fn shade(&self, world: [f32; 3], normal: [f32; 3], color: [f32; 4]) -> [f32; 4] {
        let a = color[3];
        if a <= 0.0 {
            return [0.0; 4];
        }
        let base: [f32; 3] = core::array::from_fn(|i| decode(color[i] / a));
        let l = [self.light_dir[0], self.light_dir[1], self.light_dir[2]];
        let intensity = self.light_dir[3];
        let to_eye = sub([self.camera[0], self.camera[1], self.camera[2]], world);
        let distance = length(to_eye);
        let v = scale(to_eye, 1.0 / distance.max(1e-6));
        let n = normal;
        let up = n[1] * 0.5 + 0.5;
        let diffuse = dot(n, l).max(0.0) * intensity;
        let h = normalize(add(l, v));
        let specular = if diffuse > 0.0 {
            dot(n, h).max(0.0).powf(32.0) * 0.25 * intensity
        } else {
            0.0
        };
        let fog = 1.0 - (-self.fog[3] * distance).exp();
        let lit: [f32; 3] = core::array::from_fn(|i| {
            let ambient = self.ground[i] + (self.sky[i] - self.ground[i]) * up;
            let c = base[i] * (ambient + diffuse * self.light_color[i])
                + specular * self.light_color[i];
            c + (self.fog[i] - c) * fog
        });
        [
            encode(lit[0]) * a,
            encode(lit[1]) * a,
            encode(lit[2]) * a,
            a,
        ]
    }
}

/// An encoded channel in linear light: the 2.2 power the scene programs use.
#[inline]
pub fn decode(c: f32) -> f32 {
    c.max(0.0).powf(2.2)
}

/// A linear channel encoded for the target, clamped to `[0, 1]`.
#[inline]
pub fn encode(c: f32) -> f32 {
    c.clamp(0.0, 1.0).powf(1.0 / 2.2)
}

#[inline]
fn add(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

#[inline]
fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

#[inline]
fn scale(a: [f32; 3], s: f32) -> [f32; 3] {
    [a[0] * s, a[1] * s, a[2] * s]
}

#[inline]
fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

#[inline]
fn length(a: [f32; 3]) -> f32 {
    dot(a, a).sqrt()
}

/// `a` scaled to unit length; the zero vector stays zero.
#[inline]
pub fn normalize(a: [f32; 3]) -> [f32; 3] {
    let len = length(a);
    if len > 0.0 { scale(a, 1.0 / len) } else { a }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_layouts_round_trip_through_their_bytes() {
        let instance = SceneInstance {
            rows: [
                [1.0, 2.0, 3.0, 4.0],
                [5.0, 6.0, 7.0, 8.0],
                [9.0, 10.0, 11.0, 12.0],
            ],
            color: [0.1, 0.2, 0.3, 1.0],
        };
        let mut bytes = [0u8; SceneInstance::SIZE];
        instance.write_to(&mut bytes);
        assert_eq!(SceneInstance::read_from(&bytes), instance);
        assert_eq!(&bytes[12..16], &4.0f32.to_le_bytes(), "row 0's translation");
    }

    /// A box scaled flat keeps its top face's normal pointing up.
    #[test]
    fn a_normal_follows_the_inverse_transpose() {
        let flat = SceneInstance {
            rows: [
                [4.0, 0.0, 0.0, 0.0],
                [0.0, 0.5, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0],
            ],
            color: [1.0; 4],
        };
        let tilted = normalize(flat.turn_normal(normalize([1.0, 1.0, 0.0])));
        // Squashed in y and stretched in x, the slope's normal turns toward y.
        assert!(tilted[1] > 0.99, "{tilted:?}");
    }

    #[test]
    fn a_face_toward_the_light_is_brighter_than_one_away() {
        let uniforms = SceneUniforms {
            camera: [0.0, 0.0, 5.0, 0.0],
            light_dir: [0.0, 1.0, 0.0, 1.0],
            light_color: [1.0; 4],
            sky: [0.2; 4],
            ground: [0.05; 4],
            ..SceneUniforms::default()
        };
        let color = [0.5, 0.5, 0.5, 1.0];
        let lit = uniforms.shade([0.0; 3], [0.0, 1.0, 0.0], color);
        let unlit = uniforms.shade([0.0; 3], [0.0, -1.0, 0.0], color);
        assert!(lit[0] > unlit[0] * 2.0, "{lit:?} {unlit:?}");
        assert_eq!(lit[3], 1.0);
    }
}
