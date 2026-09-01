//! The renderer facade: lower primitives → batches → GPU draw commands (§16).
//!
//! [`Renderer`] is generic over the [`GpuBackend`] so the same code drives the
//! Metal and headless-raster backends (ADR-007: the concrete backend is chosen
//! at compile time by the facade, so there is no per-frame `dyn` dispatch). It
//! owns the cold-path GPU resources (the Quad and Image pipelines, persistent
//! instance buffers) and turns each frame's `&[Primitive]` into one [`DrawList`].
//!
//! Phase 2 slice: [`Primitive::Quad`] and [`Primitive::Image`] are lowered.
//! Adjacent quads sharing one clip merge into a single instanced draw; each
//! image is a draw carrying its texture's bind group. Quad and Image draws are
//! ordered by the flat primitive stream so their z-order interleaves correctly.
//! Persistent instance buffers are reused across frames and only grow when a
//! frame needs more capacity, so a steady-state frame performs zero GPU buffer
//! allocations (exit criterion).

use viso_gpu::backend::{DrawCommand, DrawList, Geometry, RenderPass, RenderTarget};
use viso_gpu::{AddressMode, BindGroupId, FilterMode, SamplerId};
use viso_gpu::{
    BindGroupDesc, Binding, BlendMode, BufferDesc, BufferUsage, BuiltinShader, Frame, GpuBackend,
    LoadOp, PipelineDesc, PipelineId, SamplerDesc, SurfaceId, TextureDesc, TextureFormat,
    TextureId,
};

use viso_shader::{
    GLYPHRUN_MSL, IMAGE_MSL, MESH_MSL, QUAD_MSL, glyphrun_schema, image_schema, mesh_schema,
    quad_schema,
};

use crate::primitive::{GlyphInstance, ImageInstance, MeshVertex, Primitive, QuadInstance, Rect};

/// Bytes of one quad instance.
const QUAD_STRIDE: usize = core::mem::size_of::<QuadInstance>();
/// Bytes of one image instance.
const IMAGE_STRIDE: usize = core::mem::size_of::<ImageInstance>();
/// Bytes of one glyph instance.
const GLYPH_STRIDE: usize = core::mem::size_of::<GlyphInstance>();
/// Bytes of one mesh vertex.
const MESH_VERTEX_STRIDE: usize = core::mem::size_of::<MeshVertex>();
/// Bytes of one mesh index (`u32`).
const MESH_INDEX_STRIDE: usize = core::mem::size_of::<u32>();

/// What a [`Segment`] draws, and where its geometry lives.
#[derive(Debug, Clone, Copy, PartialEq)]
enum SegmentKind {
    /// A run of adjacent quads sharing this segment's clip, in the quad buffer.
    /// `start`/`count` count instances in that buffer.
    Quad,
    /// A single image, in the image buffer, sampling `bind_group`'s texture.
    /// `start`/`count` count instances in that buffer.
    Image { bind_group: BindGroupId },
    /// One run of glyphs, in the glyph buffer, sampling `bind_group`'s SDF atlas.
    /// `start`/`count` count instances in that buffer.
    GlyphRun { bind_group: BindGroupId },
    /// A run of adjacent triangle meshes (Path/Mesh) sharing this segment's
    /// clip, in the shared mesh vertex/index buffers. `start`/`count` count
    /// **indices** in the mesh index buffer (vertices are addressed by the
    /// absolute indices baked into the index data).
    Mesh,
}

/// A contiguous run of geometry that encodes as a single draw.
///
/// Segments are built in [`Renderer::upload`] by walking the flat primitive
/// stream and its `Layer`/`LayerEnd` clip stack, preserving submission order so
/// primitives interleave by z-order. Adjacent quads join one segment when their
/// clip and target match; adjacent meshes likewise; each image is its own
/// segment (it needs its texture's bind group). `clip == None` means unclipped.
///
/// The meaning of `start`/`count` depends on `kind`: instances for
/// [`SegmentKind::Quad`]/[`SegmentKind::Image`]/[`SegmentKind::GlyphRun`],
/// indices for [`SegmentKind::Mesh`].
#[derive(Debug, Clone, Copy, PartialEq)]
struct Segment {
    /// What this segment draws / which buffer its geometry indexes.
    kind: SegmentKind,
    /// Offset of the first instance or index (see `kind`) into its buffer.
    start: u32,
    /// Number of instances or indices (see `kind`) in the run.
    count: u32,
    /// The effective clip rect, or `None` for unclipped.
    ///
    /// For a segment in an offscreen pass this clip is already expressed in the
    /// offscreen texture's local space (the layer origin has been subtracted),
    /// so it scissors correctly against that pass's viewport.
    clip: Option<Rect>,
    /// Which render pass this segment belongs to.
    target: PassTarget,
}

/// Which render pass a [`Segment`] is drawn into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PassTarget {
    /// The final surface pass (drawn last, composited onto the window).
    Main,
    /// An offscreen texture pass, indexed into [`Renderer::offscreen_passes`].
    /// Emitted before the surface pass so its texture is ready to composite.
    Offscreen(usize),
}

/// One offscreen render-to-texture pass, created for a translucent
/// (`opacity < 1`) [`LayerClip`].
///
/// The layer's `Layer..LayerEnd` subtree is rendered into `texture` (sized to
/// the layer's clip rect, cleared transparent), then composited back into the
/// main pass as a textured quad at the layer's world-space rect, tinted by the
/// layer opacity. Segments whose `target` names this pass carry geometry whose
/// positions have had the layer origin subtracted, so the existing shaders draw
/// them correctly against the pass's `viewport` (= the texture extent).
struct OffscreenPass {
    /// The render-target texture this pass draws into.
    texture: TextureId,
    /// Bind group pairing `texture` with the shared sampler, for compositing.
    bind_group: BindGroupId,
    /// The pass viewport `[width, height]` in physical pixels (= texture extent,
    /// the ceil of the layer clip size).
    viewport: [f32; 2],
    /// The layer clip's world-space rect: the composite destination, and the
    /// origin subtracted from this pass's geometry.
    rect: Rect,
    /// The layer opacity in `[0, 1)`, applied as the composite tint alpha.
    opacity: f32,
}

/// An entry on the layer stack while lowering the flat primitive stream.
#[derive(Debug, Clone, Copy)]
struct LayerEntry {
    /// The effective clip rect in **world space**, already intersected with all
    /// ancestors.
    clip: Rect,
    /// The pass segments opened under this layer are routed to. Inherits the
    /// parent's target for an opaque (`opacity == 1`) layer; names this layer's
    /// own [`OffscreenPass`] for a translucent one.
    target: PassTarget,
    /// The world-space origin subtracted from geometry drawn under this layer,
    /// so an offscreen pass renders with its texture's top-left at `(0, 0)`.
    /// Zero for the main pass.
    origin: [f32; 2],
}

/// A texture's bind group, cached so repeated draws of the same texture reuse
/// one bind group (and its sampler) rather than allocating per frame.
struct TextureBinding {
    texture: TextureId,
    bind_group: BindGroupId,
}

