//! Models: glTF 2.0 assets decoded to what the scene lane draws.
//!
//! [`ModelAsset::decode`] reads a `.glb`, or a `.gltf` whose buffers and
//! images are embedded or fetched through a resolver, into:
//!
//! - its meshes, each a list of [`ModelPrimitive`]s: indexed triangles with
//!   positions, normals (flat ones generated when the file has none), texture
//!   coordinates, tangents, vertex colors, and the joints and weights of a
//!   skinned one;
//! - its metallic-roughness [`ModelMaterial`]s and the textures and images
//!   they sample, the images decoded to straight RGBA8;
//! - its [`ModelRig`]: the node hierarchy of its default scene, its skins and
//!   its animations. The rig is small and shared; a model resident in a
//!   renderer keeps it after the geometry and images are uploaded and let go
//!   of.
//!
//! A [`Pose`] is the rig's nodes placed: [`ModelRig::rest`] sets it to the
//! file's own transforms, [`ModelRig::animate`] samples an animation into it
//! (step, linear and cubic-spline keys; rotations by spherical
//! interpolation), and [`Pose::solve`] composes each node's transform down the
//! hierarchy. A skin's joint matrices come from the solved pose.
//!
//! Everything read is checked: an accessor reading past its view, an index
//! past its vertices, a node in a cycle, or a URI that leaves the asset's
//! directory makes the asset fail to decode rather than read out of bounds.

pub(crate) mod geometry;
mod gltf;
mod mips;

use std::sync::Arc;

pub use gltf::ModelError;
pub use mips::ImageKind;
use viso_math::{Mat4, Quat, Vec3, vec3};

/// A decoded glTF asset: its geometry, materials, images and rig.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelAsset {
    pub rig: Arc<ModelRig>,
    pub meshes: Vec<ModelMesh>,
    pub materials: Vec<ModelMaterial>,
    pub textures: Vec<ModelTexture>,
    pub images: Vec<ModelImage>,
}

impl ModelAsset {
    /// The model in `bytes`, a `.glb` or a `.gltf`. A buffer or image the
    /// file names by a relative URI is read by `resolve`, given the URI
    /// decoded; data URIs and the binary chunk need none.
    ///
    /// # Errors
    ///
    /// Bytes that are not glTF 2.0, a required extension it does not
    /// support, or anything out of bounds or inconsistent.
    pub fn decode(
        bytes: &[u8],
        resolve: &mut dyn FnMut(&str) -> Option<Vec<u8>>,
    ) -> Result<ModelAsset, ModelError> {
        gltf::decode(bytes, resolve)
    }

    /// The model in `bytes`, which needs nothing beyond itself: a `.glb`, or
    /// a `.gltf` with its buffers and images in data URIs.
    ///
    /// # Errors
    ///
    /// As [`decode`](Self::decode), and a URI that is not a data URI.
    pub fn decode_embedded(bytes: &[u8]) -> Result<ModelAsset, ModelError> {
        gltf::decode(bytes, &mut |_| None)
    }

    /// About the bytes it holds.
    pub fn bytes(&self) -> u64 {
        let mut n = 0u64;
        for mesh in &self.meshes {
            for p in &mesh.primitives {
                n += p.bytes();
            }
        }
        for image in &self.images {
            n += image.rgba.len() as u64;
            n += image.mips.iter().map(|m| m.len() as u64).sum::<u64>();
        }
        n + self.rig.bytes()
    }
}

/// A glTF mesh: the primitives drawn together at a node.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ModelMesh {
    pub name: String,
    pub primitives: Vec<ModelPrimitive>,
}

/// One primitive of a mesh: indexed, counter-clockwise triangles of one
/// material. Every attribute list is as long as `positions`, or empty when
/// the primitive has no such attribute.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ModelPrimitive {
    pub positions: Vec<[f32; 3]>,
    /// Unit normals; always present (generated flat when the file has none).
    pub normals: Vec<[f32; 3]>,
    /// The first texture coordinate set, `TEXCOORD_0`.
    pub uvs: Vec<[f32; 2]>,
    /// Unit tangents along `+u` with the bitangent's sign in `w`.
    pub tangents: Vec<[f32; 4]>,
    /// Linear RGBA vertex colors, `COLOR_0`.
    pub colors: Vec<[f32; 4]>,
    /// The four joints each vertex follows, as indices into its skin's
    /// joints.
    pub joints: Vec<[u16; 4]>,
    /// How much each joint moves the vertex, summing to one.
    pub weights: Vec<[f32; 4]>,
    pub indices: Vec<u32>,
    /// Its material, by index; `None` for the glTF default material.
    pub material: Option<u32>,
    /// Its bounding box in its mesh's space: `(min, max)`.
    pub bounds: ([f32; 3], [f32; 3]),
}

