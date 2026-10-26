//! The renderer's side of the scene lane: the meshes and materials on the
//! device, each slot's frame, targets, environment and instance buffer, and
//! the passes that render them.

use std::sync::Arc;

use viso_gpu::backend::{DepthAttachment, DrawCommand, Geometry, IndexFormat, InlineUniforms};
use viso_gpu::backend::{RenderPass, RenderTarget};
use viso_gpu::scene::{
    Alpha, AoUniforms, ENV_HEIGHT, ENV_WIDTH, FrameUniforms, MATERIAL_DOUBLE_SIDED,
    MATERIAL_NORMAL_MAP, MAX_JOINTS, SHADOW_CASCADES, SceneProgram, Shading, SkinnedVertex,
    SkyUniforms, VertexExtra,
};
use viso_gpu::{
    AddressMode, BindGroupDesc, BindGroupId, Binding, BlendMode, BufferDesc, BufferId, BufferUsage,
    BuiltinShader, CompareFunction, CullMode, DepthState, FilterMode, GpuBackend, GpuPod, LoadOp,
    PipelineDesc, PipelineId, SamplerDesc, SamplerId, TextureDesc, TextureFormat, TextureId,
};
use viso_shader::scene_program_code;

use super::occlusion::BIAS_PER_DISTANCE;
use super::shadow::{self, Cascade};
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

/// The format of a shadow atlas: a packed depth in every texel.
const SHADOW_FORMAT: TextureFormat = TextureFormat::Rgba8Data;

/// The scene vertex as the pipeline's vertex layout reads it.
#[repr(C)]
#[derive(Clone, Copy, GpuPod)]
struct VertexLayout {
    position: [f32; 3],
    normal: [f32; 3],
    uv: [f32; 2],
    tangent: [f32; 4],
}

/// The skinned scene vertex as the pipeline's vertex layout reads it.
#[repr(C)]
#[derive(Clone, Copy, GpuPod)]
struct SkinnedLayout {
    position: [f32; 3],
    normal: [f32; 3],
    uv: [f32; 2],
    tangent: [f32; 4],
    joints: [u32; 2],
    weights: [u32; 2],
}

/// The scene vertex with its extra as the pipeline's vertex layout reads
/// it.
#[repr(C)]
#[derive(Clone, Copy, GpuPod)]
struct ExtraLayout {
    position: [f32; 3],
    normal: [f32; 3],
    uv: [f32; 2],
    tangent: [f32; 4],
    uv1: [f32; 2],
    color: [u32; 2],
}

/// The skinned scene vertex with its extra as the pipeline's vertex layout
/// reads it.
#[repr(C)]
#[derive(Clone, Copy, GpuPod)]
struct SkinnedExtraLayout {
    position: [f32; 3],
    normal: [f32; 3],
    uv: [f32; 2],
    tangent: [f32; 4],
    joints: [u32; 2],
    weights: [u32; 2],
    uv1: [f32; 2],
    color: [u32; 2],
}

/// The bytes of a joint palette's uniform block.
const PALETTE_SIZE: usize = MAX_JOINTS * 48;

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
    /// Target pairs (color and depth) allocated for scenes, not counting
    /// shadow atlases.
    pub allocations: u32,
    /// Environments uploaded to a slot.
    pub environments: u32,
    /// Shadow caster passes rendered, one per filled scene with shadows.
    pub shadow_passes: u32,
    /// Instanced draws of shadow casters, across all cascades.
    pub shadow_draws: u32,
    /// Ambient occlusion rendered, one per filled scene with it: each its
    /// depth, occlusion and two blur passes.
    pub occlusion_passes: u32,
}

/// A mesh on the device.
#[derive(Debug, Clone, Copy)]
struct GpuMesh {
    vertices: BufferId,
    indices: BufferId,
    format: IndexFormat,
    count: u32,
    bounds: Bounds,
    /// Its vertices are [`SkinnedVertex`]es.
    skinned: bool,
    /// Each of its vertices carries a [`VertexExtra`].
    extra: bool,
    /// The joints a skinned mesh's vertices follow with some weight, a bit
    /// each.
    joints: [u64; MAX_JOINTS / 64],
}

/// How a draw's placement turns the winding of its mesh's triangles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Winding {
    /// Every triangle keeps its winding.
    Kept,
    /// Every triangle's winding is reversed: the placement mirrors.
    Mirrored,
    /// Some triangles' joints mirror them and others' do not.
    Mixed,
}

impl Winding {
    /// An unskinned mesh's, placed by `instance`.
    fn of(instance: &SceneInstance) -> Winding {
        if instance.mirrors() {
            Winding::Mirrored
        } else {
            Winding::Kept
        }
    }

    /// A skinned mesh's whose vertices follow the joints `used` marks,
    /// posed by palette `rows` (three a joint; a joint past them stays in
    /// place) and placed by `instance`. A joint scaled flat turns nothing.
    fn of_skin(instance: &SceneInstance, rows: &[[f32; 4]], used: &[u64]) -> Winding {
        let (mut kept, mut mirrored) = (false, false);
        for (w, &word) in used.iter().enumerate() {
            let mut bits = word;
            while bits != 0 {
                let j = w * 64 + bits.trailing_zeros() as usize;
                bits &= bits - 1;
                let det = match rows.get(j * 3..j * 3 + 3) {
                    Some(r) => {
                        r[0][0] * (r[1][1] * r[2][2] - r[1][2] * r[2][1])
                            - r[0][1] * (r[1][0] * r[2][2] - r[1][2] * r[2][0])
                            + r[0][2] * (r[1][0] * r[2][1] - r[1][1] * r[2][0])
                    }
                    None => 1.0,
                };
                kept |= det > 0.0;
                mirrored |= det < 0.0;
            }
        }
        match (kept, mirrored, instance.mirrors()) {
            (true, true, _) => Winding::Mixed,
            (_, true, false) | (_, false, true) => Winding::Mirrored,
            _ => Winding::Kept,
        }
    }
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
    /// Lit by the light, so shadowed by it.
    lit: bool,
    /// Distinguishes it from a material that held its id before.
    generation: u32,
}

impl GpuMaterial {
    /// The index in [`SceneLane::pipelines`] of its pipeline over `mesh`,
    /// placed with `winding`: a mirrored placement's faces wind the other
    /// way, so a one-sided material culls its front faces instead; where
    /// joints mirror some triangles and not others, no face is culled by
    /// its winding and the skinned program culls each fragment by its
    /// facing.
    fn pipeline(&self, mesh: &GpuMesh, winding: Winding) -> usize {
        let cull = match winding {
            _ if self.double_sided => 1,
            Winding::Kept => 0,
            Winding::Mixed => 1,
            Winding::Mirrored => 2,
        };
        usize::from(mesh.extra) * 12
            + usize::from(mesh.skinned) * 6
            + usize::from(self.alpha == Alpha::Blend) * 3
            + cull
    }
}

/// The index in [`SceneLane::depth_pipelines`] of the depth program's
/// pipeline over `mesh`.
fn depth_index(mesh: &GpuMesh) -> usize {
    usize::from(mesh.extra) * 2 + usize::from(mesh.skinned)
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
    /// One triangle over the whole target, in clip space: what a screen
    /// program draws.
    screen: GpuMesh,
}

/// A slot's color and depth targets.
#[derive(Debug, Clone, Copy)]
struct Targets {
    color: TextureId,
    depth: TextureId,
    size: [u32; 2],
}

