//! The renderer's side of the scene lane: the meshes and materials on the
//! device, each slot's frame, targets, environment and instance buffer, and
//! the passes that render them.

use std::sync::Arc;

use viso_gpu::backend::{DepthAttachment, DrawCommand, Geometry, IndexFormat, InlineUniforms};
use viso_gpu::backend::{RenderPass, RenderTarget};
use viso_gpu::scene::{
    Alpha, ENV_HEIGHT, ENV_WIDTH, MATERIAL_DOUBLE_SIDED, MATERIAL_NORMAL_MAP, SceneProgram, Shading,
};
use viso_gpu::{
    AddressMode, BindGroupDesc, BindGroupId, Binding, BlendMode, BufferDesc, BufferId, BufferUsage,
    BuiltinShader, CullMode, DepthState, FilterMode, GpuBackend, GpuPod, LoadOp, PipelineDesc,
    PipelineId, SamplerDesc, SamplerId, TextureDesc, TextureFormat, TextureId,
};
use viso_shader::{PrimitiveKind, shader_code};

use super::{AlphaMode, Bounds, Environment, Frustum, Material, MaterialId, MaterialUniforms};
use super::{MeshId, SceneFrame, SceneInstance, SceneVertex, Shape, fit_view};
use crate::primitive::{ImageDraw, Rect, SceneDraw, SceneSlot};

/// The pixel step a scene's targets grow and shrink by.
const TARGET_STEP: u32 = 64;

/// The radius, in pixels, a sprite is drawn at least at: its soft disc
/// covers about a pixel at its centre.
const MIN_SPRITE_PIXELS: f32 = 1.5;

/// The color format of a scene's target: what an image texture holds.
const COLOR_FORMAT: TextureFormat = TextureFormat::Bgra8UnormSrgb;

/// How many sky environments the lane keeps built for frames that name none.
const KEPT_SKIES: usize = 4;

/// The scene vertex as the pipeline's vertex layout reads it.
#[repr(C)]
#[derive(Clone, Copy, GpuPod)]
struct VertexLayout {
    position: [f32; 3],
    normal: [f32; 3],
    uv: [f32; 2],
    tangent: [f32; 4],
}

/// What the scene lane did in the frames since the counters were last
/// taken.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SceneStats {
    /// Scene passes rendered.
    pub passes: u32,
    /// Instanced draws issued.
    pub draws: u32,
    /// Instances drawn.
    pub instances: u32,
    /// Instances culled against the frustum.
    pub culled: u32,
    /// Target pairs (color and depth) allocated.
    pub allocations: u32,
    /// Environments uploaded to a slot.
    pub environments: u32,
}

/// A mesh on the device.
#[derive(Debug, Clone, Copy)]
struct GpuMesh {
    vertices: BufferId,
    indices: BufferId,
    format: IndexFormat,
    count: u32,
    bounds: Bounds,
}

/// A material on the device: its uniform block, maps and sampler.
#[derive(Debug, Clone, Copy)]
struct GpuMaterial {
    buffer: BufferId,
    /// Base color, metallic-roughness, normal, occlusion and emission;
    /// `None` reads white.
    maps: [Option<TextureId>; 5],
    sampler: SamplerId,
    alpha: Alpha,
    double_sided: bool,
    /// Distinguishes it from a material that held its id before.
    generation: u32,
}

impl GpuMaterial {
    /// The index of its pipeline in [`SceneLane::pipelines`].
    fn pipeline(&self) -> usize {
        usize::from(self.alpha == Alpha::Blend) * 2 + usize::from(self.double_sided)
    }
}

/// The lane's own resources every scene binds.
#[derive(Debug, Clone, Copy)]
struct Shared {
    /// A white texel: a map a material lacks.
    white: TextureId,
    /// The uniform block of a slot a program does not read.
    spare: BufferId,
    linear: SamplerId,
    point: SamplerId,
}

/// A slot's color and depth targets.
#[derive(Debug, Clone, Copy)]
struct Targets {
    color: TextureId,
    depth: TextureId,
    size: [u32; 2],
}