impl ModelPrimitive {
    /// Whether joints move it.
    pub fn skinned(&self) -> bool {
        !self.joints.is_empty() && self.weights.len() == self.joints.len()
    }

    fn bytes(&self) -> u64 {
        (self.positions.len() * 12
            + self.normals.len() * 12
            + self.uvs.len() * 8
            + self.tangents.len() * 16
            + self.colors.len() * 16
            + self.joints.len() * 8
            + self.weights.len() * 16
            + self.indices.len() * 4) as u64
    }
}

/// How a material's alpha covers what is behind it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AlphaMode {
    /// Fully opaque; alpha is ignored.
    Opaque,
    /// Opaque where alpha reaches the cutoff, absent elsewhere.
    Mask(f32),
    /// Blended over what is behind it.
    Blend,
}

/// A texture a material samples, and which texture coordinate set it reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextureRef {
    /// Into [`ModelAsset::textures`].
    pub texture: u32,
    pub uv_set: u32,
}

/// A glTF metallic-roughness material. Colors are linear.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelMaterial {
    pub name: String,
    /// Linear RGBA, straight alpha.
    pub base_color: [f32; 4],
    /// sRGB RGBA; multiplies `base_color`.
    pub base_color_texture: Option<TextureRef>,
    pub metallic: f32,
    pub roughness: f32,
    /// Roughness in green, metalness in blue, multiplying the factors.
    pub metallic_roughness_texture: Option<TextureRef>,
    /// A tangent-space normal map.
    pub normal_texture: Option<TextureRef>,
    pub normal_scale: f32,
    /// Ambient occlusion in red.
    pub occlusion_texture: Option<TextureRef>,
    pub occlusion_strength: f32,
    /// Linear RGB light it emits, its strength folded in.
    pub emissive: [f32; 3],
    /// sRGB RGB; multiplies `emissive`.
    pub emissive_texture: Option<TextureRef>,
    pub alpha: AlphaMode,
    /// Its back faces are drawn too, lit by the flipped normal.
    pub double_sided: bool,
    /// `KHR_materials_unlit`: its base color as is.
    pub unlit: bool,
}

impl Default for ModelMaterial {
    /// The glTF default material: white, fully metallic, fully rough.
    fn default() -> ModelMaterial {
        ModelMaterial {
            name: String::new(),
            base_color: [1.0; 4],
            base_color_texture: None,
            metallic: 1.0,
            roughness: 1.0,
            metallic_roughness_texture: None,
            normal_texture: None,
            normal_scale: 1.0,
            occlusion_texture: None,
            occlusion_strength: 1.0,
            emissive: [0.0; 3],
            emissive_texture: None,
            alpha: AlphaMode::Opaque,
            double_sided: false,
            unlit: false,
        }
    }
}

/// How a texture's coordinates wrap outside `[0, 1]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Wrap {
    #[default]
    Repeat,
    Clamp,
    Mirror,
}

/// A texture: an image and how it is sampled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelTexture {
    /// Into [`ModelAsset::images`]; `None` when its image did not decode.
    pub image: Option<u32>,
    pub wrap: Wrap,
    /// Sampled at its nearest texel when magnified, rather than filtered.
    pub nearest: bool,
}

/// An image, decoded: straight-alpha RGBA8, rows top first, with the mip
/// chain below it built for what its materials sample it as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    /// What its texels hold: color when a material samples it as base color
    /// or emission, a normal map, or data.
    pub kind: ImageKind,
    /// The levels below `rgba`, each half the previous, down to 1×1.
    pub mips: Vec<Vec<u8>>,
}

/// A node's transform: translation, then rotation, then scale.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Trs {
    pub translation: Vec3,
    pub rotation: Quat,
    pub scale: Vec3,
}

impl Default for Trs {
    fn default() -> Trs {
        Trs {
            translation: Vec3::ZERO,
            rotation: Quat::IDENTITY,
            scale: vec3(1.0, 1.0, 1.0),
        }
    }
}

impl Trs {
    /// It as a matrix: `T · R · S`.
    pub fn matrix(&self) -> Mat4 {
        let r = self.rotation.to_mat4().v;
        let s = [self.scale.x, self.scale.y, self.scale.z];
        let t = self.translation;
        Mat4 {
            v: [
                r[0] * s[0],
                r[1] * s[0],
                r[2] * s[0],
                0.0,
                r[4] * s[1],
                r[5] * s[1],
                r[6] * s[1],
                0.0,
                r[8] * s[2],
                r[9] * s[2],
                r[10] * s[2],
                0.0,
                t.x,
                t.y,
                t.z,
                1.0,
            ],
        }
    }
}