/// One instanced draw of a rendered scene: `count` instances of `mesh` in
/// `material` from instance `first`, through pipeline `pipeline`, posed by
/// the slot's palette `skin` for a skinned mesh.
#[derive(Debug, Clone, Copy)]
struct Draw {
    material: MaterialId,
    mesh: MeshId,
    first: u32,
    count: u32,
    pipeline: usize,
    skin: Option<u32>,
}

/// A slot's shadow atlas, its cascades' tiles side by side, and the depth
/// its casters are drawn against.
#[derive(Debug, Clone, Copy)]
struct ShadowTargets {
    color: TextureId,
    depth: TextureId,
    /// A tile's edge in texels.
    tile: u32,
}

/// One instanced draw of shadow casters into cascade `cascade`: `count`
/// instances of `mesh` from instance `first`, as material `material`
/// casts (the built-in lit one for every material that masks nothing).
#[derive(Debug, Clone, Copy)]
struct ShadowDraw {
    cascade: usize,
    material: MaterialId,
    mesh: MeshId,
    first: u32,
    count: u32,
    skin: Option<u32>,
}

/// A slot's ambient occlusion targets, half its targets' size: the view's
/// packed depth and the depth it is drawn against, the occlusion and the
/// blur's intermediate, and the three passes' bind groups over them.
#[derive(Debug, Clone, Copy)]
struct OcclusionTargets {
    view_depth: TextureId,
    depth: TextureId,
    /// The occlusion, blurred into it last: what the meshes sample.
    occlusion: TextureId,
    across: TextureId,
    size: [u32; 2],
    /// The occlusion pass's group, then the two blur passes'.
    groups: [BindGroupId; 3],
}

/// A caster of a fill.
#[derive(Debug, Clone, Copy)]
struct Caster {
    /// The material it casts as: its own when it masks, else the built-in
    /// lit one.
    material: MaterialId,
    mesh: MeshId,
    /// The slot's palette posing it, for a skinned mesh.
    skin: Option<u32>,
    instance: SceneInstance,
    /// Its world box.
    centre: [f32; 3],
    half: [f32; 3],
}

/// A visible blended instance of a fill, by its distance along the view.
#[derive(Debug, Clone, Copy)]
struct Blended {
    depth: f32,
    material: MaterialId,
    mesh: MeshId,
    skin: Option<u32>,
    instance: SceneInstance,
    /// Into [`SceneLane::pipelines`].
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
    /// The environment atlas, the shadow atlas and the ambient occlusion
    /// the groups bind.
    bound: Option<[TextureId; 3]>,
    draws: Vec<Draw>,
    shadow: Option<ShadowTargets>,
    /// Each cascade's frame uniform block, for its casters' draws.
    shadow_frames: [Option<BufferId>; SHADOW_CASCADES],
    /// Each casting material's bind groups, one per cascade, by
    /// [`MaterialId::index`], with the material's generation.
    shadow_groups: Vec<Option<(u32, [BindGroupId; SHADOW_CASCADES])>>,
    shadow_draws: Vec<ShadowDraw>,
    /// Each skinned draw's joint palette, by its place among the frame's
    /// skinned draws.
    palettes: Vec<BufferId>,
    /// Each palette's bind group, with the material and its generation it
    /// was made for.
    skin_groups: Vec<Option<(MaterialId, u32, BindGroupId)>>,
    /// Each palette's caster groups, one per cascade, likewise.
    skin_shadow_groups: Vec<Option<(MaterialId, u32, [BindGroupId; SHADOW_CASCADES])>>,
    used: [u32; 2],
    clear: [f32; 4],
    /// How many of `draws` are opaque: the sky is drawn after them, before
    /// the blended ones.
    opaque: usize,
    /// The sky's uniform block and bind group, while the frame has a sky.
    sky: Option<(BufferId, BindGroupId)>,
    /// The ambient occlusion's targets, while the frame has it.
    occlusion: Option<OcclusionTargets>,
    /// The occlusion passes' uniform blocks: the occlusion and the blur
    /// across, then the blur down.
    occlusion_blocks: Option<[BufferId; 2]>,
    /// The frame has a sky.
    sky_drawn: bool,
}

#[derive(Debug, Default)]
pub(crate) struct SceneLane {
    /// The mesh program's pipelines, over meshes then skinned meshes, each
    /// without and then with vertex extras: opaque or blended, each culling
    /// back faces, drawing both or culling front faces.
    pipelines: [Option<PipelineId>; 24],
    /// The depth program's pipelines, drawing shadow casters: over meshes
    /// and over skinned meshes, without and then with vertex extras.
    depth_pipelines: [Option<PipelineId>; 4],
    /// The sky program's pipeline.
    sky_pipeline: Option<PipelineId>,
    /// The occlusion program's and the occlusion blur's pipelines.
    ao_pipelines: [Option<PipelineId>; 2],
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
    /// The environments of the latest skies and lightings of frames that
    /// named none.
    skies: Vec<([u32; 7], Arc<Environment>)>,
    slots: Vec<Option<Slot>>,
    /// The bytes of a fill's surviving instances.
    scratch: Vec<u8>,
    /// A fill's visible blended instances, by their distance along the view.
    blended: Vec<Blended>,
    /// A fill's shadow casters.
    casters: Vec<Caster>,
    /// The bytes of a palette being written.
    palette_bytes: Vec<u8>,
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
        for (_, groups) in state.shadow_groups.into_iter().flatten() {
            for group in groups {
                backend.destroy_bind_group(group);
            }
        }
        for (_, _, group) in state.skin_groups.into_iter().flatten() {
            backend.destroy_bind_group(group);
        }
        for (_, _, groups) in state.skin_shadow_groups.into_iter().flatten() {
            for group in groups {
                backend.destroy_bind_group(group);
            }
        }
        for buffer in state.palettes {
            backend.destroy_buffer(buffer);
        }
        if let Some((buffer, group)) = state.sky {
            backend.destroy_bind_group(group);
            backend.destroy_buffer(buffer);
        }
        if let Some(targets) = state.occlusion {
            release_occlusion(backend, targets);
        }
        for buffer in state.occlusion_blocks.into_iter().flatten() {
            backend.destroy_buffer(buffer);
        }
        for buffer in state
            .uniforms
            .into_iter()
            .chain(state.shadow_frames.into_iter().flatten())
        {
            backend.destroy_buffer(buffer);
        }
        if let Some(shadow) = state.shadow {
            backend.destroy_texture(shadow.color);
            backend.destroy_texture(shadow.depth);
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
        let mut bytes = vec![0u8; vertices.len() * SceneVertex::SIZE];
        for (v, out) in vertices
            .iter()
            .zip(bytes.as_chunks_mut::<{ SceneVertex::SIZE }>().0)
        {
            v.write_to(out);
        }
        let bounds = Bounds::around(vertices.iter().map(|v| v.position));
        self.add_mesh(backend, &bytes, vertices.len(), bounds, indices, None)
    }

    /// Upload a mesh of `vertices`, each with its extra in `extras` (the
    /// default past them), and triangle `indices`.
    pub(crate) fn create_mesh_extra<B: GpuBackend>(
        &mut self,
        backend: &mut B,
        vertices: &[SceneVertex],
        extras: &[VertexExtra],
        indices: &[u32],
    ) -> MeshId {
        const SIZE: usize = SceneVertex::SIZE + VertexExtra::SIZE;
        let mut bytes = vec![0u8; vertices.len() * SIZE];
        for (i, (v, out)) in vertices
            .iter()
            .zip(bytes.as_chunks_mut::<SIZE>().0)
            .enumerate()
        {
            v.write_to(out);
            let extra = extras.get(i).copied().unwrap_or_default();
            extra.write_to(&mut out[SceneVertex::SIZE..]);
        }
        let bounds = Bounds::around(vertices.iter().map(|v| v.position));
        let id = self.add_mesh(backend, &bytes, vertices.len(), bounds, indices, None);
        self.mark_extra(id);
        id
    }