/// Draw-call and instance counts for the frame the renderer has just lowered.
///
/// Read after [`Renderer::upload`] and before [`Renderer::submit`]: `upload`
/// has built the full segment list (one segment per draw command, across the
/// offscreen and main passes) but `submit` has not consumed it. Exposed for
/// tooling/tests/benches (§34, §61); the steady-state bench asserts these stay
/// constant across identical frames, guarding the dispatch contract (§7.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameStats {
    /// Total draw commands this frame will encode, summed over every pass
    /// (offscreen passes plus the main surface pass, including composite draws).
    pub draw_calls: usize,
    /// Total geometry units across all segments: instances for
    /// quad/image/glyph/composite segments, indices for mesh segments. A stable
    /// scene keeps this fixed, so it doubles as a change detector for the bench.
    pub instances: usize,
}

/// Turns per-frame primitives into GPU draw commands for one surface.
pub struct Renderer {
    /// The Quad built-in pipeline (registered once).
    quad_pipeline: PipelineId,
    /// The Image built-in pipeline (registered once).
    image_pipeline: PipelineId,
    /// The GlyphRun built-in pipeline (registered once).
    glyph_pipeline: PipelineId,
    /// A linear-filter clamp sampler shared by image and glyph draws (Phase 2
    /// uses one sampler configuration; the SDF atlas needs bilinear filtering,
    /// which this provides). Per-image sampler variety lands later.
    sampler: SamplerId,
    /// Persistent quad instance buffer, reused across frames.
    quad_buffer: viso_gpu::BufferId,
    /// Capacity of `quad_buffer`, in instances.
    quad_capacity: usize,
    /// Persistent image instance buffer, reused across frames.
    image_buffer: viso_gpu::BufferId,
    /// Capacity of `image_buffer`, in instances.
    image_capacity: usize,
    /// Persistent glyph instance buffer, reused across frames.
    glyph_buffer: viso_gpu::BufferId,
    /// Capacity of `glyph_buffer`, in instances.
    glyph_capacity: usize,
    /// The general triangle-mesh pipeline (Path/Mesh), registered once.
    mesh_pipeline: PipelineId,
    /// Persistent mesh vertex buffer, reused across frames.
    mesh_vertex_buffer: viso_gpu::BufferId,
    /// Capacity of `mesh_vertex_buffer`, in vertices.
    mesh_vertex_capacity: usize,
    /// Persistent mesh index buffer, reused across frames.
    mesh_index_buffer: viso_gpu::BufferId,
    /// Capacity of `mesh_index_buffer`, in indices.
    mesh_index_capacity: usize,
    /// Cached per-texture bind groups, reused across frames.
    texture_bindings: Vec<TextureBinding>,
    /// Scratch quad instance data, reused each frame.
    quad_scratch: Vec<QuadInstance>,
    /// Scratch image instance data, reused each frame.
    image_scratch: Vec<ImageInstance>,
    /// Scratch glyph instance data, reused each frame.
    glyph_scratch: Vec<GlyphInstance>,
    /// Scratch mesh vertex data, reused each frame.
    mesh_vertex_scratch: Vec<MeshVertex>,
    /// Scratch mesh index data, reused each frame.
    mesh_index_scratch: Vec<u32>,
    /// Draw segments in submission order, reused each frame.
    segments: Vec<Segment>,
    /// Layer stack used while lowering the flat primitive stream, reused each
    /// frame. The top carries the current effective clip (already intersected
    /// with its ancestors), the pass its segments are routed to, and the origin
    /// its geometry is translated by; empty means unclipped, main pass, no
    /// offset.
    layer_stack: Vec<LayerEntry>,
    /// The offscreen render-to-texture passes for this frame's translucent
    /// layers, in creation order. Reused each frame; emitted before the surface
    /// pass in [`Renderer::encode`].
    offscreen_passes: Vec<OffscreenPass>,
    /// A pool of render-target textures keyed by extent, so a steady-state frame
    /// whose translucent layers keep the same sizes reuses textures instead of
    /// allocating (exit criterion). Grown on demand; never shrunk.
    offscreen_pool: Vec<PooledTexture>,
    /// How many pooled textures are already claimed by this frame's passes,
    /// reset each frame so successive same-size layers each get a distinct one.
    offscreen_pool_used: usize,
}

/// A reusable render-target texture in [`Renderer::offscreen_pool`].
struct PooledTexture {
    texture: TextureId,
    bind_group: BindGroupId,
    width: u32,
    height: u32,
}