/// One node of a model's hierarchy.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelNode {
    pub name: String,
    pub parent: Option<u32>,
    pub children: Vec<u32>,
    /// Its transform at rest, relative to its parent.
    pub rest: Trs,
    /// The mesh it draws, by index.
    pub mesh: Option<u32>,
    /// The skin its mesh follows, by index.
    pub skin: Option<u32>,
}

/// A skin: the joints a skinned mesh follows.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelSkin {
    pub name: String,
    /// The joint nodes, in the order a vertex's joint indices name them.
    pub joints: Vec<u32>,
    /// Each joint's inverse bind matrix: model space at bind time to the
    /// joint's space.
    pub inverse_bind: Vec<Mat4>,
    /// Each joint's reach in its own space: the sphere `(centre, radius)`
    /// around the vertices it moves, for bounding the posed mesh.
    pub reach: Vec<([f32; 3], f32)>,
}

/// Which part of a node's transform a channel animates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelPath {
    Translation,
    Rotation,
    Scale,
}

/// How a channel's value runs between its keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Interpolation {
    /// Held until the next key.
    Step,
    /// Linearly, rotations spherically.
    Linear,
    /// A cubic Hermite spline through the keys, each with an in and an out
    /// tangent.
    CubicSpline,
}

/// One animated property of one node.
#[derive(Debug, Clone, PartialEq)]
pub struct Channel {
    pub node: u32,
    pub path: ChannelPath,
    pub interpolation: Interpolation,
    /// Key times in seconds, ascending.
    pub times: Vec<f32>,
    /// Three floats a key (four for a rotation, `x y z w`); a cubic spline's
    /// keys are each an in tangent, the value and an out tangent.
    pub values: Vec<f32>,
}

/// An animation: channels played together over `duration` seconds.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelAnimation {
    pub name: String,
    pub channels: Vec<Channel>,
    /// The last key's time.
    pub duration: f32,
}

/// A model's hierarchy, skins and animations: what placing it needs.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ModelRig {
    pub nodes: Vec<ModelNode>,
    /// The root nodes of the scene it shows, in order.
    pub roots: Vec<u32>,
    /// Every node under the roots, each after its parent.
    pub order: Vec<u32>,
    pub skins: Vec<ModelSkin>,
    pub animations: Vec<ModelAnimation>,
    /// Each mesh's bounding box in its own space, `(min, max)`.
    pub mesh_bounds: Vec<([f32; 3], [f32; 3])>,
}

impl ModelRig {
    fn bytes(&self) -> u64 {
        let channels: usize = self
            .animations
            .iter()
            .flat_map(|a| &a.channels)
            .map(|c| (c.times.len() + c.values.len()) * 4)
            .sum();
        let skins: usize = self.skins.iter().map(|s| s.joints.len() * 84).sum();
        (self.nodes.len() * size_of::<ModelNode>() + channels + skins) as u64
    }

    /// The animation named `name`.
    pub fn animation(&self, name: &str) -> Option<usize> {
        self.animations.iter().position(|a| a.name == name)
    }

    /// The node named `name`.
    pub fn node(&self, name: &str) -> Option<usize> {
        self.nodes.iter().position(|n| n.name == name)
    }

    /// A pose of every node at rest, solved.
    pub fn rest_pose(&self) -> Pose {
        let mut pose = Pose::default();
        self.rest(&mut pose);
        pose.solve(self);
        pose
    }

    /// Sets `pose` to every node's rest transform, reusing its storage. Solve
    /// it before reading world transforms.
    pub fn rest(&self, pose: &mut Pose) {
        pose.locals.clear();
        pose.locals.extend(self.nodes.iter().map(|n| n.rest));
    }

    /// Samples animation `index` at `time` seconds (clamped to its keys)
    /// into `pose`, over whatever it held: blend weight `weight` toward the
    /// sampled transforms, `1.0` replacing them. Solve it before reading world
    /// transforms.
    pub fn animate(&self, pose: &mut Pose, index: usize, time: f32, weight: f32) {
        if pose.locals.len() != self.nodes.len() {
            self.rest(pose);
        }
        let Some(animation) = self.animations.get(index) else {
            return;
        };
        let weight = weight.clamp(0.0, 1.0);
        for channel in &animation.channels {
            let Some(local) = pose.locals.get_mut(channel.node as usize) else {
                continue;
            };
            let lanes = if channel.path == ChannelPath::Rotation {
                4
            } else {
                3
            };
            let value = sample(channel, lanes, time);
            match channel.path {
                ChannelPath::Translation => {
                    let v = vec3(value[0], value[1], value[2]);
                    local.translation = local.translation + (v - local.translation) * weight;
                }
                ChannelPath::Scale => {
                    let v = vec3(value[0], value[1], value[2]);
                    local.scale = local.scale + (v - local.scale) * weight;
                }
                ChannelPath::Rotation => {
                    let q = Quat::new(value[0], value[1], value[2], value[3]).normalize();
                    local.rotation = if weight >= 1.0 {
                        q
                    } else {
                        local.rotation.slerp(q, weight)
                    };
                }
            }
        }
    }