    /// Upload a skinned mesh of `vertices` in their bind pose and triangle
    /// `indices`.
    pub(crate) fn create_skinned_mesh<B: GpuBackend>(
        &mut self,
        backend: &mut B,
        vertices: &[SkinnedVertex],
        indices: &[u32],
    ) -> MeshId {
        let mut bytes = vec![0u8; vertices.len() * SkinnedVertex::SIZE];
        for (v, out) in vertices
            .iter()
            .zip(bytes.as_chunks_mut::<{ SkinnedVertex::SIZE }>().0)
        {
            v.write_to(out);
        }
        let bounds = Bounds::around(vertices.iter().map(|v| v.position));
        let joints = used_joints(vertices);
        self.add_mesh(
            backend,
            &bytes,
            vertices.len(),
            bounds,
            indices,
            Some(joints),
        )
    }

    /// Upload a skinned mesh of `vertices` in their bind pose, each with its
    /// extra in `extras` (the default past them), and triangle `indices`.
    pub(crate) fn create_skinned_mesh_extra<B: GpuBackend>(
        &mut self,
        backend: &mut B,
        vertices: &[SkinnedVertex],
        extras: &[VertexExtra],
        indices: &[u32],
    ) -> MeshId {
        const SIZE: usize = SkinnedVertex::SIZE + VertexExtra::SIZE;
        let mut bytes = vec![0u8; vertices.len() * SIZE];
        for (i, (v, out)) in vertices
            .iter()
            .zip(bytes.as_chunks_mut::<SIZE>().0)
            .enumerate()
        {
            v.write_to(out);
            let extra = extras.get(i).copied().unwrap_or_default();
            extra.write_to(&mut out[SkinnedVertex::SIZE..]);
        }
        let bounds = Bounds::around(vertices.iter().map(|v| v.position));
        let joints = used_joints(vertices);
        let id = self.add_mesh(
            backend,
            &bytes,
            vertices.len(),
            bounds,
            indices,
            Some(joints),
        );
        self.mark_extra(id);
        id
    }

    /// Mark uploaded mesh `id` as carrying vertex extras.
    fn mark_extra(&mut self, id: MeshId) {
        if let Some(Some(mesh)) = self.meshes.get_mut(id.index()) {
            mesh.extra = true;
        }
    }