/// One instanced draw of a rendered scene: `count` instances of `mesh` in
/// `material` from instance `first`, through pipeline `pipeline`.
#[derive(Debug, Clone, Copy)]
struct Draw {
    material: MaterialId,
    mesh: MeshId,
    first: u32,
    count: u32,
    pipeline: usize,
}

/// What the renderer holds for one scene slot.
#[derive(Debug, Default)]
struct Slot {
    frame: SceneFrame,
    /// Bumped by every swap.
    revision: u64,
    targets: Option<Targets>,
    /// The frame revision and pixel size the targets were last filled for.
    filled: Option<(u64, [u32; 2])>,
    /// The fill is not yet rendered.
    pending: bool,
    instances: Option<(BufferId, usize)>,
    /// The frame's uniform block.
    uniforms: Option<BufferId>,
    /// The environment atlas and the environment it holds.
    env: Option<(TextureId, Arc<Environment>)>,
    /// Each material's bind group, by [`MaterialId::index`], with the
    /// material's generation.
    groups: Vec<Option<(u32, BindGroupId)>>,
    draws: Vec<Draw>,
    used: [u32; 2],
    clear: [f32; 4],
}

#[derive(Debug, Default)]
pub(crate) struct SceneLane {
    /// The mesh program's pipelines: opaque or blended, each culling back
    /// faces or drawing both.
    pipelines: [Option<PipelineId>; 4],
    meshes: Vec<Option<GpuMesh>>,
    /// Uploaded mesh ids free for reuse.
    free: Vec<u32>,
    materials: Vec<Option<GpuMaterial>>,
    /// Material ids free for reuse.
    free_materials: Vec<u32>,
    generation: u32,
    shared: Option<Shared>,
    /// Every sampler a material samples through, made once each.
    samplers: Vec<(SamplerDesc, SamplerId)>,
    /// The sky environments of the latest lightings that named none.
    skies: Vec<([u32; 6], Arc<Environment>)>,
    slots: Vec<Option<Slot>>,
    /// The bytes of a fill's surviving instances.
    scratch: Vec<u8>,
    /// A fill's visible blended instances, by their distance along the view.
    blended: Vec<(f32, MaterialId, MeshId, SceneInstance)>,
    pub(crate) stats: SceneStats,
}

impl SceneLane {
    /// Hand `frame` to `slot`, taking back the slot's previous frame in its
    /// place.
    pub(crate) fn swap(&mut self, slot: SceneSlot, frame: &mut SceneFrame) {
        let i = slot.0 as usize;
        if self.slots.len() <= i {
            self.slots.resize_with(i + 1, || None);
        }
        let state = self.slots[i].get_or_insert_with(Slot::default);
        core::mem::swap(&mut state.frame, frame);
        state.revision += 1;
    }

    /// Free `slot` and everything it holds on the device; the color target
    /// is returned for the renderer to release.
    pub(crate) fn release<B: GpuBackend>(
        &mut self,
        backend: &mut B,
        slot: SceneSlot,
    ) -> Option<TextureId> {
        let state = self.slots.get_mut(slot.0 as usize)?.take()?;
        if let Some((buffer, _)) = state.instances {
            backend.destroy_buffer(buffer);
        }
        for (_, group) in state.groups.into_iter().flatten() {
            backend.destroy_bind_group(group);
        }
        if let Some(buffer) = state.uniforms {
            backend.destroy_buffer(buffer);
        }
        if let Some((texture, _)) = state.env {
            backend.destroy_texture(texture);
        }
        let targets = state.targets?;
        backend.destroy_texture(targets.depth);
        Some(targets.color)
    }

