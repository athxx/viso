//! A model resident in a renderer: its primitives uploaded as meshes, and
//! the rig a frame places them by.

use std::ops::Range;
use std::sync::Arc;

use viso_math::Mat4;

use super::model::{ModelAsset, ModelRig, Pose};
use super::{LinearStraight, MeshId, SceneFrame, SceneInstance, SceneVertex, premultiplied};

/// A model uploaded with [`Renderer::create_scene_model`]: what
/// [`SceneFrame::push_model`] draws. Release it with
/// [`Renderer::release_scene_model`].
///
/// [`Renderer::create_scene_model`]: crate::Renderer::create_scene_model
/// [`Renderer::release_scene_model`]: crate::Renderer::release_scene_model
#[derive(Debug, Clone)]
pub struct SceneModel {
    rig: Arc<ModelRig>,
    /// Each glTF mesh's primitives, as a range of `primitives`.
    meshes: Vec<Range<u32>>,
    primitives: Vec<Primitive>,
    /// The bytes its buffers take on the device.
    bytes: u64,
}

/// One primitive on the device.
#[derive(Debug, Clone, Copy)]
struct Primitive {
    mesh: MeshId,
    /// Its material's base color, linear straight RGBA.
    color: [f32; 4],
}

impl SceneModel {
    /// Uploads `asset`'s primitives through `upload`, which returns the mesh
    /// of a primitive's vertices and indices.
    pub(crate) fn upload(
        asset: &ModelAsset,
        mut upload: impl FnMut(&[SceneVertex], &[u32]) -> MeshId,
    ) -> SceneModel {
        let mut meshes = Vec::with_capacity(asset.meshes.len());
        let mut primitives = Vec::new();
        let mut vertices = Vec::new();
        let mut bytes = 0u64;
        for mesh in &asset.meshes {
            let start = primitives.len() as u32;
            for p in &mesh.primitives {
                vertices.clear();
                vertices.extend(
                    p.positions
                        .iter()
                        .enumerate()
                        .map(|(i, &position)| SceneVertex {
                            position,
                            normal: p.normals.get(i).copied().unwrap_or([0.0, 1.0, 0.0]),
                            uv: p.uvs.get(i).copied().unwrap_or([0.0; 2]),
                        }),
                );
                bytes += (vertices.len() * size_of::<SceneVertex>() + p.indices.len() * 4) as u64;
                let material = p.material.and_then(|m| asset.materials.get(m as usize));
                primitives.push(Primitive {
                    mesh: upload(&vertices, &p.indices),
                    color: material.map_or([1.0; 4], |m| m.base_color),
                });
            }
            meshes.push(start..primitives.len() as u32);
        }
        SceneModel {
            rig: Arc::clone(&asset.rig),
            meshes,
            primitives,
            bytes,
        }
    }

    /// The bytes its buffers take on the device.
    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    /// Its hierarchy, skins and animations: what a [`Pose`] of it is posed
    /// against.
    pub fn rig(&self) -> &Arc<ModelRig> {
        &self.rig
    }

    /// The meshes it uploaded, for the renderer to release.
    pub(crate) fn uploaded(&self) -> impl Iterator<Item = MeshId> + '_ {
        self.primitives.iter().map(|p| p.mesh)
    }
}

/// The instance placing a mesh by the affine part of `m`, in `color`
/// (premultiplied).
fn placed(m: &Mat4, color: [f32; 4]) -> SceneInstance {
    let v = &m.v;
    SceneInstance {
        rows: core::array::from_fn(|r| [v[r], v[4 + r], v[8 + r], v[12 + r]]),
        color,
    }
}

impl SceneFrame {
    /// Draws `model` in `pose` (solved against its rig), placed in the world
    /// by `transform` and its colors multiplied by `tint`: every node of the
    /// model's scene that holds a mesh, each primitive in its material.
    pub fn push_model(
        &mut self,
        model: &SceneModel,
        pose: &Pose,
        transform: &Mat4,
        tint: LinearStraight,
    ) {
        let rig = &model.rig;
        for &n in &rig.order {
            let node = &rig.nodes[n as usize];
            let Some(range) = node.mesh.and_then(|m| model.meshes.get(m as usize)) else {
                continue;
            };
            let world = match pose.world.get(n as usize) {
                // A skinned mesh is placed by its joints, not its node.
                Some(_) if node.skin.is_some() => *transform,
                Some(at) => transform.mul(at),
                None => *transform,
            };
            for p in &model.primitives[range.start as usize..range.end as usize] {
                let c = p.color;
                let color = premultiplied(LinearStraight::new(
                    c[0] * tint.r,
                    c[1] * tint.g,
                    c[2] * tint.b,
                    c[3] * tint.a,
                ));
                self.push(p.mesh, placed(&world, color));
            }
        }
    }
}
