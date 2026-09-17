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

use viso_gpu::backend::{
    DrawCommand, DrawList, Geometry, IndexFormat, InlineUniforms, RenderPass, RenderTarget,
};
use viso_gpu::{
    BindGroupDesc, Binding, BlendMode, BufferUsage, Frame, GpuBackend, LoadOp, PipelineDesc,
    PipelineId, SamplerDesc, SurfaceId, TextureDesc, TextureFormat, TextureId,
};
use viso_gpu::{BindGroupId, SamplerId};

use viso_shader::{PipelineEntry, PipelineFamily, standard_manifest};

use crate::batch::{BatchFamily, BatchItem, BatchKey, BatchTarget, joins};
use crate::clip::{ClipShape, plan_clip};
use crate::gradient_lut::{GradientLutAtlas, LUT_WIDTH, LutAlloc, LutKey};
use crate::mask::{MaskCache, MaskKey, MaskKind, MaskRequest};
use crate::mask_page::MaskPage;
use crate::pool::InstancePool;
use crate::primitive::{
    AnalyticCapsuleInstance, AnalyticEllipseInstance, AnalyticLineInstance, AnalyticRRectInstance,
    GlyphInstance, GradientInstance, ImageInstance, MeshVertex, PathCmd, Primitive, QuadInstance,
    Rect, rgba_array,
};
use crate::raster_mask::{path_bounds, rasterize_path_coverage};
use crate::scene::store::{ClipFillRule, StoreRef, glyph_instances};
use crate::scene::{EmitContext, Scene};
use viso_math::InterpolationSpace;

/// Bytes of one quad instance.
const QUAD_STRIDE: usize = core::mem::size_of::<QuadInstance>();
/// Bytes of one analytic rounded-rectangle instance.
const ANALYTIC_RRECT_STRIDE: usize = core::mem::size_of::<AnalyticRRectInstance>();
/// Bytes of one analytic ellipse instance.
const ANALYTIC_ELLIPSE_STRIDE: usize = core::mem::size_of::<AnalyticEllipseInstance>();
/// Bytes of one analytic capsule instance.
const ANALYTIC_CAPSULE_STRIDE: usize = core::mem::size_of::<AnalyticCapsuleInstance>();
/// Bytes of one analytic line instance.
const ANALYTIC_LINE_STRIDE: usize = core::mem::size_of::<AnalyticLineInstance>();
/// Bytes of one image instance.
const IMAGE_STRIDE: usize = core::mem::size_of::<ImageInstance>();
/// Bytes of one glyph instance.
const GLYPH_STRIDE: usize = core::mem::size_of::<GlyphInstance>();
/// Bytes of one gradient instance.
const GRADIENT_STRIDE: usize = core::mem::size_of::<GradientInstance>();
/// Rows in the renderer-owned 1D gradient LUT atlas: each row is one baked ramp
/// (a 3+-stop or non-linear-space gradient), `LUT_WIDTH × ROWS` RGBA8. 64 rows
/// is 64 KB — ample for a frame's distinct multi-stop gradients while trivial
/// beside image/glyph atlases.
const GRADIENT_LUT_ROWS: u32 = 64;
/// Edge of the renderer-owned R8 clip/mask page (§14.4): one shared square
/// coverage texture ROIs pack into via the mask cache's max-rects allocator.
/// 2048² is 4 MiB of R8 — ample for a frame's distinct path clips, each a tight
/// ROI, while a single texture keeps every masked draw on one bind group.
const MASK_PAGE_SIZE: u32 = 2048;

/// A stable revision hash of a path's command stream, for the mask cache key
/// (§14.4). The coverage of a filled path is a pure function of its commands, so
/// hashing the raw `f32` bits of every point gives a key that changes exactly
/// when the geometry does: an unchanged path re-resolves to the same slot with
/// no re-raster, an edited path bumps the key and rebuilds.
fn hash_path_cmds(cmds: &[PathCmd]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    let pt = |p: crate::primitive::Point,
              hasher: &mut std::collections::hash_map::DefaultHasher| {
        p.x.to_bits().hash(hasher);
        p.y.to_bits().hash(hasher);
    };
    for cmd in cmds {
        match *cmd {
            PathCmd::MoveTo(p) => {
                0u8.hash(&mut h);
                pt(p, &mut h);
            }
            PathCmd::LineTo(p) => {
                1u8.hash(&mut h);
                pt(p, &mut h);
            }
            PathCmd::QuadTo(c, p) => {
                2u8.hash(&mut h);
                pt(c, &mut h);
                pt(p, &mut h);
            }
            PathCmd::CubicTo(c0, c1, p) => {
                3u8.hash(&mut h);
                pt(c0, &mut h);
                pt(c1, &mut h);
                pt(p, &mut h);
            }
            PathCmd::Close => 4u8.hash(&mut h),
        }
    }
    h.finish()
}

/// Whether a filled path's outline is a simple convex straight-edge polygon —
/// the shape class the tessellated path lane handles ideally (a fan of
/// triangles, geometry reused across pure translations).
///
/// A curve command (`QuadTo`/`CubicTo`) is never convex here: its flattened
/// outline is generally non-convex and its coverage is what analytic
/// rasterization is for, so any curve-bearing path returns `false`. For a
/// straight-edge outline, convexity is the sign of the cross product of
/// consecutive edge vectors: convex iff every turn has the same sign (the
/// wrapping edge back to the start included). Fewer than three vertices is
/// degenerate and treated as convex (nothing to mask).
///
/// This is the hot-path discriminator for [`mask_solid_fill`](Renderer::mask_solid_fill):
/// convex fills keep the tessellated lane (with its transform-only diff reuse),
/// concave or curved fills — exactly where coverage caching beats
/// re-tessellating — divert to the R8 mask lane.
fn path_is_convex(cmds: &[PathCmd]) -> bool {
    let mut pts: Vec<crate::primitive::Point> = Vec::with_capacity(cmds.len());
    for cmd in cmds {
        match *cmd {
            PathCmd::MoveTo(p) | PathCmd::LineTo(p) => pts.push(p),
            PathCmd::QuadTo(..) | PathCmd::CubicTo(..) => return false,
            PathCmd::Close => {}
        }
    }
    if pts.len() < 3 {
        return true;
    }
    let mut sign = 0.0f32;
    let n = pts.len();
    for i in 0..n {
        let a = pts[i];
        let b = pts[(i + 1) % n];
        let c = pts[(i + 2) % n];
        let cross = (b.x - a.x) * (c.y - b.y) - (b.y - a.y) * (c.x - b.x);
        if cross != 0.0 {
            if sign == 0.0 {
                sign = cross;
            } else if (cross > 0.0) != (sign > 0.0) {
                return false;
            }
        }
    }
    true
}

/// What a [`Segment`] draws, and where its geometry lives.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum SegmentKind {
    /// A run of adjacent quads sharing this segment's clip, in the quad buffer.
    /// `start`/`count` count instances in that buffer.
    Quad,
    /// A run of adjacent analytic rounded rectangles sharing this segment's
    /// clip, in the analytic-rrect buffer. `start`/`count` count instances.
    AnalyticRRect,
    /// A run of adjacent analytic ellipses sharing this segment's clip, in the
    /// analytic-ellipse buffer. `start`/`count` count instances.
    AnalyticEllipse,
    /// A run of adjacent analytic capsules sharing this segment's clip, in the
    /// analytic-capsule buffer. `start`/`count` count instances.
    AnalyticCapsule,
    /// A run of adjacent analytic lines sharing this segment's clip, in the
    /// analytic-line buffer. `start`/`count` count instances.
    AnalyticLine,
    /// A single image, in the image buffer, sampling `bind_group`'s texture.
    /// `start`/`count` count instances in that buffer.
    Image { bind_group: BindGroupId },
    /// One run of glyphs, in the glyph buffer, sampling `bind_group`'s A8 pool.
    /// `start`/`count` count instances in that buffer.
    GlyphRun { bind_group: BindGroupId },
    /// A single gradient fill, in the gradient buffer, sampling `bind_group`'s
    /// baked 1D LUT atlas. `start`/`count` count instances in that buffer.
    Gradient { bind_group: BindGroupId },
    /// A run of adjacent triangle meshes (Path/Mesh) sharing this segment's
    /// clip, in the shared mesh vertex/index buffers. `start`/`count` count
    /// **indices** in the mesh index buffer (vertices are addressed by the
    /// absolute indices baked into the index data).
    Mesh,
}

impl SegmentKind {
    /// The batch-planner family this kind draws through: the pipeline and the
    /// family buffer its geometry indexes.
    pub(crate) fn family(self) -> BatchFamily {
        match self {
            SegmentKind::Quad => BatchFamily::Quad,
            SegmentKind::AnalyticRRect => BatchFamily::AnalyticRRect,
            SegmentKind::AnalyticEllipse => BatchFamily::AnalyticEllipse,
            SegmentKind::AnalyticCapsule => BatchFamily::AnalyticCapsule,
            SegmentKind::AnalyticLine => BatchFamily::AnalyticLine,
            SegmentKind::Image { .. } => BatchFamily::Image,
            SegmentKind::GlyphRun { .. } => BatchFamily::GlyphRun,
            SegmentKind::Gradient { .. } => BatchFamily::Gradient,
            SegmentKind::Mesh => BatchFamily::Mesh,
        }
    }

    /// The resource bind group folded into this kind's [`BatchKey`], if any:
    /// the sampled texture/atlas for image and glyph draws, `None` for quad and
    /// mesh (which bind no per-draw resource).
    pub(crate) fn resource(self) -> Option<BindGroupId> {
        match self {
            SegmentKind::Image { bind_group }
            | SegmentKind::GlyphRun { bind_group }
            | SegmentKind::Gradient { bind_group } => Some(bind_group),
            SegmentKind::Quad
            | SegmentKind::AnalyticRRect
            | SegmentKind::AnalyticEllipse
            | SegmentKind::AnalyticCapsule
            | SegmentKind::AnalyticLine
            | SegmentKind::Mesh => None,
        }
    }
}

