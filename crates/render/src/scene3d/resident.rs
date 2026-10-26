//! A model resident in a renderer: its primitives uploaded as meshes, its
//! materials and the images they sample, and the rig a frame places them by.

use std::ops::Range;
use std::sync::Arc;

use viso_gpu::TextureId;
use viso_gpu::scene::{
    Alpha, MATERIAL_DOUBLE_SIDED, MATERIAL_NORMAL_MAP, MaterialUniforms, Shading,
};
use viso_gpu::{AddressMode, FilterMode, GpuBackend, SamplerDesc, TextureDesc, TextureFormat};
use viso_math::Mat4;

use super::model::{
    AlphaMode, ModelAsset, ModelMaterial, ModelRig, ModelTexture, Pose, TextureRef, Wrap,
};
use super::{LinearStraight, MaterialId, MeshId, SceneFrame, SceneInstance, SceneLane};
use super::{SceneVertex, premultiplied};

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
    materials: Vec<MaterialId>,
    textures: Vec<TextureId>,
    /// The bytes its buffers and textures take on the device.
    bytes: u64,
}

/// One primitive on the device.
#[derive(Debug, Clone, Copy)]
struct Primitive {
    mesh: MeshId,
    material: MaterialId,
}

/// A material's maps in the order its program samples them, each with
/// whether it holds color.
fn maps(m: &ModelMaterial) -> [(Option<TextureRef>, bool); 5] {
    [
        (m.base_color_texture, true),
        (m.metallic_roughness_texture, false),
        (m.normal_texture, false),
        (m.occlusion_texture, false),
        (m.emissive_texture, true),
    ]
}

/// The sampler of a texture.
fn sampler(texture: &ModelTexture) -> SamplerDesc {
    SamplerDesc {
        filter: if texture.nearest {
            FilterMode::Nearest
        } else {
            FilterMode::MipmapLinear
        },
        address: match texture.wrap {
            Wrap::Repeat => AddressMode::Repeat,
            Wrap::Clamp => AddressMode::ClampToEdge,
            Wrap::Mirror => AddressMode::Mirror,
        },
    }
}

