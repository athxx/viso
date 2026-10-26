//! The interface of the 3D scene built-in ([`BuiltinShader::Scene`]):
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

/// How many uniform buffers a scene draw's bind group holds: the frame's
/// block, the material's and the skin's, in that order.
pub const UNIFORM_SLOTS: usize = 3;

/// How many textures a scene draw's bind group holds, after its uniform
/// buffers: the material's base color, metallic-roughness, normal, occlusion
/// and emission maps, then the environment, the shadow atlas and the ambient
/// occlusion, in that order. A pass over the scene's own images binds them
/// in the slots its program names.
pub const TEXTURE_SLOTS: usize = 8;

/// How many samplers a scene draw's bind group holds, after its textures:
/// the material's, a linear clamp-to-edge and a nearest clamp-to-edge one.
pub const SAMPLER_SLOTS: usize = 3;

/// The sampler slot each texture slot is sampled through. A backend whose
/// texture unit carries its sampler state (GLES) binds this one with each
/// texture; the programs sample no texture through another.
pub const TEXTURE_SAMPLERS: [u8; TEXTURE_SLOTS] = [0, 0, 0, 0, 0, 1, 2, 1];

/// The bindings of the scene slots in the one descriptor set of a native
/// (Vulkan) device, beside the 2D programs' `tex` (0), `dst_tex` (1),
/// sampler (2) and uniform buffer (3), which slot 0 of each kind reuses.
pub const NATIVE_TEXTURE_BINDINGS: [u32; TEXTURE_SLOTS] = [0, 1, 4, 5, 6, 7, 8, 9];
pub const NATIVE_SAMPLER_BINDINGS: [u32; SAMPLER_SLOTS] = [2, 10, 11];
pub const NATIVE_UNIFORM_BINDINGS: [u32; UNIFORM_SLOTS] = [3, 12, 13];

/// A program of the scene lane. Every one is drawn as a
/// [`Geometry::InstancedMesh`] whose vertices are the program's
/// [`vertex_schema`](Self::vertex_schema) and whose instances are
/// [`SceneInstance`]s, with a bind group of [`UNIFORM_SLOTS`] uniform
/// buffers, [`TEXTURE_SLOTS`] textures and [`SAMPLER_SLOTS`] samplers in that
/// order; a pipeline names its program in the low byte of its `variant`.
///
/// [`Geometry::InstancedMesh`]: https://docs.rs/viso-gpu
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u8)]
pub enum SceneProgram {
    /// A mesh in its material: metallic-roughness shading lit by the
    /// directional light and the environment, or unlit, or a sprite's soft
    /// disc; tone mapped and fogged.
    Mesh = 0,
}

impl SceneProgram {
    /// Every program, by its code.
    pub const ALL: [SceneProgram; 1] = [SceneProgram::Mesh];

    /// The vertex layout it reads.
    pub const fn vertex_schema(self) -> InstanceSchema {
        match self {
            SceneProgram::Mesh => VERTEX_SCHEMA,
        }
    }

    /// The program a pipeline's `variant` names.
    pub fn of_variant(variant: u32) -> Option<SceneProgram> {
        Self::ALL.get((variant & 0xff) as usize).copied()
    }
}

