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
/// block, the material's and the skin's joint palette, in that order.
pub const UNIFORM_SLOTS: usize = 3;

/// How many joints a skinned mesh's palette holds at most: uniform slot 2
/// of a skinned program's draw, three rows of a joint's affine each,
/// `[m00 m01 m02 tx]`, `MAX_JOINTS * 48` bytes.
pub const MAX_JOINTS: usize = 256;

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
    /// A mesh's distance from the light, packed by [`pack_depth`] into an
    /// RGBA8 target: the shadow caster pass. A masked material discards
    /// what its mask cuts away; the frame's `view_proj` maps the world into
    /// the cascade's tile of the shadow atlas, with reversed depth.
    Depth = 1,
    /// [`Mesh`](Self::Mesh), its vertices [`SkinnedVertex`]es moved by
    /// their joints first ([`skin_vertex`]). Its joints may mirror some
    /// triangles and not others, so it discards a one-sided material's
    /// fragments that [`faces_away`] itself; a pipeline may cull too.
    SkinnedMesh = 2,
    /// [`Depth`](Self::Depth) of a skinned mesh.
    SkinnedDepth = 3,
    /// The sky where no mesh is drawn: a triangle over the whole target,
    /// its vertices' positions in clip space at the far plane, whose
    /// fragments see [`sky_radiance`] along their view ray, exposed and tone
    /// mapped as a mesh is. Uniform slot 1 holds the [`SkyUniforms`].
    Sky = 4,
    /// The ambient occlusion of every pixel of a view, a triangle over the
    /// target ([`shade_ao`]): taps of a hemisphere about the surface the
    /// view's depth (texture slot 6, [`pack_depth`]ed `1 - z`) holds there,
    /// each occluded by what stands in front of it. Uniform slot 1 holds
    /// the [`AoUniforms`].
    Ao = 5,
    /// One axis of the depth-aware blur of [`Ao`](Self::Ao)'s output
    /// (texture slot 6), [`shade_ao_blur`].
    AoBlur = 6,
    /// [`Mesh`](Self::Mesh) of a mesh whose vertices each carry a
    /// [`VertexExtra`]: a second texture coordinate set and a color.
    ExtraMesh = 7,
    /// [`Depth`](Self::Depth) of a mesh whose vertices carry
    /// [`VertexExtra`]s.
    ExtraDepth = 8,
    /// [`SkinnedMesh`](Self::SkinnedMesh) of a skinned mesh whose vertices
    /// carry [`VertexExtra`]s.
    ExtraSkinnedMesh = 9,
    /// [`SkinnedDepth`](Self::SkinnedDepth) of a skinned mesh whose
    /// vertices carry [`VertexExtra`]s.
    ExtraSkinnedDepth = 10,
}

impl SceneProgram {
    /// Every program, by its code.
    pub const ALL: [SceneProgram; 11] = [
        SceneProgram::Mesh,
        SceneProgram::Depth,
        SceneProgram::SkinnedMesh,
        SceneProgram::SkinnedDepth,
        SceneProgram::Sky,
        SceneProgram::Ao,
        SceneProgram::AoBlur,
        SceneProgram::ExtraMesh,
        SceneProgram::ExtraDepth,
        SceneProgram::ExtraSkinnedMesh,
        SceneProgram::ExtraSkinnedDepth,
    ];

    /// The vertex layout it reads.
    pub const fn vertex_schema(self) -> InstanceSchema {
        match self {
            SceneProgram::SkinnedMesh | SceneProgram::SkinnedDepth => SKINNED_VERTEX_SCHEMA,
            SceneProgram::ExtraMesh | SceneProgram::ExtraDepth => EXTRA_VERTEX_SCHEMA,
            SceneProgram::ExtraSkinnedMesh | SceneProgram::ExtraSkinnedDepth => {
                EXTRA_SKINNED_VERTEX_SCHEMA
            }
            _ => VERTEX_SCHEMA,
        }
    }