impl Renderer {
    /// Create a renderer for `surface`, registering the Quad and Image
    /// pipelines and a shared linear-clamp sampler.
    ///
    /// `surface_format` is the color-attachment format the pipelines target.
    pub fn new<B: GpuBackend>(backend: &mut B, surface_format: TextureFormat) -> Self {
        let quad_pipeline = backend
            .create_pipeline(
                &PipelineDesc {
                    label: "quad",
                    builtin: BuiltinShader::Quad,
                    shader_source: QUAD_MSL,
                    vertex_entry: "vertex_main",
                    fragment_entry: "fragment_main",
                    color_format: surface_format,
                    depth_format: None,
                    blend: BlendMode::PremultipliedOver,
                    instance_schema: quad_schema(),
                },
                &QuadInstance::LAYOUT,
            )
            .expect("QuadInstance layout matches the quad shader schema");

        let image_pipeline = backend
            .create_pipeline(
                &PipelineDesc {
                    label: "image",
                    builtin: BuiltinShader::Image,
                    shader_source: IMAGE_MSL,
                    vertex_entry: "vertex_main",
                    fragment_entry: "fragment_main",
                    color_format: surface_format,
                    depth_format: None,
                    blend: BlendMode::PremultipliedOver,
                    instance_schema: image_schema(),
                },
                &ImageInstance::LAYOUT,
            )
            .expect("ImageInstance layout matches the image shader schema");

        let glyph_pipeline = backend
            .create_pipeline(
                &PipelineDesc {
                    label: "glyph",
                    builtin: BuiltinShader::GlyphRun,
                    shader_source: GLYPHRUN_MSL,
                    vertex_entry: "vertex_main",
                    fragment_entry: "fragment_main",
                    color_format: surface_format,
                    depth_format: None,
                    blend: BlendMode::PremultipliedOver,
                    instance_schema: glyphrun_schema(),
                },
                &GlyphInstance::LAYOUT,
            )
            .expect("GlyphInstance layout matches the glyph shader schema");

        let mesh_pipeline = backend
            .create_pipeline(
                &PipelineDesc {
                    label: "mesh",
                    builtin: BuiltinShader::Path,
                    shader_source: MESH_MSL,
                    vertex_entry: "vertex_main",
                    fragment_entry: "fragment_main",
                    color_format: surface_format,
                    depth_format: None,
                    blend: BlendMode::PremultipliedOver,
                    instance_schema: mesh_schema(),
                },
                &MeshVertex::LAYOUT,
            )
            .expect("MeshVertex layout matches the mesh shader schema");

        let sampler = backend.create_sampler(&SamplerDesc {
            filter: FilterMode::Linear,
            address: AddressMode::ClampToEdge,
        });

        let quad_capacity = 256;
        let quad_buffer = backend.create_buffer(&BufferDesc {
            size: quad_capacity * QUAD_STRIDE,
            usage: BufferUsage::INSTANCE | BufferUsage::CPU_WRITE,
            label: "quad-instances",
        });
        let image_capacity = 64;
        let image_buffer = backend.create_buffer(&BufferDesc {
            size: image_capacity * IMAGE_STRIDE,
            usage: BufferUsage::INSTANCE | BufferUsage::CPU_WRITE,
            label: "image-instances",
        });
        let glyph_capacity = 256;
        let glyph_buffer = backend.create_buffer(&BufferDesc {
            size: glyph_capacity * GLYPH_STRIDE,
            usage: BufferUsage::INSTANCE | BufferUsage::CPU_WRITE,
            label: "glyph-instances",
        });
        let mesh_vertex_capacity = 1024;
        let mesh_vertex_buffer = backend.create_buffer(&BufferDesc {
            size: mesh_vertex_capacity * MESH_VERTEX_STRIDE,
            usage: BufferUsage::VERTEX | BufferUsage::CPU_WRITE,
            label: "mesh-vertices",
        });
        let mesh_index_capacity = 2048;
        let mesh_index_buffer = backend.create_buffer(&BufferDesc {
            size: mesh_index_capacity * MESH_INDEX_STRIDE,
            usage: BufferUsage::INDEX | BufferUsage::CPU_WRITE,
            label: "mesh-indices",
        });

        Self {
            quad_pipeline,
            image_pipeline,
            glyph_pipeline,
            sampler,
            quad_buffer,
            quad_capacity,
            image_buffer,
            image_capacity,
            glyph_buffer,
            glyph_capacity,
            mesh_pipeline,
            mesh_vertex_buffer,
            mesh_vertex_capacity,
            mesh_index_buffer,
            mesh_index_capacity,
            texture_bindings: Vec::with_capacity(8),
            quad_scratch: Vec::with_capacity(quad_capacity),
            image_scratch: Vec::with_capacity(image_capacity),
            glyph_scratch: Vec::with_capacity(glyph_capacity),
            mesh_vertex_scratch: Vec::with_capacity(mesh_vertex_capacity),
            mesh_index_scratch: Vec::with_capacity(mesh_index_capacity),
            segments: Vec::with_capacity(8),
            layer_stack: Vec::with_capacity(8),
            offscreen_passes: Vec::with_capacity(4),
            offscreen_pool: Vec::with_capacity(4),
            offscreen_pool_used: 0,
        }
    }

    /// Get (or lazily create) the bind group for `texture`, pairing it with the
    /// shared sampler. Cached across frames so a repeated texture reuses its
    /// bind group (no per-frame allocation in steady state).
    fn bind_group_for<B: GpuBackend>(
        &mut self,
        backend: &mut B,
        texture: TextureId,
    ) -> BindGroupId {
        if let Some(tb) = self
            .texture_bindings
            .iter()
            .find(|tb| tb.texture == texture)
        {
            return tb.bind_group;
        }
        let bind_group = backend.create_bind_group(&BindGroupDesc {
            label: "image",
            bindings: vec![Binding::Texture(texture), Binding::Sampler(self.sampler)],
        });
        self.texture_bindings.push(TextureBinding {
            texture,
            bind_group,
        });
        bind_group
    }

    /// Collect this frame's quad and image instances into scratch buffers,
    /// building submission-ordered [`Segment`]s (with the effective clip from
    /// the `Layer`/`LayerEnd` stack), then upload both instance buffers.
    ///
    /// Growing an instance buffer allocates a new one only when the frame needs
    /// more capacity than before; steady-state frames reuse the buffers.
    pub fn upload<B: GpuBackend>(&mut self, backend: &mut B, primitives: &[Primitive]) {
        self.quad_scratch.clear();
        self.image_scratch.clear();
        self.glyph_scratch.clear();
        self.mesh_vertex_scratch.clear();
        self.mesh_index_scratch.clear();
        self.segments.clear();
        self.layer_stack.clear();
        self.offscreen_passes.clear();
        self.offscreen_pool_used = 0;

        for prim in primitives {
            let (clip, target, origin) = self.active();
            match prim {
                Primitive::Quad(quad) => {
                    let start = self.quad_scratch.len() as u32;
                    let mut inst = quad.to_instance();
                    inst.rect_pos[0] -= origin[0];
                    inst.rect_pos[1] -= origin[1];
                    self.quad_scratch.push(inst);
                    // Extend the current segment only if it is a quad run under
                    // the same clip and target; otherwise open a new one.
                    match self.segments.last_mut() {
                        Some(seg)
                            if seg.kind == SegmentKind::Quad
                                && seg.clip == clip
                                && seg.target == target =>
                        {
                            seg.count += 1;
                        }
                        _ => self.segments.push(Segment {
                            kind: SegmentKind::Quad,
                            start,
                            count: 1,
                            clip,
                            target,
                        }),
                    }
                }
                Primitive::Image(image) => {
                    let bind_group = self.bind_group_for(backend, image.texture);
                    let start = self.image_scratch.len() as u32;
                    let mut inst = image.to_instance();
                    inst.rect_pos[0] -= origin[0];
                    inst.rect_pos[1] -= origin[1];
                    self.image_scratch.push(inst);
                    // Each image is its own draw (it binds a texture); never
                    // merged, even with an adjacent same-texture image, in this
                    // Phase 2 slice.
                    self.segments.push(Segment {
                        kind: SegmentKind::Image { bind_group },
                        start,
                        count: 1,
                        clip,
                        target,
                    });
                }
                Primitive::Path(path) => {
                    let index_start = self.mesh_index_scratch.len() as u32;
                    let vertex_start = self.mesh_vertex_scratch.len();
                    self.tessellate_into(|verts, idx| path.tessellate(verts, idx));
                    self.translate_vertices(vertex_start, origin);
                    let count = self.mesh_index_scratch.len() as u32 - index_start;
                    self.push_mesh_segment(index_start, count, clip, target);
                }
                Primitive::Mesh(mesh) => {
                    let index_start = self.mesh_index_scratch.len() as u32;
                    let vertex_start = self.mesh_vertex_scratch.len();
                    self.tessellate_into(|verts, idx| {
                        let base = verts.len() as u32;
                        verts.extend_from_slice(&mesh.vertices);
                        idx.extend(mesh.indices.iter().map(|&i| base + i));
                    });
                    self.translate_vertices(vertex_start, origin);
                    let count = self.mesh_index_scratch.len() as u32 - index_start;
                    self.push_mesh_segment(index_start, count, clip, target);
                }
                Primitive::GlyphRun(run) => {
                    if run.glyphs.is_empty() {
                        continue;
                    }
                    let bind_group = self.bind_group_for(backend, run.atlas);
                    let start = self.glyph_scratch.len() as u32;
                    for glyph in &run.glyphs {
                        let mut inst = run.instance(glyph);
                        inst.rect_pos[0] -= origin[0];
                        inst.rect_pos[1] -= origin[1];
                        self.glyph_scratch.push(inst);
                    }
                    let count = self.glyph_scratch.len() as u32 - start;
                    // A run is one instanced draw (it binds the atlas texture);
                    // never merged with adjacent runs in this Phase 2 slice.
                    self.segments.push(Segment {
                        kind: SegmentKind::GlyphRun { bind_group },
                        start,
                        count,
                        clip,
                        target,
                    });
                }
                Primitive::Layer(layer) => {
                    // The layer clip, intersected with the parent's effective
                    // clip, in world space.
                    let world_clip = match self.layer_stack.last() {
                        Some(parent) => parent.clip.intersect(layer.clip),
                        None => layer.clip,
                    };
                    if layer.opacity >= 1.0 {
                        // Opaque: a plain in-pass scissor clip. Inherit the
                        // parent's pass target and origin unchanged.
                        self.layer_stack.push(LayerEntry {
                            clip: world_clip,
                            target,
                            origin,
                        });
                    } else {
                        // Translucent: open an offscreen pass. Its geometry is
                        // translated so the layer's top-left maps to the
                        // texture's (0, 0); the pass is sized to the layer rect
                        // and composited back at LayerEnd.
                        let pass_origin = [world_clip.x, world_clip.y];
                        let idx = self.open_offscreen(backend, world_clip, layer.opacity);
                        self.layer_stack.push(LayerEntry {
                            clip: world_clip,
                            target: PassTarget::Offscreen(idx),
                            origin: pass_origin,
                        });
                    }
                }
                Primitive::LayerEnd => {
                    if let Some(entry) = self.layer_stack.pop()
                        && let PassTarget::Offscreen(idx) = entry.target
                    {
                        // Composite the finished offscreen texture back into the
                        // parent target as a textured quad at the layer's
                        // world-space rect, tinted by the layer opacity.
                        self.close_offscreen(idx);
                    }
                }
            }
        }

        self.upload_quads(backend);
        self.upload_images(backend);
        self.upload_glyphs(backend);
        self.upload_mesh(backend);
    }

