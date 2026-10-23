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
    /// The camera's position; `w` the [`SceneMaterial`] of the draws.
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

/// How a scene draw colors its fragments: what its uniforms' `camera.w`
/// holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u8)]
pub enum SceneMaterial {
    /// Lit by the scene's light and ambient, then fogged.
    Lit = 0,
    /// Its color as is, fogged: debug geometry and glowing surfaces.
    Unlit = 1,
    /// A soft disc across the mesh's texture coordinates, unlit and fogged:
    /// a particle on a camera-facing quad, blended over what is behind it.
    Sprite = 2,
}

impl SceneMaterial {
    /// The value of `camera.w` that selects it.
    #[inline]
    pub const fn code(self) -> f32 {
        self as u8 as f32
    }

    /// The material `camera.w` selects.
    #[inline]
    pub fn of(code: f32) -> SceneMaterial {
        if code > 1.5 {
            SceneMaterial::Sprite
        } else if code > 0.5 {
            SceneMaterial::Unlit
        } else {
            SceneMaterial::Lit
        }
    }
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

    /// The color of a surface point at `world` with the unit normal `normal`,
    /// the texture coordinate `uv` and the instance color `color`
    /// (premultiplied linear light), by the uniforms' material:
    /// - lit: hemispheric ambient, Lambert diffuse and Blinn-Phong specular
    ///   from the light;
    /// - unlit: the color as is;
    /// - sprite: the color as is, covering a disc across `uv` that fades
    ///   smoothly to its rim;
    ///
    /// then fog, all in linear light, clamped to `[0, 1]` and premultiplied:
    /// the target's sRGB format encodes it.
    ///
    /// The scene programs of every backend compute the same expression.
    pub fn shade(
        &self,
        world: [f32; 3],
        normal: [f32; 3],
        uv: [f32; 2],
        color: [f32; 4],
    ) -> [f32; 4] {
        let mut a = color[3];
        if a <= 0.0 {
            return [0.0; 4];
        }
        let base: [f32; 3] = core::array::from_fn(|i| (color[i] / a).max(0.0));
        let to_eye = sub([self.camera[0], self.camera[1], self.camera[2]], world);
        let distance = length(to_eye);
        let fog = 1.0 - (-self.fog[3] * distance).exp();
        let material = SceneMaterial::of(self.camera[3]);
        let c: [f32; 3] = if material == SceneMaterial::Lit {
            let l = [self.light_dir[0], self.light_dir[1], self.light_dir[2]];
            let intensity = self.light_dir[3];
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
            core::array::from_fn(|i| {
                let ambient = self.ground[i] + (self.sky[i] - self.ground[i]) * up;
                base[i] * (ambient + diffuse * self.light_color[i]) + specular * self.light_color[i]
            })
        } else {
            if material == SceneMaterial::Sprite {
                a *= disc(uv);
                if a <= 0.0 {
                    return [0.0; 4];
                }
            }
            base
        };
        let lit: [f32; 3] =
            core::array::from_fn(|i| (c[i] + (self.fog[i] - c[i]) * fog).clamp(0.0, 1.0));
        [lit[0] * a, lit[1] * a, lit[2] * a, a]
    }
}

/// How much of a sprite covers `uv`: 1 at the centre of the unit square,
/// falling smoothly to 0 at the inscribed circle.
#[inline]
pub fn disc(uv: [f32; 2]) -> f32 {
    let (x, y) = (uv[0] * 2.0 - 1.0, uv[1] * 2.0 - 1.0);
    let t = (1.0 - (x * x + y * y).sqrt()).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
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
        let lit = uniforms.shade([0.0; 3], [0.0, 1.0, 0.0], [0.5; 2], color);
        let unlit = uniforms.shade([0.0; 3], [0.0, -1.0, 0.0], [0.5; 2], color);
        assert!(lit[0] > unlit[0] * 2.0, "{lit:?} {unlit:?}");
        assert_eq!(lit[3], 1.0);
    }

    #[test]
    fn unlit_keeps_its_color_and_a_sprite_fades_to_its_rim() {
        let mut uniforms = SceneUniforms {
            camera: [0.0, 0.0, 5.0, SceneMaterial::Unlit.code()],
            light_dir: [0.0, 1.0, 0.0, 1.0],
            ..SceneUniforms::default()
        };
        let color = [0.5, 0.25, 0.75, 1.0];
        let shaded = |u: &SceneUniforms, normal, uv| u.shade([0.0; 3], normal, uv, color);
        let up = shaded(&uniforms, [0.0, 1.0, 0.0], [0.5; 2]);
        assert_eq!(up, shaded(&uniforms, [0.0, -1.0, 0.0], [0.5; 2]));
        for (a, b) in up.iter().zip(color) {
            assert!((a - b).abs() < 1e-5, "{up:?}");
        }

        uniforms.camera[3] = SceneMaterial::Sprite.code();
        let centre = shaded(&uniforms, [0.0; 3], [0.5, 0.5]);
        let halfway = shaded(&uniforms, [0.0; 3], [0.75, 0.5]);
        let rim = shaded(&uniforms, [0.0; 3], [1.0, 0.5]);
        assert!((centre[3] - 1.0).abs() < 1e-6);
        assert!((halfway[3] - 0.5).abs() < 1e-6, "{halfway:?}");
        assert_eq!(rim, [0.0; 4]);
        assert_eq!(
            shaded(&uniforms, [0.0; 3], [0.0, 0.0]),
            [0.0; 4],
            "a corner"
        );
        assert!(
            (halfway[0] / halfway[3] - color[0]).abs() < 1e-5,
            "premultiplied"
        );
    }
}