    /// Upload a mesh of `vertices` and triangle `indices`.
    pub(crate) fn create_mesh<B: GpuBackend>(
        &mut self,
        backend: &mut B,
        vertices: &[SceneVertex],
        indices: &[u32],
    ) -> MeshId {
        let narrow = vertices.len() <= usize::from(u16::MAX) + 1;
        let index_bytes: Vec<u8> = if narrow {
            indices
                .iter()
                .flat_map(|&i| (i as u16).to_le_bytes())
                .collect()
        } else {
            indices.iter().flat_map(|&i| i.to_le_bytes()).collect()
        };
        let format = if narrow {
            IndexFormat::U16
        } else {
            IndexFormat::U32
        };
        let mesh = upload(backend, vertices, &index_bytes, format, indices.len());
        let id = match self.free.pop() {
            Some(id) => id,
            None => {
                let id = self.meshes.len().max(Shape::ALL.len()) as u32;
                self.meshes.resize(id as usize + 1, None);
                id
            }
        };
        self.meshes[id as usize] = Some(mesh);
        MeshId::new(id)
    }

    /// Destroy an uploaded mesh; a built-in shape stays.
    pub(crate) fn release_mesh<B: GpuBackend>(&mut self, backend: &mut B, mesh: MeshId) {
        if mesh.index() < Shape::ALL.len() {
            return;
        }
        if let Some(m) = self.meshes.get_mut(mesh.index()).and_then(Option::take) {
            backend.destroy_buffer(m.vertices);
            backend.destroy_buffer(m.indices);
            self.free.push(mesh.0);
        }
    }

    /// The lane's shared resources, made on first use.
    fn shared<B: GpuBackend>(&mut self, backend: &mut B) -> Shared {
        if let Some(shared) = self.shared {
            return shared;
        }
        let white = backend.create_texture(&TextureDesc {
            width: 1,
            height: 1,
            format: TextureFormat::Rgba8Unorm,
            render_target: false,
            label: "scene-white",
        });
        backend.write_texture(white, 0, 0, 1, 1, &[255; 4]);
        let spare = backend.create_buffer(&BufferDesc {
            size: 16,
            usage: BufferUsage::UNIFORM | BufferUsage::CPU_WRITE,
            label: "scene-spare-uniforms",
        });
        backend.write_buffer(spare, 0, &[0; 16]);
        let linear = backend.create_sampler(&SamplerDesc::LINEAR_CLAMP);
        let point = backend.create_sampler(&SamplerDesc {
            filter: FilterMode::Nearest,
            address: AddressMode::ClampToEdge,
        });
        let shared = Shared {
            white,
            spare,
            linear,
            point,
        };
        self.shared = Some(shared);
        shared
    }

    /// The sampler `desc` describes, made once.
    pub(crate) fn sampler<B: GpuBackend>(
        &mut self,
        backend: &mut B,
        desc: SamplerDesc,
    ) -> SamplerId {
        if let Some(&(_, id)) = self.samplers.iter().find(|(d, _)| *d == desc) {
            return id;
        }
        let id = backend.create_sampler(&desc);
        self.samplers.push((desc, id));
        id
    }

    /// Make a material of `uniforms` sampling `maps` (base color,
    /// metallic-roughness, normal, occlusion, emission; `None` reads white)
    /// through `sampler`.
    pub(crate) fn create_material<B: GpuBackend>(
        &mut self,
        backend: &mut B,
        uniforms: &MaterialUniforms,
        maps: [Option<TextureId>; 5],
        sampler: SamplerDesc,
    ) -> MaterialId {
        self.built_in(backend);
        let id = match self.free_materials.pop() {
            Some(id) => id,
            None => {
                let id = self.materials.len().max(MaterialId::BUILT_IN as usize) as u32;
                self.materials.resize(id as usize + 1, None);
                id
            }
        };
        let material = self.material_on_device(backend, uniforms, maps, sampler);
        self.materials[id as usize] = Some(material);
        MaterialId(id)
    }

    /// `material`'s uniform block, as the programs read it.
    pub(crate) fn uniforms_of(material: &Material) -> MaterialUniforms {
        let c = material.base_color;
        let (alpha, cutoff) = match material.alpha {
            AlphaMode::Opaque => (Alpha::Opaque, 0.5),
            AlphaMode::Mask(cutoff) => (Alpha::Mask, cutoff),
            AlphaMode::Blend => (Alpha::Blend, 0.5),
        };
        let shading = if material.unlit {
            Shading::Unlit
        } else {
            Shading::Lit
        };
        let flags = if material.double_sided {
            MATERIAL_DOUBLE_SIDED
        } else {
            0
        };
        MaterialUniforms {
            base_color: [c.r, c.g, c.b, c.a],
            emissive: [
                material.emissive[0],
                material.emissive[1],
                material.emissive[2],
                0.0,
            ],
            params: [material.metallic, material.roughness, 1.0, 1.0],
            mode: [
                shading as u8 as f32,
                alpha as u8 as f32,
                cutoff,
                flags as f32,
            ],
        }
    }