    /// The bytes of one of its vertices.
    pub const fn vertex_size(self) -> usize {
        let base = if self.skinned() {
            SkinnedVertex::SIZE
        } else {
            SceneVertex::SIZE
        };
        if self.extra() {
            base + VertexExtra::SIZE
        } else {
            base
        }
    }

    /// Whether each of its vertices carries a [`VertexExtra`] after it.
    pub const fn extra(self) -> bool {
        matches!(
            self,
            SceneProgram::ExtraMesh
                | SceneProgram::ExtraDepth
                | SceneProgram::ExtraSkinnedMesh
                | SceneProgram::ExtraSkinnedDepth
        )
    }

    /// The program that draws what it draws from vertices that each carry a
    /// [`VertexExtra`]; a screen program is its own.
    pub const fn with_extra(self) -> SceneProgram {
        match self {
            SceneProgram::Mesh => SceneProgram::ExtraMesh,
            SceneProgram::Depth => SceneProgram::ExtraDepth,
            SceneProgram::SkinnedMesh => SceneProgram::ExtraSkinnedMesh,
            SceneProgram::SkinnedDepth => SceneProgram::ExtraSkinnedDepth,
            other => other,
        }
    }

    /// Whether it covers the target rather than drawing a mesh in the
    /// world: its vertices' positions are clip-space `x` and `y`, its
    /// instance unread.
    pub const fn screen(self) -> bool {
        matches!(
            self,
            SceneProgram::Sky | SceneProgram::Ao | SceneProgram::AoBlur
        )
    }

    /// Whether its vertices are [`SkinnedVertex`]es.
    pub const fn skinned(self) -> bool {
        matches!(
            self,
            SceneProgram::SkinnedMesh
                | SceneProgram::SkinnedDepth
                | SceneProgram::ExtraSkinnedMesh
                | SceneProgram::ExtraSkinnedDepth
        )
    }