impl PassTarget {
    /// The batch-planner target this pass maps to (surface vs. offscreen `i`).
    pub(crate) fn batch_target(self) -> BatchTarget {
        match self {
            PassTarget::Main => BatchTarget::Main,
            PassTarget::Offscreen(i) => BatchTarget::Offscreen(i),
        }
    }
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
pub(crate) struct Segment {
    /// What this segment draws / which buffer its geometry indexes.
    pub(crate) kind: SegmentKind,
    /// Offset of the first instance or index (see `kind`) into its buffer.
    pub(crate) start: u32,
    /// Number of instances or indices (see `kind`) in the run.
    pub(crate) count: u32,
    /// The effective clip rect, or `None` for unclipped.
    ///
    /// For a segment in an offscreen pass this clip is already expressed in the
    /// offscreen texture's local space (the layer origin has been subtracted),
    /// so it scissors correctly against that pass's viewport.
    pub(crate) clip: Option<Rect>,
    /// Which render pass this segment belongs to.
    pub(crate) target: PassTarget,
}

impl Segment {
    /// The batch item this segment presents to the planner: its packed state
    /// key, its structural clip, and whether its family can grow a run. Two
    /// adjacent segments merge exactly when [`joins`] holds for their items —
    /// the single merge predicate every lowering and introspection site shares.
    fn batch_item(&self) -> BatchItem {
        BatchItem {
            key: BatchKey::pack(
                self.kind.family(),
                self.target.batch_target(),
                self.kind.resource(),
            ),
            clip: self.clip,
            mergeable: self.kind.family().mergeable(),
        }
    }
}

/// Which render pass a [`Segment`] is drawn into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PassTarget {
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

/// A (texture, sampler) pair's bind group, cached so repeated draws sharing both
/// reuse one bind group rather than allocating per frame. Sampler is part of the
/// key: the same texture drawn with Nearest and with Linear needs two bind groups.
struct TextureBinding {
    texture: TextureId,
    sampler: SamplerId,
    bind_group: BindGroupId,
}

/// Interns [`SamplerDesc`] to a shared [`SamplerId`] so the renderer keeps one
/// device sampler per distinct descriptor, never one per draw (§12, §17.1). The
/// cardinality is tiny (a handful of filter/address combinations), so a scanned
/// `Vec` is leaner than a hash map and matches `texture_bindings`' cold-path
/// pattern; a lookup happens once when a new descriptor first appears.
#[derive(Default)]
struct SamplerCache {
    entries: Vec<(SamplerDesc, SamplerId)>,
}

impl SamplerCache {
    /// Return the interned sampler for `desc`, creating it on first sight.
    fn intern<B: GpuBackend>(&mut self, backend: &mut B, desc: SamplerDesc) -> SamplerId {
        if let Some((_, id)) = self.entries.iter().find(|(d, _)| *d == desc) {
            return *id;
        }
        let id = backend.create_sampler(&desc);
        self.entries.push((desc, id));
        id
    }
}

/// The built-in pipelines the renderer prewarms once in [`Renderer::new`]
/// (§7.1): SolidRect (quad), Image, MaskComposite (glyph), PathFill (mesh),
/// AnalyticRRect, AnalyticEllipse, AnalyticCapsule, AnalyticLine, and Gradient.
/// Reported as `FrameStats::shader_pipeline_creations` — a construction-time
/// constant, since no draw ever triggers a runtime shader compile.
const SHADER_PIPELINE_PREWARM_COUNT: u32 = 9;

/// Draw-call and instance counts for the frame the renderer has just lowered.
///
/// Read after [`Renderer::upload`] and before [`Renderer::submit`]: `upload`
/// has built the full segment list (one segment per draw command, across the
/// offscreen and main passes) but `submit` has not consumed it. Exposed for
/// tooling/tests/benches (§34, §61); the steady-state bench asserts these stay
/// constant across identical frames, guarding the dispatch contract (§7.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FrameStats {
    /// Total draw commands this frame will encode, summed over every pass
    /// (offscreen passes plus the main surface pass, including composite draws).
    pub draw_calls: usize,
    /// Total geometry units across all segments: instances for
    /// quad/image/glyph/composite segments, indices for mesh segments. A stable
    /// scene keeps this fixed, so it doubles as a change detector for the bench.
    pub instances: usize,
    /// Retained primitives the ingest walk visited this frame (§61). One per
    /// `ingest_*` call, composites excluded (they are per-frame derived draws).
    pub visible_primitives: u32,
    /// Retained primitives whose diff moved at least one revision plane this
    /// frame — the steady-state target is 0 for an unchanged scene (§61, §8.4).
    pub dirty_primitives: u32,
    /// Quad instances retained this frame (§61).
    pub quad_instances: u32,
    /// Glyph instances retained this frame, summed across runs (§61).
    pub glyph_instances: u32,
    /// Paths (re-)tessellated this frame — a geometry/paint change or a cold
    /// append; a cache hit does not count (§61).
    pub path_tessellations: u32,
    /// Order-safe batches the planner emitted this frame (§9.6, §61): the count
    /// of contiguous draws after adjacent compatible primitives merged. One
    /// batch is one draw command today, so this equals `draw_calls`; the two
    /// stay separate counters because a later pass (indirect/multi-draw) can
    /// fold several batches into one call without changing the batch count.
    pub batches: usize,
    /// Render chunks encoded this frame (§30): one per draw command across all
    /// passes. One chunk is one draw command today, so this tracks `draw_calls`;
    /// kept separate so a later chunking pass can group draws without moving the
    /// draw-call count.
    pub render_chunks: usize,
    /// Times the encoded pipeline changed between two adjacent draw commands in
    /// execution order (§30). A run of same-family segments is one switch on
    /// entry; fewer switches means less state churn on the GPU.
    pub pipeline_switches: u32,
    /// Times the bound texture set changed between two adjacent draw commands in
    /// execution order (§30). Untextured (solid-rect) draws carry no binding, so
    /// a pure-quad frame reports 0.
    pub texture_binding_switches: u32,
    /// Contiguous upload ranges written to GPU buffers this frame, summed over
    /// every pool (§30, §9.3): the number of `write_buffer` calls the dirty-range
    /// coalescer produced. Zero on a steady frame; a single local change is one
    /// range, not one per slot.
    pub uploaded_ranges: usize,
    /// Offscreen render-to-texture passes this frame (§30): one per translucent
    /// layer. Zero for a flat scene with no group opacity (the D0 case).
    pub offscreen_passes: usize,
    /// Bytes backing this frame's transient offscreen render targets (§30):
    /// summed `width * height * 4` (Bgra8) over every offscreen pass. Zero when
    /// there are no offscreen passes.
    pub transient_target_bytes: usize,
    /// GPU pipelines created since the renderer was built (§30). The four
    /// builtins are prewarmed once at construction and never recompiled at
    /// steady state (§7.1: no runtime first-use compile), so this is a fixed
    /// prewarm count, not a per-frame value.
    pub shader_pipeline_creations: u32,
    /// Retained primitives skipped this frame because they fell fully outside the
    /// visible/clip region (§30). No culling stage exists yet, so this is 0;
    /// a later layer lights it up without changing the counter's meaning.
    pub culled_primitives: u32,
    /// Instance buffers rebuilt (grown, forcing a full re-upload) this frame
    /// (§30). Amortized to 0 at steady state once buffers reach their high-water
    /// mark; a later layer wires the pool's grow signal in here.
    pub instance_rebuilds: u32,
    /// Hardware clip masks built this frame (§30). D0 clips are axis-aligned
    /// scissors, never masks, so this is 0; the masked-clip layer lights it up.
    pub clip_mask_builds: u32,
    /// Bytes uploaded to GPU instance/vertex/index buffers this frame, summed
    /// over every pool (§61). Zero on a steady frame that changed nothing — a
    /// local paint change uploads only its changed slots (§9.1).
    pub gpu_upload_bytes: usize,
}