    fn material_on_device<B: GpuBackend>(
        &mut self,
        backend: &mut B,
        uniforms: &MaterialUniforms,
        maps: [Option<TextureId>; 5],
        sampler: SamplerDesc,
    ) -> GpuMaterial {
        let buffer = backend.create_buffer(&BufferDesc {
            size: MaterialUniforms::SIZE,
            usage: BufferUsage::UNIFORM | BufferUsage::CPU_WRITE,
            label: "scene-material",
        });
        let mut uniforms = *uniforms;
        if maps[2].is_none() {
            uniforms.mode[3] = (uniforms.flags() & !MATERIAL_NORMAL_MAP) as f32;
        }
        let mut bytes = [0u8; MaterialUniforms::SIZE];
        uniforms.write_to(&mut bytes);
        backend.write_buffer(buffer, 0, &bytes);
        self.generation = self.generation.wrapping_add(1);
        let sampler = self.sampler(backend, sampler);
        let sprite = uniforms.shading() == Shading::Sprite;
        GpuMaterial {
            buffer,
            maps,
            sampler,
            alpha: uniforms.alpha(),
            double_sided: sprite || uniforms.flags() & MATERIAL_DOUBLE_SIDED != 0,
            generation: self.generation,
        }
    }

    /// The built-in materials, made on first use.
    fn built_in<B: GpuBackend>(&mut self, backend: &mut B) {
        if self.materials.first().is_some_and(Option::is_some) {
            return;
        }
        if self.materials.len() < MaterialId::BUILT_IN as usize {
            self.materials.resize(MaterialId::BUILT_IN as usize, None);
        }
        let lit = Self::uniforms_of(&Material::default());
        let mut unlit = lit;
        unlit.mode[0] = Shading::Unlit as u8 as f32;
        let mut sprite = lit;
        sprite.mode[0] = Shading::Sprite as u8 as f32;
        sprite.mode[1] = Alpha::Blend as u8 as f32;
        let sampler = SamplerDesc::LINEAR_CLAMP;
        for (id, uniforms) in [
            (MaterialId::LIT, lit),
            (MaterialId::UNLIT, unlit),
            (MaterialId::SPRITE, sprite),
        ] {
            let m = self.material_on_device(backend, &uniforms, [None; 5], sampler);
            self.materials[id.index()] = Some(m);
        }
    }

    /// Destroy a material; a built-in one stays.
    pub(crate) fn release_material<B: GpuBackend>(&mut self, backend: &mut B, id: MaterialId) {
        if id.0 < MaterialId::BUILT_IN {
            return;
        }
        let Some(material) = self.materials.get_mut(id.index()).and_then(Option::take) else {
            return;
        };
        backend.destroy_buffer(material.buffer);
        for slot in self.slots.iter_mut().flatten() {
            if let Some(entry) = slot.groups.get_mut(id.index())
                && let Some((_, group)) = entry.take()
            {
                backend.destroy_bind_group(group);
            }
        }
        self.free_materials.push(id.0);
    }

    /// The mesh `id` on the device: a built-in shape is generated and
    /// uploaded the first time it is drawn.
    fn mesh<B: GpuBackend>(&mut self, backend: &mut B, id: MeshId) -> Option<GpuMesh> {
        if let Some(Some(mesh)) = self.meshes.get(id.index()) {
            return Some(*mesh);
        }
        let shape = *Shape::ALL.get(id.index())?;
        let (vertices, indices) = shape.geometry();
        let bytes: Vec<u8> = indices.iter().flat_map(|i| i.to_le_bytes()).collect();
        let mut mesh = upload(backend, &vertices, &bytes, IndexFormat::U16, indices.len());
        mesh.bounds = Bounds::UNIT;
        if self.meshes.len() <= id.index() {
            self.meshes.resize(id.index() + 1, None);
        }
        self.meshes[id.index()] = Some(mesh);
        Some(mesh)
    }