    /// The draw-call and instance counts for the frame just lowered by
    /// [`upload`](Self::upload).
    ///
    /// Every [`Segment`] maps 1:1 to a draw command (across all passes,
    /// composites included), so `draw_calls` is simply the segment count; a
    /// scene whose nodes did not change lowers to the same segments and keeps it
    /// fixed. `instances` sums each segment's `count`. See [`FrameStats`].
    pub fn frame_stats(&self) -> FrameStats {
        FrameStats {
            draw_calls: self.segments.len(),
            instances: self.segments.iter().map(|s| s.count as usize).sum(),
        }
    }

    /// Append tessellated geometry via `f`, which writes into the shared mesh
    /// vertex/index scratch. `f` receives both scratch vectors so it can bake
    /// absolute vertex indices (the mesh draw addresses vertices by the indices
    /// stored in the index buffer, so each primitive offsets its own indices by
    /// the current vertex count).
    fn tessellate_into(&mut self, f: impl FnOnce(&mut Vec<MeshVertex>, &mut Vec<u32>)) {
        f(&mut self.mesh_vertex_scratch, &mut self.mesh_index_scratch);
    }

    /// Push (or extend) a mesh segment covering `count` indices at `index_start`.
    /// Adjacent meshes sharing the same clip and target merge into one indexed
    /// draw.
    fn push_mesh_segment(
        &mut self,
        index_start: u32,
        count: u32,
        clip: Option<Rect>,
        target: PassTarget,
    ) {
        if count == 0 {
            return;
        }
        match self.segments.last_mut() {
            Some(seg)
                if seg.kind == SegmentKind::Mesh && seg.clip == clip && seg.target == target =>
            {
                seg.count += count;
            }
            _ => self.segments.push(Segment {
                kind: SegmentKind::Mesh,
                start: index_start,
                count,
                clip,
                target,
            }),
        }
    }

    /// The active `(clip, target, origin)` from the top of the layer stack:
    /// the effective clip in the segment's own space (world space for the main
    /// pass, texture-local for an offscreen pass), the pass its segments route
    /// to, and the origin subtracted from geometry positions.
    ///
    /// The stored `LayerEntry.clip` is world-space; for an offscreen layer we
    /// return it shifted into the texture's local space (origin subtracted) so
    /// the scissor lines up with that pass's viewport.
    fn active(&self) -> (Option<Rect>, PassTarget, [f32; 2]) {
        match self.layer_stack.last() {
            Some(entry) => {
                let clip = Rect {
                    x: entry.clip.x - entry.origin[0],
                    y: entry.clip.y - entry.origin[1],
                    w: entry.clip.w,
                    h: entry.clip.h,
                };
                (Some(clip), entry.target, entry.origin)
            }
            None => (None, PassTarget::Main, [0.0, 0.0]),
        }
    }

    /// Translate the mesh vertices staged since `vertex_start` by `-origin`, so
    /// geometry drawn into an offscreen pass has the layer's top-left at the
    /// texture origin. A no-op for the main pass (origin is zero).
    fn translate_vertices(&mut self, vertex_start: usize, origin: [f32; 2]) {
        if origin == [0.0, 0.0] {
            return;
        }
        for v in &mut self.mesh_vertex_scratch[vertex_start..] {
            v.pos[0] -= origin[0];
            v.pos[1] -= origin[1];
        }
    }

    /// Open an offscreen pass for a translucent layer whose world-space clip is
    /// `world_clip`, returning its index in `offscreen_passes`. Claims (or grows)
    /// a pooled render-target texture sized to the layer rect, so a steady-state
    /// frame with same-size layers reuses textures. The texture is filled in and
    /// composited at [`Renderer::close_offscreen`].
    fn open_offscreen<B: GpuBackend>(
        &mut self,
        backend: &mut B,
        world_clip: Rect,
        opacity: f32,
    ) -> usize {
        // Size the texture to cover the (possibly fractional) layer rect.
        let width = (world_clip.w.ceil() as u32).max(1);
        let height = (world_clip.h.ceil() as u32).max(1);
        let pooled = self.claim_pooled_texture(backend, width, height);
        let idx = self.offscreen_passes.len();
        self.offscreen_passes.push(OffscreenPass {
            texture: pooled.texture,
            bind_group: pooled.bind_group,
            viewport: [width as f32, height as f32],
            rect: world_clip,
            opacity,
        });
        idx
    }

