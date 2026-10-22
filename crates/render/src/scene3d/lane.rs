//! The renderer's side of the scene lane: the meshes on the device, each
//! slot's frame, targets and instance buffer, and the passes that render
//! them.

use viso_gpu::backend::{DepthAttachment, DrawCommand, Geometry, IndexFormat, InlineUniforms};
use viso_gpu::backend::{RenderPass, RenderTarget};
use viso_gpu::{
    BindGroupDesc, BindGroupId, Binding, BlendMode, BufferDesc, BufferId, BufferUsage,
    BuiltinShader, CullMode, DepthState, GpuBackend, GpuPod, LoadOp, PipelineDesc, PipelineId,
    TextureDesc, TextureFormat, TextureId,
};
use viso_shader::{PrimitiveKind, shader_code};

use super::{Bounds, Frustum, MeshId, SceneFrame, SceneInstance, SceneUniforms, SceneVertex};
use super::{Shape, fit_view};
use crate::primitive::{ImageDraw, Rect, SceneDraw, SceneSlot};

/// The pixel step a scene's targets grow and shrink by.
const TARGET_STEP: u32 = 64;

/// The color format of a scene's target: what an image texture holds.
const COLOR_FORMAT: TextureFormat = TextureFormat::Bgra8Unorm;

/// The scene vertex as the pipeline's vertex layout reads it.
#[repr(C)]
#[derive(Clone, Copy, GpuPod)]
struct VertexLayout {
    position: [f32; 3],
    normal: [f32; 3],
    uv: [f32; 2],
}

/// What the scene lane did in the frames since the counters were last
/// taken (§61).
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

/// A slot's color and depth targets.
#[derive(Debug, Clone, Copy)]
struct Targets {
    color: TextureId,
    depth: TextureId,
    size: [u32; 2],
}

/// One instanced draw of a rendered scene: `count` instances of `mesh` from
/// instance `first`.
#[derive(Debug, Clone, Copy)]
struct Draw {
    mesh: MeshId,
    first: u32,
    count: u32,
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
    uniforms: Option<(BufferId, BindGroupId)>,
    draws: Vec<Draw>,
    used: [u32; 2],
    clear: [f32; 4],
}

#[derive(Debug, Default)]
pub(crate) struct SceneLane {
    pipeline: Option<PipelineId>,
    meshes: Vec<Option<GpuMesh>>,
    /// Uploaded mesh ids free for reuse.
    free: Vec<u32>,
    slots: Vec<Option<Slot>>,
    /// The bytes of a fill's surviving instances.
    scratch: Vec<u8>,
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
        if let Some((buffer, group)) = state.uniforms {
            backend.destroy_bind_group(group);
            backend.destroy_buffer(buffer);
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

    fn pipeline<B: GpuBackend>(&mut self, backend: &mut B) -> PipelineId {
        *self.pipeline.get_or_insert_with(|| {
            let code = shader_code(PrimitiveKind::Scene, B::SHADER_LANG)
                .expect("every backend language prints the scene built-in");
            backend
                .create_pipeline(
                    &PipelineDesc {
                        label: "scene",
                        builtin: BuiltinShader::Scene,
                        variant: 0,
                        code,
                        vertex_entry: "vertex_main",
                        fragment_entry: "fragment_main",
                        color_format: COLOR_FORMAT,
                        depth: Some(DepthState::REVERSED),
                        cull: CullMode::Back,
                        blend: BlendMode::Replace,
                        instance_schema: viso_gpu::scene::VERTEX_SCHEMA,
                    },
                    &VertexLayout::LAYOUT,
                )
                .expect("the scene vertex matches the scene built-in's schema")
        })
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
        self.pipeline(backend);
        let i = draw.slot.0 as usize;
        self.slots.get(i)?.as_ref()?;
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

    /// Cull slot `i`'s frame into its instance buffer, write its uniforms,
    /// and record its draws, for a target `size` pixels large.
    fn fill<B: GpuBackend>(&mut self, backend: &mut B, i: usize, size: [u32; 2]) {
        let mut scratch = core::mem::take(&mut self.scratch);
        scratch.clear();
        let mut slot = self.slots[i].take().expect("the caller checked the slot");
        let used = slot.used;
        let view_proj = slot.frame.camera.view_proj(used[0] as f32 / used[1] as f32);
        let frustum = Frustum::of(&view_proj);
        slot.draws.clear();
        let mut total = 0u32;
        let mut draws = core::mem::take(&mut slot.draws);
        for (mesh_id, batch) in slot.frame.batches() {
            let Some(mesh) = self.mesh(backend, mesh_id) else {
                continue;
            };
            let first = total;
            for instance in batch {
                let (centre, half) = mesh.bounds.placed(instance);
                if !frustum.sees(centre, half) {
                    self.stats.culled += 1;
                    continue;
                }
                let at = scratch.len();
                scratch.resize(at + SceneInstance::SIZE, 0);
                instance.write_to(&mut scratch[at..]);
                total += 1;
            }
            if total > first {
                draws.push(Draw {
                    mesh: mesh_id,
                    first,
                    count: total - first,
                });
            }
        }
        slot.draws = draws;
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
        let (uniforms, _) = *slot.uniforms.get_or_insert_with(|| {
            let buffer = backend.create_buffer(&BufferDesc {
                size: SceneUniforms::SIZE,
                usage: BufferUsage::UNIFORM | BufferUsage::CPU_WRITE,
                label: "scene-uniforms",
            });
            let group = backend.create_bind_group(&BindGroupDesc {
                label: "scene",
                bindings: vec![Binding::Uniform(buffer)],
            });
            (buffer, group)
        });
        let mut bytes = [0u8; SceneUniforms::SIZE];
        slot.frame
            .uniforms(&fit_view(&view_proj, used, size))
            .write_to(&mut bytes);
        backend.write_buffer(uniforms, 0, &bytes);
        slot.clear = slot.frame.clear_color();
        self.slots[i] = Some(slot);
        self.scratch = scratch;
    }

    /// Append a pass for every slot filled since it last rendered.
    pub(crate) fn encode(&mut self, commands: &mut Vec<DrawCommand>, passes: &mut Vec<RenderPass>) {
        let Some(pipeline) = self.pipeline else {
            return;
        };
        for slot in self.slots.iter_mut().flatten() {
            let Some(targets) = slot.targets.filter(|_| slot.pending) else {
                continue;
            };
            slot.pending = false;
            let first_command = commands.len() as u32;
            let (_, group) = slot.uniforms.expect("a filled slot has uniforms");
            for draw in &slot.draws {
                let Some(Some(mesh)) = self.meshes.get(draw.mesh.index()) else {
                    continue;
                };
                let Some((instances, _)) = slot.instances else {
                    continue;
                };
                commands.push(DrawCommand {
                    pipeline,
                    bind_group: Some(group),
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