    /// The material `id` names, or [`MaterialId::LIT`] in its place when it
    /// names none.
    fn material(&self, id: MaterialId) -> (MaterialId, GpuMaterial) {
        match self.materials.get(id.index()).copied().flatten() {
            Some(m) => (id, m),
            None => (
                MaterialId::LIT,
                self.materials[MaterialId::LIT.index()].expect("the built-ins are made first"),
            ),
        }
    }

    fn pipeline<B: GpuBackend>(&mut self, backend: &mut B, index: usize) -> PipelineId {
        *self.pipelines[index]
            .get_or_insert_with(|| mesh_pipeline(backend, index >= 2, index % 2 == 1))
    }

    /// The sky environment of `frame`'s lighting, built once for the latest
    /// few lightings.
    fn environment(&mut self, frame: &SceneFrame) -> Arc<Environment> {
        if let Some(env) = &frame.environment {
            return Arc::clone(env);
        }
        let l = &frame.lighting;
        let key = [
            l.sky[0].to_bits(),
            l.sky[1].to_bits(),
            l.sky[2].to_bits(),
            l.ground[0].to_bits(),
            l.ground[1].to_bits(),
            l.ground[2].to_bits(),
        ];
        if let Some(i) = self.skies.iter().position(|(k, _)| *k == key) {
            let entry = self.skies.remove(i);
            let env = Arc::clone(&entry.1);
            self.skies.push(entry);
            return env;
        }
        let env = Arc::new(l.sky_environment());
        if self.skies.len() >= KEPT_SKIES {
            self.skies.remove(0);
        }
        self.skies.push((key, Arc::clone(&env)));
        env
    }

    /// Size `draw`'s slot to its device pixels at `scale`, and fill it when
    /// its frame or size changed. Returns a color target the slot let go of,
    /// for the renderer to release with every binding it cached for it.
    pub(crate) fn place<B: GpuBackend>(
        &mut self,
        backend: &mut B,
        draw: &SceneDraw,
        scale: f32,
        max_size: u32,
    ) -> Option<TextureId> {
        let px = |points: f32| ((points * scale).ceil().max(0.0) as u32).min(max_size);
        let used = [px(draw.rect.w), px(draw.rect.h)];
        if used[0] == 0 || used[1] == 0 {
            return None;
        }
        let size = used.map(|n| n.div_ceil(TARGET_STEP) * TARGET_STEP);
        let size = size.map(|n| n.min(max_size));
        let i = draw.slot.0 as usize;
        self.slots.get(i)?.as_ref()?;
        self.built_in(backend);
        self.shared(backend);
        let mut retired = None;
        let slot = self.slots[i].as_mut().expect("checked above");
        if slot.targets.is_none_or(|t| t.size != size) {
            if let Some(old) = slot.targets.take() {
                backend.destroy_texture(old.depth);
                retired = Some(old.color);
            }
            let texture = |backend: &mut B, format, label| {
                backend.create_texture(&TextureDesc {
                    width: size[0],
                    height: size[1],
                    format,
                    render_target: true,
                    label,
                })
            };
            slot.targets = Some(Targets {
                color: texture(backend, COLOR_FORMAT, "scene"),
                depth: texture(backend, TextureFormat::Depth32Float, "scene-depth"),
                size,
            });
            slot.filled = None;
            self.stats.allocations += 1;
        }
        slot.used = used;
        if slot.filled != Some((slot.revision, used)) {
            slot.filled = Some((slot.revision, used));
            slot.pending = true;
            self.fill(backend, i, size);
        }
        retired
    }