    /// Claim a pooled render-target texture of `width`×`height`, reusing an
    /// unclaimed one of that size if present, else creating (and pooling) a new
    /// one. Returns the texture and its sampling bind group.
    fn claim_pooled_texture<B: GpuBackend>(
        &mut self,
        backend: &mut B,
        width: u32,
        height: u32,
    ) -> PooledTexture {
        // Scan the not-yet-claimed tail for a size match, swapping it into the
        // claimed prefix so each pass gets a distinct texture.
        for i in self.offscreen_pool_used..self.offscreen_pool.len() {
            if self.offscreen_pool[i].width == width && self.offscreen_pool[i].height == height {
                self.offscreen_pool.swap(self.offscreen_pool_used, i);
                let pooled = PooledTexture {
                    ..self.offscreen_pool[self.offscreen_pool_used]
                };
                self.offscreen_pool_used += 1;
                return pooled;
            }
        }
        let texture = backend.create_texture(&TextureDesc {
            width,
            height,
            format: TextureFormat::Bgra8Unorm,
            render_target: true,
            label: "offscreen-layer",
        });
        let bind_group = backend.create_bind_group(&BindGroupDesc {
            label: "offscreen-layer",
            bindings: vec![Binding::Texture(texture), Binding::Sampler(self.sampler)],
        });
        let pooled = PooledTexture {
            texture,
            bind_group,
            width,
            height,
        };
        // Insert at the claimed boundary so used ones stay in the prefix.
        self.offscreen_pool
            .insert(self.offscreen_pool_used, PooledTexture { ..pooled });
        self.offscreen_pool_used += 1;
        pooled
    }

    /// Close the offscreen pass at `idx`, appending a composite segment to the
    /// main pass: a textured quad at the layer's world-space rect sampling the
    /// offscreen texture, tinted by the layer opacity (a = opacity).
    fn close_offscreen(&mut self, idx: usize) {
        let pass = &self.offscreen_passes[idx];
        let start = self.image_scratch.len() as u32;
        self.image_scratch.push(ImageInstance {
            rect_pos: [pass.rect.x, pass.rect.y],
            rect_size: [pass.rect.w, pass.rect.h],
            uv_pos: [0.0, 0.0],
            uv_size: [1.0, 1.0],
            color: [1.0, 1.0, 1.0, pass.opacity],
        });
        self.segments.push(Segment {
            kind: SegmentKind::Image {
                bind_group: pass.bind_group,
            },
            start,
            count: 1,
            // The composite draws into the parent target unclipped: the offscreen
            // texture already holds only the clipped subtree. (A parent offscreen
            // layer is not nested in this Phase 2 slice's composite clip.)
            clip: None,
            target: PassTarget::Main,
        });
    }

    /// Grow (if needed) and upload the staged quad instances.
    fn upload_quads<B: GpuBackend>(&mut self, backend: &mut B) {
        let count = self.quad_scratch.len();
        if count == 0 {
            return;
        }
        if count > self.quad_capacity {
            let new_cap = count.next_power_of_two();
            self.quad_buffer = backend.create_buffer(&BufferDesc {
                size: new_cap * QUAD_STRIDE,
                usage: BufferUsage::INSTANCE | BufferUsage::CPU_WRITE,
                label: "quad-instances",
            });
            self.quad_capacity = new_cap;
        }
        backend.write_buffer(self.quad_buffer, 0, instances_as_bytes(&self.quad_scratch));
    }

    /// Grow (if needed) and upload the staged image instances.
    fn upload_images<B: GpuBackend>(&mut self, backend: &mut B) {
        let count = self.image_scratch.len();
        if count == 0 {
            return;
        }
        if count > self.image_capacity {
            let new_cap = count.next_power_of_two();
            self.image_buffer = backend.create_buffer(&BufferDesc {
                size: new_cap * IMAGE_STRIDE,
                usage: BufferUsage::INSTANCE | BufferUsage::CPU_WRITE,
                label: "image-instances",
            });
            self.image_capacity = new_cap;
        }
        backend.write_buffer(
            self.image_buffer,
            0,
            instances_as_bytes(&self.image_scratch),
        );
    }

    /// Grow (if needed) and upload the staged glyph instances.
    fn upload_glyphs<B: GpuBackend>(&mut self, backend: &mut B) {
        let count = self.glyph_scratch.len();
        if count == 0 {
            return;
        }
        if count > self.glyph_capacity {
            let new_cap = count.next_power_of_two();
            self.glyph_buffer = backend.create_buffer(&BufferDesc {
                size: new_cap * GLYPH_STRIDE,
                usage: BufferUsage::INSTANCE | BufferUsage::CPU_WRITE,
                label: "glyph-instances",
            });
            self.glyph_capacity = new_cap;
        }
        backend.write_buffer(
            self.glyph_buffer,
            0,
            instances_as_bytes(&self.glyph_scratch),
        );
    }

    /// Grow (if needed) and upload the staged mesh vertices and indices.
    fn upload_mesh<B: GpuBackend>(&mut self, backend: &mut B) {
        let vcount = self.mesh_vertex_scratch.len();
        let icount = self.mesh_index_scratch.len();
        if vcount == 0 || icount == 0 {
            return;
        }
        if vcount > self.mesh_vertex_capacity {
            let new_cap = vcount.next_power_of_two();
            self.mesh_vertex_buffer = backend.create_buffer(&BufferDesc {
                size: new_cap * MESH_VERTEX_STRIDE,
                usage: BufferUsage::VERTEX | BufferUsage::CPU_WRITE,
                label: "mesh-vertices",
            });
            self.mesh_vertex_capacity = new_cap;
        }
        if icount > self.mesh_index_capacity {
            let new_cap = icount.next_power_of_two();
            self.mesh_index_buffer = backend.create_buffer(&BufferDesc {
                size: new_cap * MESH_INDEX_STRIDE,
                usage: BufferUsage::INDEX | BufferUsage::CPU_WRITE,
                label: "mesh-indices",
            });
            self.mesh_index_capacity = new_cap;
        }
        backend.write_buffer(
            self.mesh_vertex_buffer,
            0,
            instances_as_bytes(&self.mesh_vertex_scratch),
        );
        backend.write_buffer(
            self.mesh_index_buffer,
            0,
            instances_as_bytes(&self.mesh_index_scratch),
        );
    }

    /// Encode the staged draws into a draw list against `surface` and present it.
    /// `clear` is the background color (premultiplied RGBA); `viewport` is the
    /// surface size in physical pixels `[width, height]`, which the Metal shaders
    /// use to map pixel-space rects to NDC. (The headless raster backend ignores
    /// uniforms and works directly in pixel space.)
    ///
    /// This frame's translucent layers each become an offscreen [`RenderPass`]
    /// (cleared transparent) emitted before the surface pass, so their textures
    /// are ready when the surface pass composites them.
    pub fn submit<B: GpuBackend>(
        &mut self,
        backend: &mut B,
        surface: SurfaceId,
        clear: [f32; 4],
        viewport: [f32; 2],
    ) {
        let frame = backend.begin_frame(surface);
        self.encode(backend, frame, clear, viewport);
        backend.present(frame);
    }