    /// Whether it draws shadow casters, writing [`pack_depth`]ed depth.
    pub const fn casts(self) -> bool {
        matches!(
            self,
            SceneProgram::Depth
                | SceneProgram::SkinnedDepth
                | SceneProgram::ExtraDepth
                | SceneProgram::ExtraSkinnedDepth
        )
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

/// The vertex attributes of a skinned scene mesh, [`SkinnedVertex`]'s
/// fields.
pub const SKINNED_VERTEX_SCHEMA: InstanceSchema = InstanceSchema {
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
        SchemaAttr {
            name: "joints",
            format: AttrFormat::Uint2,
        },
        SchemaAttr {
            name: "weights",
            format: AttrFormat::Uint2,
        },
    ],
};

/// The two attributes of a [`VertexExtra`], after a vertex's own.
const EXTRA_ATTRS: [SchemaAttr; 2] = [
    SchemaAttr {
        name: "uv1",
        format: AttrFormat::Float2,
    },
    SchemaAttr {
        name: "color",
        format: AttrFormat::Uint2,
    },
];

/// The vertex attributes of a scene mesh whose vertices carry
/// [`VertexExtra`]s: [`VERTEX_SCHEMA`]'s, then the extra's.
pub const EXTRA_VERTEX_SCHEMA: InstanceSchema = InstanceSchema {
    attributes: &[
        VERTEX_SCHEMA.attributes[0],
        VERTEX_SCHEMA.attributes[1],
        VERTEX_SCHEMA.attributes[2],
        VERTEX_SCHEMA.attributes[3],
        EXTRA_ATTRS[0],
        EXTRA_ATTRS[1],
    ],
};

/// The vertex attributes of a skinned scene mesh whose vertices carry
/// [`VertexExtra`]s: [`SKINNED_VERTEX_SCHEMA`]'s, then the extra's.
pub const EXTRA_SKINNED_VERTEX_SCHEMA: InstanceSchema = InstanceSchema {
    attributes: &[
        SKINNED_VERTEX_SCHEMA.attributes[0],
        SKINNED_VERTEX_SCHEMA.attributes[1],
        SKINNED_VERTEX_SCHEMA.attributes[2],
        SKINNED_VERTEX_SCHEMA.attributes[3],
        SKINNED_VERTEX_SCHEMA.attributes[4],
        SKINNED_VERTEX_SCHEMA.attributes[5],
        EXTRA_ATTRS[0],
        EXTRA_ATTRS[1],
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

/// One vertex of a skinned scene mesh: a [`SceneVertex`] in its bind pose,
/// and the four joints that move it with how much each does. `joints` holds
/// four 16-bit indices into the palette, two to a word, the first in the low
/// half; `weights` four 16-bit fractions of `65535` the same way. 64 bytes.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
#[repr(C)]
pub struct SkinnedVertex {
    pub position: [f32; 3],
    pub normal: [f32; 3],
    pub uv: [f32; 2],
    pub tangent: [f32; 4],
    pub joints: [u32; 2],
    pub weights: [u32; 2],
}

impl SkinnedVertex {
    /// Its size in bytes, as the programs read it.
    pub const SIZE: usize = 64;

    /// `vertex`, moved by `joints` (indices into the palette) by `weights`
    /// (each in `[0, 1]`, summing to one), packed.
    pub fn new(vertex: SceneVertex, joints: [u16; 4], weights: [f32; 4]) -> SkinnedVertex {
        let unit = |w: f32| (w.clamp(0.0, 1.0) * 65535.0 + 0.5) as u32;
        let pair = |a: u32, b: u32| a | b << 16;
        SkinnedVertex {
            position: vertex.position,
            normal: vertex.normal,
            uv: vertex.uv,
            tangent: vertex.tangent,
            joints: [
                pair(joints[0].into(), joints[1].into()),
                pair(joints[2].into(), joints[3].into()),
            ],
            weights: [
                pair(unit(weights[0]), unit(weights[1])),
                pair(unit(weights[2]), unit(weights[3])),
            ],
        }
    }

    /// Its four joints.
    pub fn joint_indices(&self) -> [usize; 4] {
        let [a, b] = self.joints;
        [a & 0xffff, a >> 16, b & 0xffff, b >> 16].map(|j| j as usize)
    }

    /// Its four weights, normalized to sum to one.
    pub fn joint_weights(&self) -> [f32; 4] {
        let [a, b] = self.weights;
        let w = [a & 0xffff, a >> 16, b & 0xffff, b >> 16].map(|w| w as f32 / 65535.0);
        let sum = w[0] + w[1] + w[2] + w[3];
        if sum > 0.0 {
            w.map(|x| x / sum)
        } else {
            [1.0, 0.0, 0.0, 0.0]
        }
    }

    /// Its bytes, little-endian, in field order.
    pub fn write_to(&self, out: &mut [u8]) {
        let words = self
            .position
            .iter()
            .chain(&self.normal)
            .chain(&self.uv)
            .chain(&self.tangent)
            .map(|f| f.to_bits())
            .chain(self.joints)
            .chain(self.weights);
        for (chunk, w) in out[..Self::SIZE]
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .zip(words)
        {
            chunk.copy_from_slice(&w.to_le_bytes());
        }
    }

    /// It, read back from `bytes` as [`write_to`](Self::write_to) wrote it.
    pub fn read_from(bytes: &[u8]) -> SkinnedVertex {
        let word = |i: usize| {
            let c = &bytes[i * 4..i * 4 + 4];
            u32::from_le_bytes([c[0], c[1], c[2], c[3]])
        };
        let f = |i: usize| f32::from_bits(word(i));
        SkinnedVertex {
            position: [f(0), f(1), f(2)],
            normal: [f(3), f(4), f(5)],
            uv: [f(6), f(7)],
            tangent: [f(8), f(9), f(10), f(11)],
            joints: [word(12), word(13)],
            weights: [word(14), word(15)],
        }
    }
}

/// What a vertex of a mesh that has them carries past its [`SceneVertex`]
/// or [`SkinnedVertex`]: a second texture coordinate set, which a
/// material's map may read in place of the first, and a linear, straight
/// RGBA color multiplying the material's base color. `color` holds four
/// 16-bit fractions of `65535`, two to a word, the first in the low half.
/// The [`SceneProgram::extra`] programs read it right after the vertex it
/// belongs to. 16 bytes.
#[derive(Debug, Clone, Copy, PartialEq)]
#[repr(C)]
pub struct VertexExtra {
    pub uv1: [f32; 2],
    pub color: [u32; 2],
}

impl Default for VertexExtra {
    /// The first texture coordinate, white.
    fn default() -> VertexExtra {
        VertexExtra::new([0.0; 2], [1.0; 4])
    }
}

impl VertexExtra {
    /// Its size in bytes, as the programs read it.
    pub const SIZE: usize = 16;

    /// Second texture coordinate `uv1` and color `color` (each channel
    /// clamped to `[0, 1]`), packed.
    pub fn new(uv1: [f32; 2], color: [f32; 4]) -> VertexExtra {
        let unit = |c: f32| (c.clamp(0.0, 1.0) * 65535.0 + 0.5) as u32;
        let pair = |a: f32, b: f32| unit(a) | unit(b) << 16;
        VertexExtra {
            uv1,
            color: [pair(color[0], color[1]), pair(color[2], color[3])],
        }
    }

    /// Its color.
    pub fn rgba(&self) -> [f32; 4] {
        let [a, b] = self.color;
        [a & 0xffff, a >> 16, b & 0xffff, b >> 16].map(|c| c as f32 / 65535.0)
    }

    /// Its bytes, little-endian, in field order.
    pub fn write_to(&self, out: &mut [u8]) {
        let words = [self.uv1[0].to_bits(), self.uv1[1].to_bits()]
            .into_iter()
            .chain(self.color);
        for (chunk, w) in out[..Self::SIZE]
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .zip(words)
        {
            chunk.copy_from_slice(&w.to_le_bytes());
        }
    }

    /// It, read back from `bytes` as [`write_to`](Self::write_to) wrote it.
    pub fn read_from(bytes: &[u8]) -> VertexExtra {
        let word = |i: usize| {
            let c = &bytes[i * 4..i * 4 + 4];
            u32::from_le_bytes([c[0], c[1], c[2], c[3]])
        };
        VertexExtra {
            uv1: [f32::from_bits(word(0)), f32::from_bits(word(1))],
            color: [word(2), word(3)],
        }
    }
}

/// `v` moved by its joints: the weighted sum of the palette's affines
/// (`palette[3 j + r]` row `r` of joint `j`'s), applied to its position,
/// its normal through the sum's cofactor (turned back out where the sum
/// mirrors) and its tangent through the sum. A joint index past
/// [`MAX_JOINTS`] reads the last joint; one past the palette given stays in
/// place.
pub fn skin_vertex(v: &SkinnedVertex, palette: &[[f32; 4]]) -> SceneVertex {
    let w = v.joint_weights();
    let identity = [
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
    ];
    let mut rows = [[0.0f32; 4]; 3];
    for (j, w) in v.joint_indices().into_iter().zip(w) {
        let j = j.min(MAX_JOINTS - 1);
        for (r, row) in rows.iter_mut().enumerate() {
            let m = palette.get(j * 3 + r).copied().unwrap_or(identity[r]);
            for k in 0..4 {
                row[k] += m[k] * w;
            }
        }
    }
    let skin = SceneInstance {
        rows,
        color: [0.0; 4],
    };
    SceneVertex {
        position: skin.place(v.position),
        normal: skin.turn_normal(v.normal),
        uv: v.uv,
        tangent: skin.turn_tangent(v.tangent),
    }
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

/// The frame's uniform block, uniform slot 0 of every scene draw. 512 bytes.
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
    /// reflections; `z` how far from the eye the fog begins.
    pub ambient: [f32; 4],
    /// The environment's diffuse irradiance over π as nine spherical
    /// harmonic terms, each scaled so the irradiance at unit normal `n` is
    /// [`sh_irradiance`]`(sh, n)`.
    pub sh: [[f32; 4]; 9],
    /// Each of the [`SHADOW_CASCADES`] cascades, four rows apiece: three that
    /// take a world point `p` to `dot(row.xyz, p) + row.w` — the cascade's
    /// x and y in `[-1, 1]` (y up) and its depth from the light in `[0, 1]`
    /// — then its depth bias, in `x`, and the world size of one of its
    /// texels, in `y`.
    pub shadow: [[f32; 4]; SHADOW_CASCADES * 4],
    /// How dark the shadow makes the direct light, `0.0` casting none; the
    /// size of one texel of a cascade's tile, `1 / resolution`.
    pub shadow_params: [f32; 4],
    /// How much of the environment's light the ambient occlusion (texture
    /// slot 7) takes away, `0.0` none; then one over the target's width
    /// and height in pixels, which take a fragment's pixel to the
    /// occlusion's coordinate.
    pub occlusion: [f32; 4],
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
    /// masked material, and the [`MATERIAL_NORMAL_MAP`],
    /// [`MATERIAL_DOUBLE_SIDED`] and [`MATERIAL_UV1`] flags.
    pub mode: [f32; 4],
}

/// The sky's uniform block, uniform slot 1 of a [`SceneProgram::Sky`]
/// draw: the Preetham daylight model for one sun, as [`sky_radiance`]
/// evaluates it. 208 bytes.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
#[repr(C)]
pub struct SkyUniforms {
    /// Clip to world space, column-major: the inverse of the frame's
    /// `view_proj`, which a fragment's view ray is unprojected by.
    pub inv_view_proj: [f32; 16],
    /// The Perez coefficients `A`, `B`, `C` and `D` of the sky's
    /// luminance `Y` and its chromaticities `x` and `y`.
    pub perez_y: [f32; 4],
    pub perez_x: [f32; 4],
    pub perez_yc: [f32; 4],
    /// Their `E`s, then what the luminance is scaled by into the scene's
    /// linear light.
    pub perez_e: [f32; 4],
    /// The zenith's `Y`, `x` and `y` over the Perez function toward the
    /// zenith, so a channel is this times its Perez function; then how much
    /// below the horizon is dimmed.
    pub zenith: [f32; 4],
    /// The unit direction toward the sun the model is evaluated for, never
    /// lower than the model holds for.
    pub sun_model: [f32; 4],
    /// The unit direction toward the sun, and the angular radius of its
    /// disc (`0.0` when it shows none).
    pub sun: [f32; 4],
    /// The disc's radiance, linear RGB.
    pub sun_radiance: [f32; 4],
    /// How much of the day sky shows (`0.0` at night), and how much of the
    /// twilight's glow.
    pub blend: [f32; 4],
}

/// The ambient occlusion passes' uniform block, uniform slot 1 of an
/// [`SceneProgram::Ao`] or [`SceneProgram::AoBlur`] draw, whose frame block
/// is the view's. 112 bytes.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
#[repr(C)]
pub struct AoUniforms {
    /// Clip to world space, column-major: the inverse of the frame's
    /// `view_proj`, which a texel's depth is unprojected by.
    pub inv_view_proj: [f32; 16],
    /// The hemisphere's radius, in world units; how far in front of a tap
    /// a surface must stand to occlude it at least, and how much farther
    /// per unit of distance from the camera.
    pub params: [f32; 4],
    /// One texel of the passes' targets in their coordinates, then their
    /// width and height in texels.
    pub texel: [f32; 4],
    /// The blur's step, one texel along its axis, in the coordinates.
    pub step: [f32; 4],
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
/// [`MaterialUniforms::mode`]`[3]` flags: map `k` (base color,
/// metallic-roughness, normal, occlusion, emission, in texture slot order)
/// reads the second texture coordinate set where `MATERIAL_UV1 << k` is
/// set. A mesh without one reads the first in its place.
pub const MATERIAL_UV1: u32 = 4;

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

/// How many cascades the directional light's shadow is split into. The
/// shadow atlas, texture slot 6, holds them as square tiles side by side,
/// nearest first, each texel a [`pack_depth`]ed depth from the light; it
/// is sampled through the nearest sampler.
pub const SHADOW_CASCADES: usize = 3;

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
as_bytes!(FrameUniforms, 512);
as_bytes!(MaterialUniforms, 64);
as_bytes!(SkyUniforms, 208);
as_bytes!(AoUniforms, 112);

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

mod screen;
mod shade;

pub use screen::{shade_ao, shade_ao_blur, shade_sky, sky_radiance};

pub use shade::{
    MeshFragment, SceneTextures, disc, env_atlas_uv, env_brdf, env_direction_uv, faces_away,
    pack_depth, sh_irradiance, shade_depth, shade_mesh, shadow_visibility, tonemap, unpack_depth,
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
    fn a_skinned_vertex_packs_and_blends_its_joints() {
        let base = SceneVertex {
            position: [1.0, 0.0, 0.0],
            normal: [0.0, 1.0, 0.0],
            uv: [0.25, 0.75],
            tangent: [1.0, 0.0, 0.0, 1.0],
        };
        let v = SkinnedVertex::new(base, [3, 1, 0, 0], [0.75, 0.25, 0.0, 0.0]);
        let mut bytes = [0u8; SkinnedVertex::SIZE];
        v.write_to(&mut bytes);
        let v = SkinnedVertex::read_from(&bytes);
        assert_eq!(v.joint_indices(), [3, 1, 0, 0]);
        let w = v.joint_weights();
        assert!((w[0] - 0.75).abs() < 1e-4 && (w[1] - 0.25).abs() < 1e-4);
        // Joint 3 lifts by 4, joint 1 turns a quarter about z.
        let mut palette = vec![[0.0f32; 4]; MAX_JOINTS * 3];
        for j in 0..MAX_JOINTS {
            palette[j * 3] = [1.0, 0.0, 0.0, 0.0];
            palette[j * 3 + 1] = [0.0, 1.0, 0.0, 0.0];
            palette[j * 3 + 2] = [0.0, 0.0, 1.0, 0.0];
        }
        palette[3 * 3 + 1][3] = 4.0;
        palette[3] = [0.0, -1.0, 0.0, 0.0];
        palette[4] = [1.0, 0.0, 0.0, 0.0];
        let moved = skin_vertex(&v, &palette);
        let p = moved.position;
        assert!(
            (p[0] - 0.75).abs() < 1e-4 && (p[1] - 3.25).abs() < 1e-4,
            "{p:?}"
        );
        assert_eq!(moved.uv, [0.25, 0.75]);
    }

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
        for program in SceneProgram::ALL {
            assert_eq!(
                schema_bytes(program.vertex_schema()),
                program.vertex_size(),
                "{program:?}"
            );
            assert_eq!(SceneProgram::of_variant(program as u32), Some(program));
            assert_eq!(program.with_extra().skinned(), program.skinned());
            assert_eq!(program.with_extra().casts(), program.casts());
        }
        let extra = VertexExtra::new([0.25, -2.0], [1.0, 0.5, 0.0, 2.0]);
        let mut bytes = [0u8; VertexExtra::SIZE];
        extra.write_to(&mut bytes);
        let back = VertexExtra::read_from(&bytes);
        assert_eq!(back, extra);
        assert_eq!(back.uv1, [0.25, -2.0]);
        let c = back.rgba();
        assert!(c[0] == 1.0 && (c[1] - 0.5).abs() < 1e-4 && c[2] == 0.0 && c[3] == 1.0);
        assert_eq!(VertexExtra::default().rgba(), [1.0; 4]);
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