    /// The image that composites `draw`'s slot as last placed, or `None`
    /// when it was not.
    pub(crate) fn image(&self, draw: &SceneDraw) -> Option<ImageDraw> {
        let slot = self.slots.get(draw.slot.0 as usize)?.as_ref()?;
        let targets = slot.targets?;
        let (used, size) = (slot.used, targets.size);
        let mut image = ImageDraw::new(draw.rect, targets.color);
        image.uv = Rect::new(
            0.0,
            0.0,
            used[0] as f32 / size[0] as f32,
            used[1] as f32 / size[1] as f32,
        );
        image.mask = draw.mask;
        Some(image)
    }

    /// Cull slot `i`'s frame into its instance buffer, write its uniforms
    /// and environment, and record its draws, for a target `size` pixels
    /// large.
    fn fill<B: GpuBackend>(&mut self, backend: &mut B, i: usize, size: [u32; 2]) {
        let mut scratch = core::mem::take(&mut self.scratch);
        scratch.clear();
        let mut blended = core::mem::take(&mut self.blended);
        blended.clear();
        let mut slot = self.slots[i].take().expect("the caller checked the slot");
        let used = slot.used;
        let view_proj = slot.frame.camera.view_proj(used[0] as f32 / used[1] as f32);
        let frustum = Frustum::of(&view_proj);
        let (right, up, forward) = slot.frame.camera.basis();
        let eye = slot.frame.camera.eye;
        let mut total = 0u32;
        let mut draws = core::mem::take(&mut slot.draws);
        draws.clear();
        let push = |scratch: &mut Vec<u8>, instance: &SceneInstance| {
            let at = scratch.len();
            scratch.resize(at + SceneInstance::SIZE, 0);
            instance.write_to(&mut scratch[at..]);
        };
        for (material, mesh_id, batch) in slot.frame.batches() {
            let Some(mesh) = self.mesh(backend, mesh_id) else {
                continue;
            };
            let (material, gpu) = self.material(material);
            let first = total;
            for instance in batch {
                let (centre, half) = mesh.bounds.placed(instance);
                if !frustum.sees(centre, half) {
                    self.stats.culled += 1;
                    continue;
                }
                if gpu.alpha == Alpha::Blend {
                    let depth = (super::vec3(centre[0], centre[1], centre[2]) - eye).dot(forward);
                    blended.push((depth, material, mesh_id, *instance));
                    continue;
                }
                push(&mut scratch, instance);
                total += 1;
            }
            if total > first {
                draws.push(Draw {
                    material,
                    mesh: mesh_id,
                    first,
                    count: total - first,
                    pipeline: gpu.pipeline(),
                });
            }
        }
        let camera = slot.frame.camera;
        for sprite in slot.frame.sprites() {
            if !frustum.sees(sprite.position.to_array(), [sprite.radius; 3]) {
                self.stats.culled += 1;
                continue;
            }
            let depth = (sprite.position - eye).dot(forward);
            let least = MIN_SPRITE_PIXELS * camera.pixel_at(depth, used[1]);
            let instance = sprite.facing((right, up, forward), least);
            blended.push((depth, MaterialId::SPRITE, Shape::Plane.into(), instance));
        }
        if !blended.is_empty() {
            self.mesh(backend, Shape::Plane.into());
            // Back to front, so each blends over what is behind it; a run of
            // one mesh in one material is one draw.
            blended.sort_by(|a, b| b.0.total_cmp(&a.0));
            for &(_, material, mesh, instance) in &blended {
                push(&mut scratch, &instance);
                match draws.last_mut() {
                    Some(d)
                        if d.material == material
                            && d.mesh == mesh
                            && d.first + d.count == total =>
                    {
                        d.count += 1;
                    }
                    _ => {
                        let (_, gpu) = self.material(material);
                        draws.push(Draw {
                            material,
                            mesh,
                            first: total,
                            count: 1,
                            pipeline: gpu.pipeline(),
                        });
                    }
                }
                total += 1;
            }
        }
        if !scratch.is_empty() {
            let buffer = match slot.instances {
                Some((buffer, capacity)) if capacity >= scratch.len() => buffer,
                old => {
                    if let Some((buffer, _)) = old {
                        backend.destroy_buffer(buffer);
                    }
                    let capacity = scratch.len().next_power_of_two();
                    let buffer = backend.create_buffer(&BufferDesc {
                        size: capacity,
                        usage: BufferUsage::INSTANCE | BufferUsage::CPU_WRITE,
                        label: "scene-instances",
                    });
                    slot.instances = Some((buffer, capacity));
                    buffer
                }
            };
            backend.write_buffer(buffer, 0, &scratch);
        }
        // The frame's uniforms and environment.
        let env = self.environment(&slot.frame);
        let env_texture = match &slot.env {
            Some((texture, held)) if Arc::ptr_eq(held, &env) => *texture,
            held => {
                // A frame in flight may still sample the old atlas: it is
                // retired, and every group that binds it with it.
                if let Some((old, _)) = held {
                    backend.destroy_texture(*old);
                    for (_, group) in slot.groups.iter_mut().filter_map(Option::take) {
                        backend.destroy_bind_group(group);
                    }
                }
                let texture = backend.create_texture(&TextureDesc {
                    width: ENV_WIDTH,
                    height: ENV_HEIGHT,
                    format: TextureFormat::Rgba16Float,
                    render_target: false,
                    label: "scene-environment",
                });
                let bytes: Vec<u8> = env.texels.iter().flat_map(|t| t.to_le_bytes()).collect();
                backend.write_texture(texture, 0, 0, ENV_WIDTH, ENV_HEIGHT, &bytes);
                self.stats.environments += 1;
                slot.env = Some((texture, Arc::clone(&env)));
                texture
            }
        };
        let frame_buffer = *slot.uniforms.get_or_insert_with(|| {
            backend.create_buffer(&BufferDesc {
                size: viso_gpu::scene::FrameUniforms::SIZE,
                usage: BufferUsage::UNIFORM | BufferUsage::CPU_WRITE,
                label: "scene-frame",
            })
        });
        let fitted = fit_view(&view_proj, used, size);
        let mut bytes = [0u8; viso_gpu::scene::FrameUniforms::SIZE];
        slot.frame.uniforms(&fitted, &env.sh).write_to(&mut bytes);
        backend.write_buffer(frame_buffer, 0, &bytes);
        // Every drawn material's bind group, and the pipelines they need.
        let shared = self.shared(backend);
        for d in &draws {
            self.pipeline(backend, d.pipeline);
            let (_, gpu) = self.material(d.material);
            let index = d.material.index();
            if slot.groups.len() <= index {
                slot.groups.resize(index + 1, None);
            }
            match slot.groups[index] {
                Some((generation, _)) if generation == gpu.generation => continue,
                Some((_, stale)) => backend.destroy_bind_group(stale),
                None => {}
            }
            let maps = gpu
                .maps
                .map(|m| Binding::Texture(m.unwrap_or(shared.white)));
            let mut bindings = vec![
                Binding::Uniform(frame_buffer),
                Binding::Uniform(gpu.buffer),
                Binding::Uniform(shared.spare),
            ];
            bindings.extend(maps);
            bindings.extend([
                Binding::Texture(env_texture),
                Binding::Texture(shared.white),
                Binding::Texture(shared.white),
                Binding::Sampler(gpu.sampler),
                Binding::Sampler(shared.linear),
                Binding::Sampler(shared.point),
            ]);
            let group = backend.create_bind_group(&BindGroupDesc {
                label: "scene",
                bindings,
            });
            slot.groups[index] = Some((gpu.generation, group));
        }
        slot.draws = draws;
        slot.clear = slot.frame.clear_color();
        self.slots[i] = Some(slot);
        self.scratch = scratch;
        self.blended = blended;
    }