impl SceneModel {
    /// Uploads `asset`'s primitives, images and materials into `lane`.
    pub(crate) fn upload<B: GpuBackend>(
        asset: &ModelAsset,
        lane: &mut SceneLane,
        backend: &mut B,
    ) -> SceneModel {
        let mut bytes = 0u64;
        // Each image on the device once per encoding it is sampled in.
        let mut uploaded: Vec<[Option<TextureId>; 2]> = vec![[None; 2]; asset.images.len()];
        let mut textures = Vec::new();
        let mut image_texture = |image: u32, color: bool, backend: &mut B| -> Option<TextureId> {
            let held = uploaded.get_mut(image as usize)?;
            if let Some(t) = held[usize::from(color)] {
                return Some(t);
            }
            let img = &asset.images[image as usize];
            if img.width == 0 || img.height == 0 || img.rgba.is_empty() {
                return None;
            }
            let format = if color {
                TextureFormat::Rgba8UnormSrgb
            } else {
                TextureFormat::Rgba8Data
            };
            let mut levels: Vec<&[u8]> = vec![&img.rgba];
            levels.extend(img.mips.iter().map(Vec::as_slice));
            let t = backend.create_texture_mipped(
                &TextureDesc {
                    width: img.width,
                    height: img.height,
                    format,
                    render_target: false,
                    label: "scene-model-image",
                },
                &levels,
            );
            bytes += levels.iter().map(|l| l.len() as u64).sum::<u64>();
            held[usize::from(color)] = Some(t);
            textures.push(t);
            Some(t)
        };
        let mut materials = Vec::with_capacity(asset.materials.len() + 1);
        for m in asset.materials.iter().chain([&ModelMaterial::default()]) {
            let mut on_device = [None; 5];
            let mut first_sampler = None;
            for (slot, (texture, color)) in maps(m).into_iter().enumerate() {
                let Some(texture) = texture.and_then(|t| asset.textures.get(t.texture as usize))
                else {
                    continue;
                };
                first_sampler.get_or_insert(sampler(texture));
                on_device[slot] = texture
                    .image
                    .and_then(|image| image_texture(image, color, backend));
            }
            let uniforms = uniforms(m, on_device[2].is_some());
            let sampler = first_sampler.unwrap_or(SamplerDesc {
                filter: FilterMode::MipmapLinear,
                address: AddressMode::Repeat,
            });
            materials.push(lane.create_material(backend, &uniforms, on_device, sampler));
        }
        let default_material = *materials.last().expect("the default material is made");
        let mut meshes = Vec::with_capacity(asset.meshes.len());
        let mut primitives = Vec::new();
        let mut vertices = Vec::new();
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
                            tangent: p.tangents.get(i).copied().unwrap_or([0.0; 4]),
                        }),
                );
                bytes += (vertices.len() * size_of::<SceneVertex>() + p.indices.len() * 4) as u64;
                let material = p
                    .material
                    .and_then(|m| materials.get(m as usize).copied())
                    .unwrap_or(default_material);
                primitives.push(Primitive {
                    mesh: lane.create_mesh(backend, &vertices, &p.indices),
                    material,
                });
            }
            meshes.push(start..primitives.len() as u32);
        }
        SceneModel {
            rig: Arc::clone(&asset.rig),
            meshes,
            primitives,
            materials,
            textures,
            bytes,
        }
    }

    /// Destroy what [`upload`](Self::upload) made.
    pub(crate) fn release<B: GpuBackend>(&self, lane: &mut SceneLane, backend: &mut B) {
        for p in &self.primitives {
            lane.release_mesh(backend, p.mesh);
        }
        for &m in &self.materials {
            lane.release_material(backend, m);
        }
        for &t in &self.textures {
            backend.destroy_texture(t);
        }
    }

    /// The bytes its buffers and textures take on the device.
    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    /// Its hierarchy, skins and animations: what a [`Pose`] of it is posed
    /// against.
    pub fn rig(&self) -> &Arc<ModelRig> {
        &self.rig
    }
}

/// The uniform block of glTF material `m`, `normal_map` when its normal
/// texture is on the device.
fn uniforms(m: &ModelMaterial, normal_map: bool) -> MaterialUniforms {
    let (alpha, cutoff) = match m.alpha {
        AlphaMode::Opaque => (Alpha::Opaque, 0.5),
        AlphaMode::Mask(cutoff) => (Alpha::Mask, cutoff),
        AlphaMode::Blend => (Alpha::Blend, 0.5),
    };
    let shading = if m.unlit {
        Shading::Unlit
    } else {
        Shading::Lit
    };
    let mut flags = 0;
    if normal_map {
        flags |= MATERIAL_NORMAL_MAP;
    }
    if m.double_sided {
        flags |= MATERIAL_DOUBLE_SIDED;
    }
    MaterialUniforms {
        base_color: m.base_color,
        emissive: [m.emissive[0], m.emissive[1], m.emissive[2], 0.0],
        params: [
            m.metallic,
            m.roughness,
            m.normal_scale,
            m.occlusion_strength,
        ],
        mode: [
            shading as u8 as f32,
            alpha as u8 as f32,
            cutoff,
            flags as f32,
        ],
    }
}

/// The instance placing a mesh by the affine part of `m`, tinted `color`
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
    /// by `transform` and tinted by `tint`: every node of the model's scene
    /// that holds a mesh, each primitive in its material.
    pub fn push_model(
        &mut self,
        model: &SceneModel,
        pose: &Pose,
        transform: &Mat4,
        tint: LinearStraight,
    ) {
        let rig = &model.rig;
        let color = premultiplied(tint);
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
                self.push_with(p.mesh, p.material, placed(&world, color));
            }
        }
    }
}