/// The vertex attributes of a scene mesh, [`SceneVertex`]'s fields: the
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
        SchemaAttr {
            name: "tangent",
            format: AttrFormat::Float4,
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

/// One vertex of a scene mesh, in the mesh's own space: position, unit
/// normal, texture coordinate, and the unit tangent along `+u` with the
/// bitangent's handedness in `w` (`cross(normal, tangent) * w` points along
/// `-v`, up the image). Only a normal-mapped material reads the tangent.
/// 48 bytes.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
#[repr(C)]
pub struct SceneVertex {
    pub position: [f32; 3],
    pub normal: [f32; 3],
    pub uv: [f32; 2],
    pub tangent: [f32; 4],
}

/// One instance of a scene mesh: the affine from the mesh's space to the
/// world, as three rows `[m00 m01 m02 tx]`, and its tint, premultiplied
/// linear light, which multiplies its material's color. 64 bytes.
///
/// Normals are carried to the world through the cofactor of the affine's
/// linear part, so a non-uniform scale (a box's half extents) keeps faces
/// lit by their true orientation; tangents by the linear part itself.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
#[repr(C)]
pub struct SceneInstance {
    pub rows: [[f32; 4]; 3],
    pub color: [f32; 4],
}

/// The frame's uniform block, uniform slot 0 of every scene draw. 288 bytes.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
#[repr(C)]
pub struct FrameUniforms {
    /// World to clip space, column-major.
    pub view_proj: [f32; 16],
    /// The camera's position; `w` the exposure lit colors are scaled by
    /// before tone mapping.
    pub camera: [f32; 4],
    /// The unit direction toward the light; `w` its intensity: a white
    /// diffuse surface facing a light of intensity 1 reflects its color.
    pub light_dir: [f32; 4],
    /// The light's color, linear.
    pub light_color: [f32; 4],
    /// The fog's color, linear, and in `w` its density per unit of distance
    /// (`0.0`: no fog).
    pub fog: [f32; 4],
    /// How strongly the environment lights: `x` its diffuse light, `y` its
    /// reflections.
    pub ambient: [f32; 4],
    /// The environment's diffuse irradiance over π as nine spherical
    /// harmonic terms, each scaled so the irradiance at unit normal `n` is
    /// [`sh_irradiance`]`(sh, n)`.
    pub sh: [[f32; 4]; 9],
}

/// A material's uniform block, uniform slot 1 of a scene draw. 64 bytes.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
#[repr(C)]
pub struct MaterialUniforms {
    /// Linear RGBA, straight alpha; multiplies the base color texture.
    pub base_color: [f32; 4],
    /// Linear RGB emission; multiplies the emission texture.
    pub emissive: [f32; 4],
    /// Metalness, roughness, the normal map's scale and the occlusion
    /// map's strength; the first two multiply the metallic-roughness
    /// texture's blue and green.
    pub params: [f32; 4],
    /// The [`Shading`] code, the [`Alpha`] code, the alpha cutoff of a
    /// masked material, and the [`MATERIAL_NORMAL_MAP`] and
    /// [`MATERIAL_DOUBLE_SIDED`] flags.
    pub mode: [f32; 4],
}

/// How a material colors its fragments: [`MaterialUniforms::mode`]`[0]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u8)]
pub enum Shading {
    /// Metallic-roughness shading, lit by the light and the environment.
    Lit = 0,
    /// Its color as is, fogged: debug geometry and glowing surfaces.
    Unlit = 1,
    /// A soft disc across the mesh's texture coordinates, unlit and fogged:
    /// a particle on a camera-facing quad.
    Sprite = 2,
}

/// How a material's alpha covers what is behind it:
/// [`MaterialUniforms::mode`]`[1]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u8)]
pub enum Alpha {
    /// Opaque; the instance's alpha is its coverage of the scene's target.
    Opaque = 0,
    /// Opaque where the base color's alpha reaches the cutoff, absent
    /// elsewhere.
    Mask = 1,
    /// Blended over what is behind it by the base color's alpha.
    Blend = 2,
}

/// [`MaterialUniforms::mode`]`[3]` flag: the material samples a normal map.
pub const MATERIAL_NORMAL_MAP: u32 = 1;
/// [`MaterialUniforms::mode`]`[3]` flag: a back face is lit by its flipped
/// normal.
pub const MATERIAL_DOUBLE_SIDED: u32 = 2;

/// The width of the environment atlas, uniform texture slot 5: the
/// prefiltered radiance of every direction, an equirectangular map per
/// roughness level, the level `k` map `ENV_WIDTH >> k` by
/// `ENV_WIDTH >> (k + 1)` texels at row `ENV_WIDTH - (ENV_WIDTH >> k)`.
pub const ENV_WIDTH: u32 = 256;
/// The atlas's height.
pub const ENV_HEIGHT: u32 = 256;
/// How many roughness levels the atlas holds: level `k` is the radiance
/// seen by a surface of roughness `k / (ENV_LEVELS - 1)`.
pub const ENV_LEVELS: u32 = 6;

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

as_bytes!(SceneVertex, 48);
as_bytes!(SceneInstance, 64);
as_bytes!(FrameUniforms, 288);
as_bytes!(MaterialUniforms, 64);

impl SceneInstance {
    /// `p`, a point of the mesh, in the world.
    #[inline]
    pub fn place(&self, p: [f32; 3]) -> [f32; 3] {
        let r = &self.rows;
        core::array::from_fn(|i| r[i][0] * p[0] + r[i][1] * p[1] + r[i][2] * p[2] + r[i][3])
    }

    /// Whether the linear part mirrors: its determinant is negative.
    #[inline]
    pub fn mirrors(&self) -> bool {
        let r = &self.rows;
        let col = |j: usize| [r[0][j], r[1][j], r[2][j]];
        dot(col(0), cross(col(1), col(2))) < 0.0
    }

    /// `n`, a normal of the mesh, in the world, not normalized: the cofactor
    /// of the linear part applied to it, turned back out by a mirroring
    /// instance.
    #[inline]
    pub fn turn_normal(&self, n: [f32; 3]) -> [f32; 3] {
        let r = &self.rows;
        let col = |j: usize| [r[0][j], r[1][j], r[2][j]];
        let (c0, c1, c2) = (col(0), col(1), col(2));
        let (k0, k1, k2) = (cross(c1, c2), cross(c2, c0), cross(c0, c1));
        let side = if self.mirrors() { -1.0 } else { 1.0 };
        core::array::from_fn(|i| (k0[i] * n[0] + k1[i] * n[1] + k2[i] * n[2]) * side)
    }

    /// `t`, a tangent of the mesh with its handedness in `w`, in the world:
    /// the linear part applied to it, the handedness flipped by a mirroring
    /// instance.
    #[inline]
    pub fn turn_tangent(&self, t: [f32; 4]) -> [f32; 4] {
        let r = &self.rows;
        let side = if self.mirrors() { -1.0 } else { 1.0 };
        [
            r[0][0] * t[0] + r[0][1] * t[1] + r[0][2] * t[2],
            r[1][0] * t[0] + r[1][1] * t[1] + r[1][2] * t[2],
            r[2][0] * t[0] + r[2][1] * t[1] + r[2][2] * t[2],
            t[3] * side,
        ]
    }
}

impl FrameUniforms {
    /// `p`, a world point, in clip space.
    #[inline]
    pub fn clip(&self, p: [f32; 3]) -> [f32; 4] {
        let m = &self.view_proj;
        core::array::from_fn(|i| m[i] * p[0] + m[4 + i] * p[1] + m[8 + i] * p[2] + m[12 + i])
    }
}

impl MaterialUniforms {
    /// Its shading.
    #[inline]
    pub fn shading(&self) -> Shading {
        let code = self.mode[0];
        if code > 1.5 {
            Shading::Sprite
        } else if code > 0.5 {
            Shading::Unlit
        } else {
            Shading::Lit
        }
    }

    /// Its alpha mode.
    #[inline]
    pub fn alpha(&self) -> Alpha {
        let code = self.mode[1];
        if code > 1.5 {
            Alpha::Blend
        } else if code > 0.5 {
            Alpha::Mask
        } else {
            Alpha::Opaque
        }
    }

    /// Its flags.
    #[inline]
    pub fn flags(&self) -> u32 {
        self.mode[3] as u32
    }
}

mod shade;

pub use shade::{
    MeshFragment, SceneTextures, disc, env_atlas_uv, env_brdf, env_direction_uv, sh_irradiance,
    shade_mesh, tonemap,
};

#[inline]
pub(crate) fn add(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

#[inline]
pub(crate) fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

#[inline]
pub(crate) fn scale(a: [f32; 3], s: f32) -> [f32; 3] {
    [a[0] * s, a[1] * s, a[2] * s]
}

#[inline]
pub(crate) fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

#[inline]
pub(crate) fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

#[inline]
pub(crate) fn length(a: [f32; 3]) -> f32 {
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
        let schema_bytes =
            |s: InstanceSchema| s.attributes.iter().map(|a| a.format.size()).sum::<usize>();
        assert_eq!(schema_bytes(VERTEX_SCHEMA), SceneVertex::SIZE);
        assert_eq!(schema_bytes(INSTANCE_SCHEMA), SceneInstance::SIZE);
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

    /// A mirrored instance keeps its normals pointing out of the surface
    /// and flips its bitangents' handedness.
    #[test]
    fn a_mirror_keeps_normals_outward() {
        let mirror = SceneInstance {
            rows: [
                [-1.0, 0.0, 0.0, 0.0],
                [0.0, 1.0, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0],
            ],
            color: [1.0; 4],
        };
        assert!(mirror.mirrors());
        assert_eq!(mirror.turn_normal([1.0, 0.0, 0.0]), [-1.0, 0.0, 0.0]);
        assert_eq!(
            mirror.turn_tangent([0.0, 0.0, 1.0, 1.0]),
            [0.0, 0.0, 1.0, -1.0]
        );
    }
}