    /// Append a pass for every slot filled since it last rendered.
    pub(crate) fn encode(&mut self, commands: &mut Vec<DrawCommand>, passes: &mut Vec<RenderPass>) {
        for slot in self.slots.iter_mut().flatten() {
            let Some(targets) = slot.targets.filter(|_| slot.pending) else {
                continue;
            };
            slot.pending = false;
            let first_command = commands.len() as u32;
            for draw in &slot.draws {
                let Some(Some(mesh)) = self.meshes.get(draw.mesh.index()) else {
                    continue;
                };
                let Some((instances, _)) = slot.instances else {
                    continue;
                };
                let Some(Some((_, group))) = slot.groups.get(draw.material.index()) else {
                    continue;
                };
                let Some(pipeline) = self.pipelines[draw.pipeline] else {
                    continue;
                };
                commands.push(DrawCommand {
                    pipeline,
                    bind_group: Some(*group),
                    geometry: Geometry::InstancedMesh {
                        vertex_buffer: mesh.vertices,
                        index_buffer: mesh.indices,
                        index_format: mesh.format,
                        index_offset: 0,
                        index_count: mesh.count,
                        count: draw.count,
                    },
                    instance_buffer: instances,
                    instance_offset: draw.first as usize * SceneInstance::SIZE,
                    uniforms: InlineUniforms::EMPTY,
                    scissor: Some((0, 0, slot.used[0], slot.used[1])),
                });
                self.stats.draws += 1;
                self.stats.instances += draw.count;
            }
            passes.push(RenderPass {
                target: RenderTarget::Texture(targets.color),
                load: LoadOp::Clear(slot.clear),
                depth: Some(DepthAttachment {
                    texture: targets.depth,
                    clear: 0.0,
                }),
                first_command,
                command_count: commands.len() as u32 - first_command,
            });
            self.stats.passes += 1;
        }
    }