    /// Skin `index`'s joint matrices in `pose` (solved): each joint's world
    /// transform times its inverse bind matrix, taking a vertex from bind
    /// space to where the pose puts it in the model's space. Reuses `out`.
    pub fn joint_matrices(&self, pose: &Pose, index: usize, out: &mut Vec<Mat4>) {
        out.clear();
        let Some(skin) = self.skins.get(index) else {
            return;
        };
        out.extend(skin.joints.iter().zip(&skin.inverse_bind).map(|(&j, ibm)| {
            pose.world
                .get(j as usize)
                .map_or(*ibm, |world| world.mul(ibm))
        }));
    }
}

/// A model's nodes placed: each node's transform relative to its parent and,
/// once [`solve`](Self::solve)d, relative to the model.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Pose {
    pub locals: Vec<Trs>,
    pub world: Vec<Mat4>,
}

impl Pose {
    /// Composes each node's transform with its parents', down the rig's
    /// hierarchy. Reuses its storage.
    pub fn solve(&mut self, rig: &ModelRig) {
        if self.locals.len() != rig.nodes.len() {
            rig.rest(self);
        }
        self.world.clear();
        self.world.resize(rig.nodes.len(), Mat4::IDENTITY);
        for &n in &rig.order {
            let n = n as usize;
            let local = self.locals[n].matrix();
            self.world[n] = match rig.nodes[n].parent {
                Some(p) => self.world[p as usize].mul(&local),
                None => local,
            };
        }
    }
}

/// `channel`'s value at `time`, `lanes` floats a key.
fn sample(channel: &Channel, lanes: usize, time: f32) -> [f32; 4] {
    let times = &channel.times;
    let cubic = channel.interpolation == Interpolation::CubicSpline;
    let stride = if cubic { lanes * 3 } else { lanes };
    let at = |key: usize, part: usize| -> [f32; 4] {
        let base = key * stride + part * lanes;
        core::array::from_fn(|i| {
            if i < lanes {
                channel.values.get(base + i).copied().unwrap_or(0.0)
            } else {
                0.0
            }
        })
    };
    let value_part = usize::from(cubic);
    let last = times.len().saturating_sub(1);
    if times.is_empty() {
        return [0.0, 0.0, 0.0, 1.0];
    }
    if time <= times[0] || last == 0 {
        return at(0, value_part);
    }
    if time >= times[last] {
        return at(last, value_part);
    }
    // The key at or before `time`.
    let k = times.partition_point(|&t| t <= time) - 1;
    let (t0, t1) = (times[k], times[k + 1]);
    let dt = t1 - t0;
    let s = if dt > 0.0 { (time - t0) / dt } else { 0.0 };
    match channel.interpolation {
        Interpolation::Step => at(k, 0),
        Interpolation::Linear => {
            let (a, b) = (at(k, 0), at(k + 1, 0));
            if lanes == 4 {
                let q = Quat::new(a[0], a[1], a[2], a[3])
                    .normalize()
                    .slerp(Quat::new(b[0], b[1], b[2], b[3]).normalize(), s);
                [q.x, q.y, q.z, q.w]
            } else {
                core::array::from_fn(|i| a[i] + (b[i] - a[i]) * s)
            }
        }
        Interpolation::CubicSpline => {
            let (p0, m0) = (at(k, 1), at(k, 2));
            let (p1, m1) = (at(k + 1, 1), at(k + 1, 0));
            let (s2, s3) = (s * s, s * s * s);
            let h00 = 2.0 * s3 - 3.0 * s2 + 1.0;
            let h10 = s3 - 2.0 * s2 + s;
            let h01 = -2.0 * s3 + 3.0 * s2;
            let h11 = s3 - s2;
            core::array::from_fn(|i| {
                h00 * p0[i] + h10 * dt * m0[i] + h01 * p1[i] + h11 * dt * m1[i]
            })
        }
    }
}

#[cfg(test)]
mod tests;