    /// Build the draw list and hand it to the backend (no present).
    ///
    /// Emits one [`RenderPass`] per offscreen texture (in creation order, cleared
    /// transparent) followed by the surface pass, matching the `passes` ordering
    /// contract (offscreen layers first, then main). Each offscreen pass uses its
    /// own texture-extent viewport uniform; the surface pass uses `viewport`.
    fn encode<B: GpuBackend>(
        &self,
        backend: &mut B,
        frame: Frame,
        clear: [f32; 4],
        viewport: [f32; 2],
    ) {
        // Per-pass viewports, kept alive for the borrowed uniform bytes below.
        // Index 0 is the surface; the rest map 1:1 to `offscreen_passes`.
        let mut viewports: Vec<[f32; 2]> = Vec::with_capacity(self.offscreen_passes.len() + 1);
        viewports.push(viewport);
        for pass in &self.offscreen_passes {
            viewports.push(pass.viewport);
        }

        // Build each pass's command list (borrowing the matching viewport bytes),
        // offscreen passes first, then the surface pass. These Vecs must outlive
        // the `RenderPass` slices that borrow them, so they live in locals.
        let mut command_lists: Vec<Vec<DrawCommand>> =
            Vec::with_capacity(self.offscreen_passes.len() + 1);
        for (i, _pass) in self.offscreen_passes.iter().enumerate() {
            let uniform_bytes = bytemuck_viewport(&viewports[i + 1]);
            let vp = viewports[i + 1];
            let commands = self
                .segments
                .iter()
                .filter(|seg| seg.target == PassTarget::Offscreen(i))
                .map(|seg| self.command_for(seg, uniform_bytes, vp))
                .collect();
            command_lists.push(commands);
        }
        let main_uniform = bytemuck_viewport(&viewports[0]);
        let main_commands: Vec<DrawCommand> = self
            .segments
            .iter()
            .filter(|seg| seg.target == PassTarget::Main)
            .map(|seg| self.command_for(seg, main_uniform, viewport))
            .collect();
        command_lists.push(main_commands);

        // Assemble the passes: offscreen textures cleared transparent, then the
        // surface cleared to the background.
        let mut passes: Vec<RenderPass> = Vec::with_capacity(command_lists.len());
        for (i, pass) in self.offscreen_passes.iter().enumerate() {
            passes.push(RenderPass {
                target: RenderTarget::Texture(pass.texture),
                load: LoadOp::Clear([0.0, 0.0, 0.0, 0.0]),
                commands: &command_lists[i],
            });
        }
        passes.push(RenderPass {
            target: RenderTarget::Surface(frame),
            load: LoadOp::Clear(clear),
            commands: &command_lists[self.offscreen_passes.len()],
        });

        backend.encode(&DrawList { passes: &passes });
    }

    /// Map one [`Segment`] to its [`DrawCommand`], given the pass's inline
    /// viewport uniform bytes and the viewport its scissor is clamped to.
    fn command_for<'a>(
        &self,
        seg: &Segment,
        uniform_bytes: &'a [u8],
        viewport: [f32; 2],
    ) -> DrawCommand<'a> {
        let scissor = seg.clip.map(|c| clip_to_scissor(c, viewport));
        match seg.kind {
            SegmentKind::Quad => DrawCommand {
                pipeline: self.quad_pipeline,
                bind_group: None,
                geometry: Geometry::Generated { count: seg.count },
                instance_buffer: self.quad_buffer,
                instance_offset: seg.start as usize * QUAD_STRIDE,
                uniforms: uniform_bytes,
                scissor,
            },
            SegmentKind::Image { bind_group } => DrawCommand {
                pipeline: self.image_pipeline,
                bind_group: Some(bind_group),
                geometry: Geometry::Generated { count: seg.count },
                instance_buffer: self.image_buffer,
                instance_offset: seg.start as usize * IMAGE_STRIDE,
                uniforms: uniform_bytes,
                scissor,
            },
            SegmentKind::GlyphRun { bind_group } => DrawCommand {
                pipeline: self.glyph_pipeline,
                bind_group: Some(bind_group),
                geometry: Geometry::Generated { count: seg.count },
                instance_buffer: self.glyph_buffer,
                instance_offset: seg.start as usize * GLYPH_STRIDE,
                uniforms: uniform_bytes,
                scissor,
            },
            SegmentKind::Mesh => DrawCommand {
                pipeline: self.mesh_pipeline,
                bind_group: None,
                geometry: Geometry::IndexedMesh {
                    vertex_buffer: self.mesh_vertex_buffer,
                    index_buffer: self.mesh_index_buffer,
                    index_offset: seg.start,
                    index_count: seg.count,
                },
                // The mesh draw reads its per-vertex buffer at index 0 and has no
                // per-instance data; the instance buffer is unused (the vertex
                // buffer is passed only to fill the field).
                instance_buffer: self.mesh_vertex_buffer,
                instance_offset: 0,
                uniforms: uniform_bytes,
                scissor,
            },
        }
    }
}

/// Convert a pixel-space clip rect into a `(x, y, w, h)` scissor rectangle in
/// integer physical pixels, clamped to the viewport.
///
/// Scissor rects must stay within the surface — Metal errors on an
/// out-of-bounds `setScissorRect`. We floor the origin and ceil the far edge so
/// the integer rect never clips *inside* the requested float rect, then clamp
/// both to `[0, viewport]`. A fully off-screen or empty clip yields a zero-area
/// rect (draws nothing), which is the correct clip result.
fn clip_to_scissor(clip: Rect, viewport: [f32; 2]) -> (u32, u32, u32, u32) {
    let vw = viewport[0].max(0.0);
    let vh = viewport[1].max(0.0);
    let x0 = clip.x.floor().clamp(0.0, vw);
    let y0 = clip.y.floor().clamp(0.0, vh);
    let x1 = (clip.x + clip.w).ceil().clamp(0.0, vw);
    let y1 = (clip.y + clip.h).ceil().clamp(0.0, vh);
    (
        x0 as u32,
        y0 as u32,
        (x1 - x0).max(0.0) as u32,
        (y1 - y0).max(0.0) as u32,
    )
}

/// View the viewport `[width, height]` as raw bytes for the inline uniform.
fn bytemuck_viewport(viewport: &[f32; 2]) -> &[u8] {
    // Safe: `[f32; 2]` is `#[repr(C)]`-equivalent POD with no padding.
    unsafe {
        core::slice::from_raw_parts(
            viewport.as_ptr() as *const u8,
            core::mem::size_of::<[f32; 2]>(),
        )
    }
}