    /// The counters since the last call, which resets them.
    pub(crate) fn take_stats(&mut self) -> SceneStats {
        core::mem::take(&mut self.stats)
    }
}

/// The mesh program's pipeline: opaque, or `blended` (premultiplied over,
/// depth-tested but never written); culling back faces, or drawing `both`.
fn mesh_pipeline<B: GpuBackend>(backend: &mut B, blended: bool, both: bool) -> PipelineId {
    let code = shader_code(PrimitiveKind::Scene, B::SHADER_LANG)
        .expect("every backend language prints the scene programs");
    let (depth, blend) = if blended {
        (DepthState::REVERSED_TEST_ONLY, BlendMode::PremultipliedOver)
    } else {
        (DepthState::REVERSED, BlendMode::Replace)
    };
    let cull = if both { CullMode::None } else { CullMode::Back };
    let program = SceneProgram::Mesh;
    backend
        .create_pipeline(
            &PipelineDesc {
                label: if blended { "scene-blended" } else { "scene" },
                builtin: BuiltinShader::Scene,
                variant: program as u32 | u32::from(blended) << 8 | u32::from(both) << 9,
                code,
                vertex_entry: "vertex_main",
                fragment_entry: "fragment_main",
                color_format: COLOR_FORMAT,
                depth: Some(depth),
                cull,
                blend,
                instance_schema: program.vertex_schema(),
            },
            &VertexLayout::LAYOUT,
        )
        .expect("the scene vertex matches the mesh program's schema")
}

/// A mesh's buffers on the device.
fn upload<B: GpuBackend>(
    backend: &mut B,
    vertices: &[SceneVertex],
    index_bytes: &[u8],
    format: IndexFormat,
    count: usize,
) -> GpuMesh {
    let mut bytes = vec![0u8; vertices.len() * SceneVertex::SIZE];
    for (v, out) in vertices
        .iter()
        .zip(bytes.as_chunks_mut::<{ SceneVertex::SIZE }>().0)
    {
        v.write_to(out);
    }
    let buffer = |backend: &mut B, bytes: &[u8], usage, label| {
        let b = backend.create_buffer(&BufferDesc {
            size: bytes.len().max(4),
            usage: usage | BufferUsage::CPU_WRITE,
            label,
        });
        backend.write_buffer(b, 0, bytes);
        b
    };
    GpuMesh {
        vertices: buffer(backend, &bytes, BufferUsage::VERTEX, "scene-mesh"),
        indices: buffer(
            backend,
            index_bytes,
            BufferUsage::INDEX,
            "scene-mesh-indices",
        ),
        format,
        count: count as u32,
        bounds: Bounds::of(vertices),
    }
}