/// Turns per-frame primitives into GPU draw commands for one surface.
pub struct Renderer {
    /// The Quad built-in pipeline (registered once).
    quad_pipeline: PipelineId,
    /// The AnalyticRRect built-in pipeline (registered once).
    analytic_rrect_pipeline: PipelineId,
    /// The AnalyticEllipse built-in pipeline (registered once).
    analytic_ellipse_pipeline: PipelineId,
    /// The AnalyticCapsule built-in pipeline (registered once).
    analytic_capsule_pipeline: PipelineId,
    /// The AnalyticLine built-in pipeline (registered once).
    analytic_line_pipeline: PipelineId,
    /// The Image built-in pipeline (registered once).
    image_pipeline: PipelineId,
    /// The GlyphRun built-in pipeline (registered once).
    glyph_pipeline: PipelineId,
    /// The Gradient built-in pipeline (registered once).
    gradient_pipeline: PipelineId,
    /// The default linear-filter clamp sampler, used by glyph runs, gradient
    /// LUT sampling, and offscreen-layer compositing (all of which want bilinear
    /// clamp). Image draws select their sampler via `sampler_cache` instead.
    sampler: SamplerId,
    /// Interns image draws' [`SamplerDesc`] to shared [`SamplerId`]s (§12).
    sampler_cache: SamplerCache,
    /// Persistent quad instance pool: a long-lived device buffer uploaded per
    /// changed slot against a CPU shadow (§9.1), so a local paint change costs a
    /// local upload rather than a full-buffer re-upload.
    quad_pool: InstancePool<QuadInstance>,
    /// Persistent analytic rounded-rectangle instance pool (same slot-diff
    /// upload as `quad_pool`).
    analytic_rrect_pool: InstancePool<AnalyticRRectInstance>,
    /// Persistent analytic ellipse instance pool (same slot-diff upload as
    /// `quad_pool`).
    analytic_ellipse_pool: InstancePool<AnalyticEllipseInstance>,
    /// Persistent analytic capsule instance pool (same slot-diff upload as
    /// `quad_pool`).
    analytic_capsule_pool: InstancePool<AnalyticCapsuleInstance>,
    /// Persistent analytic line instance pool (same slot-diff upload as
    /// `quad_pool`).
    analytic_line_pool: InstancePool<AnalyticLineInstance>,
    /// Persistent image instance pool (same slot-diff upload as `quad_pool`).
    image_pool: InstancePool<ImageInstance>,
    /// Persistent glyph instance pool (same slot-diff upload as `quad_pool`).
    glyph_pool: InstancePool<GlyphInstance>,
    /// Persistent gradient instance pool (same slot-diff upload as `quad_pool`).
    gradient_pool: InstancePool<GradientInstance>,
    /// The renderer-owned 1D gradient LUT atlas: 3+-stop and non-linear-space
    /// gradients bake one ramp row here and sample `(t, lut_v)`. Unlike the
    /// image/glyph atlases (caller-owned textures), this atlas is internal — its
    /// `TextureId` is created once in [`Renderer::new`] and its dirty rows are
    /// flushed to the device each frame after lowering.
    gradient_lut: GradientLutAtlas,
    /// The retained clip/mask coverage cache (§14.4): keys a requested mask to a
    /// packed slot in [`mask_page`](Self::mask_page) so a stable mask is
    /// rasterized once and reused. Cold-path policy; the physical texture lives
    /// in `mask_page`.
    mask_cache: MaskCache,
    /// The physical R8 mask page — the GPU texture, CPU backing, and dirty rect
    /// the resolved coverage from `mask_cache` blits into. Internal texture like
    /// `gradient_lut`; created once in [`Renderer::new`], drained each frame.
    mask_page: MaskPage,
    /// Clip masks rasterized this frame (§30/§61). Reset at frame start, bumped
    /// each time a masked clip actually rasterizes coverage; surfaced as
    /// [`FrameStats::clip_mask_builds`]. Steady state evicts/rasterizes nothing,
    /// so this returns to 0.
    mask_builds_this_frame: u32,
    /// The general triangle-mesh pipeline (Path/Mesh), registered once.
    mesh_pipeline: PipelineId,
    /// Persistent mesh vertex pool (slot-diff upload; vertices, not instances).
    mesh_vertex_pool: InstancePool<MeshVertex>,
    /// Persistent mesh index pool (slot-diff upload of the `u32` index stream).
    mesh_index_pool: InstancePool<u32>,
    /// Cached per-texture bind groups, reused across frames.
    texture_bindings: Vec<TextureBinding>,
    /// Scratch quad instance data, reused each frame.
    quad_scratch: Vec<QuadInstance>,
    /// Scratch analytic rounded-rectangle instance data, reused each frame.
    analytic_rrect_scratch: Vec<AnalyticRRectInstance>,
    /// Scratch analytic ellipse instance data, reused each frame.
    analytic_ellipse_scratch: Vec<AnalyticEllipseInstance>,
    /// Scratch analytic capsule instance data, reused each frame.
    analytic_capsule_scratch: Vec<AnalyticCapsuleInstance>,
    /// Scratch analytic line instance data, reused each frame.
    analytic_line_scratch: Vec<AnalyticLineInstance>,
    /// Scratch image instance data, reused each frame.
    image_scratch: Vec<ImageInstance>,
    /// Scratch glyph instance data, reused each frame.
    glyph_scratch: Vec<GlyphInstance>,
    /// Scratch gradient instance data, reused each frame.
    gradient_scratch: Vec<GradientInstance>,
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
    /// Per-pass viewports for this frame, reused each frame. Index 0 is the
    /// surface; the rest map 1:1 to `offscreen_passes`. Their bytes feed each
    /// command's inline uniform (copied by value, no borrow).
    viewports: Vec<[f32; 2]>,
    /// The frame's draw commands, flat across all passes in execution order,
    /// reused each frame. `passes` slices this by range. Borrow-free, so its
    /// backing allocation is retained across frames via `clear` (0 steady-state
    /// heap allocations in `encode`, §7.1).
    commands: Vec<DrawCommand>,
    /// The frame's render passes (offscreen layers first, then surface), reused
    /// each frame. Each references its commands by range into `commands`.
    passes: Vec<RenderPass>,
    /// The retained scene (§8), diffed each frame from the ingested primitive
    /// stream and the single source of truth for the frame's GPU scratch: the
    /// stream walk only folds primitives into its stores and records paint order,
    /// then [`Renderer::lower_from_scene`] derives every instance buffer and
    /// [`Segment`] from the scene (§8).
    scene: Scene,
    /// Bytes uploaded to instance/vertex/index buffers by the last
    /// [`upload`](Self::upload), summed over every pool (each pool's
    /// [`last_upload_bytes`](InstancePool::last_upload_bytes)). Zero on a steady
    /// frame that changed nothing. Read by [`frame_stats`](Self::frame_stats)
    /// into `FrameStats::gpu_upload_bytes` (§61); a plain counter, no alloc.
    gpu_upload_bytes: usize,
    /// Contiguous upload ranges the last [`upload`](Self::upload) wrote across
    /// every pool — the sum of each pool's [`sync`](InstancePool::sync) return,
    /// which is the number of `write_buffer` calls the dirty-range coalescer
    /// produced (§9.3). Zero on a steady frame; a single local change is one
    /// range. Read into `FrameStats::uploaded_ranges` (§30); a plain counter.
    uploaded_ranges: usize,
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
        // Every standard pipeline is created from its frozen manifest entry, not
        // from caller-assembled source. This is the device-init prewarm (§7.1):
        // the fixed set is materialized once, so no draw ever triggers a runtime
        // shader compile.
        let manifest = standard_manifest();
        let desc = |entry: &PipelineEntry, label: &'static str| PipelineDesc {
            label,
            builtin: entry.builtin,
            variant: entry.variant.packed(),
            msl: entry.msl,
            vertex_entry: entry.vertex_entry,
            fragment_entry: entry.fragment_entry,
            color_format: surface_format,
            depth_format: None,
            blend: BlendMode::PremultipliedOver,
            instance_schema: entry.schema,
        };
        let entry = |family| {
            manifest
                .entry(family)
                .expect("standard manifest populates every built-in family")
        };

        let quad_pipeline = backend
            .create_pipeline(
                &desc(entry(PipelineFamily::SolidRect), "quad"),
                &QuadInstance::LAYOUT,
            )
            .expect("QuadInstance layout matches the quad shader schema");

        let analytic_rrect_pipeline = backend
            .create_pipeline(
                &desc(entry(PipelineFamily::AnalyticRRect), "analytic-rrect"),
                &AnalyticRRectInstance::LAYOUT,
            )
            .expect("AnalyticRRectInstance layout matches the analytic-rrect shader schema");

        let analytic_ellipse_pipeline = backend
            .create_pipeline(
                &desc(entry(PipelineFamily::AnalyticEllipse), "analytic-ellipse"),
                &AnalyticEllipseInstance::LAYOUT,
            )
            .expect("AnalyticEllipseInstance layout matches the analytic-ellipse shader schema");

        let analytic_capsule_pipeline = backend
            .create_pipeline(
                &desc(entry(PipelineFamily::AnalyticCapsule), "analytic-capsule"),
                &AnalyticCapsuleInstance::LAYOUT,
            )
            .expect("AnalyticCapsuleInstance layout matches the analytic-capsule shader schema");

        let analytic_line_pipeline = backend
            .create_pipeline(
                &desc(entry(PipelineFamily::AnalyticLine), "analytic-line"),
                &AnalyticLineInstance::LAYOUT,
            )
            .expect("AnalyticLineInstance layout matches the analytic-line shader schema");

        let image_pipeline = backend
            .create_pipeline(
                &desc(entry(PipelineFamily::Image), "image"),
                &ImageInstance::LAYOUT,
            )
            .expect("ImageInstance layout matches the image shader schema");

        let glyph_pipeline = backend
            .create_pipeline(
                &desc(entry(PipelineFamily::MaskComposite), "glyph"),
                &GlyphInstance::LAYOUT,
            )
            .expect("GlyphInstance layout matches the glyph shader schema");

        let mesh_pipeline = backend
            .create_pipeline(
                &desc(entry(PipelineFamily::PathFill), "mesh"),
                &MeshVertex::LAYOUT,
            )
            .expect("MeshVertex layout matches the mesh shader schema");

        let gradient_pipeline = backend
            .create_pipeline(
                &desc(entry(PipelineFamily::Gradient), "gradient"),
                &GradientInstance::LAYOUT,
            )
            .expect("GradientInstance layout matches the gradient shader schema");

        // The 1D gradient LUT atlas is renderer-internal: baked from stops at
        // lowering, uploaded into this texture before the pass. Unlike image and
        // glyph textures (caller-owned), the renderer creates and owns it here.
        let lut_texture = backend.create_texture(&TextureDesc {
            width: LUT_WIDTH,
            height: GRADIENT_LUT_ROWS,
            format: GradientLutAtlas::FORMAT,
            render_target: false,
            label: "gradient-lut",
        });

        // The R8 mask page is renderer-internal like the gradient LUT: resolved
        // clip/mask coverage is blitted into this texture's CPU backing and
        // uploaded before the pass. `MASK_PAGE_SIZE` is a single shared page;
        // real UI clips are small ROIs that pack many-to-a-page.
        let mask_texture = backend.create_texture(&TextureDesc {
            width: MASK_PAGE_SIZE,
            height: MASK_PAGE_SIZE,
            format: MaskPage::FORMAT,
            render_target: false,
            label: "mask-page",
        });

        let default_sampler_desc = SamplerDesc::LINEAR_CLAMP;
        let sampler = backend.create_sampler(&default_sampler_desc);
        // Seed the image sampler cache with the default so a default-sampler
        // image draw reuses this one device sampler instead of creating another.
        let sampler_cache = SamplerCache {
            entries: vec![(default_sampler_desc, sampler)],
        };

        Self {
            quad_pipeline,
            analytic_rrect_pipeline,
            analytic_ellipse_pipeline,
            analytic_capsule_pipeline,
            analytic_line_pipeline,
            image_pipeline,
            glyph_pipeline,
            gradient_pipeline,
            sampler,
            sampler_cache,
            quad_pool: InstancePool::new(BufferUsage::INSTANCE, "quad-instances"),
            analytic_rrect_pool: InstancePool::new(
                BufferUsage::INSTANCE,
                "analytic-rrect-instances",
            ),
            analytic_ellipse_pool: InstancePool::new(
                BufferUsage::INSTANCE,
                "analytic-ellipse-instances",
            ),
            analytic_capsule_pool: InstancePool::new(
                BufferUsage::INSTANCE,
                "analytic-capsule-instances",
            ),
            analytic_line_pool: InstancePool::new(BufferUsage::INSTANCE, "analytic-line-instances"),
            image_pool: InstancePool::new(BufferUsage::INSTANCE, "image-instances"),
            glyph_pool: InstancePool::new(BufferUsage::INSTANCE, "glyph-instances"),
            gradient_pool: InstancePool::new(BufferUsage::INSTANCE, "gradient-instances"),
            gradient_lut: GradientLutAtlas::new(GRADIENT_LUT_ROWS, lut_texture),
            mask_cache: MaskCache::new(MASK_PAGE_SIZE),
            mask_page: MaskPage::new(MASK_PAGE_SIZE, mask_texture),
            mask_builds_this_frame: 0,
            mesh_pipeline,
            mesh_vertex_pool: InstancePool::new(BufferUsage::VERTEX, "mesh-vertices"),
            mesh_index_pool: InstancePool::new(BufferUsage::INDEX, "mesh-indices"),
            texture_bindings: Vec::with_capacity(8),
            quad_scratch: Vec::with_capacity(256),
            analytic_rrect_scratch: Vec::with_capacity(256),
            analytic_ellipse_scratch: Vec::with_capacity(256),
            analytic_capsule_scratch: Vec::with_capacity(256),
            analytic_line_scratch: Vec::with_capacity(256),
            image_scratch: Vec::with_capacity(64),
            glyph_scratch: Vec::with_capacity(256),
            gradient_scratch: Vec::with_capacity(64),
            mesh_vertex_scratch: Vec::with_capacity(1024),
            mesh_index_scratch: Vec::with_capacity(2048),
            segments: Vec::with_capacity(8),
            layer_stack: Vec::with_capacity(8),
            offscreen_passes: Vec::with_capacity(4),
            offscreen_pool: Vec::with_capacity(4),
            offscreen_pool_used: 0,
            viewports: Vec::with_capacity(4),
            commands: Vec::with_capacity(8),
            passes: Vec::with_capacity(4),
            scene: Scene::new(),
            gpu_upload_bytes: 0,
            uploaded_ranges: 0,
        }
    }

    /// Get (or lazily create) the bind group pairing `texture` with `sampler`.
    /// Cached across frames keyed by both, so a repeated (texture, sampler) pair
    /// reuses its bind group (no per-frame allocation in steady state); the same
    /// texture sampled two ways keeps two bind groups.
    fn bind_group_for<B: GpuBackend>(
        &mut self,
        backend: &mut B,
        texture: TextureId,
        sampler: SamplerId,
    ) -> BindGroupId {
        if let Some(tb) = self
            .texture_bindings
            .iter()
            .find(|tb| tb.texture == texture && tb.sampler == sampler)
        {
            return tb.bind_group;
        }
        let bind_group = backend.create_bind_group(&BindGroupDesc {
            label: "image",
            bindings: vec![Binding::Texture(texture), Binding::Sampler(sampler)],
        });
        self.texture_bindings.push(TextureBinding {
            texture,
            sampler,
            bind_group,
        });
        bind_group
    }

    /// Lower a solid-fill-only [`Primitive::Path`] as a self-masked solid fill
    /// (§14.4): rasterize its own coverage into an R8 mask-page slot and emit one
    /// masked draw — the ROI world rect as geometry, the slot as UV, the fill as
    /// the constant color — through the reused glyph coverage pipeline (a masked
    /// solid fill is one coverage texture times a constant color, exactly the
    /// glyph fragment, so no new pipeline or instance kind is needed).
    ///
    /// Returns `true` when the fill was lowered this way; `false` when it should
    /// fall through to the tessellated path lane — a degenerate ROI, or a mask the
    /// cache declined to allocate (empty/oversize), in which case the tessellated
    /// fill (bounded by the active scissor) is the correct fallback.
    fn mask_solid_fill(
        &mut self,
        path: &crate::primitive::Path,
        fill: crate::primitive::Rgba,
        ctx: EmitContext,
        clip: Option<Rect>,
    ) -> bool {
        // The path's own bounds are the mask ROI; a degenerate path has no
        // coverage to mask, so it falls through to the (also-degenerate) lane.
        let roi = path_bounds(path.cmds.iter().copied());
        if roi.w <= 0.0 || roi.h <= 0.0 {
            return false;
        }

        // Plan the clip: a filled complex path plans a mask tier. The reject path
        // (EvenOdd, unreachable from `Primitive::Path` which is always NonZero)
        // would return a non-mask tier and fall through to the bounding scissor.
        let plan = plan_clip(ClipShape::Path { bounds: roi }, false);
        if !plan.tier.builds_mask() {
            return false;
        }

        // Key the mask on its geometry: the path's coverage is a function of its
        // command stream (NonZero at the primitive level), so a stable path builds
        // its mask once and reuses the slot every frame after.
        let key = MaskKey {
            kind: MaskKind::Path,
            source_revision: hash_path_cmds(&path.cmds),
            transform_bucket: 0,
            device_scale_q: 1,
            fill_rule: ClipFillRule::NonZero,
        };
        let request = MaskRequest {
            kind: MaskKind::Path,
            needs_color: false,
            roi,
            key,
        };
        let Some(res) = self.mask_cache.resolve(&request) else {
            // Empty or oversize ROI the packer declined — fall through to the
            // tessellated fill within the active scissor.
            return false;
        };

        // Rasterize on a cold build, a key change, or after a repack moved slot
        // origins (the whole page must be re-blitted regardless of cache-hit).
        if res.rasterized || self.mask_page.needs_full_reblit() {
            let cov = rasterize_path_coverage(
                path.cmds.iter().copied(),
                roi,
                1.0,
                res.slot.w,
                res.slot.h,
            );
            self.mask_page.blit(res.slot, &cov);
            self.mask_builds_this_frame += 1;
        }

        // Sample the slot sub-rect of the shared page as normalized UVs.
        let size = self.mask_page.size() as f32;
        let inst = GlyphInstance {
            rect_pos: [roi.x, roi.y],
            rect_size: [roi.w, roi.h],
            uv_pos: [res.slot.x as f32 / size, res.slot.y as f32 / size],
            uv_size: [res.slot.w as f32 / size, res.slot.h as f32 / size],
            color: [fill.r, fill.g, fill.b, fill.a],
        };
        let bounds = crate::scene::bounds::Bounds::from_world(roi, clip, 0.0, 0.0);
        self.scene
            .ingest_glyph_run(std::iter::once(inst), self.mask_page.texture(), ctx, bounds);
        true
    }

    /// Ingest this frame's primitive stream into the retained scene, then lower
    /// the retained stores to instance scratch + submission-ordered [`Segment`]s
    /// and upload the instance buffers.
    ///
    /// The stream walk is stateful — it resolves each primitive's lowering
    /// context `(clip, target, origin)` from the `Layer`/`LayerEnd` stack and
    /// opens/closes offscreen passes — but it no longer produces the GPU scratch
    /// directly: it only diffs each primitive into the stores (bumping the moved
    /// revision planes, §8.4) and records its paint-order slot. The scratch and
    /// segments are then derived from the retained scene by [`lower_from_scene`],
    /// which is the single source of truth (F3.3, §8).
    ///
    /// Growing an instance buffer allocates a new one only when the frame needs
    /// more capacity than before; steady-state frames reuse the buffers.
    ///
    /// [`lower_from_scene`]: Self::lower_from_scene
    pub fn upload<B: GpuBackend>(&mut self, backend: &mut B, primitives: &[Primitive]) {
        self.layer_stack.clear();
        self.offscreen_passes.clear();
        self.offscreen_pool_used = 0;
        self.scene.begin_frame();
        self.mask_cache.begin_frame();
        self.mask_builds_this_frame = 0;

        for prim in primitives {
            let (clip, target, origin) = self.active();
            let ctx = EmitContext {
                clip,
                offscreen: match target {
                    PassTarget::Main => None,
                    PassTarget::Offscreen(idx) => Some(idx),
                },
                origin,
            };
            match prim {
                Primitive::Quad(quad) => {
                    // Diff the world-space instance (origin not yet subtracted)
                    // into the retained store, bumping only the moved planes, and
                    // record its paint-order slot.
                    let inst = quad.to_instance();
                    let bounds = crate::scene::bounds::Bounds::from_world(
                        Rect {
                            x: inst.rect_pos[0],
                            y: inst.rect_pos[1],
                            w: inst.rect_size[0],
                            h: inst.rect_size[1],
                        },
                        clip,
                        0.0,
                        0.0,
                    );
                    self.scene.ingest_quad(inst, ctx, bounds);
                }
                Primitive::AnalyticRRect(rrect) => {
                    let inst = rrect.to_instance();
                    let bounds = crate::scene::bounds::Bounds::from_world(
                        Rect {
                            x: inst.rect_pos[0],
                            y: inst.rect_pos[1],
                            w: inst.rect_size[0],
                            h: inst.rect_size[1],
                        },
                        clip,
                        0.0,
                        0.0,
                    );
                    self.scene.ingest_analytic_rrect(inst, ctx, bounds);
                }
                Primitive::AnalyticEllipse(ellipse) => {
                    let inst = ellipse.to_instance();
                    let bounds = crate::scene::bounds::Bounds::from_world(
                        Rect {
                            x: inst.rect_pos[0],
                            y: inst.rect_pos[1],
                            w: inst.rect_size[0],
                            h: inst.rect_size[1],
                        },
                        clip,
                        0.0,
                        0.0,
                    );
                    self.scene.ingest_analytic_ellipse(inst, ctx, bounds);
                }
                Primitive::AnalyticCapsule(capsule) => {
                    let inst = capsule.to_instance();
                    let bounds = crate::scene::bounds::Bounds::from_world(
                        Rect {
                            x: inst.rect_pos[0],
                            y: inst.rect_pos[1],
                            w: inst.rect_size[0],
                            h: inst.rect_size[1],
                        },
                        clip,
                        0.0,
                        0.0,
                    );
                    self.scene.ingest_analytic_capsule(inst, ctx, bounds);
                }
                Primitive::AnalyticLine(line) => {
                    let inst = line.to_instance();
                    // AABB over both endpoints, inflated by half the stroke width
                    // plus the border on every side (a square cap can extend by a
                    // further half-width along the axis; `from_world`'s uniform
                    // inflation covers it, since cap extension never exceeds the
                    // perpendicular half-width already added).
                    let min_x = inst.p0[0].min(inst.p1[0]);
                    let min_y = inst.p0[1].min(inst.p1[1]);
                    let max_x = inst.p0[0].max(inst.p1[0]);
                    let max_y = inst.p0[1].max(inst.p1[1]);
                    let world = Rect {
                        x: min_x,
                        y: min_y,
                        w: max_x - min_x,
                        h: max_y - min_y,
                    };
                    let bounds = crate::scene::bounds::Bounds::from_world(
                        world,
                        clip,
                        inst.width + 2.0 * inst.border_width,
                        0.0,
                    );
                    self.scene.ingest_analytic_line(inst, ctx, bounds);
                }
                Primitive::Image(image) => {
                    let inst = image.to_instance();
                    let bounds = crate::scene::bounds::Bounds::from_world(
                        Rect {
                            x: inst.rect_pos[0],
                            y: inst.rect_pos[1],
                            w: inst.rect_size[0],
                            h: inst.rect_size[1],
                        },
                        clip,
                        0.0,
                        0.0,
                    );
                    self.scene
                        .ingest_image(inst, image.texture, image.sampler, ctx, bounds);
                }
                Primitive::Gradient(gradient) => {
                    // The LUT decision is a lowering-time property the renderer
                    // owns: a 2-stop linear-RGB gradient interpolates exactly in
                    // the shader (inline `color0`/`color1`), so it needs no LUT;
                    // a 3+-stop or non-linear-space gradient bakes a 1D row into
                    // the renderer-internal atlas and samples it (§10.1, §12.3).
                    let use_lut = gradient.stops.len() >= 3
                        || gradient.interp != InterpolationSpace::LinearRgb;
                    let lut_v = if use_lut {
                        let key = LutKey::new(&gradient.stops, gradient.interp, gradient.extend);
                        match self.gradient_lut.alloc(key.clone()) {
                            LutAlloc::Row { v, .. } => v,
                            LutAlloc::Overflow => {
                                // The atlas wiped and re-based on overflow; the
                                // retry lands in the freshly cleared atlas.
                                match self.gradient_lut.alloc(key) {
                                    LutAlloc::Row { v, .. } => v,
                                    LutAlloc::Overflow => 0.0,
                                }
                            }
                        }
                    } else {
                        0.0
                    };
                    let inst = gradient.to_instance(lut_v, use_lut);
                    let bounds = crate::scene::bounds::Bounds::from_world(
                        Rect {
                            x: inst.rect_pos[0],
                            y: inst.rect_pos[1],
                            w: inst.rect_size[0],
                            h: inst.rect_size[1],
                        },
                        clip,
                        0.0,
                        0.0,
                    );
                    self.scene
                        .ingest_gradient(inst, self.gradient_lut.texture(), ctx, bounds);
                }
                Primitive::Path(path) => {
                    // A concave or curve-bearing solid-fill-only path is a
                    // self-masked solid fill (§14.4): its own coverage is one R8
                    // mask, and the fill color times that coverage is exactly the
                    // glyph fragment, so it lowers as a single masked draw sampling
                    // its mask-page slot rather than through tessellation — the lane
                    // where coverage caching beats re-tessellating a complex outline.
                    // A convex straight-edge fill (a triangle, a quad) keeps the
                    // tessellated lane, which fans it trivially and reuses geometry
                    // across pure translations; a stroked path or a degenerate fill
                    // keeps that lane too.
                    if let Some(fill) = path.fill
                        && path.stroke.is_none()
                        && !path_is_convex(&path.cmds)
                        && self.mask_solid_fill(path, fill, ctx, clip)
                    {
                        continue;
                    }
                    self.scene
                        .ingest_path(path, ctx, crate::scene::bounds::Bounds::default());
                }
                Primitive::Mesh(mesh) => {
                    self.scene.ingest_mesh(
                        &mesh.vertices,
                        &mesh.indices,
                        ctx,
                        crate::scene::bounds::Bounds::default(),
                    );
                }
                Primitive::GlyphRun(run) => {
                    if run.glyphs.is_empty() {
                        continue;
                    }
                    let color = [run.color.r, run.color.g, run.color.b, run.color.a];
                    self.scene.ingest_glyph_run(
                        glyph_instances(&run.glyphs, color),
                        run.atlas,
                        ctx,
                        crate::scene::bounds::Bounds::default(),
                    );
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

        // Trim any store tail the walk did not revisit (the scene shrank) so the
        // retained stores hold exactly this frame's primitives.
        self.scene.finish_frame();

        // Reclaim masks this frame no longer references. A repack moves surviving
        // slot origins, so a `true` return forces every resolving mask to re-blit
        // next frame regardless of cache-hit status (eviction is a cold event; a
        // steady scene evicts nothing and this stays `false`).
        self.mask_page
            .set_needs_full_reblit(self.mask_cache.end_frame());

        // Derive the frame's scratch + segments from the retained scene.
        self.lower_from_scene(backend);

        // Flush any gradient LUT rows baked this frame into the atlas texture
        // (§12.2: bake once, upload once). The atlas coalesces the frame's newly
        // baked rows into one contiguous dirty span; a frame that baked no new
        // gradient uploads nothing.
        if let Some((x, y, w, h, bytes)) = self.gradient_lut.take_dirty() {
            backend.write_texture(self.gradient_lut.texture(), x, y, w, h, &bytes);
        }

        // Flush the frame's baked mask coverage into the R8 page in one batched
        // upload of the coalesced dirty sub-rect, mirroring the gradient LUT seam;
        // a frame that built no masks uploads nothing.
        if let Some((x, y, w, h, bytes)) = self.mask_page.take_dirty() {
            backend.write_texture(self.mask_page.texture(), x, y, w, h, &bytes);
        }

        // Reconcile each family's persistent device buffer with the freshly
        // lowered draw-order scratch, uploading only the slots that changed since
        // last frame (§9.1) — a local paint change is a local upload, an
        // unchanged frame uploads nothing.
        // Each `sync` returns the number of `write_buffer` calls it issued (the
        // coalesced dirty-range count, §9.3); summed, that is this frame's
        // `uploaded_ranges` counter (§30). A steady frame syncs zero ranges.
        self.uploaded_ranges = self.quad_pool.sync(backend, &self.quad_scratch)
            + self
                .analytic_rrect_pool
                .sync(backend, &self.analytic_rrect_scratch)
            + self
                .analytic_ellipse_pool
                .sync(backend, &self.analytic_ellipse_scratch)
            + self
                .analytic_capsule_pool
                .sync(backend, &self.analytic_capsule_scratch)
            + self
                .analytic_line_pool
                .sync(backend, &self.analytic_line_scratch)
            + self.image_pool.sync(backend, &self.image_scratch)
            + self.glyph_pool.sync(backend, &self.glyph_scratch)
            + self.gradient_pool.sync(backend, &self.gradient_scratch)
            + self
                .mesh_vertex_pool
                .sync(backend, &self.mesh_vertex_scratch)
            + self.mesh_index_pool.sync(backend, &self.mesh_index_scratch);

        // Sum the bytes each pool actually handed to the backend this frame into
        // the frame-scoped upload counter (§61). A steady frame that changed no
        // slot uploaded nothing, so this is 0 — the same signal the steady-state
        // bench uses to prove a local change stays a local upload (§9.1).
        self.gpu_upload_bytes = self.quad_pool.last_upload_bytes()
            + self.analytic_rrect_pool.last_upload_bytes()
            + self.analytic_ellipse_pool.last_upload_bytes()
            + self.analytic_capsule_pool.last_upload_bytes()
            + self.analytic_line_pool.last_upload_bytes()
            + self.image_pool.last_upload_bytes()
            + self.glyph_pool.last_upload_bytes()
            + self.gradient_pool.last_upload_bytes()
            + self.mesh_vertex_pool.last_upload_bytes()
            + self.mesh_index_pool.last_upload_bytes();
    }

    /// Lower the retained scene to this frame's instance scratch + submission-
    /// ordered [`Segment`]s (§8). The paint-order record is the frozen spine: for
    /// each entry, pull the retained store slot, subtract the emit's origin, push
    /// the instance(s), and open or extend the covering segment. Same-kind runs
    /// under the same clip and target merge into one draw; images, glyph runs, and
    /// composites are their own draws (each binds a texture).
    ///
    /// This replaces the old immediate walk as the single source of truth: the
    /// stores hold exactly this frame's primitives (after `finish_frame`), and the
    /// context each was emitted under (`clip`, `target`, `origin`) is on its
    /// paint-order entry, so the derived scratch is identical to what a direct
    /// walk of the stream would produce.
    fn lower_from_scene<B: GpuBackend>(&mut self, backend: &mut B) {
        self.quad_scratch.clear();
        self.analytic_rrect_scratch.clear();
        self.analytic_ellipse_scratch.clear();
        self.analytic_capsule_scratch.clear();
        self.analytic_line_scratch.clear();
        self.image_scratch.clear();
        self.glyph_scratch.clear();
        self.gradient_scratch.clear();
        self.mesh_vertex_scratch.clear();
        self.mesh_index_scratch.clear();
        self.segments.clear();

        // Take the paint-order record so the store/backend borrows below do not
        // overlap the record borrow. It is swapped back before returning, so the
        // record's `Vec` allocation is reused across frames (kept flat, §28).
        let record = std::mem::take(&mut self.scene.paint_order);
        for entry in &record {
            let clip = entry.context.clip;
            let target = match entry.context.offscreen {
                None => PassTarget::Main,
                Some(idx) => PassTarget::Offscreen(idx),
            };
            let origin = entry.context.origin;
            match entry.store {
                StoreRef::Quad(id) => {
                    let mut inst = self.scene.quads.get(id).expect("quad slot").instance;
                    inst.rect_pos[0] -= origin[0];
                    inst.rect_pos[1] -= origin[1];
                    let start = self.quad_scratch.len() as u32;
                    self.quad_scratch.push(inst);
                    // Extend the current segment only if the planner says this
                    // quad joins the previous draw; otherwise open a new one.
                    self.merge_or_push(Segment {
                        kind: SegmentKind::Quad,
                        start,
                        count: 1,
                        clip,
                        target,
                    });
                }
                StoreRef::AnalyticRRect(id) => {
                    let mut inst = self
                        .scene
                        .analytic_rrects
                        .get(id)
                        .expect("analytic-rrect slot")
                        .instance;
                    inst.rect_pos[0] -= origin[0];
                    inst.rect_pos[1] -= origin[1];
                    let start = self.analytic_rrect_scratch.len() as u32;
                    self.analytic_rrect_scratch.push(inst);
                    self.merge_or_push(Segment {
                        kind: SegmentKind::AnalyticRRect,
                        start,
                        count: 1,
                        clip,
                        target,
                    });
                }
                StoreRef::AnalyticEllipse(id) => {
                    let mut inst = self
                        .scene
                        .analytic_ellipses
                        .get(id)
                        .expect("analytic-ellipse slot")
                        .instance;
                    inst.rect_pos[0] -= origin[0];
                    inst.rect_pos[1] -= origin[1];
                    let start = self.analytic_ellipse_scratch.len() as u32;
                    self.analytic_ellipse_scratch.push(inst);
                    self.merge_or_push(Segment {
                        kind: SegmentKind::AnalyticEllipse,
                        start,
                        count: 1,
                        clip,
                        target,
                    });
                }
                StoreRef::AnalyticCapsule(id) => {
                    let mut inst = self
                        .scene
                        .analytic_capsules
                        .get(id)
                        .expect("analytic-capsule slot")
                        .instance;
                    inst.rect_pos[0] -= origin[0];
                    inst.rect_pos[1] -= origin[1];
                    let start = self.analytic_capsule_scratch.len() as u32;
                    self.analytic_capsule_scratch.push(inst);
                    self.merge_or_push(Segment {
                        kind: SegmentKind::AnalyticCapsule,
                        start,
                        count: 1,
                        clip,
                        target,
                    });
                }
                StoreRef::AnalyticLine(id) => {
                    let mut inst = self
                        .scene
                        .analytic_lines
                        .get(id)
                        .expect("analytic-line slot")
                        .instance;
                    // Both endpoints are world-space; shift by the emit origin.
                    inst.p0[0] -= origin[0];
                    inst.p0[1] -= origin[1];
                    inst.p1[0] -= origin[0];
                    inst.p1[1] -= origin[1];
                    let start = self.analytic_line_scratch.len() as u32;
                    self.analytic_line_scratch.push(inst);
                    self.merge_or_push(Segment {
                        kind: SegmentKind::AnalyticLine,
                        start,
                        count: 1,
                        clip,
                        target,
                    });
                }
                StoreRef::Image(id) => {
                    let e = self.scene.images.get(id).expect("image slot");
                    let mut inst = e.instance;
                    let texture = e.texture;
                    let sampler_desc = e.sampler;
                    let sampler = self.sampler_cache.intern(backend, sampler_desc);
                    let bind_group = self.bind_group_for(backend, texture, sampler);
                    inst.rect_pos[0] -= origin[0];
                    inst.rect_pos[1] -= origin[1];
                    let start = self.image_scratch.len() as u32;
                    self.image_scratch.push(inst);
                    // Each image is its own draw (it binds a texture); the image
                    // family is unmergeable, so this always opens a new segment.
                    self.merge_or_push(Segment {
                        kind: SegmentKind::Image { bind_group },
                        start,
                        count: 1,
                        clip,
                        target,
                    });
                }
                StoreRef::Gradient(id) => {
                    let e = self.scene.gradients.get(id).expect("gradient slot");
                    let mut inst = e.instance;
                    let texture = e.texture;
                    let bind_group = self.bind_group_for(backend, texture, self.sampler);
                    inst.rect_pos[0] -= origin[0];
                    inst.rect_pos[1] -= origin[1];
                    let start = self.gradient_scratch.len() as u32;
                    self.gradient_scratch.push(inst);
                    // Each gradient binds its baked LUT atlas; the gradient family
                    // is unmergeable, so this always opens a new segment.
                    self.merge_or_push(Segment {
                        kind: SegmentKind::Gradient { bind_group },
                        start,
                        count: 1,
                        clip,
                        target,
                    });
                }
                StoreRef::Composite { mut instance, pass } => {
                    // A composite draws into the parent target (main / unclipped /
                    // zero-origin) sampling offscreen pass `pass`; its bind group
                    // is the pass's live sampling bind group.
                    let bind_group = self.offscreen_passes[pass].bind_group;
                    instance.rect_pos[0] -= origin[0];
                    instance.rect_pos[1] -= origin[1];
                    let start = self.image_scratch.len() as u32;
                    self.image_scratch.push(instance);
                    self.merge_or_push(Segment {
                        kind: SegmentKind::Image { bind_group },
                        start,
                        count: 1,
                        clip,
                        target,
                    });
                }
                StoreRef::GlyphRun(run) => {
                    let e = *self.scene.glyph_runs.run(run).expect("glyph run slot");
                    let bind_group = self.bind_group_for(backend, e.atlas, self.sampler);
                    let start = self.glyph_scratch.len() as u32;
                    for g in self.scene.glyph_runs.glyphs(&e) {
                        let mut inst = *g;
                        inst.rect_pos[0] -= origin[0];
                        inst.rect_pos[1] -= origin[1];
                        self.glyph_scratch.push(inst);
                    }
                    let count = self.glyph_scratch.len() as u32 - start;
                    // A run is one instanced draw (it binds the atlas texture);
                    // the glyph family is unmergeable, so this opens a new segment.
                    self.merge_or_push(Segment {
                        kind: SegmentKind::GlyphRun { bind_group },
                        start,
                        count,
                        clip,
                        target,
                    });
                }
                StoreRef::Path(id) => {
                    let e = self.scene.paths.get(id).expect("path slot");
                    let index_start = self.mesh_index_scratch.len() as u32;
                    let base = self.mesh_vertex_scratch.len() as u32;
                    // Color and place the retained colorless geometry here at
                    // lowering: apply the per-primitive transform to position and
                    // the per-primitive paint to color, producing the frozen
                    // `MeshVertex` layout. The cached geometry stays colorless and
                    // in its own local space, so a recolor/transform-only change
                    // reuses it without re-tessellating (§13.4).
                    //
                    // The colored vertices flow through the `mesh_vertex_pool`
                    // ring, whose CPU shadow diff makes an unchanged geometry +
                    // paint + transform frame a zero-byte upload — the F4 "static
                    // geometry stays off the ring" guarantee in effect for the
                    // steady state. A dedicated device-local geometry buffer that
                    // keeps the colorless mesh resident and passes paint/transform
                    // through a per-primitive uniform (so even a dirty recolor
                    // uploads no vertices) needs a mesh-shader uniform dimension
                    // and is a §13 follow-up.
                    let geo = &e.geometry;
                    let [ox, oy] = e.xform.offset;
                    let scale = e.xform.scale;
                    let fill_col = e.paint.fill.map(rgba_array).unwrap_or([0.0; 4]);
                    let stroke_col = e.paint.stroke_color.map(rgba_array).unwrap_or([0.0; 4]);
                    let opacity = e.paint.opacity;
                    self.mesh_vertex_scratch.reserve(geo.verts.len());
                    for (vi, v) in geo.verts.iter().enumerate() {
                        let mut col = if (vi as u32) < geo.fill_vert_end {
                            fill_col
                        } else {
                            stroke_col
                        };
                        col[3] *= opacity;
                        self.mesh_vertex_scratch.push(MeshVertex {
                            pos: [v.pos[0] * scale + ox, v.pos[1] * scale + oy],
                            color: col,
                            edge: v.edge,
                        });
                    }
                    self.mesh_index_scratch
                        .extend(geo.indices.iter_u32().map(|i| base + i));
                    self.translate_vertices(base as usize, origin);
                    let count = self.mesh_index_scratch.len() as u32 - index_start;
                    self.push_mesh_segment(index_start, count, clip, target);
                }
                StoreRef::Mesh(id) => {
                    let e = self.scene.meshes.get(id).expect("mesh slot");
                    let index_start = self.mesh_index_scratch.len() as u32;
                    let base = self.mesh_vertex_scratch.len() as u32;
                    self.mesh_vertex_scratch.extend_from_slice(&e.vertices);
                    self.mesh_index_scratch
                        .extend(e.indices.iter().map(|&i| base + i));
                    self.translate_vertices(base as usize, origin);
                    let count = self.mesh_index_scratch.len() as u32 - index_start;
                    self.push_mesh_segment(index_start, count, clip, target);
                }
            }
        }
        self.scene.paint_order = record;
    }

    /// The frame's counters (§30, §61), as they stand after the last
    /// [`upload`](Self::upload).
    ///
    /// Every [`Segment`] maps 1:1 to a draw command across all passes
    /// (composites included), so `draw_calls`, `batches`, and `render_chunks`
    /// all equal the segment count today; a scene whose nodes did not change
    /// lowers to the same segments and keeps them fixed. `instances` sums each
    /// segment's `count`. `pipeline_switches` and `texture_binding_switches`
    /// walk the segments in submission order and count the transitions between
    /// adjacent draws. `uploaded_ranges` and `gpu_upload_bytes` come from the
    /// pool syncs. `offscreen_passes` and `transient_target_bytes` come from
    /// this frame's translucent-layer passes. `shader_pipeline_creations` is the
    /// fixed prewarm count (§7.1: no runtime compile). The counters with no
    /// source in this layer stay 0 with their meaning fixed (see [`FrameStats`]).
    pub fn frame_stats(&self) -> FrameStats {
        let ingest = self.scene.ingest_stats;

        // Walk the draw segments in submission order, counting a pipeline switch
        // whenever the pipeline family changes between adjacent draws and a
        // texture-binding switch whenever the bound texture set changes. The
        // first draw is a switch on entry from the cleared state.
        let mut pipeline_switches = 0u32;
        let mut texture_binding_switches = 0u32;
        let mut prev: Option<(BatchFamily, Option<BindGroupId>)> = None;
        for seg in &self.segments {
            let family = seg.kind.family();
            let binding = seg.kind.resource();
            match prev {
                None => {
                    pipeline_switches += 1;
                    if binding.is_some() {
                        texture_binding_switches += 1;
                    }
                }
                Some((prev_family, prev_binding)) => {
                    if prev_family != family {
                        pipeline_switches += 1;
                    }
                    if prev_binding != binding {
                        texture_binding_switches += 1;
                    }
                }
            }
            prev = Some((family, binding));
        }

        // Transient offscreen targets: one Bgra8 texture per translucent-layer
        // pass, sized to its viewport (physical pixels).
        let transient_target_bytes = self
            .offscreen_passes
            .iter()
            .map(|p| (p.viewport[0] as usize) * (p.viewport[1] as usize) * 4)
            .sum();

        FrameStats {
            draw_calls: self.segments.len(),
            instances: self.segments.iter().map(|s| s.count as usize).sum(),
            visible_primitives: ingest.visible_primitives,
            dirty_primitives: ingest.dirty_primitives,
            quad_instances: ingest.quad_instances,
            glyph_instances: ingest.glyph_instances,
            path_tessellations: ingest.path_tessellations,
            batches: self.segments.len(),
            render_chunks: self.segments.len(),
            pipeline_switches,
            texture_binding_switches,
            uploaded_ranges: self.uploaded_ranges,
            offscreen_passes: self.offscreen_passes.len(),
            transient_target_bytes,
            shader_pipeline_creations: SHADER_PIPELINE_PREWARM_COUNT,
            // Masks rasterized into the page this frame (§14.4): cold builds,
            // re-rasters after a key change, and re-blits after a repack.
            clip_mask_builds: self.mask_builds_this_frame,
            // Counters no stage below D0 lights up yet; meaning fixed, value 0.
            culled_primitives: 0,
            instance_rebuilds: 0,
            gpu_upload_bytes: self.gpu_upload_bytes,
        }
    }

    /// A snapshot of the retained scene's revision planes (§8.4, §62), as they
    /// stand after the last [`upload`](Self::upload).
    ///
    /// Each plane is an independent monotone counter the ingest diff bumps only
    /// when the change it names actually moved — a recolor advances `paint` and
    /// leaves `geometry` where it was. Comparing two snapshots across frames
    /// tells a consumer (or a test, or Studio) exactly which dimension of the
    /// scene changed. Cold-path introspection only; not read on the hot path.
    pub fn scene_revisions(&self) -> crate::scene::revision::Revisions {
        self.scene.revisions
    }

    /// The frame's draw segments, for the cold-path batch introspection surface
    /// ([`inspect_batches`](Self::inspect_batches)). Read in the same window as
    /// [`frame_stats`](Self::frame_stats).
    pub(crate) fn segments_snapshot(&self) -> &[Segment] {
        &self.segments
    }

    /// The retained scene, for the cold-path primitive introspection surface
    /// ([`inspect_primitives`](Self::inspect_primitives)). Read in the same
    /// window as [`frame_stats`](Self::frame_stats): the paint-order record
    /// holds the frame just lowered.
    pub(crate) fn scene_snapshot(&self) -> &Scene {
        &self.scene
    }

    /// The Quad pipeline handle, for batch introspection.
    pub(crate) fn quad_pipeline_id(&self) -> PipelineId {
        self.quad_pipeline
    }

    /// The AnalyticRRect pipeline handle, for batch introspection.
    pub(crate) fn analytic_rrect_pipeline_id(&self) -> PipelineId {
        self.analytic_rrect_pipeline
    }

    /// The AnalyticEllipse pipeline handle, for batch introspection.
    pub(crate) fn analytic_ellipse_pipeline_id(&self) -> PipelineId {
        self.analytic_ellipse_pipeline
    }

    /// The AnalyticCapsule pipeline handle, for batch introspection.
    pub(crate) fn analytic_capsule_pipeline_id(&self) -> PipelineId {
        self.analytic_capsule_pipeline
    }

    /// The AnalyticLine pipeline handle, for batch introspection.
    pub(crate) fn analytic_line_pipeline_id(&self) -> PipelineId {
        self.analytic_line_pipeline
    }

    /// The Image pipeline handle, for batch introspection.
    pub(crate) fn image_pipeline_id(&self) -> PipelineId {
        self.image_pipeline
    }

    /// The GlyphRun pipeline handle, for batch introspection.
    pub(crate) fn glyph_pipeline_id(&self) -> PipelineId {
        self.glyph_pipeline
    }

    /// The Mesh pipeline handle, for batch introspection.
    pub(crate) fn mesh_pipeline_id(&self) -> PipelineId {
        self.mesh_pipeline
    }

    /// The Gradient pipeline handle, for batch introspection.
    pub(crate) fn gradient_pipeline_id(&self) -> PipelineId {
        self.gradient_pipeline
    }

    /// Add `segment` to the batch list, merging it into the previous segment
    /// when the planner says the two [`joins`], otherwise opening a new draw.
    ///
    /// This is the one merge site: the quad run in [`Self::lower_from_scene`],
    /// the mesh run in [`Self::push_mesh_segment`], and the batch dump in
    /// `inspect_primitives` all route their adjacency decision through the same
    /// [`joins`] predicate over [`BatchItem`]s, so the emitted segment
    /// boundaries can never drift from what introspection reports. A merge grows
    /// the previous segment's `count` by the incoming one's; the two always
    /// share a family buffer, so their geometry is already contiguous.
    fn merge_or_push(&mut self, segment: Segment) {
        let next = segment.batch_item();
        match self.segments.last_mut() {
            Some(prev) if joins(&prev.batch_item(), &next) => prev.count += segment.count,
            _ => self.segments.push(segment),
        }
    }

    /// Push (or extend) a mesh segment covering `count` indices at `index_start`.
    /// Adjacent meshes sharing the same batch key and clip merge into one
    /// indexed draw.
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
        self.merge_or_push(Segment {
            kind: SegmentKind::Mesh,
            start: index_start,
            count,
            clip,
            target,
        });
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

    /// Close the offscreen pass at `idx`, recording a composite draw in the
    /// scene's paint order: a textured quad at the layer's world-space rect
    /// sampling pass `idx`'s texture, tinted by the layer opacity (a = opacity).
    ///
    /// A composite is a per-frame derived draw, not a retained store slot, so it
    /// carries the offscreen pass index; [`lower_from_scene`] resolves its
    /// sampling bind group from `self.offscreen_passes[idx]` and emits the image
    /// instance + segment. Its emit context is main-pass / no-clip / zero-origin:
    /// it draws into the parent target unclipped, since the offscreen texture
    /// already holds only the clipped subtree.
    ///
    /// [`lower_from_scene`]: Self::lower_from_scene
    fn close_offscreen(&mut self, idx: usize) {
        let pass = &self.offscreen_passes[idx];
        let composite = ImageInstance {
            rect_pos: [pass.rect.x, pass.rect.y],
            rect_size: [pass.rect.w, pass.rect.h],
            uv_pos: [0.0, 0.0],
            uv_size: [1.0, 1.0],
            color: [1.0, 1.0, 1.0, pass.opacity],
        };
        let bounds = crate::scene::bounds::Bounds::from_world(pass.rect, None, 0.0, 0.0);
        self.scene.ingest_composite(
            composite,
            idx,
            EmitContext {
                clip: None,
                offscreen: None,
                origin: [0.0, 0.0],
            },
            bounds,
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
        // The drawable can be momentarily unavailable (surface out of date after a
        // resize/DPI change, or the drawable pool is exhausted). That is transient:
        // skip this frame and let the next tick redraw the unchanged scene.
        let Some(frame) = backend.begin_frame(surface) else {
            return;
        };
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
        &mut self,
        backend: &mut B,
        frame: Frame,
        clear: [f32; 4],
        viewport: [f32; 2],
    ) {
        // All three scratch buffers are `Renderer`-owned and borrow-free
        // (`DrawCommand`/`RenderPass` carry no lifetime — uniforms are stored by
        // value). We take them out with `mem::take`, clear them (retaining their
        // backing allocations), refill, and put them back: 0 heap allocations on
        // a steady frame. Taking them out lets the fill loops borrow `&self`
        // (segments, offscreen passes) without aliasing the buffers being filled.
        let mut viewports = std::mem::take(&mut self.viewports);
        let mut commands = std::mem::take(&mut self.commands);
        let mut passes = std::mem::take(&mut self.passes);
        viewports.clear();
        commands.clear();
        passes.clear();

        // Per-pass viewports. Index 0 is the surface; the rest map 1:1 to
        // `offscreen_passes`. Their bytes are copied by value into each command.
        viewports.push(viewport);
        for pass in &self.offscreen_passes {
            viewports.push(pass.viewport);
        }

        // Offscreen passes first (textures cleared transparent), then the surface
        // pass (cleared to the background). Each pass appends its commands to the
        // flat `commands` buffer and records its range in a `RenderPass`.
        for (i, pass) in self.offscreen_passes.iter().enumerate() {
            let vp = viewports[i + 1];
            let uniforms = InlineUniforms::new(bytemuck_viewport(&vp));
            let first_command = commands.len() as u32;
            for seg in self
                .segments
                .iter()
                .filter(|seg| seg.target == PassTarget::Offscreen(i))
            {
                commands.push(self.command_for(seg, uniforms, vp));
            }
            passes.push(RenderPass {
                target: RenderTarget::Texture(pass.texture),
                load: LoadOp::Clear([0.0, 0.0, 0.0, 0.0]),
                first_command,
                command_count: commands.len() as u32 - first_command,
            });
        }

        let main_uniforms = InlineUniforms::new(bytemuck_viewport(&viewports[0]));
        let first_command = commands.len() as u32;
        for seg in self
            .segments
            .iter()
            .filter(|seg| seg.target == PassTarget::Main)
        {
            commands.push(self.command_for(seg, main_uniforms, viewport));
        }
        passes.push(RenderPass {
            target: RenderTarget::Surface(frame),
            load: LoadOp::Clear(clear),
            first_command,
            command_count: commands.len() as u32 - first_command,
        });

        backend.encode(&DrawList {
            commands: &commands,
            passes: &passes,
        });

        // Return the buffers so their capacity is reused next frame.
        self.viewports = viewports;
        self.commands = commands;
        self.passes = passes;
    }

    /// Map one [`Segment`] to its [`DrawCommand`], given the pass's inline
    /// viewport uniform bytes and the viewport its scissor is clamped to.
    fn command_for(
        &self,
        seg: &Segment,
        uniforms: InlineUniforms,
        viewport: [f32; 2],
    ) -> DrawCommand {
        let scissor = seg.clip.map(|c| clip_to_scissor(c, viewport));
        match seg.kind {
            SegmentKind::Quad => DrawCommand {
                pipeline: self.quad_pipeline,
                bind_group: None,
                geometry: Geometry::Generated { count: seg.count },
                instance_buffer: self
                    .quad_pool
                    .buffer()
                    .expect("quad pool buffer exists when a quad segment references it"),
                instance_offset: seg.start as usize * QUAD_STRIDE,
                uniforms,
                scissor,
            },
            SegmentKind::AnalyticRRect => DrawCommand {
                pipeline: self.analytic_rrect_pipeline,
                bind_group: None,
                geometry: Geometry::Generated { count: seg.count },
                instance_buffer: self.analytic_rrect_pool.buffer().expect(
                    "analytic-rrect pool buffer exists when an analytic-rrect segment references it",
                ),
                instance_offset: seg.start as usize * ANALYTIC_RRECT_STRIDE,
                uniforms,
                scissor,
            },
            SegmentKind::AnalyticEllipse => DrawCommand {
                pipeline: self.analytic_ellipse_pipeline,
                bind_group: None,
                geometry: Geometry::Generated { count: seg.count },
                instance_buffer: self.analytic_ellipse_pool.buffer().expect(
                    "analytic-ellipse pool buffer exists when an analytic-ellipse segment references it",
                ),
                instance_offset: seg.start as usize * ANALYTIC_ELLIPSE_STRIDE,
                uniforms,
                scissor,
            },
            SegmentKind::AnalyticCapsule => DrawCommand {
                pipeline: self.analytic_capsule_pipeline,
                bind_group: None,
                geometry: Geometry::Generated { count: seg.count },
                instance_buffer: self.analytic_capsule_pool.buffer().expect(
                    "analytic-capsule pool buffer exists when an analytic-capsule segment references it",
                ),
                instance_offset: seg.start as usize * ANALYTIC_CAPSULE_STRIDE,
                uniforms,
                scissor,
            },
            SegmentKind::AnalyticLine => DrawCommand {
                pipeline: self.analytic_line_pipeline,
                bind_group: None,
                geometry: Geometry::Generated { count: seg.count },
                instance_buffer: self.analytic_line_pool.buffer().expect(
                    "analytic-line pool buffer exists when an analytic-line segment references it",
                ),
                instance_offset: seg.start as usize * ANALYTIC_LINE_STRIDE,
                uniforms,
                scissor,
            },
            SegmentKind::Image { bind_group } => DrawCommand {
                pipeline: self.image_pipeline,
                bind_group: Some(bind_group),
                geometry: Geometry::Generated { count: seg.count },
                instance_buffer: self
                    .image_pool
                    .buffer()
                    .expect("image pool buffer exists when an image segment references it"),
                instance_offset: seg.start as usize * IMAGE_STRIDE,
                uniforms,
                scissor,
            },
            SegmentKind::GlyphRun { bind_group } => DrawCommand {
                pipeline: self.glyph_pipeline,
                bind_group: Some(bind_group),
                geometry: Geometry::Generated { count: seg.count },
                instance_buffer: self
                    .glyph_pool
                    .buffer()
                    .expect("glyph pool buffer exists when a glyph segment references it"),
                instance_offset: seg.start as usize * GLYPH_STRIDE,
                uniforms,
                scissor,
            },
            SegmentKind::Gradient { bind_group } => DrawCommand {
                pipeline: self.gradient_pipeline,
                bind_group: Some(bind_group),
                geometry: Geometry::Generated { count: seg.count },
                instance_buffer: self
                    .gradient_pool
                    .buffer()
                    .expect("gradient pool buffer exists when a gradient segment references it"),
                instance_offset: seg.start as usize * GRADIENT_STRIDE,
                uniforms,
                scissor,
            },
            SegmentKind::Mesh => {
                let vertex_buffer = self
                    .mesh_vertex_pool
                    .buffer()
                    .expect("mesh vertex pool buffer exists when a mesh segment references it");
                DrawCommand {
                    pipeline: self.mesh_pipeline,
                    bind_group: None,
                    geometry: Geometry::IndexedMesh {
                        vertex_buffer,
                        index_buffer: self.mesh_index_pool.buffer().expect(
                            "mesh index pool buffer exists when a mesh segment references it",
                        ),
                        // Path/Mesh indices are rebased into the shared 32-bit
                        // index ring, so the frame draw is always U32. Native
                        // 16-bit index buffers live on the static geometry store
                        // (§13.4 F4 boundary), which selects its own width.
                        index_format: IndexFormat::U32,
                        index_offset: seg.start,
                        index_count: seg.count,
                    },
                    // The mesh draw reads its per-vertex buffer at index 0 and has
                    // no per-instance data; the instance buffer is unused (the
                    // vertex buffer is passed only to fill the field).
                    instance_buffer: vertex_buffer,
                    instance_offset: 0,
                    uniforms,
                    scissor,
                }
            }
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
        Primitive::Image(ImageDraw::new(
            Rect {
                x: 0.0,
                y: 0.0,
                w: 8.0,
                h: 8.0,
            },
            texture,
        ))
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

    /// A solid-filled *concave* path (a chevron/arrow with one reflex vertex,
    /// fill only, no stroke): the self-masked solid-fill case (§14.4). A convex
    /// outline keeps the tessellated lane, so the mask-lane fixtures must be
    /// non-convex to divert.
    fn solid_path(ox: f32, oy: f32) -> Primitive {
        use crate::primitive::Point as P;
        Primitive::Path(crate::primitive::Path {
            cmds: vec![
                PathCmd::MoveTo(P::new(ox, oy)),
                PathCmd::LineTo(P::new(ox + 16.0, oy + 8.0)),
                PathCmd::LineTo(P::new(ox, oy + 16.0)),
                PathCmd::LineTo(P::new(ox + 6.0, oy + 8.0)), // reflex vertex
                PathCmd::Close,
            ],
            fill: Some(Rgba {
                r: 0.2,
                g: 0.4,
                b: 0.6,
                a: 1.0,
            }),
            stroke: None,
        })
    }

    /// A cold masked solid fill rasterizes its coverage once: exactly one mask
    /// build, and it lowers through the reused glyph-coverage pipeline.
    #[test]
    fn cold_masked_solid_fill_builds_one_mask() {
        let mut gpu = HeadlessRaster::new();
        let surface = gpu.create_surface(RawWindowHandle::Headless, 64, 64);
        let format = gpu.surface_format(surface);
        let mut r = Renderer::new(&mut gpu, format);
        r.upload(&mut gpu, &[solid_path(4.0, 4.0)]);
        assert_eq!(r.frame_stats().clip_mask_builds, 1);
        assert!(
            r.segments
                .iter()
                .any(|s| matches!(s.kind, SegmentKind::GlyphRun { .. })),
            "masked solid fill lowers as a glyph-coverage draw"
        );
    }

    /// A stable path re-resolves to its retained slot with no re-raster: the
    /// second identical frame builds zero masks (the §14.4 stable fast path).
    #[test]
    fn stable_masked_solid_fill_rebuilds_nothing() {
        let mut gpu = HeadlessRaster::new();
        let surface = gpu.create_surface(RawWindowHandle::Headless, 64, 64);
        let format = gpu.surface_format(surface);
        let mut r = Renderer::new(&mut gpu, format);
        r.upload(&mut gpu, &[solid_path(4.0, 4.0)]);
        assert_eq!(r.frame_stats().clip_mask_builds, 1);
        r.upload(&mut gpu, &[solid_path(4.0, 4.0)]);
        assert_eq!(r.frame_stats().clip_mask_builds, 0);
    }

    /// Dropping a mask evicts it; re-introducing it repacks the page, which forces
    /// a full re-blit and rebuilds the survivor.
    #[test]
    fn eviction_forces_reblit_next_frame() {
        let mut gpu = HeadlessRaster::new();
        let surface = gpu.create_surface(RawWindowHandle::Headless, 64, 64);
        let format = gpu.surface_format(surface);
        let mut r = Renderer::new(&mut gpu, format);
        // Two distinct masks in frame 1.
        r.upload(&mut gpu, &[solid_path(4.0, 4.0), solid_path(30.0, 30.0)]);
        assert_eq!(r.frame_stats().clip_mask_builds, 2);
        // Frame 2 references only the first: the second is evicted, repacking the
        // page. Re-resolve of the first is a cache hit, but the repack sets
        // needs_full_reblit for the following frame.
        r.upload(&mut gpu, &[solid_path(4.0, 4.0)]);
        // Frame 3: needs_full_reblit is set, so the surviving mask re-blits even
        // though its key is unchanged.
        r.upload(&mut gpu, &[solid_path(4.0, 4.0)]);
        assert_eq!(r.frame_stats().clip_mask_builds, 1);
    }

    /// A stroked path is not a self-masked solid fill: it stays on the tessellated
    /// path lane and builds no mask.
    #[test]
    fn stroked_path_skips_mask_lane() {
        use crate::primitive::{Point as P, Stroke};
        let prim = Primitive::Path(crate::primitive::Path {
            cmds: vec![
                PathCmd::MoveTo(P::new(4.0, 4.0)),
                PathCmd::LineTo(P::new(20.0, 4.0)),
                PathCmd::LineTo(P::new(12.0, 20.0)),
                PathCmd::Close,
            ],
            fill: Some(Rgba {
                r: 1.0,
                g: 0.0,
                b: 0.0,
                a: 1.0,
            }),
            stroke: Some(Stroke::new(
                2.0,
                Rgba {
                    r: 0.0,
                    g: 0.0,
                    b: 0.0,
                    a: 1.0,
                },
            )),
        });
        let mut gpu = HeadlessRaster::new();
        let surface = gpu.create_surface(RawWindowHandle::Headless, 64, 64);
        let format = gpu.surface_format(surface);
        let mut r = Renderer::new(&mut gpu, format);
        r.upload(&mut gpu, &[prim]);
        assert_eq!(r.frame_stats().clip_mask_builds, 0);
    }

    /// The convexity discriminator: convex polygons and degenerate stubs are
    /// convex; a reflex vertex or any curve command is not.
    #[test]
    fn convexity_discriminator() {
        use crate::primitive::Point as P;
        let square = [
            PathCmd::MoveTo(P::new(0.0, 0.0)),
            PathCmd::LineTo(P::new(4.0, 0.0)),
            PathCmd::LineTo(P::new(4.0, 4.0)),
            PathCmd::LineTo(P::new(0.0, 4.0)),
            PathCmd::Close,
        ];
        assert!(path_is_convex(&square));
        let chevron = [
            PathCmd::MoveTo(P::new(0.0, 0.0)),
            PathCmd::LineTo(P::new(4.0, 2.0)),
            PathCmd::LineTo(P::new(0.0, 4.0)),
            PathCmd::LineTo(P::new(1.5, 2.0)),
            PathCmd::Close,
        ];
        assert!(!path_is_convex(&chevron));
        let curved = [
            PathCmd::MoveTo(P::new(0.0, 0.0)),
            PathCmd::QuadTo(P::new(2.0, 2.0), P::new(4.0, 0.0)),
            PathCmd::Close,
        ];
        assert!(!path_is_convex(&curved));
        // Fewer than three vertices is degenerate, treated as convex.
        assert!(path_is_convex(&[
            PathCmd::MoveTo(P::new(0.0, 0.0)),
            PathCmd::LineTo(P::new(1.0, 0.0)),
        ]));
    }

    /// A convex straight-edge solid fill (a triangle) is not diverted: it stays on
    /// the tessellated path lane, which fans it trivially, and builds no mask.
    #[test]
    fn convex_solid_fill_stays_tessellated() {
        use crate::primitive::Point as P;
        let prim = Primitive::Path(crate::primitive::Path {
            cmds: vec![
                PathCmd::MoveTo(P::new(4.0, 4.0)),
                PathCmd::LineTo(P::new(20.0, 4.0)),
                PathCmd::LineTo(P::new(12.0, 20.0)),
                PathCmd::Close,
            ],
            fill: Some(Rgba {
                r: 0.2,
                g: 0.4,
                b: 0.6,
                a: 1.0,
            }),
            stroke: None,
        });
        let mut gpu = HeadlessRaster::new();
        let surface = gpu.create_surface(RawWindowHandle::Headless, 64, 64);
        let format = gpu.surface_format(surface);
        let mut r = Renderer::new(&mut gpu, format);
        r.upload(&mut gpu, &[prim]);
        assert_eq!(r.frame_stats().clip_mask_builds, 0);
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
    /// `SegmentKind::GlyphRun` path — A8 coverage sample → premultiplied blend —
    /// that the golden also covers, but with an explicit
    /// assertion on ink-vs-background so a regression names itself.
    #[test]
    fn glyph_run_paints_ink_over_background() {
        const W: u32 = 96;
        const H: u32 = 48;
        let mut gpu = HeadlessRaster::new();
        let surface = gpu.create_surface(RawWindowHandle::Headless, W, H);
        let format = gpu.surface_format(surface);
        let mut r = Renderer::new(&mut gpu, format);

        // Prepare a small run and upload its A8 coverage pool.
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