/// View a slice of `#[repr(C)]` `Copy` POD instances as raw bytes for upload.
fn instances_as_bytes<T: Copy>(instances: &[T]) -> &[u8] {
    // Safe: `T` is a `GpuInstance` (`#[repr(C)]`, `Copy`, only POD scalars), so
    // its byte representation is a valid contiguous instance buffer.
    unsafe {
        core::slice::from_raw_parts(
            instances.as_ptr() as *const u8,
            core::mem::size_of_val(instances),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitive::{Border, ImageDraw, LayerClip, Quad, Rgba};
    use viso_gpu::{HeadlessRaster, RawWindowHandle};

    fn quad(x: f32, y: f32) -> Primitive {
        Primitive::Quad(Quad {
            rect: Rect {
                x,
                y,
                w: 10.0,
                h: 10.0,
            },
            color: Rgba {
                r: 1.0,
                g: 0.0,
                b: 0.0,
                a: 1.0,
            },
            radius: 0.0,
            border: Border::NONE,
        })
    }

    fn layer(x: f32, y: f32, w: f32, h: f32) -> Primitive {
        Primitive::Layer(LayerClip {
            clip: Rect { x, y, w, h },
            opacity: 1.0,
        })
    }

    /// A translucent layer: same clip rect, but `opacity < 1` triggers offscreen
    /// compositing.
    fn layer_opacity(x: f32, y: f32, w: f32, h: f32, opacity: f32) -> Primitive {
        Primitive::Layer(LayerClip {
            clip: Rect { x, y, w, h },
            opacity,
        })
    }

    fn image(texture: TextureId) -> Primitive {
        Primitive::Image(ImageDraw {
            rect: Rect {
                x: 0.0,
                y: 0.0,
                w: 8.0,
                h: 8.0,
            },
            uv: Rect {
                x: 0.0,
                y: 0.0,
                w: 1.0,
                h: 1.0,
            },
            tint: Rgba {
                r: 1.0,
                g: 1.0,
                b: 1.0,
                a: 1.0,
            },
            texture,
        })
    }

    /// Build a renderer over a headless surface, run `upload`, and return the
    /// resulting segments (plus the backend so callers can inspect textures).
    fn segments_for(prims: &[Primitive]) -> Vec<Segment> {
        let mut gpu = HeadlessRaster::new();
        let surface = gpu.create_surface(RawWindowHandle::Headless, 64, 64);
        let format = gpu.surface_format(surface);
        let mut r = Renderer::new(&mut gpu, format);
        r.upload(&mut gpu, prims);
        r.segments.clone()
    }

    #[test]
    fn adjacent_unclipped_quads_share_one_segment() {
        let segs = segments_for(&[quad(0.0, 0.0), quad(20.0, 20.0)]);
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].kind, SegmentKind::Quad);
        assert_eq!(segs[0].count, 2);
        assert_eq!(segs[0].clip, None);
    }

    #[test]
    fn layer_opens_a_clipped_segment_and_layer_end_restores() {
        let segs = segments_for(&[
            quad(0.0, 0.0),              // unclipped
            layer(5.0, 5.0, 30.0, 30.0), // push clip
            quad(10.0, 10.0),            // clipped
            Primitive::LayerEnd,         // pop
            quad(40.0, 40.0),            // unclipped again
        ]);
        assert_eq!(segs.len(), 3);
        assert_eq!(segs[0].clip, None);
        assert_eq!(
            segs[1].clip,
            Some(Rect {
                x: 5.0,
                y: 5.0,
                w: 30.0,
                h: 30.0,
            })
        );
        assert_eq!(segs[2].clip, None);
    }

    #[test]
    fn nested_layers_intersect_their_clips() {
        let segs = segments_for(&[
            layer(0.0, 0.0, 40.0, 40.0),
            layer(20.0, 10.0, 40.0, 40.0), // intersect → (20,10,20,30)
            quad(25.0, 15.0),
            Primitive::LayerEnd,
            Primitive::LayerEnd,
        ]);
        assert_eq!(segs.len(), 1);
        assert_eq!(
            segs[0].clip,
            Some(Rect {
                x: 20.0,
                y: 10.0,
                w: 20.0,
                h: 30.0,
            })
        );
    }

    #[test]
    fn image_between_quads_breaks_the_quad_run_and_preserves_order() {
        let mut gpu = HeadlessRaster::new();
        let surface = gpu.create_surface(RawWindowHandle::Headless, 64, 64);
        let format = gpu.surface_format(surface);
        let tex = gpu.create_texture(&viso_gpu::TextureDesc {
            width: 2,
            height: 2,
            format: TextureFormat::Bgra8Unorm,
            render_target: false,
            label: "t",
        });
        let mut r = Renderer::new(&mut gpu, format);
        r.upload(&mut gpu, &[quad(0.0, 0.0), image(tex), quad(20.0, 20.0)]);
        // Quad, Image, Quad — three segments, order preserved, quad run split.
        assert_eq!(r.segments.len(), 3);
        assert_eq!(r.segments[0].kind, SegmentKind::Quad);
        assert!(matches!(r.segments[1].kind, SegmentKind::Image { .. }));
        assert_eq!(r.segments[2].kind, SegmentKind::Quad);
    }

    #[test]
    fn same_texture_reuses_one_bind_group() {
        let mut gpu = HeadlessRaster::new();
        let surface = gpu.create_surface(RawWindowHandle::Headless, 64, 64);
        let format = gpu.surface_format(surface);
        let tex = gpu.create_texture(&viso_gpu::TextureDesc {
            width: 2,
            height: 2,
            format: TextureFormat::Bgra8Unorm,
            render_target: false,
            label: "t",
        });
        let mut r = Renderer::new(&mut gpu, format);
        r.upload(&mut gpu, &[image(tex), image(tex)]);
        // Two image segments, but one cached bind group for the shared texture.
        assert_eq!(r.segments.len(), 2);
        assert_eq!(r.texture_bindings.len(), 1);
    }

    #[test]
    fn clip_to_scissor_clamps_to_viewport() {
        let s = clip_to_scissor(
            Rect {
                x: 50.0,
                y: 50.0,
                w: 100.0,
                h: 100.0,
            },
            [64.0, 64.0],
        );
        assert_eq!(s, (50, 50, 14, 14));

        let empty = clip_to_scissor(
            Rect {
                x: 200.0,
                y: 200.0,
                w: 10.0,
                h: 10.0,
            },
            [64.0, 64.0],
        );
        assert_eq!(empty.2, 0);
        assert_eq!(empty.3, 0);
    }

    /// End-to-end headless glyph raster: a run painted onto a cleared surface
    /// must lay down ink where glyphs cover pixels and leave the background
    /// untouched far outside the text block. This exercises the full
    /// `SegmentKind::GlyphRun` path — R8 SDF sample → coverage decode →
    /// premultiplied blend — that the golden also covers, but with an explicit
    /// assertion on ink-vs-background so a regression names itself.
    #[test]
    fn glyph_run_paints_ink_over_background() {
        const W: u32 = 96;
        const H: u32 = 48;
        let mut gpu = HeadlessRaster::new();
        let surface = gpu.create_surface(RawWindowHandle::Headless, W, H);
        let format = gpu.surface_format(surface);
        let mut r = Renderer::new(&mut gpu, format);

        // Prepare a small run and upload its R8 SDF atlas.
        let tg = crate::test_glyphs([8.0, 6.0], 24.0);
        let atlas = gpu.create_texture(&viso_gpu::TextureDesc {
            width: tg.atlas_size,
            height: tg.atlas_size,
            format: TextureFormat::R8Unorm,
            render_target: false,
            label: "glyph-atlas",
        });
        gpu.write_texture(atlas, 0, 0, tg.atlas_size, tg.atlas_size, &tg.atlas_pixels);
        assert!(!tg.glyphs.is_empty(), "test run produced glyphs");

        let run = crate::GlyphRunDraw {
            glyphs: tg.glyphs.clone(),
            atlas,
            color: tg.color,
        };
        r.upload(&mut gpu, &[Primitive::GlyphRun(run)]);
        // Opaque black clear so any glyph ink (near-white) is unmistakable.
        r.submit(
            &mut gpu,
            surface,
            [0.0, 0.0, 0.0, 1.0],
            [W as f32, H as f32],
        );
        let px = gpu.read_pixels_bgra8(surface);

        let luma_at = |x: u32, y: u32| -> u32 {
            let i = ((y * W + x) * 4) as usize;
            px[i] as u32 + px[i + 1] as u32 + px[i + 2] as u32
        };

        // Bottom-right corner is far below/right of the two-line block: pure
        // background (black clear).
        assert_eq!(luma_at(W - 1, H - 1), 0, "corner must stay background");

        // Somewhere inside the first glyph's rect there must be lit ink. Scan
        // the first glyph's screen rect for the brightest pixel and require it
        // to be clearly above background.
        let g = &tg.glyphs[0];
        let (rx, ry) = (g.rect.x as u32, g.rect.y as u32);
        let (rw, rh) = (g.rect.w as u32, g.rect.h as u32);
        let mut brightest = 0;
        for y in ry..(ry + rh).min(H) {
            for x in rx..(rx + rw).min(W) {
                brightest = brightest.max(luma_at(x, y));
            }
        }
        assert!(
            brightest > 300,
            "first glyph rect must contain lit ink, got max luma {brightest}"
        );
    }

    #[test]
    fn opaque_layer_stays_in_pass_with_no_offscreen() {
        let mut gpu = HeadlessRaster::new();
        let surface = gpu.create_surface(RawWindowHandle::Headless, 64, 64);
        let format = gpu.surface_format(surface);
        let mut r = Renderer::new(&mut gpu, format);
        r.upload(
            &mut gpu,
            &[
                layer(5.0, 5.0, 30.0, 30.0), // opacity 1.0 → scissor clip
                quad(10.0, 10.0),
                Primitive::LayerEnd,
            ],
        );
        // No offscreen pass; the single clipped quad segment routes to the main
        // pass with a scissor, exactly as the Step 10a in-pass path.
        assert!(r.offscreen_passes.is_empty());
        assert_eq!(r.segments.len(), 1);
        assert_eq!(r.segments[0].target, PassTarget::Main);
        assert_eq!(
            r.segments[0].clip,
            Some(Rect {
                x: 5.0,
                y: 5.0,
                w: 30.0,
                h: 30.0,
            })
        );
    }

    #[test]
    fn translucent_layer_opens_offscreen_and_composites() {
        let mut gpu = HeadlessRaster::new();
        let surface = gpu.create_surface(RawWindowHandle::Headless, 64, 64);
        let format = gpu.surface_format(surface);
        let mut r = Renderer::new(&mut gpu, format);
        r.upload(
            &mut gpu,
            &[
                layer_opacity(8.0, 8.0, 20.0, 20.0, 0.5),
                quad(10.0, 10.0),
                Primitive::LayerEnd,
            ],
        );

        // Exactly one offscreen pass, sized to the layer rect.
        assert_eq!(r.offscreen_passes.len(), 1);
        let pass = &r.offscreen_passes[0];
        assert_eq!(pass.viewport, [20.0, 20.0]);
        assert_eq!(pass.opacity, 0.5);

        // The child quad routes to that offscreen pass, with its position shifted
        // into texture-local space (layer origin subtracted).
        let child = r
            .segments
            .iter()
            .find(|s| s.target == PassTarget::Offscreen(0))
            .expect("child quad segment routes to the offscreen pass");
        assert_eq!(child.kind, SegmentKind::Quad);
        assert_eq!(r.quad_scratch[child.start as usize].rect_pos, [2.0, 2.0]);

        // The main pass carries exactly one composite: an Image segment sampling
        // the offscreen texture, tinted white with alpha == opacity, positioned at
        // the layer's world rect.
        let composites: Vec<&Segment> = r
            .segments
            .iter()
            .filter(|s| s.target == PassTarget::Main)
            .collect();
        assert_eq!(composites.len(), 1);
        assert!(matches!(composites[0].kind, SegmentKind::Image { .. }));
        let inst = &r.image_scratch[composites[0].start as usize];
        assert_eq!(inst.rect_pos, [8.0, 8.0]);
        assert_eq!(inst.rect_size, [20.0, 20.0]);
        assert_eq!(inst.color, [1.0, 1.0, 1.0, 0.5]);
    }

    /// End-to-end headless composite: an opaque quad inside a `opacity == 0.5`
    /// layer, over an opaque background, must land at half strength — the pixel
    /// equals `background * 0.5 + quad * 0.5` (premultiplied over-blend with the
    /// composite alpha).
    #[test]
    fn translucent_layer_blends_at_half_strength() {
        const W: u32 = 32;
        const H: u32 = 32;
        let mut gpu = HeadlessRaster::new();
        let surface = gpu.create_surface(RawWindowHandle::Headless, W, H);
        let format = gpu.surface_format(surface);
        let mut r = Renderer::new(&mut gpu, format);

        // A green quad (fully opaque) filling the layer rect, composited at 0.5.
        let green = Primitive::Quad(Quad {
            rect: Rect {
                x: 8.0,
                y: 8.0,
                w: 12.0,
                h: 12.0,
            },
            color: Rgba {
                r: 0.0,
                g: 1.0,
                b: 0.0,
                a: 1.0,
            },
            radius: 0.0,
            border: Border::NONE,
        });
        r.upload(
            &mut gpu,
            &[
                layer_opacity(8.0, 8.0, 12.0, 12.0, 0.5),
                green,
                Primitive::LayerEnd,
            ],
        );
        // Opaque white background so the blend is unambiguous.
        r.submit(
            &mut gpu,
            surface,
            [1.0, 1.0, 1.0, 1.0],
            [W as f32, H as f32],
        );
        let px = gpu.read_pixels_bgra8(surface);

        // Center of the layer rect (14, 14): green over white at 0.5 →
        // BGRA ≈ (b=128, g=255, r=128, a=255) after quantization.
        let i = ((14 * W + 14) * 4) as usize;
        let (b, g, red, a) = (px[i], px[i + 1], px[i + 2], px[i + 3]);
        assert!((120..=135).contains(&b), "blue channel {b}");
        assert!(g >= 250, "green channel {g}");
        assert!((120..=135).contains(&red), "red channel {red}");
        assert_eq!(a, 255, "background alpha stays opaque");

        // A corner well outside the layer is untouched (pure white background).
        let c = ((W + 1) * 4) as usize;
        assert_eq!(
            (px[c], px[c + 1], px[c + 2]),
            (255, 255, 255),
            "corner must stay background"
        );
    }
}