    /// Upload a mesh of `count` vertices `vertex_bytes` inside `bounds`,
    /// skinned when the joints its vertices follow are given.
    fn add_mesh<B: GpuBackend>(
        &mut self,
        backend: &mut B,
        vertex_bytes: &[u8],
        count: usize,
        bounds: Bounds,
        indices: &[u32],
        joints: Option<[u64; MAX_JOINTS / 64]>,
    ) -> MeshId {
        let narrow = count <= usize::from(u16::MAX) + 1;
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
        let mut mesh = upload(backend, vertex_bytes, &index_bytes, format, indices.len());
        mesh.bounds = bounds;
        mesh.skinned = joints.is_some();
        mesh.joints = joints.unwrap_or_default();
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
        let corner = |x: f32, y: f32| SceneVertex {
            position: [x, y, 0.0],
            ..SceneVertex::default()
        };
        let mut vertex_bytes = [0u8; SceneVertex::SIZE * 3];
        for (v, out) in [corner(-1.0, -1.0), corner(3.0, -1.0), corner(-1.0, 3.0)]
            .iter()
            .zip(vertex_bytes.as_chunks_mut::<{ SceneVertex::SIZE }>().0)
        {
            v.write_to(out);
        }
        // Padded to whole words, as every device writes buffers.
        let index_bytes: Vec<u8> = [0u16, 1, 2, 0]
            .iter()
            .flat_map(|i| i.to_le_bytes())
            .collect();
        let screen = upload(backend, &vertex_bytes, &index_bytes, IndexFormat::U16, 3);
        let shared = Shared {
            white,
            spare,
            linear,
            point,
            screen,
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
            lit: uniforms.shading() == Shading::Lit,
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
            if let Some(entry) = slot.shadow_groups.get_mut(id.index())
                && let Some((_, groups)) = entry.take()
            {
                for group in groups {
                    backend.destroy_bind_group(group);
                }
            }
            for entry in &mut slot.skin_groups {
                if let Some((m, _, group)) = *entry
                    && m == id
                {
                    backend.destroy_bind_group(group);
                    *entry = None;
                }
            }
            for entry in &mut slot.skin_shadow_groups {
                if let Some((m, _, groups)) = *entry
                    && m == id
                {
                    for group in groups {
                        backend.destroy_bind_group(group);
                    }
                    *entry = None;
                }
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
        let mut vertex_bytes = vec![0u8; vertices.len() * SceneVertex::SIZE];
        for (v, out) in vertices
            .iter()
            .zip(vertex_bytes.as_chunks_mut::<{ SceneVertex::SIZE }>().0)
        {
            v.write_to(out);
        }
        let mut mesh = upload(
            backend,
            &vertex_bytes,
            &bytes,
            IndexFormat::U16,
            indices.len(),
        );
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
        *self.pipelines[index].get_or_insert_with(|| {
            let cull = [CullMode::Back, CullMode::None, CullMode::Front][index % 3];
            let mut program = if index % 12 >= 6 {
                SceneProgram::SkinnedMesh
            } else {
                SceneProgram::Mesh
            };
            if index >= 12 {
                program = program.with_extra();
            }
            mesh_pipeline(backend, program, index % 6 >= 3, cull)
        })
    }

    /// The environment of `frame`'s sky, or else of its lighting's sky and
    /// ground colors, built once for the latest few.
    fn environment(&mut self, frame: &SceneFrame) -> Arc<Environment> {
        if let Some(env) = &frame.environment {
            return Arc::clone(env);
        }
        let l = &frame.lighting;
        let key = match &frame.sky {
            Some(sky) => {
                let k = sky.key();
                [1, k[0], k[1], k[2], k[3], k[4], k[5]]
            }
            None => [
                0,
                l.sky[0].to_bits(),
                l.sky[1].to_bits(),
                l.sky[2].to_bits(),
                l.ground[0].to_bits(),
                l.ground[1].to_bits(),
                l.ground[2].to_bits(),
            ],
        };
        if let Some(i) = self.skies.iter().position(|(k, _)| *k == key) {
            let entry = self.skies.remove(i);
            let env = Arc::clone(&entry.1);
            self.skies.push(entry);
            return env;
        }
        let env = Arc::new(match &frame.sky {
            Some(sky) => Environment::from_sky(sky),
            None => l.sky_environment(),
        });
        if self.skies.len() >= KEPT_SKIES {
            self.skies.remove(0);
        }
        self.skies.push((key, Arc::clone(&env)));
        env
    }

    /// Sizing `draw`'s slot to its device pixels at `scale`, and fill it when
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
            self.fill(backend, i, size, max_size);
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

    /// Cull slot `i`'s frame into its instance buffer, write its uniforms,
    /// palettes and environment, fit its shadow, and record its draws, for a
    /// target `size` pixels large on a device whose textures are at most
    /// `max_size` texels wide.
    fn fill<B: GpuBackend>(&mut self, backend: &mut B, i: usize, size: [u32; 2], max_size: u32) {
        let mut scratch = core::mem::take(&mut self.scratch);
        scratch.clear();
        let mut blended = core::mem::take(&mut self.blended);
        blended.clear();
        let mut casters = core::mem::take(&mut self.casters);
        casters.clear();
        let mut slot = self.slots[i].take().expect("the caller checked the slot");
        let used = slot.used;
        let aspect = used[0] as f32 / used[1] as f32;
        let view_proj = slot.frame.camera.view_proj(aspect);
        let frustum = Frustum::of(&view_proj);
        let (right, up, forward) = slot.frame.camera.basis();
        let eye = slot.frame.camera.eye;
        let lighting = slot.frame.lighting;
        let shadows = lighting.shadows;
        let casting = shadows.strength > 0.0 && lighting.intensity > 0.0;
        // The casters' world box and the receivers' depths along the view.
        let mut caster_box = ([f32::MAX; 3], [f32::MIN; 3]);
        let mut receivers = (f32::MAX, f32::MIN);
        let mut total = 0u32;
        let mut draws = core::mem::take(&mut slot.draws);
        draws.clear();
        let push = |scratch: &mut Vec<u8>, instance: &SceneInstance| {
            let at = scratch.len();
            scratch.resize(at + SceneInstance::SIZE, 0);
            instance.write_to(&mut scratch[at..]);
        };
        // One instance of a mesh in a material, posed by palette `skin`:
        // kept as a caster, culled, then drawn now or with the blended.
        let mut place = |scratch: &mut Vec<u8>,
                         draws: &mut Vec<Draw>,
                         stats: &mut SceneStats,
                         (material, gpu): (MaterialId, &GpuMaterial),
                         (mesh_id, mesh): (MeshId, &GpuMesh),
                         bounds: &Bounds,
                         skin: Option<u32>,
                         instance: &SceneInstance,
                         winding: Winding| {
            let (centre, half) = bounds.placed(instance);
            if casting && gpu.alpha != Alpha::Blend && instance.color[3] > 0.0 {
                let cast_as = if gpu.alpha == Alpha::Mask {
                    material
                } else {
                    MaterialId::LIT
                };
                casters.push(Caster {
                    material: cast_as,
                    mesh: mesh_id,
                    skin,
                    instance: *instance,
                    centre,
                    half,
                });
                for k in 0..3 {
                    caster_box.0[k] = caster_box.0[k].min(centre[k] - half[k]);
                    caster_box.1[k] = caster_box.1[k].max(centre[k] + half[k]);
                }
            }
            if !frustum.sees(centre, half) {
                stats.culled += 1;
                return;
            }
            let depth = (super::vec3(centre[0], centre[1], centre[2]) - eye).dot(forward);
            if gpu.lit {
                let reach = super::vec3(half[0], half[1], half[2]).length();
                receivers = (
                    receivers.0.min(depth - reach),
                    receivers.1.max(depth + reach),
                );
            }
            let pipeline = gpu.pipeline(mesh, winding);
            if gpu.alpha == Alpha::Blend {
                blended.push(Blended {
                    depth,
                    material,
                    mesh: mesh_id,
                    skin,
                    instance: *instance,
                    pipeline,
                });
                return;
            }
            push(scratch, instance);
            match draws.last_mut() {
                Some(d)
                    if skin.is_none()
                        && d.skin.is_none()
                        && d.pipeline == pipeline
                        && d.material == material
                        && d.mesh == mesh_id
                        && d.first + d.count == total =>
                {
                    d.count += 1;
                }
                _ => draws.push(Draw {
                    material,
                    mesh: mesh_id,
                    first: total,
                    count: 1,
                    pipeline,
                    skin,
                }),
            }
            total += 1;
        };
        for (material, mesh_id, batch) in slot.frame.batches() {
            let Some(mesh) = self.mesh(backend, mesh_id).filter(|m| !m.skinned) else {
                continue;
            };
            let (material, gpu) = self.material(material);
            for instance in batch {
                place(
                    &mut scratch,
                    &mut draws,
                    &mut self.stats,
                    (material, &gpu),
                    (mesh_id, &mesh),
                    &mesh.bounds,
                    None,
                    instance,
                    Winding::of(instance),
                );
            }
        }
        // Each skinned draw with its palette, written to the slot's buffer
        // of its place.
        let mut skins = 0u32;
        for (d, rows) in slot.frame.skinned() {
            let Some(mesh) = self.mesh(backend, d.mesh).filter(|m| m.skinned) else {
                continue;
            };
            let (material, gpu) = self.material(d.material);
            let k = skins as usize;
            if slot.palettes.len() <= k {
                slot.palettes.push(backend.create_buffer(&BufferDesc {
                    size: PALETTE_SIZE,
                    usage: BufferUsage::UNIFORM | BufferUsage::CPU_WRITE,
                    label: "scene-palette",
                }));
            }
            let bytes = &mut self.palette_bytes;
            bytes.clear();
            for row in rows {
                for v in row {
                    bytes.extend_from_slice(&v.to_le_bytes());
                }
            }
            backend.write_buffer(slot.palettes[k], 0, bytes);
            place(
                &mut scratch,
                &mut draws,
                &mut self.stats,
                (material, &gpu),
                (d.mesh, &mesh),
                &d.bounds,
                Some(skins),
                &d.instance,
                Winding::of_skin(&d.instance, rows, &mesh.joints),
            );
            skins += 1;
        }
        let opaque = draws.len();
        // A sky draws over an instance buffer too, when nothing else does.
        if slot.frame.sky.is_some() && scratch.is_empty() && blended.is_empty() {
            push(&mut scratch, &SceneInstance::default());
            total += 1;
        }
        let camera = slot.frame.camera;
        let sprite_pipeline = match slot.frame.sprites() {
            [] => 0,
            _ => match self.mesh(backend, Shape::Plane.into()) {
                Some(plane) => self
                    .material(MaterialId::SPRITE)
                    .1
                    .pipeline(&plane, Winding::Kept),
                None => 0,
            },
        };
        for sprite in slot.frame.sprites() {
            if !frustum.sees(sprite.position.to_array(), [sprite.radius; 3]) {
                self.stats.culled += 1;
                continue;
            }
            let depth = (sprite.position - eye).dot(forward);
            let least = MIN_SPRITE_PIXELS * camera.pixel_at(depth, used[1]);
            blended.push(Blended {
                depth,
                material: MaterialId::SPRITE,
                mesh: Shape::Plane.into(),
                skin: None,
                instance: sprite.facing((right, up, forward), least),
                pipeline: sprite_pipeline,
            });
        }
        if !blended.is_empty() {
            self.mesh(backend, Shape::Plane.into());
            // Back to front, so each blends over what is behind it; a run of
            // one mesh in one material is one draw.
            blended.sort_by(|a, b| b.depth.total_cmp(&a.depth));
            for b in &blended {
                push(&mut scratch, &b.instance);
                let pipeline = b.pipeline;
                match draws.last_mut() {
                    Some(d)
                        if b.skin.is_none()
                            && d.skin.is_none()
                            && d.pipeline == pipeline
                            && d.material == b.material
                            && d.mesh == b.mesh
                            && d.first + d.count == total =>
                    {
                        d.count += 1;
                    }
                    _ => draws.push(Draw {
                        material: b.material,
                        mesh: b.mesh,
                        first: total,
                        count: 1,
                        pipeline,
                        skin: b.skin,
                    }),
                }
                total += 1;
            }
        }
        // The shadow: cascades fitted to the receivers within its reach,
        // each drawing the casters that reach its tile, a run of one mesh
        // cast as one material a draw.
        let mut shadow_draws = core::mem::take(&mut slot.shadow_draws);
        shadow_draws.clear();
        let near = receivers.0.max(camera.near);
        let far = receivers.1.min(shadows.distance);
        let mut fitted_shadow: Option<([Cascade; SHADOW_CASCADES], u32)> = None;
        if !casters.is_empty() && far > near {
            let tile = shadows.tile(used, max_size);
            let d = lighting.direction;
            let light = if d.length() > 1e-6 {
                d.normalize()
            } else {
                super::vec3(0.0, 1.0, 0.0)
            };
            let cascades = shadow::fit(
                &camera,
                aspect,
                light.to_array(),
                (near, far),
                caster_box,
                tile,
            );
            casters.sort_unstable_by_key(|c| (c.skin, c.material.index(), c.mesh));
            for (cascade, fit) in cascades.iter().enumerate() {
                for c in &casters {
                    if !fit.reaches(c.centre, c.half) {
                        continue;
                    }
                    push(&mut scratch, &c.instance);
                    match shadow_draws.last_mut() {
                        Some(d)
                            if c.skin.is_none()
                                && d.skin.is_none()
                                && d.cascade == cascade
                                && d.material == c.material
                                && d.mesh == c.mesh
                                && d.first + d.count == total =>
                        {
                            d.count += 1;
                        }
                        _ => shadow_draws.push(ShadowDraw {
                            cascade,
                            material: c.material,
                            mesh: c.mesh,
                            first: total,
                            count: 1,
                            skin: c.skin,
                        }),
                    }
                    total += 1;
                }
            }
            if !shadow_draws.is_empty() {
                fitted_shadow = Some((cascades, tile));
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
        // The shadow atlas: made for the tile the fit needs, let go of when
        // nothing casts.
        let shadow_texture = match fitted_shadow {
            Some((_, tile)) => match slot.shadow {
                Some(t) if t.tile == tile => Some(t.color),
                old => {
                    if let Some(old) = old {
                        backend.destroy_texture(old.color);
                        backend.destroy_texture(old.depth);
                    }
                    let texture = |backend: &mut B, format, label| {
                        backend.create_texture(&TextureDesc {
                            width: tile * SHADOW_CASCADES as u32,
                            height: tile,
                            format,
                            render_target: true,
                            label,
                        })
                    };
                    let targets = ShadowTargets {
                        color: texture(backend, SHADOW_FORMAT, "scene-shadow"),
                        depth: texture(backend, TextureFormat::Depth32Float, "scene-shadow-depth"),
                        tile,
                    };
                    slot.shadow = Some(targets);
                    Some(targets.color)
                }
            },
            None => {
                if let Some(old) = slot.shadow.take() {
                    backend.destroy_texture(old.color);
                    backend.destroy_texture(old.depth);
                }
                None
            }
        };
        // The frame's uniforms and environment.
        let env = self.environment(&slot.frame);
        let env_texture = match &slot.env {
            Some((texture, held)) if Arc::ptr_eq(held, &env) => *texture,
            held => {
                // A frame in flight may still sample the old atlas: it is
                // retired with every group that binds it.
                if let Some((old, _)) = held {
                    backend.destroy_texture(*old);
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
        let shared = self.shared(backend);
        let new_frame = |backend: &mut B, label| {
            backend.create_buffer(&BufferDesc {
                size: FrameUniforms::SIZE,
                usage: BufferUsage::UNIFORM | BufferUsage::CPU_WRITE,
                label,
            })
        };
        let frame_buffer = match slot.uniforms {
            Some(buffer) => buffer,
            None => *slot.uniforms.insert(new_frame(backend, "scene-frame")),
        };
        // The ambient occlusion: half the targets' size, made while an
        // opaque lit mesh is drawn, let go of otherwise.
        let occlusion = lighting.occlusion;
        let occluding = occlusion.strength > 0.0
            && draws[..opaque]
                .iter()
                .any(|d| self.material(d.material).1.lit);
        let ao_size = size.map(|n| (n / 2).max(1));
        if slot
            .occlusion
            .is_some_and(|t| !occluding || t.size != ao_size)
        {
            let old = slot.occlusion.take().expect("checked");
            release_occlusion(backend, old);
        }
        if occluding {
            let blocks = *slot.occlusion_blocks.get_or_insert_with(|| {
                [0, 1].map(|_| {
                    backend.create_buffer(&BufferDesc {
                        size: AoUniforms::SIZE,
                        usage: BufferUsage::UNIFORM | BufferUsage::CPU_WRITE,
                        label: "scene-occlusion",
                    })
                })
            });
            if slot.occlusion.is_none() {
                let texture = |backend: &mut B, format, label| {
                    backend.create_texture(&TextureDesc {
                        width: ao_size[0],
                        height: ao_size[1],
                        format,
                        render_target: true,
                        label,
                    })
                };
                let view_depth = texture(backend, SHADOW_FORMAT, "scene-view-depth");
                let depth = texture(
                    backend,
                    TextureFormat::Depth32Float,
                    "scene-occlusion-depth",
                );
                let occlusion = texture(backend, SHADOW_FORMAT, "scene-occlusion");
                let across = texture(backend, SHADOW_FORMAT, "scene-occlusion-across");
                let group = |backend: &mut B, block: BufferId, read: TextureId| {
                    let mut bindings = vec![
                        Binding::Uniform(frame_buffer),
                        Binding::Uniform(block),
                        Binding::Uniform(shared.spare),
                    ];
                    bindings.extend([Binding::Texture(shared.white); 6]);
                    bindings.extend([
                        Binding::Texture(read),
                        Binding::Texture(shared.white),
                        Binding::Sampler(shared.linear),
                        Binding::Sampler(shared.linear),
                        Binding::Sampler(shared.point),
                    ]);
                    backend.create_bind_group(&BindGroupDesc {
                        label: "scene-occlusion",
                        bindings,
                    })
                };
                let groups = [
                    group(backend, blocks[0], view_depth),
                    group(backend, blocks[0], occlusion),
                    group(backend, blocks[1], across),
                ];
                slot.occlusion = Some(OcclusionTargets {
                    view_depth,
                    depth,
                    occlusion,
                    across,
                    size: ao_size,
                    groups,
                });
            }
            for k in 0..2 {
                self.ao_pipelines[k].get_or_insert_with(|| ao_pipeline(backend, k == 1));
            }
            for d in &draws[..opaque] {
                if let Some(Some(mesh)) = self.meshes.get(d.mesh.index()) {
                    let k = depth_index(mesh);
                    self.depth_pipelines[k].get_or_insert_with(|| depth_pipeline(backend, k));
                }
            }
        }
        let bound = [
            env_texture,
            shadow_texture.unwrap_or(shared.white),
            slot.occlusion.map_or(shared.white, |t| t.occlusion),
        ];
        if slot.bound != Some(bound) {
            for (_, group) in slot.groups.iter_mut().filter_map(Option::take) {
                backend.destroy_bind_group(group);
            }
            for (_, _, group) in slot.skin_groups.iter_mut().filter_map(Option::take) {
                backend.destroy_bind_group(group);
            }
            slot.bound = Some(bound);
        }
        let fitted = fit_view(&view_proj, used, size);
        let mut uniforms = slot.frame.uniforms(&fitted, &env.sh);
        let mut bytes = [0u8; FrameUniforms::SIZE];
        if let Some((cascades, tile)) = &fitted_shadow {
            for (c, fit) in cascades.iter().enumerate() {
                uniforms.shadow[c * 4..c * 4 + 3].copy_from_slice(&fit.rows);
                uniforms.shadow[c * 4 + 3] = [fit.bias, fit.texel, 0.0, 0.0];
                let buffer = match slot.shadow_frames[c] {
                    Some(buffer) => buffer,
                    None => *slot.shadow_frames[c].insert(new_frame(backend, "scene-shadow-frame")),
                };
                let caster_frame = FrameUniforms {
                    view_proj: fit.clip(c).v,
                    ..FrameUniforms::default()
                };
                caster_frame.write_to(&mut bytes);
                backend.write_buffer(buffer, 0, &bytes);
            }
            uniforms.shadow_params = [shadows.strength.min(1.0), 1.0 / *tile as f32, 0.0, 0.0];
        }
        if let (Some(targets), Some(blocks)) = (slot.occlusion, slot.occlusion_blocks) {
            uniforms.occlusion = [
                occlusion.strength.min(1.0),
                1.0 / size[0] as f32,
                1.0 / size[1] as f32,
                0.0,
            ];
            let [w, h] = targets.size.map(|n| n as f32);
            let mut block = AoUniforms {
                inv_view_proj: fitted.invert().v,
                // The bias grows with distance at least as fast as a texel's
                // footprint, so a far surface seen at a grazing angle never
                // occludes itself between texels.
                params: [
                    occlusion.radius.max(1e-6),
                    occlusion.bias.max(1e-6),
                    BIAS_PER_DISTANCE.max(slot.frame.camera.pixel_at(1.0, targets.size[1]) * 0.5),
                    0.0,
                ],
                texel: [1.0 / w, 1.0 / h, w, h],
                step: [1.0 / w, 0.0, 0.0, 0.0],
            };
            let mut ao_bytes = [0u8; AoUniforms::SIZE];
            block.write_to(&mut ao_bytes);
            backend.write_buffer(blocks[0], 0, &ao_bytes);
            block.step = [0.0, 1.0 / h, 0.0, 0.0];
            block.write_to(&mut ao_bytes);
            backend.write_buffer(blocks[1], 0, &ao_bytes);
        }
        uniforms.write_to(&mut bytes);
        backend.write_buffer(frame_buffer, 0, &bytes);
        // The sky's block, and its group over it and the frame's.
        if let Some(sky) = &slot.frame.sky {
            self.sky_pipeline
                .get_or_insert_with(|| sky_pipeline(backend));
            let mut block = sky.uniforms(&fitted.invert().v);
            // The sun's disc at least a pixel and a half across, so a small
            // view still shows it.
            if block.sun[3] > 0.0 {
                let pixel = slot.frame.camera.pixel_at(1.0, used[1]);
                block.sun[3] = block.sun[3].max(pixel * 0.75);
            }
            let mut sky_bytes = [0u8; SkyUniforms::SIZE];
            block.write_to(&mut sky_bytes);
            let (buffer, _) = *slot.sky.get_or_insert_with(|| {
                let buffer = backend.create_buffer(&BufferDesc {
                    size: SkyUniforms::SIZE,
                    usage: BufferUsage::UNIFORM | BufferUsage::CPU_WRITE,
                    label: "scene-sky",
                });
                let mut bindings = vec![
                    Binding::Uniform(frame_buffer),
                    Binding::Uniform(buffer),
                    Binding::Uniform(shared.spare),
                ];
                bindings.extend([Binding::Texture(shared.white); 8]);
                bindings.extend([
                    Binding::Sampler(shared.linear),
                    Binding::Sampler(shared.linear),
                    Binding::Sampler(shared.point),
                ]);
                let group = backend.create_bind_group(&BindGroupDesc {
                    label: "scene-sky",
                    bindings,
                });
                (buffer, group)
            });
            backend.write_buffer(buffer, 0, &sky_bytes);
        }
        slot.sky_drawn = slot.frame.sky.is_some();
        // Every drawn material's bind group, and the pipelines they need: a
        // skinned draw's with its palette.
        let group_of = |backend: &mut B, gpu: &GpuMaterial, palette: BufferId| {
            let mut bindings = vec![
                Binding::Uniform(frame_buffer),
                Binding::Uniform(gpu.buffer),
                Binding::Uniform(palette),
            ];
            bindings.extend(
                gpu.maps
                    .map(|m| Binding::Texture(m.unwrap_or(shared.white))),
            );
            bindings.extend([
                Binding::Texture(bound[0]),
                Binding::Texture(bound[1]),
                Binding::Texture(bound[2]),
                Binding::Sampler(gpu.sampler),
                Binding::Sampler(shared.linear),
                Binding::Sampler(shared.point),
            ]);
            backend.create_bind_group(&BindGroupDesc {
                label: "scene",
                bindings,
            })
        };
        for d in &draws {
            self.pipeline(backend, d.pipeline);
            let (material, gpu) = self.material(d.material);
            match d.skin {
                Some(k) => {
                    let k = k as usize;
                    if slot.skin_groups.len() <= k {
                        slot.skin_groups.resize(k + 1, None);
                    }
                    match slot.skin_groups[k] {
                        Some((m, generation, _))
                            if m == material && generation == gpu.generation =>
                        {
                            continue;
                        }
                        Some((_, _, stale)) => backend.destroy_bind_group(stale),
                        None => {}
                    }
                    let group = group_of(backend, &gpu, slot.palettes[k]);
                    slot.skin_groups[k] = Some((material, gpu.generation, group));
                }
                None => {
                    let index = material.index();
                    if slot.groups.len() <= index {
                        slot.groups.resize(index + 1, None);
                    }
                    match slot.groups[index] {
                        Some((generation, _)) if generation == gpu.generation => continue,
                        Some((_, stale)) => backend.destroy_bind_group(stale),
                        None => {}
                    }
                    let group = group_of(backend, &gpu, shared.spare);
                    slot.groups[index] = Some((gpu.generation, group));
                }
            }
        }
        // Every casting material's groups, one per cascade.
        let shadow_groups_of = |backend: &mut B, gpu: &GpuMaterial, palette: BufferId| {
            core::array::from_fn(|c| {
                let frame = slot.shadow_frames[c].expect("every cascade's frame is written");
                let mut bindings = vec![
                    Binding::Uniform(frame),
                    Binding::Uniform(gpu.buffer),
                    Binding::Uniform(palette),
                    Binding::Texture(gpu.maps[0].unwrap_or(shared.white)),
                ];
                bindings.extend([Binding::Texture(shared.white); 7]);
                bindings.extend([
                    Binding::Sampler(gpu.sampler),
                    Binding::Sampler(shared.linear),
                    Binding::Sampler(shared.point),
                ]);
                backend.create_bind_group(&BindGroupDesc {
                    label: "scene-shadow",
                    bindings,
                })
            })
        };
        for d in &shadow_draws {
            if let Some(Some(mesh)) = self.meshes.get(d.mesh.index()) {
                let k = depth_index(mesh);
                self.depth_pipelines[k].get_or_insert_with(|| depth_pipeline(backend, k));
            }
            let (material, gpu) = self.material(d.material);
            match d.skin {
                Some(k) => {
                    let k = k as usize;
                    if slot.skin_shadow_groups.len() <= k {
                        slot.skin_shadow_groups.resize(k + 1, None);
                    }
                    match slot.skin_shadow_groups[k] {
                        Some((m, generation, _))
                            if m == material && generation == gpu.generation =>
                        {
                            continue;
                        }
                        Some((_, _, stale)) => {
                            for group in stale {
                                backend.destroy_bind_group(group);
                            }
                        }
                        None => {}
                    }
                    let groups = shadow_groups_of(backend, &gpu, slot.palettes[k]);
                    slot.skin_shadow_groups[k] = Some((material, gpu.generation, groups));
                }
                None => {
                    let index = material.index();
                    if slot.shadow_groups.len() <= index {
                        slot.shadow_groups.resize(index + 1, None);
                    }
                    match slot.shadow_groups[index] {
                        Some((generation, _)) if generation == gpu.generation => continue,
                        Some((_, stale)) => {
                            for group in stale {
                                backend.destroy_bind_group(group);
                            }
                        }
                        None => {}
                    }
                    let groups = shadow_groups_of(backend, &gpu, shared.spare);
                    slot.shadow_groups[index] = Some((gpu.generation, groups));
                }
            }
        }
        slot.draws = draws;
        slot.opaque = opaque;
        slot.shadow_draws = shadow_draws;
        slot.clear = slot.frame.clear_color();
        self.slots[i] = Some(slot);
        self.scratch = scratch;
        self.blended = blended;
        self.casters = casters;
    }

    /// Append the passes of every slot filled since it last rendered: its
    /// shadow casters, then its scene.
    pub(crate) fn encode(&mut self, commands: &mut Vec<DrawCommand>, passes: &mut Vec<RenderPass>) {
        for slot in self.slots.iter_mut().flatten() {
            let Some(targets) = slot.targets.filter(|_| slot.pending) else {
                continue;
            };
            slot.pending = false;
            let Some((instances, _)) = slot.instances else {
                continue;
            };
            let instanced = |mesh: &GpuMesh, first: u32, count: u32| {
                (
                    Geometry::InstancedMesh {
                        vertex_buffer: mesh.vertices,
                        index_buffer: mesh.indices,
                        index_format: mesh.format,
                        index_offset: 0,
                        index_count: mesh.count,
                        count,
                    },
                    first as usize * SceneInstance::SIZE,
                )
            };
            if let Some(shadow) = slot.shadow
                && !slot.shadow_draws.is_empty()
            {
                let first_command = commands.len() as u32;
                for draw in &slot.shadow_draws {
                    let Some(Some(mesh)) = self.meshes.get(draw.mesh.index()) else {
                        continue;
                    };
                    let groups = match draw.skin {
                        Some(k) => slot
                            .skin_shadow_groups
                            .get(k as usize)
                            .copied()
                            .flatten()
                            .map(|(_, _, g)| g),
                        None => slot
                            .shadow_groups
                            .get(draw.material.index())
                            .copied()
                            .flatten()
                            .map(|(_, g)| g),
                    };
                    let (Some(groups), Some(pipeline)) =
                        (groups, self.depth_pipelines[depth_index(mesh)])
                    else {
                        continue;
                    };
                    let (geometry, instance_offset) = instanced(mesh, draw.first, draw.count);
                    let tile = shadow.tile;
                    commands.push(DrawCommand {
                        pipeline,
                        bind_group: Some(groups[draw.cascade]),
                        geometry,
                        instance_buffer: instances,
                        instance_offset,
                        uniforms: InlineUniforms::EMPTY,
                        scissor: Some((draw.cascade as u32 * tile, 0, tile, tile)),
                    });
                    self.stats.shadow_draws += 1;
                }
                passes.push(RenderPass {
                    target: RenderTarget::Texture(shadow.color),
                    load: LoadOp::Clear([1.0; 4]),
                    depth: Some(DepthAttachment {
                        texture: shadow.depth,
                        clear: 0.0,
                    }),
                    first_command,
                    command_count: commands.len() as u32 - first_command,
                });
                self.stats.shadow_passes += 1;
            }
            // The ambient occlusion: the view's depth of the opaque draws,
            // its occlusion, then the blur across and down.
            if let (Some(ao), [Some(raw), Some(blur)], Some(shared)) =
                (slot.occlusion, self.ao_pipelines, self.shared)
            {
                let scissor = Some((
                    slot.used[0].div_ceil(2).min(ao.size[0]),
                    slot.used[1].div_ceil(2).min(ao.size[1]),
                ))
                .map(|(w, h)| (0, 0, w, h));
                let pass = |target, clear, first: u32, count: u32| RenderPass {
                    target: RenderTarget::Texture(target),
                    load: LoadOp::Clear(clear),
                    depth: Some(DepthAttachment {
                        texture: ao.depth,
                        clear: 0.0,
                    }),
                    first_command: first,
                    command_count: count,
                };
                let first_command = commands.len() as u32;
                for draw in &slot.draws[..slot.opaque.min(slot.draws.len())] {
                    let Some(Some(mesh)) = self.meshes.get(draw.mesh.index()) else {
                        continue;
                    };
                    let group = match draw.skin {
                        Some(k) => slot
                            .skin_groups
                            .get(k as usize)
                            .copied()
                            .flatten()
                            .map(|(_, _, g)| g),
                        None => slot
                            .groups
                            .get(draw.material.index())
                            .copied()
                            .flatten()
                            .map(|(_, g)| g),
                    };
                    let (Some(group), Some(pipeline)) =
                        (group, self.depth_pipelines[depth_index(mesh)])
                    else {
                        continue;
                    };
                    let (geometry, instance_offset) = instanced(mesh, draw.first, draw.count);
                    commands.push(DrawCommand {
                        pipeline,
                        bind_group: Some(group),
                        geometry,
                        instance_buffer: instances,
                        instance_offset,
                        uniforms: InlineUniforms::EMPTY,
                        scissor,
                    });
                }
                passes.push(pass(
                    ao.view_depth,
                    [1.0; 4],
                    first_command,
                    commands.len() as u32 - first_command,
                ));
                let (geometry, instance_offset) = instanced(&shared.screen, 0, 1);
                for (k, (pipeline, target)) in
                    [(raw, ao.occlusion), (blur, ao.across), (blur, ao.occlusion)]
                        .into_iter()
                        .enumerate()
                {
                    passes.push(pass(target, [1.0, 0.0, 0.0, 1.0], commands.len() as u32, 1));
                    commands.push(DrawCommand {
                        pipeline,
                        bind_group: Some(ao.groups[k]),
                        geometry,
                        instance_buffer: instances,
                        instance_offset,
                        uniforms: InlineUniforms::EMPTY,
                        scissor,
                    });
                }
                self.stats.occlusion_passes += 1;
            }
            let first_command = commands.len() as u32;
            let sky = match (slot.sky, self.sky_pipeline, self.shared) {
                (Some((_, group)), Some(pipeline), Some(shared)) if slot.sky_drawn => {
                    let (geometry, instance_offset) = instanced(&shared.screen, 0, 1);
                    Some(DrawCommand {
                        pipeline,
                        bind_group: Some(group),
                        geometry,
                        instance_buffer: instances,
                        instance_offset,
                        uniforms: InlineUniforms::EMPTY,
                        scissor: Some((0, 0, slot.used[0], slot.used[1])),
                    })
                }
                _ => None,
            };
            for (k, draw) in slot.draws.iter().enumerate() {
                if k == slot.opaque
                    && let Some(sky) = sky
                {
                    commands.push(sky);
                }
                let Some(Some(mesh)) = self.meshes.get(draw.mesh.index()) else {
                    continue;
                };
                let group = match draw.skin {
                    Some(k) => slot
                        .skin_groups
                        .get(k as usize)
                        .copied()
                        .flatten()
                        .map(|(_, _, g)| g),
                    None => slot
                        .groups
                        .get(draw.material.index())
                        .copied()
                        .flatten()
                        .map(|(_, g)| g),
                };
                let (Some(group), Some(pipeline)) = (group, self.pipelines[draw.pipeline]) else {
                    continue;
                };
                let (geometry, instance_offset) = instanced(mesh, draw.first, draw.count);
                commands.push(DrawCommand {
                    pipeline,
                    bind_group: Some(group),
                    geometry,
                    instance_buffer: instances,
                    instance_offset,
                    uniforms: InlineUniforms::EMPTY,
                    scissor: Some((0, 0, slot.used[0], slot.used[1])),
                });
                self.stats.draws += 1;
                self.stats.instances += draw.count;
            }
            if slot.opaque >= slot.draws.len()
                && let Some(sky) = sky
            {
                commands.push(sky);
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

/// The pipeline of mesh program `program`: opaque, or `blended`
/// (premultiplied over, depth-tested but never written); culling faces by
/// `cull`.
fn mesh_pipeline<B: GpuBackend>(
    backend: &mut B,
    program: SceneProgram,
    blended: bool,
    cull: CullMode,
) -> PipelineId {
    let (depth, blend) = if blended {
        (DepthState::REVERSED_TEST_ONLY, BlendMode::PremultipliedOver)
    } else {
        (DepthState::REVERSED, BlendMode::Replace)
    };
    scene_pipeline(
        backend,
        &PipelineDesc {
            label: if blended { "scene-blended" } else { "scene" },
            builtin: BuiltinShader::Scene,
            variant: program as u32 | u32::from(blended) << 8 | (cull as u32) << 9,
            code: scene_program_code(program, B::SHADER_LANG),
            vertex_entry: "vertex_main",
            fragment_entry: "fragment_main",
            color_format: COLOR_FORMAT,
            depth: Some(depth),
            cull,
            blend,
            instance_schema: program.vertex_schema(),
        },
    )
}

/// Destroy a slot's ambient occlusion targets and their groups.
fn release_occlusion<B: GpuBackend>(backend: &mut B, targets: OcclusionTargets) {
    for group in targets.groups {
        backend.destroy_bind_group(group);
    }
    for texture in [
        targets.view_depth,
        targets.depth,
        targets.occlusion,
        targets.across,
    ] {
        backend.destroy_texture(texture);
    }
}

/// The occlusion program's pipeline, or the occlusion blur's when `blur`:
/// over the whole occlusion target, depth neither tested nor written.
fn ao_pipeline<B: GpuBackend>(backend: &mut B, blur: bool) -> PipelineId {
    let program = if blur {
        SceneProgram::AoBlur
    } else {
        SceneProgram::Ao
    };
    scene_pipeline(
        backend,
        &PipelineDesc {
            label: if blur {
                "scene-occlusion-blur"
            } else {
                "scene-occlusion"
            },
            builtin: BuiltinShader::Scene,
            variant: program as u32,
            code: scene_program_code(program, B::SHADER_LANG),
            vertex_entry: "vertex_main",
            fragment_entry: "fragment_main",
            color_format: SHADOW_FORMAT,
            depth: Some(DepthState {
                format: TextureFormat::Depth32Float,
                compare: CompareFunction::Always,
                write: false,
            }),
            cull: CullMode::None,
            blend: BlendMode::Replace,
            instance_schema: program.vertex_schema(),
        },
    )
}

/// The sky program's pipeline: drawn where the depth is still clear, never
/// writing it.
fn sky_pipeline<B: GpuBackend>(backend: &mut B) -> PipelineId {
    scene_pipeline(
        backend,
        &PipelineDesc {
            label: "scene-sky",
            builtin: BuiltinShader::Scene,
            variant: SceneProgram::Sky as u32,
            code: scene_program_code(SceneProgram::Sky, B::SHADER_LANG),
            vertex_entry: "vertex_main",
            fragment_entry: "fragment_main",
            color_format: COLOR_FORMAT,
            depth: Some(DepthState {
                format: TextureFormat::Depth32Float,
                compare: CompareFunction::GreaterEqual,
                write: false,
            }),
            cull: CullMode::None,
            blend: BlendMode::Replace,
            instance_schema: SceneProgram::Sky.vertex_schema(),
        },
    )
}

/// The depth program's pipeline at `index` of
/// [`SceneLane::depth_pipelines`]: shadow casters into an atlas, drawing
/// both faces so open meshes cast too.
fn depth_pipeline<B: GpuBackend>(backend: &mut B, index: usize) -> PipelineId {
    let mut program = if index % 2 == 1 {
        SceneProgram::SkinnedDepth
    } else {
        SceneProgram::Depth
    };
    if index >= 2 {
        program = program.with_extra();
    }
    scene_pipeline(
        backend,
        &PipelineDesc {
            label: "scene-shadow",
            builtin: BuiltinShader::Scene,
            variant: program as u32,
            code: scene_program_code(program, B::SHADER_LANG),
            vertex_entry: "vertex_main",
            fragment_entry: "fragment_main",
            color_format: SHADOW_FORMAT,
            depth: Some(DepthState::REVERSED),
            cull: CullMode::None,
            blend: BlendMode::Replace,
            instance_schema: program.vertex_schema(),
        },
    )
}

/// The pipeline `desc` describes, over its program's vertex layout.
fn scene_pipeline<B: GpuBackend>(backend: &mut B, desc: &PipelineDesc) -> PipelineId {
    let program = SceneProgram::of_variant(desc.variant).unwrap_or(SceneProgram::Mesh);
    let layout = match (program.skinned(), program.extra()) {
        (false, false) => &VertexLayout::LAYOUT,
        (true, false) => &SkinnedLayout::LAYOUT,
        (false, true) => &ExtraLayout::LAYOUT,
        (true, true) => &SkinnedExtraLayout::LAYOUT,
    };
    backend
        .create_pipeline(desc, layout)
        .expect("the scene vertex layouts match their programs' schemas")
}

/// A mesh's buffers on the device: `vertex_bytes`, and `count` indices
/// `index_bytes` of `format`.
fn upload<B: GpuBackend>(
    backend: &mut B,
    vertex_bytes: &[u8],
    index_bytes: &[u8],
    format: IndexFormat,
    count: usize,
) -> GpuMesh {
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
        vertices: buffer(backend, vertex_bytes, BufferUsage::VERTEX, "scene-mesh"),
        indices: buffer(
            backend,
            index_bytes,
            BufferUsage::INDEX,
            "scene-mesh-indices",
        ),
        format,
        count: count as u32,
        bounds: Bounds::UNIT,
        skinned: false,
        extra: false,
        joints: [0; MAX_JOINTS / 64],
    }
}

/// The joints `vertices` follow with some weight, a bit each; an index past
/// the palette reads its last joint.
fn used_joints(vertices: &[SkinnedVertex]) -> [u64; MAX_JOINTS / 64] {
    let mut used = [0u64; MAX_JOINTS / 64];
    for v in vertices {
        let [a, b] = v.weights;
        let weights = [a & 0xffff, a >> 16, b & 0xffff, b >> 16];
        for (j, w) in v.joint_indices().into_iter().zip(weights) {
            if w > 0 {
                let j = j.min(MAX_JOINTS - 1);
                used[j / 64] |= 1 << (j % 64);
            }
        }
    }
    used
}
