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
    BindGroupDesc, Binding, BufferUsage, ColorDomain, ColorSpace, Frame, GpuBackend, LoadOp,
    PipelineDesc, PipelineId, SamplerDesc, SurfaceId, TextureDesc, TextureFormat, TextureId,
};
use viso_gpu::{BindGroupId, SamplerId};

use viso_shader::{PipelineEntry, PipelineFamily, standard_manifest};

use crate::batch::{BatchFamily, BatchItem, BatchKey, BatchTarget, joins};
use crate::blend::Blend;
use crate::clip::{ClipShape, plan_clip};
use crate::color_effect::{ColorOp, fuse};
use crate::effect_plan::{LayerPlan, LayerRequest, plan_layer};
use crate::gradient_lut::{GradientLutAtlas, LUT_WIDTH, LutAlloc, LutKey};
use crate::graph::{PassLoad, PassWork, RenderGraph};
use crate::mask::{MaskCache, MaskKey, MaskKind, MaskRequest};
use crate::mask_page::MaskPage;
use crate::opacity::ChildOverlap;
use crate::pool::InstancePool;
use crate::primitive::{
    AdvancedBlendInstance, AnalyticCapsuleInstance, AnalyticEllipseInstance, AnalyticLineInstance,
    AnalyticRRectInstance, BlurInstance, ColorTransformInstance, GlyphInstance, GradientInstance,
    ImageInstance, MaterialInstance, MaterialLane, MeshVertex, NativeMaterialRegion, PathCmd,
    Primitive, QuadInstance, Rect, ShadowInstance, rgba_array,
};
use crate::raster_mask::{path_bounds, rasterize_path_coverage};
use crate::scene::store::{ClipFillRule, StoreRef, glyph_instances};
use crate::scene::{EmitContext, PaintEntry, Scene};
use crate::transient::{TargetDesc, TargetId, TargetUsage, TransientTargets};
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
/// Bytes of one analytic soft-shadow instance.
const SHADOW_STRIDE: usize = core::mem::size_of::<ShadowInstance>();
/// Bytes of one blur instance.
const BLUR_STRIDE: usize = core::mem::size_of::<BlurInstance>();
/// Bytes of one fused color-transform instance.
const COLOR_TRANSFORM_STRIDE: usize = core::mem::size_of::<ColorTransformInstance>();
/// Bytes of one isolated advanced-blend composite instance.
const ADVANCED_BLEND_STRIDE: usize = core::mem::size_of::<AdvancedBlendInstance>();
/// Bytes of one frosted material composite instance.
const MATERIAL_STRIDE: usize = core::mem::size_of::<MaterialInstance>();
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
    /// A run of adjacent analytic soft shadows sharing this segment's clip, in
    /// the analytic-shadow buffer. `start`/`count` count instances; binds no
    /// texture.
    AnalyticShadow,
    /// A run of adjacent triangle meshes (Path/Mesh) sharing this segment's
    /// clip, in the shared mesh vertex/index buffers. `start`/`count` count
    /// **indices** in the mesh index buffer (vertices are addressed by the
    /// absolute indices baked into the index data).
    Mesh,
    /// One fused color op applied to `bind_group`'s source texture, in the
    /// color-transform buffer: an affine color matrix plus an optional gamma.
    /// `start`/`count` count instances in that buffer.
    ColorTransform { bind_group: BindGroupId },
    /// One isolated advanced-blend composite, in the advanced-blend buffer.
    /// `bind_group` binds **two** textures — the isolated layer at slot 0 and the
    /// bounded destination snapshot at slot 1 — and the fragment returns the
    /// finished composite, so this is the one kind whose pipeline writes with
    /// [`BlendMode::Replace`](viso_gpu::BlendMode::Replace).
    AdvancedBlend { bind_group: BindGroupId },
    /// One frosted material surface, in the material buffer, sampling
    /// `bind_group`'s blurred backdrop. `start`/`count` count instances in that
    /// buffer.
    Material { bind_group: BindGroupId },
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
            SegmentKind::AnalyticShadow => BatchFamily::AnalyticShadow,
            SegmentKind::Mesh => BatchFamily::Mesh,
            SegmentKind::ColorTransform { .. } => BatchFamily::ColorTransform,
            SegmentKind::AdvancedBlend { .. } => BatchFamily::AdvancedBlend,
            SegmentKind::Material { .. } => BatchFamily::Material,
        }
    }

    /// The resource bind group folded into this kind's [`BatchKey`], if any:
    /// the sampled texture/atlas for image and glyph draws, `None` for quad and
    /// mesh (which bind no per-draw resource).
    pub(crate) fn resource(self) -> Option<BindGroupId> {
        match self {
            SegmentKind::Image { bind_group }
            | SegmentKind::GlyphRun { bind_group }
            | SegmentKind::Gradient { bind_group }
            | SegmentKind::ColorTransform { bind_group }
            | SegmentKind::AdvancedBlend { bind_group }
            | SegmentKind::Material { bind_group } => Some(bind_group),
            SegmentKind::Quad
            | SegmentKind::AnalyticRRect
            | SegmentKind::AnalyticEllipse
            | SegmentKind::AnalyticCapsule
            | SegmentKind::AnalyticLine
            | SegmentKind::AnalyticShadow
            | SegmentKind::Mesh => None,
        }
    }
}

impl PassTarget {
    /// The batch-planner target this pass maps to (surface vs. offscreen `i` vs.
    /// backdrop capture `i`).
    pub(crate) fn batch_target(self) -> BatchTarget {
        match self {
            PassTarget::Main => BatchTarget::Main,
            PassTarget::Offscreen(i) => BatchTarget::Offscreen(i),
            PassTarget::Capture(i) => BatchTarget::Backdrop(i),
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
    /// A backdrop capture pass, indexed into [`Renderer::backdrop_captures`]:
    /// the content behind one backdrop group, re-rendered into a tight ROI
    /// target so the blurred result can be composited under the group's layers.
    Capture(usize),
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
    /// The transient target this pass draws into. `None` until the pass is
    /// finalized at `LayerEnd`, when the content ROI is known and a target of
    /// that ROI's size class is declared against the frame-local pool (§16.2,
    /// §16.4). The concrete texture is resolved later, in
    /// [`TransientTargets::assign`].
    base: Option<TargetId>,
    /// The transient target the composite samples: `base` for an unblurred layer,
    /// the last rung of the blur ladder for a blurred one. `None` until finalize.
    sample: Option<TargetId>,
    /// The pass viewport `[width, height]` in physical pixels — the *size class*
    /// of the ROI, which is the extent of the pooled texture this pass writes and
    /// therefore the extent the shaders map pixels to NDC against. `[0, 0]` until
    /// finalize. Always `>= used`.
    viewport: [f32; 2],
    /// The extent this pass actually draws into, anchored at the target's
    /// top-left: the ceil of the tight content ROI, before size-class rounding.
    /// The composite samples exactly this sub-rect. `[0, 0]` until finalize.
    used: [u32; 2],
    /// The tight content ROI's world-space rect: the composite destination, and
    /// the origin subtracted from this pass's geometry. Equals the layer clip
    /// until finalize narrows it to `content ∩ clip ∩ surface`.
    rect: Rect,
    /// The layer opacity in `[0, 1)`, applied as the composite tint alpha.
    opacity: f32,
    /// The fused color op the composite itself applies, if this layer carried a
    /// color-effect chain: the *last* op of the chain, with the layer opacity
    /// folded into its alpha row. `None` for a plain layer, whose composite is an
    /// ordinary tinted image draw. Ops before the last one become extra
    /// [`PassWork::ColorTransform`] rungs, exactly as the blur ladder does.
    color: Option<ColorOp>,
    /// The blend this layer composites with, and the backdrop capture holding the
    /// bounded destination snapshot it reads — `None` for the overwhelmingly
    /// common `SrcOver` layer, whose composite is a plain image draw on the
    /// fixed-function blend state (§14.6).
    ///
    /// When set, the composite is lowered as one `AdvancedBlend` draw instead:
    /// the fragment samples both this pass's `sample` target and the capture,
    /// evaluates the blend, and writes with [`BlendMode::Replace`]. Because that
    /// draw has no color-matrix slot, a blended layer's whole color chain is
    /// realized as [`PassWork::ColorTransform`] rungs and `color` stays `None`.
    blend: Option<(Blend, usize)>,
}

/// How much empty area a backdrop group may absorb before a joining layer opens
/// its own capture instead: the union is accepted while its area stays within
/// this factor of the summed area of its members' own ROIs.
///
/// `1.0` would only ever accept a union that wastes nothing (so two panels with
/// a gap between them never share); an unbounded factor would let two panels in
/// opposite screen corners share one near-full-screen capture — the §17.2
/// forbidden default in reverse. This is the internal knob between those, tuned
/// against the §31 bench, not a public parameter.
const BACKDROP_UNION_SLACK: f32 = 2.0;

/// How many children a layer may hold before the planner stops trying to prove
/// them disjoint and reports [`ChildOverlap::Unknown`] instead (§14.5).
///
/// The proof is pairwise, so it is quadratic in the child count; the cap bounds
/// the cold-path work at `32 * 31 / 2` rect intersections per translucent layer
/// and no more. Above it the answer is "unknown", which costs one offscreen pass
/// — the same pass the scene would have paid before the planner existed, so the
/// cap can only ever lose an optimization, never correctness. Groups this wide are
/// also the ones least likely to be disjoint.
const MAX_FOLD_CHILDREN: usize = 32;

/// What the Effect Planner needs to know about a layer's own subtree, gathered by
/// one look-ahead over the primitive stream at the layer's open (§3145).
///
/// Both facts are cheap and neither can be recovered later: by the time the
/// subtree has been walked, its children have already been ingested with the
/// alpha they were going to keep.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SubtreeFacts {
    /// Whether the direct children provably do not overlap — the fact that
    /// decides whether a group opacity can be pushed into them.
    overlap: ChildOverlap,
    /// Whether the subtree contains a shadow shaded in closed form, which is why
    /// it never asked for a blurred copy of itself (E0's analytic shadow).
    analytic_shadow: bool,
}

/// Look ahead over the layer opened at `layer_index` and gather the facts the
/// Effect Planner needs about its content (§3145).
///
/// A group opacity folds into its children only when they provably do not overlap
/// *and* every one of them can actually carry the factor — a straight-alpha
/// instance whose alpha the renderer may scale. Anything else reports
/// [`ChildOverlap::Unknown`], which the planner treats as overlap and pays a pass
/// for (§14.5's "when unsure, keep correctness"):
///
/// - a **nested layer**, whose own subtree would have to be folded through too;
/// - a **gradient**, whose inline stops are stored premultiplied and whose LUT
///   form has no per-instance alpha at all;
/// - a **path** or **mesh**, whose payloads are owned vectors — rewriting their
///   colors would allocate on the walk (§28);
/// - a **glyph run**, whose glyphs are separate instances that may overlap each
///   other inside the run (italic and script faces routinely do), so per-instance
///   folding could double-blend within one "child".
///
/// The scan stops at the layer's matching `LayerEnd`, so a sibling layer later in
/// the stream never contaminates this one's facts.
fn scan_layer_subtree(primitives: &[Primitive], layer_index: usize) -> SubtreeFacts {
    let mut rects: [Rect; MAX_FOLD_CHILDREN] = [Rect::ZERO; MAX_FOLD_CHILDREN];
    let mut count = 0usize;
    let mut foldable = true;
    let mut analytic_shadow = false;
    let mut depth = 0u32;
    for prim in &primitives[layer_index + 1..] {
        // One rect per drawable, in the same world space its instance is lowered
        // to, or `None` for a kind that cannot carry a folded alpha.
        let rect = match prim {
            Primitive::LayerEnd => {
                if depth == 0 {
                    break;
                }
                depth -= 1;
                continue;
            }
            Primitive::Layer(_) => {
                depth += 1;
                foldable = false;
                continue;
            }
            // Pure annotations on whichever layer owns them.
            Primitive::ColorEffect(_) | Primitive::Blend(_) => continue,
            _ if depth > 0 => continue,
            Primitive::Quad(quad) => Some(quad.rect),
            Primitive::AnalyticRRect(rrect) => Some(rrect.rect),
            Primitive::AnalyticEllipse(ellipse) => Some(ellipse.rect),
            Primitive::AnalyticCapsule(capsule) => Some(capsule.rect),
            Primitive::AnalyticLine(line) => {
                let inst = line.to_instance();
                let reach = 0.5 * inst.width + inst.border_width;
                Some(Rect {
                    x: inst.p0[0].min(inst.p1[0]) - reach,
                    y: inst.p0[1].min(inst.p1[1]) - reach,
                    w: (inst.p1[0] - inst.p0[0]).abs() + 2.0 * reach,
                    h: (inst.p1[1] - inst.p0[1]).abs() + 2.0 * reach,
                })
            }
            Primitive::Image(image) => Some(image.rect),
            Primitive::AnalyticShadow(shadow) => {
                analytic_shadow = true;
                let inst = shadow.to_instance();
                let reach = BLUR_RADIUS_SIGMAS * inst.sigma
                    + inst.spread.max(0.0)
                    + inst.offset[0].abs().max(inst.offset[1].abs());
                Some(Rect {
                    x: inst.rect_pos[0] - reach,
                    y: inst.rect_pos[1] - reach,
                    w: inst.rect_size[0] + 2.0 * reach,
                    h: inst.rect_size[1] + 2.0 * reach,
                })
            }
            _ => None,
        };
        let Some(rect) = rect else {
            foldable = false;
            continue;
        };
        if !foldable {
            continue;
        }
        if count == MAX_FOLD_CHILDREN {
            foldable = false;
            continue;
        }
        rects[count] = rect;
        count += 1;
    }
    let overlap = if !foldable {
        ChildOverlap::Unknown
    } else if pairwise_disjoint(&rects[..count]) {
        ChildOverlap::Disjoint
    } else {
        ChildOverlap::Overlapping
    };
    SubtreeFacts {
        overlap,
        analytic_shadow,
    }
}

/// Whether no two of these rects share area. `O(n^2)` over at most
/// [`MAX_FOLD_CHILDREN`] rects, with no allocation.
fn pairwise_disjoint(rects: &[Rect]) -> bool {
    for (i, a) in rects.iter().enumerate() {
        for b in &rects[i + 1..] {
            let hit = a.intersect(*b);
            if hit.w > 0.0 && hit.h > 0.0 {
                return false;
            }
        }
    }
    true
}

/// One backdrop capture pass: the content already submitted *behind* one (or one
/// shared group of) backdrop layers, re-rendered into a tight ROI target, blurred
/// by the standard ladder, and composited under each member layer's own content
/// (§17.1, §17.2).
///
/// The capture is a *re-render*, not a read of the attachment the member layers
/// draw into: it depends on the producers of the content behind them, which the
/// render graph sees as ordinary read edges on those producers' targets. No
/// widget ever samples undefined framebuffer state.
///
/// Sharing is the default and the split is the exception: a layer joins the open
/// group when it asks for the same sigma, nothing has been drawn between them
/// that the group's ROI covers, and the union stays within
/// [`BACKDROP_UNION_SLACK`] of the members' own area. `N` frosted panels over one
/// background therefore cost one capture and one blur ladder, not `N` of each.
struct BackdropCapture {
    /// `paint_order.len()` at the moment the group opened: exactly the entries
    /// before this index are "behind" the group and get re-rendered into it.
    under: usize,
    /// The world-space ROI this capture renders: the union of its members' clips,
    /// each inflated by the blur reach, intersected with the surface.
    roi: Rect,
    /// Summed area of the members' own inflated ROIs, the denominator of the
    /// union-slack test (it double-counts overlap, which is what makes two
    /// overlapping panels look "compatible" by area — they are split earlier, by
    /// the blocker test).
    covered: f32,
    /// The blur sigma every member of this group asked for. A different sigma
    /// needs a different ladder, so it opens its own capture.
    sigma: f32,
    /// The transient target this pass draws into; `None` until the post-walk
    /// realizes the capture.
    base: Option<TargetId>,
    /// The transient target the member composites sample: the last rung of this
    /// group's shared blur ladder. `None` until realized.
    sample: Option<TargetId>,
    /// The pass viewport `[width, height]`: the pooled (size-class) extent of
    /// `base`, which the shaders map pixels to NDC against.
    viewport: [f32; 2],
    /// The extent this pass actually draws, at the target's top-left: the ceil of
    /// `roi`, before size-class rounding.
    used: [u32; 2],
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
    /// Zero for the main pass. For an offscreen layer this is a provisional
    /// origin (the clip top-left); finalize repatches it to the ROI top-left.
    origin: [f32; 2],
    /// Running union (world space) of the paint bounds of every primitive drawn
    /// directly under this layer, used only for an offscreen layer to size its
    /// tight ROI at `LayerEnd`. [`Rect::ZERO`] is the empty seed; unused for a
    /// main-target (opaque) layer.
    content_union: Rect,
    /// `scene.paint_order.len()` captured at Layer-open, so finalize can repatch
    /// exactly this layer's recorded children (`paint_order[start..]`). Unused
    /// for a main-target layer.
    paint_order_start: usize,
    /// The layer's requested content-blur sigma in physical pixels (§16.2, E1.2).
    /// `0.0` means no blur; `> 0.0` inserts a separable Gaussian ladder between
    /// this layer's offscreen render and its composite. Only meaningful for an
    /// offscreen (`Offscreen`) target.
    blur_sigma: f32,
    /// Where this layer's fused color ops live in [`Renderer::color_ops`]:
    /// `color_ops[color_start..color_start + color_len]`, the output of
    /// [`fuse`](crate::color_effect::fuse) over the layer's effect chain. `0` ops
    /// means no color work; `n` ops cost `n - 1` extra render-target passes,
    /// because the last op rides the layer's composite draw.
    color_start: u32,
    /// Number of fused color ops this layer owns (see `color_start`).
    color_len: u32,
    /// The group opacity the Effect Planner pushed into this layer's children
    /// instead of buying an isolation pass for it (§14.5, §3161 rung 1), already
    /// multiplied with whatever the ancestors folded. `1.0` when nothing is folded
    /// — the case for every offscreen layer, and for every opaque layer.
    ///
    /// A folded factor is only ever established for a layer whose direct children
    /// are provably disjoint drawables, and a nested layer makes the enclosing
    /// scan report [`ChildOverlap::Unknown`], so a folded layer never contains
    /// another one. The product here therefore has at most one non-unit term; it
    /// is written as a product anyway so the invariant is not load-bearing.
    fold_opacity: f32,
}

/// What one backdrop capture depends on, and whether that dependency moved since
/// the previous frame (§3202).
///
/// A backdrop filter is the one effect whose input is *not* its own properties: it
/// samples whatever happens to be painted behind it. So its invalidation cannot be
/// driven by a property dirty flag — a frosted panel over a scrolling list must
/// re-capture even though nothing about the panel changed, and a frosted panel over
/// a static header must not re-capture just because a list elsewhere scrolled.
///
/// The dependency is therefore scoped to the region of interest: the newest content
/// stamp ([`Scene::content_stamp`](crate::scene::Scene::content_stamp)) among the
/// entries the capture actually samples, mixed with how many there are. Two panels
/// over different content get different revisions and go dirty independently.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BackdropDependency {
    /// The capture's region of interest in world space — the rect the revision is
    /// scoped to.
    pub roi: Rect,
    /// The revision of the content under `roi`. Compared, never interpreted: the
    /// only meaningful operation is equality against the previous frame's value.
    pub revision: u64,
    /// Whether `revision` differs from the previous frame's for this capture slot.
    /// A slot that is new this frame is dirty.
    pub dirty: bool,
}

/// A (texture, sampler) pair's bind group, cached so repeated draws sharing both
/// reuse one bind group rather than allocating per frame. Sampler is part of the
/// key: the same texture drawn with Nearest and with Linear needs two bind groups.
struct TextureBinding {
    texture: TextureId,
    sampler: SamplerId,
    bind_group: BindGroupId,
}

/// A (source, destination) texture pair's bind group, cached the same way and for
/// the same reason as [`TextureBinding`] — an isolated advanced blend is the one
/// draw that binds two textures at once (§14.6). The shared linear-clamp sampler
/// is implied rather than keyed: a blend always samples both its layer and its
/// destination snapshot with it, since both are pooled transient targets.
struct BlendBinding {
    source: TextureId,
    destination: TextureId,
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
/// (§7.1): SolidRect (quad), AnalyticRRect, AnalyticEllipse, AnalyticCapsule,
/// AnalyticLine, Image, MaskComposite (glyph), PathFill (mesh), Gradient,
/// AnalyticShadow, ContentBlur (blur), ColorTransform (fused color effects),
/// AdvancedBlend (isolated destination-read blends), and MaterialComposite
/// (frosted material surfaces).
/// Reported as `FrameStats::shader_pipeline_creations` — a construction-time
/// constant, since no draw ever triggers a runtime shader compile.
const SHADER_PIPELINE_PREWARM_COUNT: u32 = 14;

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
    /// Bytes this frame's offscreen layer passes *address* (§30): summed
    /// `width * height * bytes_per_texel` over every offscreen pass's pooled
    /// target. Zero when there are no offscreen passes.
    ///
    /// This is a per-pass sum, not an occupancy figure: two passes whose
    /// lifetimes do not overlap can share one pooled texture, so the sum can
    /// exceed the memory actually held. Read `transient_peak_bytes` for the
    /// concurrently-live footprint and `transient_pool_bytes` for what the pool
    /// retains.
    pub transient_target_bytes: usize,
    /// Separable Gaussian blur passes this frame (§30): the number of single-axis
    /// blur passes chained ahead of a layer composite. A blurred layer that fits
    /// the small-radius tier contributes two (one horizontal, one vertical); zero
    /// when no layer blurs its content.
    pub blur_passes: u32,
    /// Bytes this frame's blur rungs *address* (§30): summed
    /// `width * height * bytes_per_texel` over every blur pass's pooled
    /// destination. Zero when there are no blur passes. A per-pass sum with the
    /// same aliasing caveat as `transient_target_bytes`.
    pub blur_target_bytes: usize,
    /// Fused color ops this frame (§17.3): the number of `ColorOp`s the layers'
    /// color-effect chains compiled down to, *not* the number of effects authored.
    /// A chain of nine matrix-expressible effects reports `1`, because they all
    /// multiply into one matrix; a stage the matrix cannot express splits the run
    /// and adds one.
    pub color_effect_ops: u32,
    /// Extra render-target passes the frame's color ops cost (§17.3): one per
    /// fused op beyond the first of each chain, since the last op of every chain
    /// rides that layer's composite draw. `0` for every chain that fused
    /// completely — the property "consecutive compatible effects do not each get a
    /// pass" is exactly `color_transform_passes == 0`.
    pub color_transform_passes: u32,
    /// Backdrop capture passes this frame (§17.1, §17.2): one per *group* of
    /// backdrop layers, not one per layer. `N` frosted panels sharing one
    /// background report `1`, and the blur ladder over that one capture is shared
    /// too (`blur_passes` does not scale with `N` either). Zero when no layer asks
    /// for a backdrop.
    pub backdrop_captures: u32,
    /// Pixels this frame's backdrop captures re-render (§17.1): summed
    /// `used_width * used_height` over every capture. This is the ROI-only cost
    /// the "capture only the required region" rule bounds — a small panel over a
    /// 4K window captures its own padded rect, never the framebuffer.
    pub backdrop_capture_pixels: usize,
    /// Layers this frame isolated because their blend mode is not `SrcOver`
    /// (§14.6): one per layer routed through the advanced-blend pipeline. Zero for
    /// every ordinary frame — this counter is precisely the "does the common
    /// `SrcOver` pipeline stay clean" gate, and each isolation costs one offscreen
    /// pass plus a share of one bounded destination snapshot (counted in
    /// `backdrop_captures`, which conflates both kinds of destination snapshot:
    /// the frosted-glass backdrop and the blend's bounded destination read).
    pub blend_isolations: u32,
    /// Frosted material surfaces composited this frame (§18): one per
    /// [`FrostedMaterial`] whose backdrop resolved to a capture. The fused chain
    /// `backdrop → blur → color transform → noise → rounded mask → opacity` is a
    /// *single* draw, so this counter never adds a pass or a target of its own —
    /// `N` panels over one background still report one `backdrop_captures` and the
    /// blur ladder of that one capture.
    ///
    /// [`FrostedMaterial`]: crate::FrostedMaterial
    pub material_composites: u32,
    /// Regions reserved this frame for a [`MaterialLane::Native`] surface (§19).
    ///
    /// These cost Viso nothing at all — no capture, no blur rung, no composite, no
    /// draw — because the platform's own material composites behind the Viso surface.
    /// A screen whose panels are all on the native lane therefore reports captures
    /// and blur passes of zero while still reporting its panels here.
    ///
    /// [`MaterialLane::Native`]: crate::MaterialLane::Native
    pub native_material_regions: u32,
    /// Potential layers the Effect Planner considered this frame (§3145): one per
    /// `Primitive::Layer` in the stream, whether or not it ended up costing a pass.
    /// The denominator for the two counters below.
    pub layers_planned: u32,
    /// Planned layers where at least one rung of the elimination ladder fired
    /// (§3161) — the layer either vanished entirely or got cheaper. Counted per
    /// layer, not per rung.
    pub layers_eliminated: u32,
    /// Planned layers whose group opacity was pushed into provably-disjoint
    /// children instead of buying an isolation pass (§14.5). Each one is an
    /// offscreen pass and its transient bytes that the frame did not pay for —
    /// the counter that makes "offscreen is an expensive mechanism, not a
    /// convenient default" measurable.
    pub opacity_folds: u32,
    /// Backdrop captures whose region-of-interest saw its content change since the
    /// previous frame (§3202). A frosted panel over static content reports 0 even
    /// while an unrelated panel elsewhere reports 1: the dependency is scoped to
    /// the ROI, not to the effect's own properties.
    pub backdrop_dirty_rois: u32,
    /// Peak concurrently-live transient render-target bytes this frame (§16.4,
    /// §31): the maximum, over every graph pass slot, of the pooled bytes whose
    /// `[first_write, last_read]` interval covers that slot. This is the real
    /// high-water footprint the frame's offscreen work demands — the figure a
    /// memory gate bounds, and always `<= transient_pool_bytes`.
    pub transient_peak_bytes: usize,
    /// Bytes the transient target pool currently retains (§16.4): summed over
    /// every physical texture it holds, including ones this frame left idle (they
    /// are kept for a bounded number of frames so a steady scene stops
    /// allocating). The resident cost of the pool, not of one frame.
    pub transient_pool_bytes: usize,
    /// Physical transient textures created this frame (§16.4). A steady scene
    /// whose layers keep their size classes reports 0 — every target is served
    /// from the pool, so no `create_texture` reaches the backend (exit criterion:
    /// never one texture per shadow / per clip / per material surface).
    pub transient_target_allocations: u32,
    /// Physical transient textures the pool holds after this frame (§16.4).
    /// Bounded by the peak concurrent demand of any recent frame, not by the
    /// number of effects drawn.
    pub transient_targets: usize,
    /// Render passes the graph compiled for this frame (§16.1, §30): the passes
    /// handed to the backend, and therefore the frame's render-target switch
    /// count. At least 1 (the surface) after any upload. Lower than
    /// `offscreen_passes + blur_passes + color_transform_passes + 1` exactly when
    /// the graph merged or culled something.
    pub render_passes: usize,
    /// Passes the graph folded into a preceding pass because both write the same
    /// attachment (§16.5): each merge is one render-target switch — and, on a tile
    /// GPU, one attachment store/load cycle — that the frame does not pay.
    pub render_pass_merges: u32,
    /// Passes the graph dropped because nothing samples what they write (§16.1).
    /// Zero for a scene whose every offscreen layer is composited, which is the
    /// normal case; nonzero means work was planned and then found dead.
    pub culled_render_passes: u32,
    /// `1` when this frame rebuilt the pass plan, `0` when it reused the cached
    /// one (§16.1). Topology alone drives a rebuild: a resize, a scroll, a
    /// recolor, or a blur sigma change that keeps the ladder's shape all reuse the
    /// plan, so a steady scene reports 0 from its second frame onward.
    pub render_graph_compiles: u32,
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
    /// The AnalyticShadow built-in pipeline (registered once).
    analytic_shadow_pipeline: PipelineId,
    /// The ContentBlur built-in pipeline (registered once): one separable
    /// Gaussian pass, chained horizontally then vertically to blur an offscreen
    /// layer's content before compositing it (§16.2, E1.2).
    blur_pipeline: PipelineId,
    /// The ColorTransform built-in pipeline (registered once): one fused color op
    /// — an affine color matrix plus an optional gamma — applied to a source
    /// texture (§17.3, E2.2).
    color_transform_pipeline: PipelineId,
    /// The AdvancedBlend built-in pipeline (registered once): one isolated blend
    /// composite that samples the layer and the bounded destination snapshot behind
    /// it, evaluates a mode the fixed-function blender cannot express, and writes
    /// with [`BlendMode::Replace`] (§14.6, E2.3).
    advanced_blend_pipeline: PipelineId,
    /// The MaterialComposite built-in pipeline (registered once): one frosted
    /// material surface — the shared blurred backdrop, a fused color op,
    /// deterministic grain, and a per-corner rounded mask — in a single draw
    /// (§18, M0.1).
    material_pipeline: PipelineId,
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
    /// Persistent analytic soft-shadow instance pool (same slot-diff upload as
    /// `quad_pool`).
    analytic_shadow_pool: InstancePool<ShadowInstance>,
    /// Persistent blur instance pool (same slot-diff upload as `quad_pool`): one
    /// [`BlurInstance`] per separable pass this frame, indexed by `BlurPass`.
    blur_pool: InstancePool<BlurInstance>,
    /// Persistent color-transform instance pool (same slot-diff upload as
    /// `quad_pool`): one [`ColorTransformInstance`] per fused color op this frame,
    /// whether the op rides a layer's composite draw or its own pass.
    color_transform_pool: InstancePool<ColorTransformInstance>,
    /// Persistent advanced-blend instance pool (same slot-diff upload as
    /// `quad_pool`): one [`AdvancedBlendInstance`] per isolated non-`SrcOver`
    /// layer composited this frame. Empty for every ordinary frame.
    advanced_blend_pool: InstancePool<AdvancedBlendInstance>,
    /// Persistent material instance pool (same slot-diff upload as `quad_pool`):
    /// one [`MaterialInstance`] per frosted surface composited this frame.
    material_pool: InstancePool<MaterialInstance>,
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
    /// Cached two-texture bind groups for isolated advanced-blend composites,
    /// reused across frames. Separate from `texture_bindings` because the key is a
    /// pair, not a (texture, sampler): folding both into one scanned `Vec` would
    /// make every ordinary image draw compare a field it never sets.
    blend_bindings: Vec<BlendBinding>,
    /// Layers isolated by a non-`SrcOver` blend this frame (§14.6). Reset at frame
    /// start, surfaced as [`FrameStats::blend_isolations`].
    blend_isolations: u32,
    /// Every layer plan the Effect Planner produced this frame, in stream order
    /// (§3145). Cleared at frame start and retained afterwards so the inspector can
    /// answer "why did this group cost a pass?" from the same data the renderer
    /// decided on — no unsafe poking, no second decision path (§62). Three bytes of
    /// bitset plus a float per layer, and a `Vec` whose capacity survives the frame.
    layer_plans: Vec<LayerPlan>,
    /// Layers considered / partly eliminated / opacity-folded this frame. Reset at
    /// frame start, surfaced as the matching [`FrameStats`] fields.
    layers_planned: u32,
    layers_eliminated: u32,
    opacity_folds: u32,
    /// Backdrop captures whose ROI content moved since the previous frame (§3202).
    /// Reset at frame start, surfaced as [`FrameStats::backdrop_dirty_rois`].
    backdrop_dirty_rois: u32,
    /// One [`BackdropDependency`] per backdrop capture, **retained across frames**:
    /// it is the previous frame's value each capture's new revision is compared
    /// against, rewritten in place once the comparison is made.
    backdrop_dependencies: Vec<BackdropDependency>,
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
    /// Scratch analytic soft-shadow instance data, reused each frame.
    analytic_shadow_scratch: Vec<ShadowInstance>,
    /// Scratch blur instance data, reused each frame: one [`BlurInstance`] per
    /// separable pass, filled in `finalize_offscreen` and synced by `upload`.
    blur_scratch: Vec<BlurInstance>,
    /// Scratch color-transform instance data, reused each frame: one
    /// [`ColorTransformInstance`] per fused color op, filled in
    /// `finalize_offscreen` (extra rungs) and in lowering (the composite's op).
    color_transform_scratch: Vec<ColorTransformInstance>,
    /// Scratch advanced-blend instance data, reused each frame: one
    /// [`AdvancedBlendInstance`] per isolated non-`SrcOver` layer, filled where
    /// the layer's composite draw is lowered.
    advanced_blend_scratch: Vec<AdvancedBlendInstance>,
    /// Scratch material instance data, reused each frame: one
    /// [`MaterialInstance`] per frosted surface, filled where its composite draw
    /// is lowered.
    material_scratch: Vec<MaterialInstance>,
    /// This frame's frosted-surface parameters, reused each frame. A
    /// [`StoreRef::MaterialComposite`] carries only the index into this list, so a
    /// paint-order entry stays small while the ~100 bytes of material parameters
    /// live once per surface here.
    material_records: Vec<MaterialRecord>,
    /// This frame's [`MaterialLane::Native`] regions, in paint order, reused each
    /// frame. These reserve space for the platform's own material rather than
    /// describing a draw, so they never reach the GPU — the platform layer reads them
    /// back with [`native_material_regions`](Self::native_material_regions) after
    /// [`upload`](Self::upload).
    native_material_regions: Vec<NativeMaterialRegion>,
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
    /// The separable Gaussian blur passes for this frame's blurred layers, in
    /// execution order. Built during `finalize_offscreen` (one per `BlurStep` of
    /// each layer's [`BlurPlan`]); drained in [`Renderer::encode`] between the
    /// offscreen layer passes and the surface pass. Reused each frame.
    blur_passes: Vec<BlurPass>,
    /// The extra color-transform passes for this frame's layers whose effect chain
    /// did not fuse into a single op, in execution order. Built during
    /// `finalize_offscreen` (one per fused op *beyond the last*, which rides the
    /// layer's composite draw instead); drained in [`Renderer::encode`] alongside
    /// the blur rungs. Empty for every chain that fused completely. Reused each
    /// frame.
    color_passes: Vec<ColorPass>,
    /// This frame's fused color ops, in layer-open order: the arena
    /// [`LayerEntry::color_start`] / `color_len` index into, filled by
    /// [`fuse`](crate::color_effect::fuse) when a layer's effect chain is read.
    /// Cleared per frame, so a steady scene reuses the allocation (§7.1).
    color_ops: Vec<ColorOp>,
    /// This frame's backdrop captures, in creation order — one per *group* of
    /// backdrop layers that share a background (§17.2), not one per layer. Each
    /// re-renders the content already recorded behind its group into a tight ROI
    /// target and blurs it; the group's members each composite that one blurred
    /// result under their own content. Reused each frame.
    backdrop_captures: Vec<BackdropCapture>,
    /// Scratch for folding one backdrop capture's read edges: the render graph
    /// does not deduplicate reads, and one producer can contribute many paint
    /// entries to a capture. Cleared per capture, retained across frames.
    backdrop_reads: Vec<TargetId>,
    /// The frame-local transient render-target pool (§16.4): every offscreen
    /// layer and every blur rung declares a virtual target here, and one
    /// lifetime-aware assignment pass maps those virtuals onto reusable physical
    /// textures keyed by format / usage / size class / sample count. Replaces
    /// per-effect `create_texture`/`destroy_texture`.
    transient: TransientTargets,
    /// This frame's pass topology (§16.1, §25): every offscreen layer, every blur
    /// rung, and the surface records a node here with the target it writes and the
    /// targets it samples. The graph validates the dependency order, culls
    /// unread passes, merges adjacent passes sharing an attachment, derives every
    /// transient lifetime, and lowers each pass's load op — and reuses the
    /// compiled plan whenever the topology is unchanged, so a resize, a recolor,
    /// or a blur sigma tweak never rebuilds it. [`Renderer::encode`] walks the
    /// compiled plan; the passes' payloads stay in `offscreen_passes` /
    /// `blur_passes`.
    graph: RenderGraph,
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
    /// The surface size in physical pixels `[width, height]`, the outer clamp
    /// for every offscreen ROI (`content ∩ clip ∩ surface`, §16.2). Defaults to
    /// [`Rect::INFINITE`]'s extent (no surface clamp) until
    /// [`set_surface_size`](Self::set_surface_size) reports the real size; the
    /// caller passes the same value it later hands `submit`.
    surface_size: [f32; 2],
    /// Offscreen-pass children lowered this frame whose paint bounds fell fully
    /// outside their pass's tight ROI (the clip excluded them), so their draw
    /// was skipped (§16.2). Read into `FrameStats::culled_primitives` (§61).
    culled_this_frame: u32,
    /// The surface's own color-attachment format — what every prewarmed pipeline
    /// was built against, so it is also the only format an offscreen pass can
    /// legally use (a pipeline is bound to its attachment format).
    surface_format: TextureFormat,
    /// The color space the compositor reads the surface through, as the backend
    /// reported it. Orthogonal to the format: the same 8-bit texels mean
    /// different colors in sRGB and Display P3.
    color_space: ColorSpace,
    /// The pixel format every intermediate color target this frame is allocated
    /// with — backdrop captures, offscreen layers, blur scratch, color scratch.
    ///
    /// The planner's rule is that this *is* [`surface_format`](Self::surface_format),
    /// and that is a decision, not a shortcut. The surface's format is
    /// simultaneously the only one in the target's own domain, lossless for its
    /// precision, no wider than its precision needs, and compatible with the
    /// single prewarmed pipeline set — so both failure modes the rule exists to
    /// forbid are structurally impossible: an SDR target never pays for
    /// half-float intermediates, and an extended-range target never has its
    /// intermediates narrowed to 8 bits and then stretched back up. A format that
    /// differed from the surface's would need a second set of 14 pipelines and a
    /// per-target format on every draw, buying nothing at either end.
    ///
    /// Coverage planes are not color and are excluded by construction: the glyph
    /// atlas and mask pages stay `R8Unorm` at every domain (see
    /// [`TextureFormat::is_color`]).
    intermediate_format: TextureFormat,
}

/// Blur below this sigma (in physical pixels) is a visual no-op: the separable
/// ladder would resolve to a near-identity single-tap resample, so a sub-pixel
/// blur is skipped and the layer composites its unblurred content directly. An
/// opaque layer asking for one stays inline entirely — it pays no offscreen pass
/// for an effect that would not survive the round trip.
const BLUR_MIN_SIGMA: f32 = 1.0;

/// The Gaussian tail is negligible past three standard deviations, so a full-
/// quality pass samples `ceil(3 * sigma)` taps on each side of center.
const BLUR_RADIUS_SIGMAS: f32 = 3.0;

/// The most taps one separable pass samples on each side of center. A blur whose
/// full-resolution radius would exceed this downsamples first, so the reduced-
/// resolution blur stays within this bound (a fixed per-pixel tap budget).
const BLUR_MAX_TAPS: u32 = 32;

/// One separable blur pass in a [`BlurPlan`]: a full-target draw that reads the
/// previous stage's texture and writes the next, sized `width`×`height`.
///
/// The `axis`/`sigma`/`radius` are expressed against the *source* being sampled
/// (source texels for `sigma`, a unit direction for `axis`), matching the
/// [`BlurInstance`] the headless raster and Metal shader both read. A downsample
/// step shrinks the target below the source; a blur step keeps the extent and
/// walks one axis. The source is the base offscreen texture for the first step
/// and the previous step's target thereafter.
///
/// `width`/`height` are the *used* extent: the tight sub-rect this rung writes
/// and the next samples. The pooled texture backing it may be larger (its size
/// class, §16.4), which is why `axis` is a plain unit direction — the per-tap uv
/// step needs the *physical* source extent, known only once the planner has
/// assigned a texture, so the normalization happens at instance-build time.
#[derive(Debug, Clone, Copy, PartialEq)]
struct BlurStep {
    /// Used target extent in physical pixels.
    width: u32,
    height: u32,
    /// Unit direction this pass walks: `[1, 0]` horizontal, `[0, 1]` vertical,
    /// `[0, 0]` for a plain single-tap resample.
    axis: [f32; 2],
    /// Gaussian standard deviation in source texels (`0` for a plain resample).
    sigma: f32,
    /// Tap count on each side of center.
    radius: f32,
}

/// The separable Gaussian ladder for one blurred layer (§16, E1.2).
///
/// A plan is a pure function of the requested sigma and the source ROI extent
/// ([`blur_plan`]); it owns no GPU resources. An empty plan means the blur is a
/// no-op (sub-pixel sigma) and the layer composites its base texture unchanged.
/// Otherwise the renderer claims one scratch target per step, chains the draws
/// base → step0 → step1 → …, and repoints the layer's composite to sample the
/// final step's texture.
#[derive(Debug, Clone, PartialEq)]
struct BlurPlan {
    steps: Vec<BlurStep>,
}

impl BlurPlan {
    /// A no-op plan: no passes, composite samples the base texture directly.
    fn skip() -> Self {
        BlurPlan { steps: Vec::new() }
    }

    /// Whether this plan inserts any blur passes.
    fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }
}

/// Plan the separable Gaussian ladder for a `sigma`-pixel blur of a `src_w`×
/// `src_h` offscreen ROI (§16.2, E1.2). Pure: no allocation of GPU targets, only
/// the step descriptors the renderer realizes.
///
/// The tiers, cheapest first:
///
/// 1. **Skip** — `sigma <= `[`BLUR_MIN_SIGMA`]: a sub-pixel blur is a visual
///    no-op, so the plan is empty and the layer composites unblurred.
/// 2. **Two separable passes** — the full-resolution radius `ceil(3 * sigma)`
///    fits [`BLUR_MAX_TAPS`]: one horizontal then one vertical pass at the source
///    extent, each `2 * radius + 1` taps.
/// 3. **Downsample pyramid** — a larger radius would blow the tap budget, so the
///    source is halved (bilinearly, one separable resample per axis) by an
///    integer factor `d` chosen so the reduced-resolution radius `ceil(3 * sigma
///    / d)` fits the budget, then blurred horizontally and vertically at the
///    reduced extent. The composite upsamples by sampling the smaller texture
///    over the full ROI (linear filter), so the wide blur costs a bounded tap
///    count per pixel rather than growing without limit.
fn blur_plan(sigma: f32, src_w: u32, src_h: u32) -> BlurPlan {
    // A blur is planned only for a super-pixel sigma over a non-empty source.
    // Phrasing the guard as the positive "plans a blur" condition (rather than a
    // negated `>`) also skips a NaN sigma, since `NaN > floor` is false.
    let blurs = sigma > BLUR_MIN_SIGMA && src_w != 0 && src_h != 0;
    if !blurs {
        return BlurPlan::skip();
    }

    let full_radius = (BLUR_RADIUS_SIGMAS * sigma).ceil().max(1.0);

    // Tier 2: the full-resolution radius fits one pass's tap budget.
    if full_radius as u32 <= BLUR_MAX_TAPS {
        let step_h = BlurStep {
            width: src_w,
            height: src_h,
            axis: [1.0, 0.0],
            sigma,
            radius: full_radius,
        };
        let step_v = BlurStep {
            width: src_w,
            height: src_h,
            axis: [0.0, 1.0],
            sigma,
            radius: full_radius,
        };
        return BlurPlan {
            steps: vec![step_h, step_v],
        };
    }

    // Tier 3: downsample so the reduced-resolution radius fits the budget. The
    // smallest integer `d` with `ceil(3 * sigma / d) <= MAX_TAPS`.
    let mut d = 2u32;
    loop {
        let reduced = (BLUR_RADIUS_SIGMAS * sigma / d as f32).ceil().max(1.0);
        if reduced as u32 <= BLUR_MAX_TAPS {
            break;
        }
        d += 1;
    }
    let dst_w = (src_w / d).max(1);
    let dst_h = (src_h / d).max(1);
    let reduced_sigma = sigma / d as f32;
    let reduced_radius = (BLUR_RADIUS_SIGMAS * reduced_sigma).ceil().max(1.0);

    // Downsample each axis (bilinear resample, sigma 0 = a straight box of one
    // linear tap) into the reduced extent, then blur horizontally and vertically
    // at that extent. The downsample reads the base at full extent; the blurs
    // read the already-reduced texture, so their per-tap step is one reduced
    // texel (the `axis` normalization happens against that texture's extent).
    let down_h = BlurStep {
        width: dst_w,
        height: src_h,
        axis: [0.0, 0.0],
        sigma: 0.0,
        radius: 0.0,
    };
    let down_v = BlurStep {
        width: dst_w,
        height: dst_h,
        axis: [0.0, 0.0],
        sigma: 0.0,
        radius: 0.0,
    };
    let blur_h = BlurStep {
        width: dst_w,
        height: dst_h,
        axis: [1.0, 0.0],
        sigma: reduced_sigma,
        radius: reduced_radius,
    };
    let blur_v = BlurStep {
        width: dst_w,
        height: dst_h,
        axis: [0.0, 1.0],
        sigma: reduced_sigma,
        radius: reduced_radius,
    };
    BlurPlan {
        steps: vec![down_h, down_v, blur_h, blur_v],
    }
}

/// One realized blur pass this frame: the transient target it samples, its
/// viewport, and the slot of its single [`BlurInstance`] in
/// [`Renderer::blur_scratch`]. Transient — rebuilt every frame in
/// `finalize_offscreen`, encoded from the compiled plan in [`Renderer::encode`].
///
/// The target it *writes* lives on its graph node, not here (§41): the graph is
/// what lowers attachments and load ops. `source` is a *virtual* id — the concrete
/// texture and bind group are resolved from [`Renderer::transient`] after the
/// frame's assignment pass, so a rung's scratch texture can be recycled from an
/// earlier rung whose lifetime has ended (§16.4).
struct BlurPass {
    source: TargetId,
    /// The *physical* extent of `target` — what the shader maps pixels to NDC
    /// against, which is the pooled texture's size class, not the used sub-rect.
    viewport: [f32; 2],
    instance: u32,
}

/// One realized color-transform pass this frame — the same shape as [`BlurPass`],
/// for the same reason: the target it writes lives on its graph node, `source` is
/// a virtual id resolved after the assignment pass, and `instance` slots into
/// [`Renderer::color_transform_scratch`].
///
/// Only a chain whose ops could not all fuse mints these, one per op beyond the
/// last. A fully-fused chain mints none — its single op rides the layer's
/// composite draw.
struct ColorPass {
    source: TargetId,
    /// The *physical* extent of the written target (see [`BlurPass::viewport`]).
    viewport: [f32; 2],
    instance: u32,
}

/// One frosted material surface's parameters, as resolved during the frame's
/// primitive walk (§18). Held per frame on the renderer rather than inline in
/// [`StoreRef::MaterialComposite`]: the composite is a derived draw, and a
/// paint-order entry is visited for every primitive, so it carries an index.
#[derive(Debug, Clone, Copy, PartialEq)]
struct MaterialRecord {
    /// The surface rect in world space — both the composited quad and the
    /// rounded-mask geometry the fragment evaluates its SDF against.
    rect: Rect,
    /// Per-corner radii, already normalized against the rect
    /// (`[top_left, top_right, bottom_right, bottom_left]`).
    radius: [f32; 4],
    /// The fused color op applied to the blurred backdrop — E2's frozen 4×5
    /// matrix plus optional gamma, reused verbatim.
    op: ColorOp,
    /// Grain amplitude in unit color, `0.0` for none.
    noise: f32,
    /// Surface opacity, group-opacity fold already applied.
    opacity: f32,
}

/// Lay a [`MaterialRecord`] out as GPU instance data over `rect` (target space)
/// sampling `uv` (normalized against the capture's physical extent).
///
/// The fused op's 4×5 matrix splits into row vectors plus the offset column, the
/// same shape [`color_transform_instance`] produces — the material pipeline runs
/// E2's color stage inline rather than in a pass of its own.
fn material_instance(
    record: MaterialRecord,
    rect_pos: [f32; 2],
    rect_size: [f32; 2],
    uv_pos: [f32; 2],
    uv_size: [f32; 2],
) -> MaterialInstance {
    let r = record.op.matrix.rows;
    let row = |i: usize| [r[i][0], r[i][1], r[i][2], r[i][3]];
    MaterialInstance {
        rect_pos,
        rect_size,
        uv_pos,
        uv_size,
        row0: row(0),
        row1: row(1),
        row2: row(2),
        row3: row(3),
        offset: [r[0][4], r[1][4], r[2][4], r[3][4]],
        radius: record.radius,
        gamma: record.op.gamma,
        noise: record.noise,
        opacity: record.opacity,
    }
}

/// Lay a fused [`ColorOp`] out as GPU instance data over `rect` (target space)
/// sampling `uv` (normalized against the source's physical extent).
///
/// The op's 4×5 matrix splits into four row vectors and the fifth (offset) column,
/// because that is the shape the shader consumes: four dot products plus an add.
fn color_transform_instance(
    op: ColorOp,
    rect_pos: [f32; 2],
    rect_size: [f32; 2],
    uv_pos: [f32; 2],
    uv_size: [f32; 2],
) -> ColorTransformInstance {
    let r = op.matrix.rows;
    let row = |i: usize| [r[i][0], r[i][1], r[i][2], r[i][3]];
    ColorTransformInstance {
        rect_pos,
        rect_size,
        uv_pos,
        uv_size,
        row0: row(0),
        row1: row(1),
        row2: row(2),
        row3: row(3),
        offset: [r[0][4], r[1][4], r[2][4], r[3][4]],
        gamma: op.gamma,
    }
}

impl Renderer {
    /// Create a renderer for a surface of `surface_format` in the ordinary SDR
    /// sRGB color space, registering the built-in pipelines and a shared
    /// linear-clamp sampler.
    ///
    /// `surface_format` is the color-attachment format the pipelines target. For
    /// a wide-gamut or HDR target use
    /// [`for_surface`](Self::for_surface), which asks the backend for both the
    /// format and the space rather than assuming either.
    pub fn new<B: GpuBackend>(backend: &mut B, surface_format: TextureFormat) -> Self {
        Self::for_target(backend, surface_format, ColorSpace::Srgb)
    }

    /// Create a renderer for `surface`, taking both its color-attachment format
    /// and its color space from the backend.
    ///
    /// This is the constructor a real window uses: it cannot disagree with the
    /// surface it draws into, and it is the only way an extended-range target
    /// gets extended-range intermediates (see
    /// [`intermediate_format`](Self::intermediate_format)).
    pub fn for_surface<B: GpuBackend>(backend: &mut B, surface: SurfaceId) -> Self {
        let format = backend.surface_format(surface);
        let space = backend.surface_color_space(surface);
        Self::for_target(backend, format, space)
    }

    /// The shared body of both constructors: build every pipeline against
    /// `surface_format` and plan the intermediates for `color_space`'s domain.
    fn for_target<B: GpuBackend>(
        backend: &mut B,
        surface_format: TextureFormat,
        color_space: ColorSpace,
    ) -> Self {
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
            // The blend state belongs to the family, not to this call site: every
            // family composites premultiplied source-over except the isolated
            // advanced blend, whose fragment returns the finished composite.
            blend: entry.blend,
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

        let analytic_shadow_pipeline = backend
            .create_pipeline(
                &desc(entry(PipelineFamily::AnalyticShadow), "analytic-shadow"),
                &ShadowInstance::LAYOUT,
            )
            .expect("ShadowInstance layout matches the analytic-shadow shader schema");

        let blur_pipeline = backend
            .create_pipeline(
                &desc(entry(PipelineFamily::ContentBlur), "blur"),
                &BlurInstance::LAYOUT,
            )
            .expect("BlurInstance layout matches the blur shader schema");

        let color_transform_pipeline = backend
            .create_pipeline(
                &desc(entry(PipelineFamily::ColorTransform), "color-transform"),
                &ColorTransformInstance::LAYOUT,
            )
            .expect("ColorTransformInstance layout matches the color-transform shader schema");

        let advanced_blend_pipeline = backend
            .create_pipeline(
                &desc(entry(PipelineFamily::AdvancedBlend), "advanced-blend"),
                &AdvancedBlendInstance::LAYOUT,
            )
            .expect("AdvancedBlendInstance layout matches the advanced-blend shader schema");

        let material_pipeline = backend
            .create_pipeline(
                &desc(entry(PipelineFamily::MaterialComposite), "material"),
                &MaterialInstance::LAYOUT,
            )
            .expect("MaterialInstance layout matches the material shader schema");

        // The 1D gradient LUT atlas is renderer-internal: baked from stops at
        // lowering, uploaded into this texture before the pass. Unlike image and
        // glyph textures (caller-owned), the renderer creates and owns it here.
        let lut_texture = backend.create_texture(&TextureDesc {
            width: LUT_WIDTH,
            height: GRADIENT_LUT_ROWS,
            format: GradientLutAtlas::format_for(color_space.domain()),
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
            analytic_shadow_pipeline,
            blur_pipeline,
            color_transform_pipeline,
            advanced_blend_pipeline,
            material_pipeline,
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
            analytic_shadow_pool: InstancePool::new(
                BufferUsage::INSTANCE,
                "analytic-shadow-instances",
            ),
            blur_pool: InstancePool::new(BufferUsage::INSTANCE, "blur-instances"),
            color_transform_pool: InstancePool::new(
                BufferUsage::INSTANCE,
                "color-transform-instances",
            ),
            advanced_blend_pool: InstancePool::new(
                BufferUsage::INSTANCE,
                "advanced-blend-instances",
            ),
            material_pool: InstancePool::new(BufferUsage::INSTANCE, "material-instances"),
            gradient_lut: GradientLutAtlas::new(
                GRADIENT_LUT_ROWS,
                lut_texture,
                GradientLutAtlas::format_for(color_space.domain()),
            ),
            mask_cache: MaskCache::new(MASK_PAGE_SIZE),
            mask_page: MaskPage::new(MASK_PAGE_SIZE, mask_texture),
            mask_builds_this_frame: 0,
            mesh_pipeline,
            mesh_vertex_pool: InstancePool::new(BufferUsage::VERTEX, "mesh-vertices"),
            mesh_index_pool: InstancePool::new(BufferUsage::INDEX, "mesh-indices"),
            texture_bindings: Vec::with_capacity(8),
            blend_bindings: Vec::with_capacity(4),
            blend_isolations: 0,
            layer_plans: Vec::new(),
            layers_planned: 0,
            layers_eliminated: 0,
            opacity_folds: 0,
            backdrop_dirty_rois: 0,
            backdrop_dependencies: Vec::new(),
            quad_scratch: Vec::with_capacity(256),
            analytic_rrect_scratch: Vec::with_capacity(256),
            analytic_ellipse_scratch: Vec::with_capacity(256),
            analytic_capsule_scratch: Vec::with_capacity(256),
            analytic_line_scratch: Vec::with_capacity(256),
            image_scratch: Vec::with_capacity(64),
            glyph_scratch: Vec::with_capacity(256),
            gradient_scratch: Vec::with_capacity(64),
            analytic_shadow_scratch: Vec::with_capacity(256),
            blur_scratch: Vec::with_capacity(8),
            color_transform_scratch: Vec::with_capacity(8),
            advanced_blend_scratch: Vec::with_capacity(4),
            material_scratch: Vec::with_capacity(8),
            material_records: Vec::with_capacity(8),
            native_material_regions: Vec::new(),
            mesh_vertex_scratch: Vec::with_capacity(1024),
            mesh_index_scratch: Vec::with_capacity(2048),
            segments: Vec::with_capacity(8),
            layer_stack: Vec::with_capacity(8),
            offscreen_passes: Vec::with_capacity(4),
            blur_passes: Vec::with_capacity(4),
            color_passes: Vec::with_capacity(2),
            color_ops: Vec::with_capacity(4),
            backdrop_captures: Vec::with_capacity(2),
            backdrop_reads: Vec::with_capacity(4),
            transient: TransientTargets::new(),
            graph: RenderGraph::new(),
            commands: Vec::with_capacity(8),
            passes: Vec::with_capacity(4),
            scene: Scene::new(),
            gpu_upload_bytes: 0,
            uploaded_ranges: 0,
            surface_size: [Rect::INFINITE.w, Rect::INFINITE.h],
            surface_format,
            color_space,
            intermediate_format: surface_format,
            culled_this_frame: 0,
        }
    }

    /// Report the surface size in physical pixels `[width, height]` — the outer
    /// clamp for every offscreen ROI (§16.2). Call before [`upload`](Self::upload)
    /// with the same value later handed to [`submit`](Self::submit); until then
    /// ROIs clamp only to content and clip (no surface bound).
    pub fn set_surface_size(&mut self, surface_size: [f32; 2]) {
        self.surface_size = surface_size;
    }

    /// The color-attachment format every built-in pipeline was created against.
    pub fn surface_format(&self) -> TextureFormat {
        self.surface_format
    }

    /// The color space the compositor reads this renderer's surface through.
    pub fn color_space(&self) -> ColorSpace {
        self.color_space
    }

    /// Which of SDR / wide-gamut / HDR this renderer is drawing for (§19) — the
    /// classification its intermediate formats are planned against.
    pub fn color_domain(&self) -> ColorDomain {
        self.color_space.domain()
    }

    /// The pixel format allocated for every intermediate color target this frame
    /// (backdrop capture, offscreen layer, blur scratch, color scratch).
    ///
    /// Reported so a test — or an inspector — can state the contract directly:
    /// an extended-range target keeps extended-range intermediates, and an SDR
    /// one is never widened past what it can display. See the field docs for why
    /// this is the surface's own format.
    pub fn intermediate_format(&self) -> TextureFormat {
        self.intermediate_format
    }

    /// The format the gradient LUT bakes its ramps into, which follows the
    /// domain: an HDR target's ramps keep stops above 1.0 instead of clamping
    /// them at the bake.
    pub fn gradient_lut_format(&self) -> TextureFormat {
        self.gradient_lut.format()
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

    /// Get (or lazily create) the two-texture bind group an isolated advanced-blend
    /// composite draws with: the isolated layer as `source`, the bounded snapshot of
    /// what is behind it as `destination` (§14.6).
    ///
    /// Slot order is the contract — slot 0 is the source, slot 1 the destination —
    /// because the shader names its samplers by role (`tex`, `dst_tex`) and binds
    /// them by index. Both share the renderer's linear-clamp sampler, so one
    /// `Sampler` binding covers the pair.
    fn blend_bind_group_for<B: GpuBackend>(
        &mut self,
        backend: &mut B,
        source: TextureId,
        destination: TextureId,
    ) -> BindGroupId {
        if let Some(bb) = self
            .blend_bindings
            .iter()
            .find(|bb| bb.source == source && bb.destination == destination)
        {
            return bb.bind_group;
        }
        let bind_group = backend.create_bind_group(&BindGroupDesc {
            label: "advanced-blend",
            bindings: vec![
                Binding::Texture(source),
                Binding::Texture(destination),
                Binding::Sampler(self.sampler),
            ],
        });
        self.blend_bindings.push(BlendBinding {
            source,
            destination,
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

    /// Draw an arbitrary path's drop shadow through the mask lane (§15.4/§20.2).
    ///
    /// The E0 fallback rasterizes the path's own tight coverage once — keyed on
    /// {geometry, sigma} so it is a distinct slot from the shape's own fill mask
    /// and survives color/offset-only changes — then composites it offset by
    /// `offset` and tinted by `color` under the shape. There is no separable blur
    /// convolution yet: the cached mask is sharp and the recorded sigma leans the
    /// key forward to E1, where the ROI-padded blurred reblit drops in without a
    /// re-key. A soft `sigma` therefore reads as a sharp offset silhouette until
    /// E1 lands; the geometry/offset/color/bounds contract is already final.
    ///
    /// Returns `false` (caller keeps the normal fill lane, shadow undrawn) for a
    /// degenerate path, an inner shadow (routed to the analytic lane — §20.3), or
    /// a packer-declined ROI.
    fn mask_path_shadow(
        &mut self,
        path: &crate::primitive::Path,
        shadow: &crate::primitive::PathShadow,
        ctx: EmitContext,
        clip: Option<Rect>,
    ) -> bool {
        // Inner shadows on a general path are an E1 filter-lane concern; the
        // analytic families carry inner shadow directly (§20.3), so the general
        // path lane only serves the outer drop shadow.
        if shadow.inner {
            return false;
        }

        let roi = path_bounds(path.cmds.iter().copied());
        if roi.w <= 0.0 || roi.h <= 0.0 {
            return false;
        }

        let plan = plan_clip(ClipShape::Path { bounds: roi }, false);
        if !plan.tier.builds_mask() {
            return false;
        }

        // Fold sigma into the geometry revision so a shadow mask never aliases the
        // shape's own solid-fill mask (same cmds, no sigma) and so E1's blurred
        // reblit re-keys automatically when the blur radius changes.
        let source_revision = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            hash_path_cmds(&path.cmds).hash(&mut h);
            shadow.sigma.to_bits().hash(&mut h);
            shadow.spread.to_bits().hash(&mut h);
            h.finish()
        };
        let key = MaskKey {
            kind: MaskKind::Path,
            source_revision,
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
            return false;
        };

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

        // Composite the cached coverage offset by the drop and tinted by the
        // shadow color. The shadow footprint extends the emitted quad's bounds by
        // the blur reach (3σ) plus outward spread plus the offset, so the retained
        // paint/effect bounds cover the soft, shifted silhouette.
        let size = self.mask_page.size() as f32;
        let shadow_rect = Rect {
            x: roi.x + shadow.offset[0],
            y: roi.y + shadow.offset[1],
            w: roi.w,
            h: roi.h,
        };
        let inst = GlyphInstance {
            rect_pos: [shadow_rect.x, shadow_rect.y],
            rect_size: [shadow_rect.w, shadow_rect.h],
            uv_pos: [res.slot.x as f32 / size, res.slot.y as f32 / size],
            uv_size: [res.slot.w as f32 / size, res.slot.h as f32 / size],
            color: [
                shadow.color.r,
                shadow.color.g,
                shadow.color.b,
                shadow.color.a,
            ],
        };
        let filter = 3.0 * shadow.sigma + shadow.spread.max(0.0);
        let bounds = crate::scene::bounds::Bounds::from_world(shadow_rect, clip, 0.0, filter);
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
        self.blur_passes.clear();
        self.color_passes.clear();
        self.color_ops.clear();
        self.backdrop_captures.clear();
        self.material_records.clear();
        self.native_material_regions.clear();
        self.blur_scratch.clear();
        self.color_transform_scratch.clear();
        self.advanced_blend_scratch.clear();
        self.graph.begin_frame();
        self.transient.begin_frame();
        self.scene.begin_frame();
        self.mask_cache.begin_frame();
        self.mask_builds_this_frame = 0;
        self.culled_this_frame = 0;
        self.blend_isolations = 0;
        self.layer_plans.clear();
        self.layers_planned = 0;
        self.layers_eliminated = 0;
        self.opacity_folds = 0;
        self.backdrop_dirty_rois = 0;

        for (prim_index, prim) in primitives.iter().enumerate() {
            let (clip, target, origin) = self.active();
            // The group opacity the Effect Planner pushed into this primitive's
            // ancestors (§14.5). `1.0` — the steady state everywhere outside a
            // folded group — leaves every instance byte-identical.
            let fold = self.active_fold();
            let ctx = EmitContext {
                clip,
                offscreen: match target {
                    PassTarget::Main => None,
                    PassTarget::Offscreen(idx) => Some(idx),
                    // A capture pass is never on the layer stack: it re-renders
                    // entries this walk already recorded, at lowering time.
                    PassTarget::Capture(_) => None,
                },
                origin,
            };
            // Paint-order slots this primitive records, so its paint bounds can
            // be folded into the innermost offscreen layer's content union after
            // the match (a nested layer's composite, recorded at its LayerEnd,
            // rolls up into the outer pass the same way). A backdrop layer moves
            // this cursor past its own backdrop composite, which belongs to the
            // *parent* target and must not be folded into the layer it opens.
            let mut record_start = self.scene.paint_order.len();
            match prim {
                Primitive::Quad(quad) => {
                    // Diff the world-space instance (origin not yet subtracted)
                    // into the retained store, bumping only the moved planes, and
                    // record its paint-order slot.
                    let mut inst = quad.to_instance();
                    if fold < 1.0 {
                        inst.color[3] *= fold;
                        inst.border_color[3] *= fold;
                    }
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
                    let mut inst = rrect.to_instance();
                    if fold < 1.0 {
                        inst.color[3] *= fold;
                        inst.border_color[3] *= fold;
                    }
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
                    let mut inst = ellipse.to_instance();
                    if fold < 1.0 {
                        inst.color[3] *= fold;
                        inst.border_color[3] *= fold;
                    }
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
                    let mut inst = capsule.to_instance();
                    if fold < 1.0 {
                        inst.color[3] *= fold;
                        inst.border_color[3] *= fold;
                    }
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
                    let mut inst = line.to_instance();
                    if fold < 1.0 {
                        inst.color[3] *= fold;
                        inst.border_color[3] *= fold;
                    }
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
                    let mut inst = image.to_instance();
                    if fold < 1.0 {
                        inst.color[3] *= fold;
                    }
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
                Primitive::AnalyticShadow(shadow) => {
                    let mut inst = shadow.to_instance();
                    if fold < 1.0 {
                        inst.color[3] *= fold;
                    }
                    // The shadow footprint extends past the shape rect by the
                    // blur reach (3σ covers a Gaussian), the outward spread, and
                    // the drop offset. Fed as the `filter` inflation term so the
                    // paint/effect bounds cover the soft-edged, offset quad.
                    let shadow_filter = 3.0 * inst.sigma
                        + inst.spread.max(0.0)
                        + inst.offset[0].abs().max(inst.offset[1].abs());
                    let bounds = crate::scene::bounds::Bounds::from_world(
                        Rect {
                            x: inst.rect_pos[0],
                            y: inst.rect_pos[1],
                            w: inst.rect_size[0],
                            h: inst.rect_size[1],
                        },
                        clip,
                        0.0,
                        shadow_filter,
                    );
                    self.scene.ingest_analytic_shadow(inst, ctx, bounds);
                }
                Primitive::Path(path) => {
                    // A path drop shadow lowers first so it sits under the fill:
                    // its tight coverage is cached once (keyed on geometry+sigma)
                    // and composited offset + tinted through the mask lane.
                    if let Some(shadow) = path.shadow.as_ref() {
                        self.mask_path_shadow(path, shadow, ctx, clip);
                    }
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
                Primitive::Frosted(material) => {
                    // A frosted surface is a leaf, not a container: it declares a
                    // backdrop dependency, joins (or opens) the capture group for
                    // it exactly the way a backdrop layer does — so `N` panels at
                    // one sigma still cost one capture and one blur ladder — and
                    // then composites the whole §18 chain in a single draw.
                    let world_clip = match clip {
                        Some(c) => material.rect.intersect(c),
                        None => material.rect,
                    };
                    // Below `BLUR_MIN_SIGMA` there is nothing to capture (a
                    // sub-pixel blur of the backdrop is the backdrop), and a
                    // surface inside an offscreen layer has no surface-pass
                    // backdrop to read — see `backdrop_roi`. Either way the
                    // surface contributes only its border ring.
                    if material.lane == MaterialLane::Native && matches!(target, PassTarget::Main) {
                        // The native lane reserves the region and draws nothing: the
                        // platform's own material composites behind the Viso surface,
                        // so capturing and blurring a backdrop here would pay for a
                        // frosted panel twice and then hide one of them. The region is
                        // reported with its generic parameters only — the platform
                        // layer owns the mapping to its material vocabulary (§19).
                        //
                        // Only on the surface pass: a material composited *behind* the
                        // Viso surface cannot be scaled, blurred, or blended by an
                        // offscreen layer's own composite, so inside one the lane
                        // falls back to whatever the GPU lane can do there rather
                        // than reserving a region the enclosing layer would then
                        // fail to honour.
                        if world_clip.w > 0.0 && world_clip.h > 0.0 {
                            self.native_material_regions.push(NativeMaterialRegion {
                                rect: world_clip,
                                radius: material.radii(),
                                sigma: material.backdrop_sigma,
                                color: material.color,
                                noise: material.noise,
                                opacity: material.opacity * fold,
                            });
                        }
                    } else if material.backdrop_sigma > BLUR_MIN_SIGMA
                        && matches!(target, PassTarget::Main)
                        && let Some(roi) = self.backdrop_roi(world_clip, material.backdrop_sigma)
                    {
                        let capture = self.join_or_open_backdrop(roi, material.backdrop_sigma);
                        let index = self.material_records.len();
                        self.material_records.push(MaterialRecord {
                            rect: material.rect,
                            radius: material.radii(),
                            op: material.color,
                            noise: material.noise,
                            opacity: material.opacity * fold,
                        });
                        let bounds =
                            crate::scene::bounds::Bounds::from_world(material.rect, clip, 0.0, 0.0);
                        self.scene
                            .ingest_material_composite(capture, index, ctx, bounds);
                    }
                    // The border/highlight ring is an ordinary analytic rrect
                    // (§18): a transparent fill with a stroke, so it batches with
                    // every other rrect on screen and needs no shader of its own.
                    if let Some(border) = material.border_rrect() {
                        let mut inst = border.to_instance();
                        if fold < 1.0 {
                            inst.border_color[3] *= fold;
                        }
                        let bounds =
                            crate::scene::bounds::Bounds::from_world(material.rect, clip, 0.0, 0.0);
                        self.scene.ingest_analytic_rrect(inst, ctx, bounds);
                    }
                }
                Primitive::Layer(layer) => {
                    // The layer clip, intersected with the parent's effective
                    // clip, in world space.
                    let world_clip = match self.layer_stack.last() {
                        Some(parent) => parent.clip.intersect(layer.clip),
                        None => layer.clip,
                    };
                    // A backdrop is an explicit dependency on the content already
                    // recorded behind this layer (§17.1): join (or open) the
                    // capture group for it and record the composite *now*, before
                    // the layer's own content, so the blurred backdrop lands
                    // under it. Only layers drawing straight into the surface
                    // pass qualify — see `backdrop_roi`.
                    let mut shared_backdrop = false;
                    if layer.backdrop_sigma > BLUR_MIN_SIGMA
                        && matches!(target, PassTarget::Main)
                        && let Some(roi) = self.backdrop_roi(world_clip, layer.backdrop_sigma)
                    {
                        shared_backdrop = true;
                        let capture = self.join_or_open_backdrop(roi, layer.backdrop_sigma);
                        let dest = world_clip.intersect(self.surface_rect());
                        let bounds = crate::scene::bounds::Bounds::from_world(dest, None, 0.0, 0.0);
                        self.scene.ingest_backdrop_composite(
                            capture,
                            dest,
                            layer.opacity,
                            ctx,
                            bounds,
                        );
                        // The composite belongs to the parent target, not to the
                        // layer this primitive opens.
                        record_start = self.scene.paint_order.len();
                    }
                    // The layer's marker run is everything between this
                    // primitive and the layer's first real content: color
                    // effects (§17.3) and blend modes (§14.6), in any order.
                    // Both kinds are pure annotations on this layer, so the run
                    // is scanned once here and the two markers read out of it
                    // independently — a `Blend` between two `ColorEffect`s ends
                    // neither chain.
                    let markers = primitives[prim_index + 1..].iter().take_while(|p| {
                        matches!(p, Primitive::ColorEffect(_) | Primitive::Blend(_))
                    });
                    // The *last* blend marker in the run wins; a run with none
                    // composites `SrcOver`, the one mode the fixed-function
                    // stage expresses on the common pipeline.
                    let blend = markers
                        .clone()
                        .filter_map(|p| match p {
                            Primitive::Blend(mode) => Some(*mode),
                            _ => None,
                        })
                        .last()
                        .unwrap_or_default();
                    // How many effects the author wrote, before fusion — the
                    // denominator the planner compares the fused op count against
                    // to know whether fusing actually removed anything (§3161).
                    let color_effects = markers
                        .clone()
                        .filter(|p| matches!(p, Primitive::ColorEffect(_)))
                        .count();
                    // Fusing the color chain here — straight off the stream,
                    // into the shared arena — is what makes a run of mergeable
                    // effects one op instead of one pass each (§17.3).
                    let color_start = self.color_ops.len() as u32;
                    fuse(
                        markers.filter_map(|p| match p {
                            Primitive::ColorEffect(effect) => Some(*effect),
                            _ => None,
                        }),
                        &mut self.color_ops,
                    );
                    let color_len = self.color_ops.len() as u32 - color_start;
                    // Every reason this layer might exist, and every rung of the
                    // elimination ladder that can retire one (§3145). The subtree
                    // scan that proves the children disjoint is the one part of
                    // this that is not free, so it runs only for the layer that
                    // could use the answer — a translucent one.
                    let facts = if layer.opacity < 1.0 {
                        scan_layer_subtree(primitives, prim_index)
                    } else {
                        SubtreeFacts {
                            overlap: ChildOverlap::Unknown,
                            analytic_shadow: false,
                        }
                    };
                    let plan = plan_layer(&LayerRequest {
                        opacity: layer.opacity,
                        overlap: facts.overlap,
                        blurs_content: layer.blur_sigma > BLUR_MIN_SIGMA,
                        // A backdrop request that could not be captured (nested, or
                        // off the surface pass) raises nothing: there is no filter
                        // to eliminate because there is no filter.
                        filters_backdrop: shared_backdrop,
                        advanced_blend: blend != Blend::SrcOver,
                        color_effects: color_effects as u32,
                        fused_color_ops: color_len,
                        analytic_shadow: facts.analytic_shadow,
                        shared_backdrop,
                        ..LayerRequest::default()
                    });
                    self.layers_planned += 1;
                    if !plan.eliminated.is_empty() {
                        self.layers_eliminated += 1;
                    }
                    if plan.folds_opacity() {
                        self.opacity_folds += 1;
                    }
                    // The factor the planner retired the group opacity with, times
                    // whatever an ancestor already folded.
                    let child_fold = fold * plan.fold_opacity;
                    self.layer_plans.push(plan);
                    if !plan.needs_offscreen() {
                        // Nothing survived the ladder: whatever this layer asked
                        // for is either the identity or was pushed into its
                        // children, so the offscreen texture would be rendered only
                        // to be composited back unchanged. A plain in-pass scissor
                        // clip instead, inheriting the parent's target and origin.
                        self.layer_stack.push(LayerEntry {
                            clip: world_clip,
                            target,
                            origin,
                            content_union: Rect::ZERO,
                            paint_order_start: self.scene.paint_order.len(),
                            blur_sigma: 0.0,
                            color_start,
                            color_len: 0,
                            fold_opacity: child_fold,
                        });
                    } else {
                        // Translucent or blurred: open an offscreen pass. Its
                        // geometry is translated so the layer's top-left maps to
                        // the texture's (0, 0). The tight content ROI is not known
                        // until the subtree is walked, so the texture is claimed
                        // and the pass sized/composited at LayerEnd; the origin
                        // recorded here is provisional (the clip top-left) and
                        // repatched to the ROI top-left then. A blur forces the
                        // offscreen path even at full opacity (§16.2, E1.2), and so
                        // does a color chain that computes anything (§17.3, E2.2)
                        // and a blend the fixed-function stage cannot express
                        // (§14.6, E2.3).
                        let pass_origin = [world_clip.x, world_clip.y];
                        let paint_order_start = self.scene.paint_order.len();
                        // An advanced blend needs to read the destination. The
                        // bounded snapshot it reads is the same capture pass a
                        // backdrop blur uses, taken at sigma 0 — so the two share
                        // one mechanism, and `capture_sigma == 0` is what keeps a
                        // blend capture from joining a frosted-glass group whose
                        // pixels are blurred. Only layers drawing straight into
                        // the surface pass can capture (see `backdrop_roi`); a
                        // nested one still isolates, but composites `SrcOver`.
                        let mut isolate = None;
                        if blend != Blend::SrcOver
                            && matches!(target, PassTarget::Main)
                            && let Some(roi) = self.backdrop_roi(world_clip, 0.0)
                        {
                            let capture = self.join_or_open_backdrop(roi, 0.0);
                            isolate = Some((blend, capture));
                            self.blend_isolations += 1;
                        }
                        let idx = self.open_offscreen(world_clip, layer.opacity, isolate);
                        self.layer_stack.push(LayerEntry {
                            clip: world_clip,
                            target: PassTarget::Offscreen(idx),
                            origin: pass_origin,
                            content_union: Rect::ZERO,
                            paint_order_start,
                            blur_sigma: layer.blur_sigma,
                            color_start,
                            color_len,
                            // An isolated layer pays its own opacity on the
                            // composite draw, so there is nothing to push down;
                            // `child_fold` is what an ancestor folded, which is
                            // `1.0` whenever a layer is reached at all.
                            fold_opacity: child_fold,
                        });
                    }
                }
                // Already consumed: the `Primitive::Layer` arm reads the whole run
                // of markers that follows it and fuses them in one pass. Walking
                // over them again here is the no-op that keeps the chain a pure
                // annotation on the layer — and makes a marker anywhere else in
                // the stream harmless rather than an error (§17.3, §14.6).
                Primitive::ColorEffect(_) | Primitive::Blend(_) => {}
                Primitive::LayerEnd => {
                    if let Some(entry) = self.layer_stack.pop()
                        && let PassTarget::Offscreen(idx) = entry.target
                    {
                        // Size the pass to its tight content ROI, claim the
                        // texture, and repatch its children's origin to the ROI
                        // top-left — all deferred to here because the content
                        // union isn't known until the subtree is walked (§16.2).
                        let surface = self.surface_rect();
                        self.finalize_offscreen(idx, &entry, surface);
                        // Composite the finished offscreen texture back into the
                        // parent target as a textured quad at the ROI world rect,
                        // tinted by the layer opacity.
                        self.close_offscreen(idx);
                    }
                }
            }

            // Fold every paint bound this primitive recorded into the innermost
            // offscreen layer's running content union (world space), the source
            // for its tight ROI at `finalize_offscreen`. Only the innermost
            // offscreen accumulates; the composite a nested LayerEnd records
            // carries the inner pass's own paint bounds and rolls up here.
            if let Some(top) = self.layer_stack.last()
                && let PassTarget::Offscreen(_) = top.target
            {
                let mut union = top.content_union;
                for entry in &self.scene.paint_order[record_start..] {
                    union = union.union(entry.bounds.paint);
                }
                if let Some(top) = self.layer_stack.last_mut() {
                    top.content_union = union;
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

        // Realize this frame's backdrop captures (§17.1, §17.2) ahead of the
        // surface pass that consumes them. They can only be recorded here: a
        // group's ROI is not final until the walk ends, since a later layer can
        // still join it and grow the union.
        self.realize_backdrop_captures();

        // Close the graph with the surface pass. It reads every layer's final
        // sampled target (what `close_offscreen` composites), and it can only be
        // recorded here: during the walk the surface node does not exist yet, and
        // the graph's read arena requires a node's reads to be contiguous.
        let surface = self.graph.open(PassWork::Surface, None);
        for i in 0..self.offscreen_passes.len() {
            if let Some(sample) = self.offscreen_passes[i].sample {
                self.graph.read(surface, sample);
            }
        }
        for i in 0..self.backdrop_captures.len() {
            if let Some(sample) = self.backdrop_captures[i].sample {
                self.graph.read(surface, sample);
            }
        }

        // Compile the frame's pass plan — validate, cull, merge — or reuse the
        // cached one when the topology is unchanged (§16.1).
        self.graph.compile();

        // Map this frame's virtual transient targets onto physical textures
        // (§16.4). Each was declared during the walk at the slot of the pass that
        // writes it; the graph's recorded reads are what close their lifetimes, so
        // it drives the interval analysis. `assign` then walks the virtuals in
        // write order and hands each the first pooled texture of a compatible key
        // whose previous tenant's interval has ended, minting a texture only when
        // none is free. Both must run before lowering, since the composites
        // resolve their sampled bind groups from the assignment.
        self.graph.apply_lifetimes(&mut self.transient);
        self.transient
            .assign(backend, self.sampler, self.graph.surface_slot());

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
                .analytic_shadow_pool
                .sync(backend, &self.analytic_shadow_scratch)
            + self.blur_pool.sync(backend, &self.blur_scratch)
            + self
                .color_transform_pool
                .sync(backend, &self.color_transform_scratch)
            + self
                .advanced_blend_pool
                .sync(backend, &self.advanced_blend_scratch)
            + self.material_pool.sync(backend, &self.material_scratch)
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
            + self.analytic_shadow_pool.last_upload_bytes()
            + self.blur_pool.last_upload_bytes()
            + self.color_transform_pool.last_upload_bytes()
            + self.advanced_blend_pool.last_upload_bytes()
            + self.material_pool.last_upload_bytes()
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
        self.analytic_shadow_scratch.clear();
        self.material_scratch.clear();
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
            // Cull an offscreen child whose world paint bounds fall fully outside
            // its pass's tight ROI — only possible when the clip excluded it, so
            // its scissor would draw nothing (§16.2). The composite has no
            // offscreen target, so it is never culled here.
            if let PassTarget::Offscreen(idx) = target {
                let paint = entry.bounds.paint;
                // A zero-area paint bound means "bounds not computed" (path /
                // mesh / glyph carry a default), not "outside the ROI"; only
                // cull a primitive that has real bounds and misses the ROI.
                if paint.w > 0.0 && paint.h > 0.0 {
                    let roi = self.offscreen_passes[idx].rect;
                    let hit = paint.intersect(roi);
                    if hit.w <= 0.0 || hit.h <= 0.0 {
                        self.culled_this_frame += 1;
                        continue;
                    }
                }
            }
            self.lower_entry(backend, entry, clip, target, origin);
        }

        // Re-render each backdrop capture's under-content into its own pass
        // (§17.1). Appended *after* the main walk so every main-walk instance
        // keeps the cursor it already had — a capture's duplicated instances land
        // at the tail of the same family scratch, and `encode` selects a pass's
        // draws by segment target, not by scratch order. A shared capture is
        // walked once for the whole group, which is what makes N frosted panels
        // over one background cost one re-render, not N (§17.2).
        for i in 0..self.backdrop_captures.len() {
            let roi = self.backdrop_captures[i].roi;
            let target = PassTarget::Capture(i);
            let origin = [roi.x, roi.y];
            let under = self.backdrop_captures[i].under;
            for entry in &record[..under] {
                // Only content of the capture's own target is behind it; an
                // offscreen layer's children reach the surface through their
                // pass's composite, which is itself a `record` entry here.
                if entry.context.offscreen.is_some() {
                    continue;
                }
                let paint = entry.bounds.paint;
                // A zero-area paint bound means "bounds not computed", so it is
                // conservatively kept; a real bound outside the ROI contributes
                // nothing to the capture and is dropped. This is not a cull of
                // the frame's visible work (the primitive still draws into its
                // own target), so it does not touch `culled_this_frame`.
                if paint.w > 0.0 && paint.h > 0.0 {
                    let hit = paint.intersect(roi);
                    if hit.w <= 0.0 || hit.h <= 0.0 {
                        continue;
                    }
                }
                // Rebase the scissor from the entry's own target space into the
                // capture's: both are world-space offsets, so the shift is the
                // difference of the two origins. An unclipped entry stays
                // unclipped.
                let clip = entry.context.clip.map(|c| Rect {
                    x: c.x + entry.context.origin[0] - origin[0],
                    y: c.y + entry.context.origin[1] - origin[1],
                    w: c.w,
                    h: c.h,
                });
                self.lower_entry(backend, entry, clip, target, origin);
            }
        }

        self.scene.paint_order = record;
    }

    /// Lower one paint-order entry into the instance scratch of its family and
    /// the covering segment, under an explicit lowering context.
    ///
    /// Split out of [`lower_from_scene`](Self::lower_from_scene) because a
    /// primitive is lowered once per pass that draws it: once for its own target,
    /// and again for each backdrop capture whose ROI it falls behind (§17.1). The
    /// context is passed in rather than read off the entry so the capture walk can
    /// retarget and re-origin it without touching the retained record.
    fn lower_entry<B: GpuBackend>(
        &mut self,
        backend: &mut B,
        entry: &PaintEntry,
        clip: Option<Rect>,
        target: PassTarget,
        origin: [f32; 2],
    ) {
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
            StoreRef::AnalyticShadow(id) => {
                let mut inst = self
                    .scene
                    .analytic_shadows
                    .get(id)
                    .expect("analytic-shadow slot")
                    .instance;
                inst.rect_pos[0] -= origin[0];
                inst.rect_pos[1] -= origin[1];
                let start = self.analytic_shadow_scratch.len() as u32;
                self.analytic_shadow_scratch.push(inst);
                self.merge_or_push(Segment {
                    kind: SegmentKind::AnalyticShadow,
                    start,
                    count: 1,
                    clip,
                    target,
                });
            }
            StoreRef::Composite { mut instance, pass } => {
                // A composite draws into the parent target (main / unclipped /
                // zero-origin) sampling offscreen pass `pass`; its bind group
                // is the pooled texture the planner assigned to that pass's
                // sampled target (the base, or the last blur rung).
                let sample = self.offscreen_passes[pass]
                    .sample
                    .expect("offscreen pass finalized before its composite lowers");
                let bind_group = self.transient.bind_group(sample);
                instance.rect_pos[0] -= origin[0];
                instance.rect_pos[1] -= origin[1];
                // An advanced blend cannot ride the fixed-function stage, so this
                // layer's composite becomes one `AdvancedBlend` draw: it samples
                // both the layer and the bounded destination snapshot its capture
                // holds, evaluates the blend per fragment, and writes the result
                // with `Replace` — the destination is already in the fragment, so
                // the blend state must not mix it in twice. One draw, two textures,
                // no pass added, and the common `SrcOver` pipeline is left exactly
                // as it was (§14.6, E2.3).
                if let Some((mode, capture)) = self.offscreen_passes[pass].blend {
                    let cap = &self.backdrop_captures[capture];
                    let (roi, cap_used) = (cap.roi, cap.used);
                    let snapshot = cap
                        .sample
                        .expect("backdrop capture is realized before a blend composite lowers");
                    // The capture ROI can be shared with other isolated layers and
                    // is padded to a pooled size class, so the destination UVs are
                    // derived from where this layer's rect sits inside that ROI —
                    // the same derivation a blurred backdrop composite uses.
                    let sampled = self.transient.used_extent(snapshot);
                    let phys = self.transient.phys_extent(snapshot);
                    let cover = [
                        sampled[0] as f32 / phys[0] as f32,
                        sampled[1] as f32 / phys[1] as f32,
                    ];
                    let (bw, bh) = (cap_used[0] as f32, cap_used[1] as f32);
                    let rect = self.offscreen_passes[pass].rect;
                    // The layer opacity rides the blend instance rather than a
                    // composite tint: the fragment folds it into the source before
                    // evaluating the blend, which is where compositing math wants
                    // it (a blend of a half-transparent layer is not a half-faded
                    // blend of an opaque one).
                    let opacity = self.offscreen_passes[pass].opacity;
                    let src_texture = self.transient.texture(sample);
                    let dst_texture = self.transient.texture(snapshot);
                    let bind_group = self.blend_bind_group_for(backend, src_texture, dst_texture);
                    let start = self.advanced_blend_scratch.len() as u32;
                    self.advanced_blend_scratch.push(AdvancedBlendInstance {
                        rect_pos: instance.rect_pos,
                        rect_size: instance.rect_size,
                        uv_pos: instance.uv_pos,
                        uv_size: instance.uv_size,
                        dst_uv_pos: [
                            (rect.x - roi.x) / bw * cover[0],
                            (rect.y - roi.y) / bh * cover[1],
                        ],
                        dst_uv_size: [rect.w / bw * cover[0], rect.h / bh * cover[1]],
                        mode: mode.mode(),
                        opacity,
                    });
                    self.merge_or_push(Segment {
                        kind: SegmentKind::AdvancedBlend { bind_group },
                        start,
                        count: 1,
                        clip,
                        target,
                    });
                    return;
                }
                // A layer whose color chain fused to a final op composites through
                // the color-transform pipeline instead of the plain image one: same
                // quad, same source texture, one extra matrix multiply in the
                // fragment shader — which is how an N-effect fusable chain costs
                // zero extra render-target passes (§17.3, E2.2). The op already
                // carries the layer opacity in its alpha row, so the composite's
                // tint has nothing left to say and is dropped.
                if let Some(op) = self.offscreen_passes[pass].color {
                    let start = self.color_transform_scratch.len() as u32;
                    self.color_transform_scratch.push(color_transform_instance(
                        op,
                        instance.rect_pos,
                        instance.rect_size,
                        instance.uv_pos,
                        instance.uv_size,
                    ));
                    self.merge_or_push(Segment {
                        kind: SegmentKind::ColorTransform { bind_group },
                        start,
                        count: 1,
                        clip,
                        target,
                    });
                    return;
                }
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
            StoreRef::BackdropComposite {
                capture,
                rect,
                opacity,
            } => {
                // The blurred backdrop, composited under the layer that asked
                // for it. Unlike a layer composite (whose texture *is* the
                // layer's ROI), a backdrop samples a sub-rect of a capture that
                // may be shared with other layers and padded by the blur reach,
                // so the UVs are derived here from the destination rect's
                // position inside the capture ROI.
                let cap = &self.backdrop_captures[capture];
                let (roi, used) = (cap.roi, cap.used);
                let sample = cap
                    .sample
                    .expect("backdrop capture is realized before its composite lowers");
                let bind_group = self.transient.bind_group(sample);
                // The sampled target's written sub-rect within its physical
                // (size-class rounded) extent: the capture ROI covers exactly
                // this fraction of the texture.
                let sampled = self.transient.used_extent(sample);
                let phys = self.transient.phys_extent(sample);
                let cover = [
                    sampled[0] as f32 / phys[0] as f32,
                    sampled[1] as f32 / phys[1] as f32,
                ];
                let (bw, bh) = (used[0] as f32, used[1] as f32);
                let instance = ImageInstance {
                    rect_pos: [rect.x - origin[0], rect.y - origin[1]],
                    rect_size: [rect.w, rect.h],
                    uv_pos: [
                        (rect.x - roi.x) / bw * cover[0],
                        (rect.y - roi.y) / bh * cover[1],
                    ],
                    uv_size: [rect.w / bw * cover[0], rect.h / bh * cover[1]],
                    color: [1.0, 1.0, 1.0, opacity],
                };
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
            StoreRef::MaterialComposite { capture, material } => {
                // A frosted surface: the same shared capture a backdrop layer
                // samples, but composited through the material pipeline, which
                // folds the color transform, the grain, the rounded mask, and the
                // opacity into that one draw (§18). No extra pass, no extra
                // target, and no capture of its own.
                let record = self.material_records[material];
                let cap = &self.backdrop_captures[capture];
                let (roi, used) = (cap.roi, cap.used);
                let sample = cap
                    .sample
                    .expect("backdrop capture is realized before its material composite lowers");
                let bind_group = self.transient.bind_group(sample);
                let sampled = self.transient.used_extent(sample);
                let phys = self.transient.phys_extent(sample);
                let cover = [
                    sampled[0] as f32 / phys[0] as f32,
                    sampled[1] as f32 / phys[1] as f32,
                ];
                let (bw, bh) = (used[0] as f32, used[1] as f32);
                let rect = record.rect;
                let instance = material_instance(
                    record,
                    [rect.x - origin[0], rect.y - origin[1]],
                    [rect.w, rect.h],
                    [
                        (rect.x - roi.x) / bw * cover[0],
                        (rect.y - roi.y) / bh * cover[1],
                    ],
                    [rect.w / bw * cover[0], rect.h / bh * cover[1]],
                );
                let start = self.material_scratch.len() as u32;
                self.material_scratch.push(instance);
                self.merge_or_push(Segment {
                    kind: SegmentKind::Material { bind_group },
                    start,
                    count: 1,
                    clip,
                    target,
                });
            }
        }
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
    /// this frame's translucent-layer passes. `render_passes`,
    /// `render_pass_merges`, `culled_render_passes`, and `render_graph_compiles`
    /// come from the compiled pass plan (§16.1). `shader_pipeline_creations` is the
    /// fixed prewarm count (§7.1: no runtime compile). The counters with no
    /// source in this layer stay 0 with their meaning fixed (see [`FrameStats`]).
    /// Every layer plan this frame's walk produced, in stream order (§3145, §62).
    /// A plan names the reasons the layer was requested for, the ladder rungs that
    /// fired, and the reasons that survived — so an inspector or a test can show
    /// *why* a group cost an offscreen pass, reading exactly the values the
    /// renderer decided on.
    pub fn layer_plans(&self) -> &[LayerPlan] {
        &self.layer_plans
    }

    /// One entry per backdrop capture this frame, in capture order (§3202, §62):
    /// the ROI its dependency is scoped to, that dependency's revision, and whether
    /// it moved since the previous frame. This is how "only that effect's ROI is
    /// dirty" is observed — two frosted panels over unrelated content report
    /// independent `dirty` flags.
    pub fn backdrop_dependencies(&self) -> &[BackdropDependency] {
        &self.backdrop_dependencies
    }

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

        // Bytes each kind of pass addresses: the pooled target's physical extent
        // (its size class), summed per pass. Aliasing means these sums are not an
        // occupancy figure — `transient` reports that separately.
        let transient_target_bytes = self
            .offscreen_passes
            .iter()
            .map(|p| (p.viewport[0] as usize) * (p.viewport[1] as usize) * 4)
            .sum();
        let blur_target_bytes = self
            .blur_passes
            .iter()
            .map(|p| (p.viewport[0] as usize) * (p.viewport[1] as usize) * 4)
            .sum();
        let transient = self.transient.stats();
        let graph = self.graph.stats();

        FrameStats {
            // Each blur rung and each non-final color op is one full-target draw
            // beyond the segment-derived draws, so both add to the draw and
            // instance totals. The *final* color op is not counted here: it rides
            // the composite, which is already a segment.
            draw_calls: self.segments.len() + self.blur_passes.len() + self.color_passes.len(),
            instances: self
                .segments
                .iter()
                .map(|s| s.count as usize)
                .sum::<usize>()
                + self.blur_passes.len()
                + self.color_passes.len(),
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
            blur_passes: self.blur_passes.len() as u32,
            blur_target_bytes,
            color_effect_ops: self.color_ops.len() as u32,
            color_transform_passes: self.color_passes.len() as u32,
            backdrop_captures: self.backdrop_captures.len() as u32,
            backdrop_capture_pixels: self
                .backdrop_captures
                .iter()
                .map(|c| (c.used[0] as usize) * (c.used[1] as usize))
                .sum(),
            blend_isolations: self.blend_isolations,
            material_composites: self.material_records.len() as u32,
            native_material_regions: self.native_material_regions.len() as u32,
            layers_planned: self.layers_planned,
            layers_eliminated: self.layers_eliminated,
            opacity_folds: self.opacity_folds,
            backdrop_dirty_rois: self.backdrop_dirty_rois,
            transient_peak_bytes: transient.peak_bytes,
            transient_pool_bytes: transient.pool_bytes,
            transient_target_allocations: transient.allocations,
            transient_targets: transient.targets,
            render_passes: graph.passes as usize,
            render_pass_merges: graph.merges,
            culled_render_passes: graph.culled,
            render_graph_compiles: graph.compiles,
            shader_pipeline_creations: SHADER_PIPELINE_PREWARM_COUNT,
            // Masks rasterized into the page this frame (§14.4): cold builds,
            // re-rasters after a key change, and re-blits after a repack.
            clip_mask_builds: self.mask_builds_this_frame,
            // Offscreen children the tight ROI excluded this frame (§16.2).
            culled_primitives: self.culled_this_frame,
            // Counters no stage below D0 lights up yet; meaning fixed, value 0.
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

    /// The AnalyticShadow pipeline handle, for batch introspection.
    pub(crate) fn analytic_shadow_pipeline_id(&self) -> PipelineId {
        self.analytic_shadow_pipeline
    }

    /// The ColorTransform pipeline handle, for batch introspection.
    pub(crate) fn color_transform_pipeline_id(&self) -> PipelineId {
        self.color_transform_pipeline
    }

    /// The AdvancedBlend pipeline handle, for batch introspection.
    pub(crate) fn advanced_blend_pipeline_id(&self) -> PipelineId {
        self.advanced_blend_pipeline
    }

    /// The MaterialComposite pipeline handle, for batch introspection.
    pub(crate) fn material_pipeline_id(&self) -> PipelineId {
        self.material_pipeline
    }

    /// The regions this frame reserved for the platform's own material (§19), in
    /// paint order, valid until the next [`upload`](Self::upload).
    ///
    /// The platform layer reads this to place, size and round its native material
    /// views. Each region carries geometry plus the *generic* parameters the surface
    /// authored — the mapping to a platform material name lives above this crate, so
    /// no platform-private material API reaches the render IR.
    pub fn native_material_regions(&self) -> &[NativeMaterialRegion] {
        &self.native_material_regions
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

    /// The group opacity folded into whatever is drawn right now (§3161 rung 1):
    /// the innermost layer's accumulated fold factor, or `1.0` at the top level.
    /// Kept out of [`active`](Renderer::active) because every caller of that
    /// function wants the clip/target triple and only the drawable arms multiply
    /// this in.
    fn active_fold(&self) -> f32 {
        match self.layer_stack.last() {
            Some(entry) => entry.fold_opacity,
            None => 1.0,
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

    /// The surface pass's world-space rect in physical pixels: the clamp every
    /// ROI is intersected with, so no capture or layer target is ever larger than
    /// the window.
    fn surface_rect(&self) -> Rect {
        Rect {
            x: 0.0,
            y: 0.0,
            w: self.surface_size[0],
            h: self.surface_size[1],
        }
    }

    /// The capture ROI a backdrop layer over `world_clip` requires: its clip
    /// inflated by the blur's reach (`BLUR_RADIUS_SIGMAS * sigma` — the widest tap
    /// offset the ladder can read, so the visible sub-rect is never contaminated
    /// by the target's transparent border) intersected with the surface. Never the
    /// whole screen (§17.1): "capture only the required ROI". `None` when the
    /// padded clip misses the surface entirely, in which case no capture is made.
    fn backdrop_roi(&self, world_clip: Rect, sigma: f32) -> Option<Rect> {
        let reach = (BLUR_RADIUS_SIGMAS * sigma).ceil();
        let padded = Rect {
            x: world_clip.x - reach,
            y: world_clip.y - reach,
            w: world_clip.w + 2.0 * reach,
            h: world_clip.h + 2.0 * reach,
        };
        let roi = padded.intersect(self.surface_rect());
        (roi.w > 0.0 && roi.h > 0.0).then_some(roi)
    }

    /// Join `roi` into the most recent backdrop capture group when the two are
    /// compatible, or open a new group; returns the capture index (§17.2).
    ///
    /// This is what keeps "N frosted panels = N full-screen captures + N blurs"
    /// from being the default: a row of panels over the same background shares one
    /// capture, one blur ladder, and gets one composite each. Joining requires all
    /// three of
    ///
    /// - the same sigma (a shared ladder can only realize one sigma);
    /// - nothing drawn between the two panels that a shared capture would sample
    ///   but the individual captures would not (see
    ///   [`backdrop_group_blocked`](Self::backdrop_group_blocked)) — a widget
    ///   painted *over* the first panel must not reappear under the second;
    /// - a union no more than [`BACKDROP_UNION_SLACK`]× the area the members
    ///   actually need, so two panels at opposite corners of the window do not
    ///   silently promote themselves to a full-screen capture.
    ///
    /// Only the last group is considered: paint order is the sharing order, and
    /// reaching further back would have to re-check every intervening draw against
    /// every older group for the same cost as a split.
    fn join_or_open_backdrop(&mut self, roi: Rect, sigma: f32) -> usize {
        let area = roi.w * roi.h;
        if let Some(last) = self.backdrop_captures.len().checked_sub(1) {
            let cap = &self.backdrop_captures[last];
            let (cap_sigma, cap_roi, cap_covered, cap_under) =
                (cap.sigma, cap.roi, cap.covered, cap.under);
            let union = cap_roi.union(roi);
            if cap_sigma == sigma
                && union.w * union.h <= (cap_covered + area) * BACKDROP_UNION_SLACK
                && !self.backdrop_group_blocked(cap_under, roi)
            {
                let cap = &mut self.backdrop_captures[last];
                cap.roi = union;
                cap.covered += area;
                return last;
            }
        }
        let idx = self.backdrop_captures.len();
        self.backdrop_captures.push(BackdropCapture {
            under: self.scene.paint_order.len(),
            roi,
            covered: area,
            sigma,
            base: None,
            sample: None,
            viewport: [0.0, 0.0],
            used: [0, 0],
        });
        idx
    }

    /// Whether anything recorded since paint-order index `from` would show through
    /// a capture shared over `roi` that must not: a draw between two candidate
    /// members that overlaps the joining member's ROI is *above* the first
    /// member's backdrop but would land *below* the second's, so the group has to
    /// split.
    ///
    /// Entries inside an offscreen layer are not blockers: their pixels reach the
    /// surface through the layer's composite, which is itself a paint-order entry
    /// and is tested here. An unknown (zero-area) paint bound counts as blocking —
    /// sharing is an optimization and a split is always correct.
    fn backdrop_group_blocked(&self, from: usize, roi: Rect) -> bool {
        self.scene.paint_order[from..].iter().any(|e| {
            if e.context.offscreen.is_some() {
                return false;
            }
            let paint = e.bounds.paint;
            if paint.w <= 0.0 || paint.h <= 0.0 {
                return true;
            }
            let hit = paint.intersect(roi);
            hit.w > 0.0 && hit.h > 0.0
        })
    }

    /// Realize every backdrop capture this frame opened, in creation order, just
    /// before the surface pass consumes them (§17.1).
    ///
    /// Per capture: declare a transient target of exactly its ROI, open a graph
    /// node for it, register a read edge to every *producer* of the content the
    /// capture re-renders, and blur the result through the shared ladder. The read
    /// edges are the whole point — a backdrop depends on already-drawn content
    /// through the render graph, never by reading the framebuffer it is itself
    /// being composited into, whose contents at that moment are undefined.
    ///
    /// Only producers that are themselves targets can be read edges (a layer's
    /// composite reads an offscreen texture; an earlier backdrop composite reads
    /// that capture's blurred texture). Plain geometry needs no edge: the capture
    /// pass re-renders it, so its input is the same vertex/instance data the
    /// surface pass draws, not a texture the GPU has to finish writing first.
    ///
    /// This cannot run during the walk: a group's ROI is not final until the walk
    /// ends, because a later layer can still join the group and grow its union.
    fn realize_backdrop_captures(&mut self) {
        if self.backdrop_captures.is_empty() {
            self.backdrop_dependencies.clear();
            return;
        }
        // Detach the paint order so the scan can borrow it while the graph, the
        // target pool, and the captures are mutated. Restored below; the vector's
        // allocation round-trips untouched.
        let record = std::mem::take(&mut self.scene.paint_order);
        let mut reads = std::mem::take(&mut self.backdrop_reads);
        for i in 0..self.backdrop_captures.len() {
            let (roi, under) = {
                let cap = &self.backdrop_captures[i];
                (cap.roi, cap.under)
            };
            let width = (roi.w.ceil() as u32).max(1);
            let height = (roi.h.ceil() as u32).max(1);

            let slot = self.graph.next_slot();
            let base = self.transient.declare(
                TargetDesc {
                    width,
                    height,
                    format: self.intermediate_format,
                    usage: TargetUsage::COLOR_ATTACHMENT,
                    samples: 1,
                    label: "backdrop-capture",
                },
                slot,
            );
            let node = self
                .graph
                .open(PassWork::BackdropCapture(i as u32), Some(base));

            // The graph does not deduplicate reads, and one offscreen layer can
            // contribute many entries to a capture, so fold the sources here.
            reads.clear();
            // The ROI-scoped dependency revision (§3202), accumulated over exactly
            // the entries this capture samples: the newest content stamp among
            // them, plus how many there were. The count is what catches a removal
            // at the tail, where no surviving slot's stamp moves.
            let mut dependency = 0u64;
            let mut members = 0u64;
            for (slot, entry) in record[..under].iter().enumerate() {
                if entry.context.offscreen.is_some() {
                    continue;
                }
                let paint = entry.bounds.paint;
                if paint.w > 0.0 && paint.h > 0.0 {
                    let hit = paint.intersect(roi);
                    if hit.w <= 0.0 || hit.h <= 0.0 {
                        continue;
                    }
                }
                dependency = dependency.max(self.scene.content_stamp(slot));
                members += 1;
                let source = match entry.store {
                    StoreRef::Composite { pass, .. } => self.offscreen_passes[pass].sample,
                    // Always an earlier capture: a group's members are recorded
                    // after it opens, so its index is below this one's.
                    StoreRef::BackdropComposite { capture, .. }
                    | StoreRef::MaterialComposite { capture, .. } => {
                        self.backdrop_captures[capture].sample
                    }
                    _ => None,
                };
                if let Some(source) = source
                    && !reads.contains(&source)
                {
                    reads.push(source);
                    self.graph.read(node, source);
                }
            }

            let phys = self.transient.phys_extent(base);
            let sigma = {
                let cap = &mut self.backdrop_captures[i];
                cap.base = Some(base);
                cap.viewport = [phys[0] as f32, phys[1] as f32];
                cap.used = [width, height];
                cap.sigma
            };
            let sample = self.build_blur_chain(base, [width, height], sigma);
            self.backdrop_captures[i].sample = Some(sample);

            // Diff against this capture slot's value from the previous frame. A
            // slot that did not exist then is new work, so it counts as dirty; the
            // vector is rewritten in place, in capture order, so slot `i` still
            // holds last frame's value when it is read here.
            let revision = dependency.wrapping_add(members);
            let previous = self.backdrop_dependencies.get(i).map(|d| d.revision);
            let dirty = previous != Some(revision);
            if dirty {
                self.backdrop_dirty_rois += 1;
            }
            let entry = BackdropDependency {
                roi,
                revision,
                dirty,
            };
            match self.backdrop_dependencies.get_mut(i) {
                Some(slot) => *slot = entry,
                None => self.backdrop_dependencies.push(entry),
            }
        }
        self.backdrop_dependencies
            .truncate(self.backdrop_captures.len());
        self.backdrop_reads = reads;
        self.scene.paint_order = record;
    }

    /// Open an offscreen pass for a translucent layer whose world-space clip is
    /// `world_clip`, returning its index in `offscreen_passes`. No GPU work and no
    /// target declaration here: the ROI is only known at
    /// [`Renderer::finalize_offscreen`], so the target is never larger than the
    /// visible content (§16.2). `rect` holds the clip provisionally until then.
    ///
    /// `blend` is `Some((mode, capture))` only for a layer isolated by an advanced
    /// blend, naming the mode and the backdrop capture holding the bounded
    /// destination snapshot its composite reads (§14.6).
    fn open_offscreen(
        &mut self,
        world_clip: Rect,
        opacity: f32,
        blend: Option<(Blend, usize)>,
    ) -> usize {
        let idx = self.offscreen_passes.len();
        self.offscreen_passes.push(OffscreenPass {
            base: None,
            sample: None,
            viewport: [0.0, 0.0],
            used: [0, 0],
            rect: world_clip,
            opacity,
            color: None,
            blend,
        });
        idx
    }

    /// Size the offscreen pass at `idx` to its tight content ROI, declare the
    /// transient target it writes, and repatch every recorded child of the pass to
    /// the ROI top-left origin (§16.2). Called from `LayerEnd` before
    /// [`close_offscreen`](Self::close_offscreen).
    ///
    /// The ROI is `content_union ∩ clip ∩ surface`: never the full clip (a small
    /// panel in a huge clip stays small) nor the full surface. An empty subtree
    /// (or a clip that excludes all content) clamps to a 1×1 target; the composite
    /// then draws a degenerate quad.
    ///
    /// No GPU resource is created here. The pass takes the next graph slot and
    /// declares a *virtual* target of the ROI's size class against the frame-local
    /// pool (§16.4); the concrete texture is picked once the whole frame's
    /// lifetimes are known, in [`TransientTargets::assign`]. The pass viewport is
    /// therefore the pooled extent (what the shaders map pixels to NDC against),
    /// and `used` the tight ROI extent it actually writes at the target top-left.
    ///
    /// All children of one pass share a single origin (each subtracts the same
    /// value at [`lower_from_scene`](Self::lower_from_scene)), so shrinking the
    /// ROI top-left means rewriting that one origin — and the clip, recomputed
    /// from the world clip against the new origin — on every recorded child.
    fn finalize_offscreen(&mut self, idx: usize, entry: &LayerEntry, surface: Rect) {
        let roi = entry.content_union.intersect(entry.clip).intersect(surface);
        let width = (roi.w.ceil() as u32).max(1);
        let height = (roi.h.ceil() as u32).max(1);

        let slot = self.graph.next_slot();
        let base = self.transient.declare(
            TargetDesc {
                width,
                height,
                format: self.intermediate_format,
                usage: TargetUsage::COLOR_ATTACHMENT,
                samples: 1,
                label: "offscreen-layer",
            },
            slot,
        );
        self.graph.open(PassWork::Offscreen(idx as u32), Some(base));
        let phys = self.transient.phys_extent(base);
        let pass = &mut self.offscreen_passes[idx];
        pass.base = Some(base);
        pass.sample = Some(base);
        pass.viewport = [phys[0] as f32, phys[1] as f32];
        pass.used = [width, height];
        pass.rect = roi;

        // Repatch this pass's recorded children to the ROI top-left origin, and
        // recompute each texture-local clip from the world clip (preserving an
        // absent clip). Only entries this pass owns are touched.
        let new_origin = [roi.x, roi.y];
        let local_clip = Rect {
            x: entry.clip.x - new_origin[0],
            y: entry.clip.y - new_origin[1],
            w: entry.clip.w,
            h: entry.clip.h,
        };
        for pe in &mut self.scene.paint_order[entry.paint_order_start..] {
            if pe.context.offscreen == Some(idx) {
                pe.context.origin = new_origin;
                pe.context.clip = pe.context.clip.map(|_| local_clip);
            }
        }

        // Insert the separable Gaussian ladder (§16.2, E1.2) between this pass's
        // render and its composite. The plan is a pure function of the sigma and
        // the ROI extent; a sub-pixel blur plans no steps and leaves the pass
        // sampling its base texture unchanged.
        self.build_blur(idx, entry.blur_sigma);

        // Then the layer's fused color ops (§17.3, E2.2), which read what the blur
        // ladder left. A chain that fused to one op adds no pass at all — that op
        // rides the composite draw.
        self.build_color_chain(idx, entry);

        debug_assert!(
            self.offscreen_passes[idx].sample.is_some(),
            "offscreen pass declares its base target before finalize returns"
        );
    }

    /// Realize the blur ladder for the offscreen pass at `idx`: plan the separable
    /// steps for `sigma` over the base ROI, declare one transient target per step,
    /// chain the passes base → step0 → step1 → …, append a [`BlurPass`] (with its
    /// [`BlurInstance`]) for each, and repoint the pass's composite target at the
    /// final blurred one (§16.2, E1.2).
    ///
    /// A no-op plan (sub-pixel sigma) leaves `offscreen_passes[idx].sample` on the
    /// base target, so the composite samples the unblurred content — the blur
    /// silently degrades to a plain offscreen layer.
    ///
    /// Each step's quad covers the *used* extent of its target at the target's
    /// top-left, and samples the *used* sub-rect of its source: pooled targets are
    /// size-class buckets, so used ≤ physical and the uv window has to be
    /// normalized against the physical source extent (as does the per-tap step
    /// `dir`, which the plan hands over as a unit axis).
    fn build_blur(&mut self, idx: usize, sigma: f32) {
        let (base, used) = {
            let pass = &self.offscreen_passes[idx];
            (
                pass.base
                    .expect("offscreen pass target declared before its blur ladder is built"),
                pass.used,
            )
        };
        // Composite samples the final step's target instead of the base (the
        // chain returns `base` unchanged when the plan is empty).
        self.offscreen_passes[idx].sample = Some(self.build_blur_chain(base, used, sigma));
    }

    /// Plan and record the separable Gaussian ladder for `sigma` over a target
    /// `base` whose written extent is `used`, returning the target the result
    /// should be sampled from — `base` itself when the plan is empty (a sub-pixel
    /// sigma). Shared by content blur ([`build_blur`](Self::build_blur)) and
    /// backdrop blur ([`realize_backdrop_captures`](Self::realize_backdrop_captures)):
    /// both blur one tight-ROI texture, so both share one ladder (§16.2, §17.2).
    fn build_blur_chain(&mut self, base: TargetId, used: [u32; 2], sigma: f32) -> TargetId {
        let plan = blur_plan(sigma, used[0], used[1]);
        if plan.is_empty() {
            return base;
        }

        // The source of step 0 is the base offscreen target; each later step reads
        // the target of the one before, and each takes the next graph slot, so the
        // graph sees a strictly ordered write-then-read chain and the pool can
        // never alias a step's source into its own target.
        let mut source = base;
        let mut source_used = used;
        for step in &plan.steps {
            let slot = self.graph.next_slot();
            let target = self.transient.declare(
                TargetDesc {
                    width: step.width,
                    height: step.height,
                    format: self.intermediate_format,
                    usage: TargetUsage::COLOR_ATTACHMENT,
                    samples: 1,
                    label: "blur-scratch",
                },
                slot,
            );
            let node = self
                .graph
                .open(PassWork::Blur(self.blur_passes.len() as u32), Some(target));
            self.graph.read(node, source);

            let src = self.transient.phys_extent(source);
            let dst = self.transient.phys_extent(target);
            let instance = self.blur_scratch.len() as u32;
            self.blur_scratch.push(BlurInstance {
                rect_pos: [0.0, 0.0],
                rect_size: [step.width as f32, step.height as f32],
                uv_pos: [0.0, 0.0],
                uv_size: [
                    source_used[0] as f32 / src[0] as f32,
                    source_used[1] as f32 / src[1] as f32,
                ],
                dir: [step.axis[0] / src[0] as f32, step.axis[1] / src[1] as f32],
                sigma: step.sigma,
                radius: step.radius,
            });
            self.blur_passes.push(BlurPass {
                source,
                viewport: [dst[0] as f32, dst[1] as f32],
                instance,
            });
            source = target;
            source_used = [step.width, step.height];
        }
        source
    }

    /// Realize the fused color-effect chain of the offscreen pass at `idx`: the
    /// ops `entry` claimed in [`Renderer::color_ops`], applied to whatever the blur
    /// ladder left the pass sampling (§17.3, E2.2).
    ///
    /// The last op is *not* given a pass: it is stored on the pass as
    /// [`OffscreenPass::color`], with the layer opacity folded into its alpha row,
    /// and the composite draw runs it while compositing. So a chain that fused into
    /// one op — every run of matrix-expressible effects, however long — costs zero
    /// extra render-target passes; only the ops a run could not absorb, one per
    /// non-expressible stage, become [`ColorPass`]es here.
    ///
    /// Each rung's quad covers the used extent of its own target and samples the
    /// used sub-rect of its source, for the same pooled-size-class reason the blur
    /// rungs do; the color math is per-texel, so no rung resizes.
    ///
    /// A layer isolated by an advanced blend is the one exception: its composite is
    /// an `AdvancedBlend` draw, which carries no color matrix, so *every* op gets a
    /// rung and the layer opacity rides the blend instance instead of the final
    /// op's alpha row. Such a layer pays one more pass than a `SrcOver` one — the
    /// honest price of a composite that already reads two textures (§14.6).
    fn build_color_chain(&mut self, idx: usize, entry: &LayerEntry) {
        if entry.color_len == 0 {
            return;
        }
        let (mut source, opacity, blended) = {
            let pass = &self.offscreen_passes[idx];
            (
                pass.sample
                    .expect("offscreen pass target declared before its color chain is built"),
                pass.opacity,
                pass.blend.is_some(),
            )
        };
        let mut used = self.transient.used_extent(source);
        let start = entry.color_start as usize;
        let last = start + entry.color_len as usize - 1;
        let rungs = if blended { last + 1 } else { last };
        for i in start..rungs {
            let op = self.color_ops[i];
            let slot = self.graph.next_slot();
            let target = self.transient.declare(
                TargetDesc {
                    width: used[0],
                    height: used[1],
                    format: self.intermediate_format,
                    usage: TargetUsage::COLOR_ATTACHMENT,
                    samples: 1,
                    label: "color-scratch",
                },
                slot,
            );
            let node = self.graph.open(
                PassWork::ColorTransform(self.color_passes.len() as u32),
                Some(target),
            );
            self.graph.read(node, source);

            let src = self.transient.phys_extent(source);
            let dst = self.transient.phys_extent(target);
            let instance = self.color_transform_scratch.len() as u32;
            self.color_transform_scratch.push(color_transform_instance(
                op,
                [0.0, 0.0],
                [used[0] as f32, used[1] as f32],
                [0.0, 0.0],
                [
                    used[0] as f32 / src[0] as f32,
                    used[1] as f32 / src[1] as f32,
                ],
            ));
            self.color_passes.push(ColorPass {
                source,
                viewport: [dst[0] as f32, dst[1] as f32],
                instance,
            });
            source = target;
            used = self.transient.used_extent(target);
        }
        let pass = &mut self.offscreen_passes[idx];
        pass.sample = Some(source);
        if !blended {
            // The layer opacity rides the final op's alpha row instead of the
            // composite's tint: one multiply in the shader either way, and it keeps
            // the composite a plain textured quad with no second color source.
            pass.color = Some(self.color_ops[last].with_opacity(opacity));
        }
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
    /// The pass wrote its ROI at the top-left of a size-class-bucketed pooled
    /// target, so the composite samples the `used / physical` sub-rect rather than
    /// the whole texture. That is the same texel mapping an exactly-sized target
    /// gives (`uv_size = 1` over a `used`-wide texture), so nothing about edge
    /// bleed changes: pixel centers stay inside `[0.5, used - 0.5]` texels.
    ///
    /// [`lower_from_scene`]: Self::lower_from_scene
    fn close_offscreen(&mut self, idx: usize) {
        let pass = &self.offscreen_passes[idx];
        let sample = pass
            .sample
            .expect("offscreen pass is finalized before it is closed");
        // The *sampled* target's own used extent, not the base ROI's: a large-sigma
        // ladder ends at a downsampled target, and the composite upsamples it back
        // over the layer rect exactly as it did with unbucketed targets.
        let used = self.transient.used_extent(sample);
        let phys = self.transient.phys_extent(sample);
        let composite = ImageInstance {
            rect_pos: [pass.rect.x, pass.rect.y],
            rect_size: [pass.rect.w, pass.rect.h],
            uv_pos: [0.0, 0.0],
            uv_size: [
                used[0] as f32 / phys[0] as f32,
                used[1] as f32 / phys[1] as f32,
            ],
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
    /// Emits one [`RenderPass`] per entry of the compiled plan the render graph
    /// produced in [`upload`](Self::upload), in plan order: every offscreen and
    /// blur pass in dependency order, then the surface pass. The graph validated
    /// that order (every source is written strictly before it is read, so an
    /// aliased target is never read after being reclaimed), merged the passes that
    /// share an attachment, and lowered each one's load op. A merged pass carries
    /// several works and encodes them back to back into the one attachment. Each
    /// offscreen pass uses its own pooled-extent viewport uniform; the surface pass
    /// uses `viewport`.
    fn encode<B: GpuBackend>(
        &mut self,
        backend: &mut B,
        frame: Frame,
        clear: [f32; 4],
        viewport: [f32; 2],
    ) {
        // Both scratch buffers are `Renderer`-owned and borrow-free
        // (`DrawCommand`/`RenderPass` carry no lifetime — uniforms are stored by
        // value). We take them out with `mem::take`, clear them (retaining their
        // backing allocations), refill, and put them back: 0 heap allocations on
        // a steady frame. Taking them out lets the fill loops borrow `&self`
        // (segments, graph, offscreen passes) without aliasing the buffers being
        // filled.
        let mut commands = std::mem::take(&mut self.commands);
        let mut passes = std::mem::take(&mut self.passes);
        commands.clear();
        passes.clear();

        for pass in self.graph.passes() {
            let works = self.graph.works(pass);
            // Every work in a merged pass writes the same attachment, hence shares
            // its pooled extent, so one viewport uniform covers the whole pass. Its
            // bytes are copied by value into each command.
            let vp = match works[0] {
                PassWork::Offscreen(i) => self.offscreen_passes[i as usize].viewport,
                PassWork::Blur(i) => self.blur_passes[i as usize].viewport,
                PassWork::ColorTransform(i) => self.color_passes[i as usize].viewport,
                PassWork::BackdropCapture(i) => self.backdrop_captures[i as usize].viewport,
                PassWork::Surface => viewport,
            };
            let uniforms = InlineUniforms::new(bytemuck_viewport(&vp));
            let first_command = commands.len() as u32;
            for work in works {
                match *work {
                    // A layer's own content: every segment routed to this pass.
                    PassWork::Offscreen(i) => {
                        for seg in self
                            .segments
                            .iter()
                            .filter(|seg| seg.target == PassTarget::Offscreen(i as usize))
                        {
                            commands.push(self.command_for(seg, uniforms, vp));
                        }
                    }
                    // One step of a separable blur ladder (§16.2, E1.2): a single
                    // full-target quad reading the previous step's target.
                    PassWork::Blur(i) => {
                        let blur = &self.blur_passes[i as usize];
                        commands.push(DrawCommand {
                            pipeline: self.blur_pipeline,
                            bind_group: Some(self.transient.bind_group(blur.source)),
                            geometry: Geometry::Generated { count: 1 },
                            instance_buffer: self
                                .blur_pool
                                .buffer()
                                .expect("blur pool buffer exists when a blur pass references it"),
                            instance_offset: blur.instance as usize * BLUR_STRIDE,
                            uniforms,
                            scissor: None,
                        });
                    }
                    // One op of a color chain that could not fuse all the way down
                    // (§17.3, E2.2): a single full-target quad applying one matrix
                    // (plus gamma) to the previous op's target. A fully fusable
                    // chain emits none of these — its one op rides the composite.
                    PassWork::ColorTransform(i) => {
                        let color = &self.color_passes[i as usize];
                        commands.push(DrawCommand {
                            pipeline: self.color_transform_pipeline,
                            bind_group: Some(self.transient.bind_group(color.source)),
                            geometry: Geometry::Generated { count: 1 },
                            instance_buffer: self.color_transform_pool.buffer().expect(
                                "color-transform pool buffer exists when a color pass references it",
                            ),
                            instance_offset: color.instance as usize * COLOR_TRANSFORM_STRIDE,
                            uniforms,
                            scissor: None,
                        });
                    }
                    // The content behind one (or one shared group of) backdrop
                    // layers, re-rendered into a tight ROI target (§17.1). The
                    // draws are the duplicated instances `lower_from_scene`
                    // appended for this capture, selected by their segment target.
                    PassWork::BackdropCapture(i) => {
                        for seg in self
                            .segments
                            .iter()
                            .filter(|seg| seg.target == PassTarget::Capture(i as usize))
                        {
                            commands.push(self.command_for(seg, uniforms, vp));
                        }
                    }
                    // The frame's visible result: everything not routed offscreen,
                    // composites included.
                    PassWork::Surface => {
                        for seg in self
                            .segments
                            .iter()
                            .filter(|seg| seg.target == PassTarget::Main)
                        {
                            commands.push(self.command_for(seg, uniforms, vp));
                        }
                    }
                }
            }
            passes.push(RenderPass {
                target: match pass.writes() {
                    Some(target) => RenderTarget::Texture(self.transient.texture(target)),
                    None => RenderTarget::Surface(frame),
                },
                load: match pass.load() {
                    PassLoad::ClearTransparent => LoadOp::Clear([0.0, 0.0, 0.0, 0.0]),
                    PassLoad::ClearBackground => LoadOp::Clear(clear),
                },
                first_command,
                command_count: commands.len() as u32 - first_command,
            });
        }

        backend.encode(&DrawList {
            commands: &commands,
            passes: &passes,
        });

        // Return the buffers so their capacity is reused next frame.
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
            SegmentKind::AnalyticShadow => DrawCommand {
                pipeline: self.analytic_shadow_pipeline,
                bind_group: None,
                geometry: Geometry::Generated { count: seg.count },
                instance_buffer: self.analytic_shadow_pool.buffer().expect(
                    "analytic-shadow pool buffer exists when an analytic-shadow segment references it",
                ),
                instance_offset: seg.start as usize * SHADOW_STRIDE,
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
            // A composite whose layer fused a color chain: the same quad an
            // `Image` segment would draw, through the pipeline that applies the
            // fused matrix (§17.3, E2.2).
            SegmentKind::ColorTransform { bind_group } => DrawCommand {
                pipeline: self.color_transform_pipeline,
                bind_group: Some(bind_group),
                geometry: Geometry::Generated { count: seg.count },
                instance_buffer: self.color_transform_pool.buffer().expect(
                    "color-transform pool buffer exists when a color segment references it",
                ),
                instance_offset: seg.start as usize * COLOR_TRANSFORM_STRIDE,
                uniforms,
                scissor,
            },
            // An isolated layer's blend composite: the same quad again, through the
            // pipeline that samples both the layer and its destination snapshot and
            // writes with `Replace` (§14.6, E2.3).
            SegmentKind::AdvancedBlend { bind_group } => DrawCommand {
                pipeline: self.advanced_blend_pipeline,
                bind_group: Some(bind_group),
                geometry: Geometry::Generated { count: seg.count },
                instance_buffer: self.advanced_blend_pool.buffer().expect(
                    "advanced-blend pool buffer exists when a blend segment references it",
                ),
                instance_offset: seg.start as usize * ADVANCED_BLEND_STRIDE,
                uniforms,
                scissor,
            },
            // A frosted material surface: the same generated quad, through the
            // pipeline that samples the shared blurred backdrop and applies the
            // whole §18 chain in one fragment (M0.1).
            SegmentKind::Material { bind_group } => DrawCommand {
                pipeline: self.material_pipeline,
                bind_group: Some(bind_group),
                geometry: Geometry::Generated { count: seg.count },
                instance_buffer: self
                    .material_pool
                    .buffer()
                    .expect("material pool buffer exists when a material segment references it"),
                instance_offset: seg.start as usize * MATERIAL_STRIDE,
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
    use crate::color_effect::{ColorEffect, ColorMatrix};
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

    /// A quad of an explicit size, used to place one child *inside* another so the
    /// two provably overlap without moving the group's content union — the shape a
    /// fixture needs when it is asserting isolation-pass geometry rather than the
    /// planner's fold (§14.5).
    fn quad_sized(x: f32, y: f32, w: f32, h: f32) -> Primitive {
        Primitive::Quad(Quad {
            rect: Rect { x, y, w, h },
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
            blur_sigma: 0.0,
            backdrop_sigma: 0.0,
        })
    }

    /// A translucent layer: same clip rect, but `opacity < 1` triggers offscreen
    /// compositing.
    fn layer_opacity(x: f32, y: f32, w: f32, h: f32, opacity: f32) -> Primitive {
        Primitive::Layer(LayerClip {
            clip: Rect { x, y, w, h },
            opacity,
            blur_sigma: 0.0,
            backdrop_sigma: 0.0,
        })
    }

    /// A layer whose color chain follows it in the stream: `clip`/`opacity` only,
    /// no blur, so any offscreen it gets is the color chain's doing.
    fn layer_color(clip: Rect, opacity: f32) -> Primitive {
        Primitive::Layer(LayerClip {
            clip,
            opacity,
            blur_sigma: 0.0,
            backdrop_sigma: 0.0,
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
            shadow: None,
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

    /// The same concave outline as `solid_path`, now carrying a drop shadow.
    fn shadowed_path(inner: bool) -> Primitive {
        use crate::primitive::{PathShadow, Point as P};
        Primitive::Path(crate::primitive::Path {
            cmds: vec![
                PathCmd::MoveTo(P::new(4.0, 4.0)),
                PathCmd::LineTo(P::new(20.0, 12.0)),
                PathCmd::LineTo(P::new(4.0, 20.0)),
                PathCmd::LineTo(P::new(10.0, 12.0)),
                PathCmd::Close,
            ],
            fill: Some(Rgba {
                r: 0.2,
                g: 0.4,
                b: 0.6,
                a: 1.0,
            }),
            stroke: None,
            shadow: Some(PathShadow {
                color: Rgba {
                    r: 0.0,
                    g: 0.0,
                    b: 0.0,
                    a: 0.5,
                },
                offset: [3.0, 4.0],
                sigma: 2.0,
                spread: 0.0,
                inner,
            }),
        })
    }

    /// An outer path shadow builds a second mask (distinct from the fill mask, so
    /// keyed apart by sigma) and composites it offset by the drop through the
    /// reused glyph-coverage pipeline — two masked draws under/over each other.
    #[test]
    fn outer_path_shadow_builds_a_second_offset_mask() {
        let mut gpu = HeadlessRaster::new();
        let surface = gpu.create_surface(RawWindowHandle::Headless, 64, 64);
        let format = gpu.surface_format(surface);
        let mut r = Renderer::new(&mut gpu, format);
        r.upload(&mut gpu, &[shadowed_path(false)]);
        // One build for the shadow silhouette, one for the fill: the sigma-folded
        // key keeps them from colliding on the same slot.
        assert_eq!(r.frame_stats().clip_mask_builds, 2);
        let glyph_draws = r
            .segments
            .iter()
            .filter(|s| matches!(s.kind, SegmentKind::GlyphRun { .. }))
            .count();
        assert!(
            glyph_draws >= 2,
            "shadow and fill each lower as a glyph-coverage draw"
        );
    }

    /// An inner path shadow is an E1 filter-lane concern: the general path lane
    /// declines it, so only the fill mask is built.
    #[test]
    fn inner_path_shadow_skips_the_general_lane() {
        let mut gpu = HeadlessRaster::new();
        let surface = gpu.create_surface(RawWindowHandle::Headless, 64, 64);
        let format = gpu.surface_format(surface);
        let mut r = Renderer::new(&mut gpu, format);
        r.upload(&mut gpu, &[shadowed_path(true)]);
        assert_eq!(r.frame_stats().clip_mask_builds, 1);
    }

    /// A stable shadowed path re-resolves both slots with no re-raster.
    #[test]
    fn stable_path_shadow_rebuilds_nothing() {
        let mut gpu = HeadlessRaster::new();
        let surface = gpu.create_surface(RawWindowHandle::Headless, 64, 64);
        let format = gpu.surface_format(surface);
        let mut r = Renderer::new(&mut gpu, format);
        r.upload(&mut gpu, &[shadowed_path(false)]);
        assert_eq!(r.frame_stats().clip_mask_builds, 2);
        r.upload(&mut gpu, &[shadowed_path(false)]);
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
            shadow: None,
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
            shadow: None,
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
                // Contained inside the first quad: the children overlap, so the
                // group opacity cannot fold and the layer really isolates.
                quad_sized(12.0, 12.0, 4.0, 4.0),
                Primitive::LayerEnd,
            ],
        );

        // Exactly one offscreen pass, sized to the tight content ROI — the 10×10
        // quad, not the 20×20 layer clip (§16.2): the target never exceeds the
        // visible content. The *physical* target is the ROI's size class, since
        // it comes from the frame-local alias pool (§16.4).
        assert_eq!(r.offscreen_passes.len(), 1);
        let pass = &r.offscreen_passes[0];
        assert_eq!(pass.used, [10, 10]);
        assert_eq!(pass.viewport, [16.0, 16.0]);
        assert_eq!(pass.opacity, 0.5);

        // The child quad routes to that offscreen pass, with its position shifted
        // into texture-local space (ROI top-left, = the quad's own corner,
        // subtracted → the origin).
        let child = r
            .segments
            .iter()
            .find(|s| s.target == PassTarget::Offscreen(0))
            .expect("child quad segment routes to the offscreen pass");
        assert_eq!(child.kind, SegmentKind::Quad);
        assert_eq!(r.quad_scratch[child.start as usize].rect_pos, [0.0, 0.0]);

        // The main pass carries exactly one composite: an Image segment sampling
        // the offscreen texture, tinted white with alpha == opacity, positioned at
        // the ROI's world rect.
        let composites: Vec<&Segment> = r
            .segments
            .iter()
            .filter(|s| s.target == PassTarget::Main)
            .collect();
        assert_eq!(composites.len(), 1);
        assert!(matches!(composites[0].kind, SegmentKind::Image { .. }));
        let inst = &r.image_scratch[composites[0].start as usize];
        assert_eq!(inst.rect_pos, [10.0, 10.0]);
        assert_eq!(inst.rect_size, [10.0, 10.0]);
        assert_eq!(inst.color, [1.0, 1.0, 1.0, 0.5]);
    }

    /// A small panel inside a large-but-onscreen clip sizes its offscreen target
    /// to the panel's content, not the clip and not the surface (§16.2): the
    /// forbidden "small panel → full-screen target" shape never occurs. A second
    /// child placed outside a tight clip is culled at lowering.
    #[test]
    fn tight_roi_sizes_target_to_content_not_clip() {
        let mut gpu = HeadlessRaster::new();
        let surface = gpu.create_surface(RawWindowHandle::Headless, 64, 64);
        let format = gpu.surface_format(surface);
        let mut r = Renderer::new(&mut gpu, format);
        r.set_surface_size([64.0, 64.0]);
        // A translucent layer with a 4000×4000 clip, but only a 10×10 quad at
        // (10, 10). ROI = content(10,10,10,10) ∩ clip ∩ surface(64×64) = the quad.
        r.upload(
            &mut gpu,
            &[
                layer_opacity(0.0, 0.0, 4000.0, 4000.0, 0.5),
                quad(10.0, 10.0),
                quad_sized(12.0, 12.0, 4.0, 4.0),
                Primitive::LayerEnd,
            ],
        );

        // Sized to the panel, never the 4000×4000 clip nor the 64×64 surface. The
        // written region is exactly the ROI; the pooled texture behind it is the
        // ROI's size class (§16.4), which is still far below the surface.
        assert_eq!(r.offscreen_passes.len(), 1);
        assert_eq!(r.offscreen_passes[0].used, [10, 10]);
        assert_eq!(r.offscreen_passes[0].viewport, [16.0, 16.0]);
        // The transient-target budget reflects the tight size (§61 resource gate).
        let bytes = r.frame_stats().transient_target_bytes;
        assert_eq!(bytes, 16 * 16 * 4);
        assert!(
            bytes < 64 * 64 * 4 / 4,
            "the pooled target stays a fraction of a full-surface one: {bytes}"
        );

        // Origin repatched to the ROI top-left (the quad's own corner), so the
        // child lands at the texture origin.
        let child = r
            .segments
            .iter()
            .find(|s| s.target == PassTarget::Offscreen(0))
            .expect("child routes to the offscreen pass");
        assert_eq!(r.quad_scratch[child.start as usize].rect_pos, [0.0, 0.0]);

        // Composite lands at the ROI's world rect.
        let composite = r
            .segments
            .iter()
            .find(|s| s.target == PassTarget::Main)
            .expect("composite in main pass");
        let inst = &r.image_scratch[composite.start as usize];
        assert_eq!(inst.rect_pos, [10.0, 10.0]);
        assert_eq!(inst.rect_size, [10.0, 10.0]);

        // Culling: a tight clip around one quad, with a second quad fully outside
        // it. The offscreen child that misses the ROI is dropped at lowering and
        // counted, so the pass sizes to the surviving quad alone.
        let mut r2 = Renderer::new(&mut gpu, format);
        r2.set_surface_size([64.0, 64.0]);
        r2.upload(
            &mut gpu,
            &[
                layer_opacity(10.0, 10.0, 10.0, 10.0, 0.5), // clip == first quad
                quad(10.0, 10.0),                           // inside the clip
                quad_sized(12.0, 12.0, 4.0, 4.0),           // overlaps it
                quad(40.0, 40.0),                           // outside → culled
                Primitive::LayerEnd,
            ],
        );
        assert_eq!(r2.frame_stats().culled_primitives, 1);
        assert_eq!(r2.offscreen_passes[0].used, [10, 10]);
        let offscreen_children = r2
            .segments
            .iter()
            .filter(|s| s.target == PassTarget::Offscreen(0))
            .count();
        assert_eq!(offscreen_children, 1, "the outside quad is not drawn");
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

    /// A layer with `blur_sigma > 0` at `opacity == 1.0` still takes the offscreen
    /// path: the blur ladder needs the layer's content in a texture it can sample,
    /// so a blurred opaque layer opens exactly one offscreen pass where an unblurred
    /// opaque layer would have stayed inline.
    #[test]
    fn blurred_layer_forces_offscreen_at_full_opacity() {
        let opaque = Primitive::Layer(LayerClip {
            clip: Rect {
                x: 8.0,
                y: 8.0,
                w: 16.0,
                h: 16.0,
            },
            opacity: 1.0,
            blur_sigma: 0.0,
            backdrop_sigma: 0.0,
        });
        let blurred = Primitive::Layer(LayerClip {
            clip: Rect {
                x: 8.0,
                y: 8.0,
                w: 16.0,
                h: 16.0,
            },
            opacity: 1.0,
            blur_sigma: 3.0,
            backdrop_sigma: 0.0,
        });

        let mut gpu = HeadlessRaster::new();
        let surface = gpu.create_surface(RawWindowHandle::Headless, 64, 64);
        let format = gpu.surface_format(surface);
        let mut r = Renderer::new(&mut gpu, format);

        // Opaque, unblurred: no offscreen pass (the inline fast path).
        r.upload(&mut gpu, &[opaque, quad(10.0, 10.0), Primitive::LayerEnd]);
        assert_eq!(
            r.frame_stats().offscreen_passes,
            0,
            "an opaque unblurred layer stays inline"
        );
        assert_eq!(r.frame_stats().blur_passes, 0);

        // Opaque, blurred: one offscreen pass plus the separable ladder.
        r.upload(&mut gpu, &[blurred, quad(10.0, 10.0), Primitive::LayerEnd]);
        assert_eq!(
            r.frame_stats().offscreen_passes,
            1,
            "a blurred layer forces an offscreen pass even at full opacity"
        );
        assert!(
            r.frame_stats().blur_passes >= 2,
            "the ladder inserts at least a horizontal and a vertical pass"
        );
    }

    /// A small-radius blur (`ceil(3 * sigma) <= MAX_TAPS`) plans exactly two
    /// separable full-resolution passes: one horizontal, one vertical, each at the
    /// source extent, walking a single axis in normalized source texels.
    #[test]
    fn blur_plan_small_sigma_is_two_separable_passes() {
        let plan = blur_plan(4.0, 40, 24);
        assert_eq!(plan.steps.len(), 2, "small blur is two passes");

        let h = &plan.steps[0];
        let v = &plan.steps[1];
        // Both keep the full source extent.
        assert_eq!((h.width, h.height), (40, 24));
        assert_eq!((v.width, v.height), (40, 24));
        // ceil(3 * 4) = 12 taps each side, well under the 32 budget.
        assert_eq!(h.radius, 12.0);
        assert_eq!(v.radius, 12.0);
        // First pass walks x, second walks y, as unit axes — the renderer divides
        // by the *pooled* source extent when it builds the instance.
        assert_eq!(h.axis, [1.0, 0.0]);
        assert_eq!(v.axis, [0.0, 1.0]);
        assert_eq!(h.sigma, 4.0);
        assert_eq!(v.sigma, 4.0);
    }

    /// A large-radius blur downsamples first: a full-resolution `ceil(3 * sigma)`
    /// past the tap budget plans a four-step ladder — two resample passes that
    /// shrink the source by an integer factor, then a horizontal and vertical blur
    /// at the reduced extent whose radius fits the budget.
    #[test]
    fn blur_plan_large_sigma_downsamples() {
        // sigma 20 → full radius ceil(60) = 60 > 32, so it must downsample.
        let src_w = 200u32;
        let src_h = 120u32;
        let plan = blur_plan(20.0, src_w, src_h);
        assert_eq!(plan.steps.len(), 4, "large blur is a downsample pyramid");

        let down_h = &plan.steps[0];
        let down_v = &plan.steps[1];
        let blur_h = &plan.steps[2];
        let blur_v = &plan.steps[3];

        // The reduced extent is strictly smaller on both axes.
        assert!(blur_v.width < src_w && blur_v.height < src_h);
        // The two resample passes carry no Gaussian weight (plain bilinear taps).
        assert_eq!(down_h.sigma, 0.0);
        assert_eq!(down_v.sigma, 0.0);
        assert_eq!(down_h.radius, 0.0);
        assert_eq!(down_v.radius, 0.0);
        // The width collapses first (down_h), then the height (down_v), landing at
        // the reduced extent the two blur passes then share.
        assert_eq!(down_h.width, blur_h.width);
        assert_eq!(down_h.height, src_h);
        assert_eq!((down_v.width, down_v.height), (blur_h.width, blur_h.height));
        // The reduced-resolution blur radius is within budget.
        assert!(blur_h.radius as u32 <= BLUR_MAX_TAPS);
        assert!(blur_v.radius as u32 <= BLUR_MAX_TAPS);
        // The blur passes walk one axis each; the resamples walk none.
        assert_eq!(blur_h.axis, [1.0, 0.0]);
        assert_eq!(blur_v.axis, [0.0, 1.0]);
        assert_eq!(down_h.axis, [0.0, 0.0]);
        assert_eq!(down_v.axis, [0.0, 0.0]);
    }

    /// A sub-pixel blur (`sigma <= BLUR_MIN_SIGMA`) is a visual no-op: the plan is
    /// empty, so no ladder is inserted and the layer composites its base texture.
    /// A zero-extent source is likewise skipped.
    #[test]
    fn subpixel_blur_skips() {
        assert!(blur_plan(0.0, 40, 40).is_empty(), "zero sigma");
        assert!(blur_plan(1.0, 40, 40).is_empty(), "sigma at the floor");
        assert!(blur_plan(0.5, 40, 40).is_empty(), "sub-pixel sigma");
        assert!(!blur_plan(2.0, 40, 40).is_empty(), "above the floor blurs");
        assert!(blur_plan(4.0, 0, 40).is_empty(), "zero-width source");
        assert!(blur_plan(4.0, 40, 0).is_empty(), "zero-height source");
    }

    /// End-to-end headless blur: a hard black/white vertical edge inside a blurred
    /// layer composites as a monotonic ramp rather than a step. Sampling a row
    /// across the former edge, luminance must never decrease left-to-right and must
    /// climb through intermediate values the hard edge never produced.
    #[test]
    fn headless_blur_softens_a_hard_edge() {
        const W: u32 = 48;
        const H: u32 = 16;
        let mut gpu = HeadlessRaster::new();
        let surface = gpu.create_surface(RawWindowHandle::Headless, W, H);
        let format = gpu.surface_format(surface);
        let mut r = Renderer::new(&mut gpu, format);

        // Left half black, right half white, filling the layer rect. The layer
        // spans the full surface and blurs its content horizontally/vertically.
        let black = Primitive::Quad(Quad {
            rect: Rect {
                x: 0.0,
                y: 0.0,
                w: (W / 2) as f32,
                h: H as f32,
            },
            color: Rgba {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 1.0,
            },
            radius: 0.0,
            border: Border::NONE,
        });
        let white = Primitive::Quad(Quad {
            rect: Rect {
                x: (W / 2) as f32,
                y: 0.0,
                w: (W / 2) as f32,
                h: H as f32,
            },
            color: Rgba {
                r: 1.0,
                g: 1.0,
                b: 1.0,
                a: 1.0,
            },
            radius: 0.0,
            border: Border::NONE,
        });
        let blurred = Primitive::Layer(LayerClip {
            clip: Rect {
                x: 0.0,
                y: 0.0,
                w: W as f32,
                h: H as f32,
            },
            opacity: 1.0,
            blur_sigma: 4.0,
            backdrop_sigma: 0.0,
        });

        r.upload(&mut gpu, &[blurred, black, white, Primitive::LayerEnd]);
        r.submit(
            &mut gpu,
            surface,
            [0.0, 0.0, 0.0, 1.0],
            [W as f32, H as f32],
        );
        let px = gpu.read_pixels_bgra8(surface);

        // Read the middle row's blue channel (grayscale, so any channel works)
        // across the transition band around the former edge. Away from the layer's
        // own boundaries (where uv clamping thins the tap window), the ramp must be
        // non-decreasing and pass through mid-gray that a hard step would skip.
        let row = H / 2;
        let edge = W / 2;
        let band = 8u32; // ceil(3 * sigma) taps, kept inside the surface margins.
        let lo = edge - band;
        let hi = edge + band;
        let mut prev = 0i32;
        let mut saw_midtone = false;
        for x in lo..=hi {
            let i = ((row * W + x) * 4) as usize;
            let v = px[i] as i32;
            assert!(
                v + 2 >= prev,
                "luminance must not fall across a blurred edge: x={x} {v} < {prev}"
            );
            if (64..=192).contains(&v) {
                saw_midtone = true;
            }
            prev = v;
        }
        assert!(
            saw_midtone,
            "a blurred edge produces mid-tones a hard step never would"
        );

        // The blurred result is bounded by the source: black on the far left,
        // white on the far right, so the edge genuinely softened between them.
        let left = px[((row * W + lo) * 4) as usize] as i32;
        let right = px[((row * W + hi) * 4) as usize] as i32;
        assert!(left < 64, "left of the edge stays near black: {left}");
        assert!(right > 192, "right of the edge stays near white: {right}");
    }

    /// The blur scratch textures are pooled: a steady scene that blurs the same
    /// layer every frame claims its ladder targets from the pool on later frames
    /// rather than creating fresh ones, so the transient target byte count is
    /// stable and no new GPU textures are minted at steady state.
    #[test]
    fn steady_state_blur_reuses_pooled_targets() {
        let scene = |sigma: f32| {
            [
                Primitive::Layer(LayerClip {
                    clip: Rect {
                        x: 4.0,
                        y: 4.0,
                        w: 24.0,
                        h: 24.0,
                    },
                    opacity: 1.0,
                    blur_sigma: sigma,
                    backdrop_sigma: 0.0,
                }),
                quad(8.0, 8.0),
                Primitive::LayerEnd,
            ]
        };

        let mut gpu = HeadlessRaster::new();
        let surface = gpu.create_surface(RawWindowHandle::Headless, 64, 64);
        let format = gpu.surface_format(surface);
        let mut r = Renderer::new(&mut gpu, format);

        r.upload(&mut gpu, &scene(4.0));
        let first = r.frame_stats();
        assert!(first.blur_passes >= 2);
        assert!(
            first.transient_target_allocations > 0,
            "the first frame has to mint its ladder's textures"
        );

        r.upload(&mut gpu, &scene(4.0));
        let second = r.frame_stats();
        assert_eq!(
            second.blur_passes, first.blur_passes,
            "an identical blurred scene plans the same ladder"
        );
        assert_eq!(
            second.blur_target_bytes, first.blur_target_bytes,
            "the blur scratch footprint is stable at steady state"
        );
        assert_eq!(
            second.transient_target_allocations, 0,
            "the second identical frame mints no new pooled textures"
        );
        assert_eq!(
            second.transient_targets, first.transient_targets,
            "and the pool neither grows nor shrinks"
        );
    }

    /// The compiled plan is what the backend sees: one pass per offscreen layer,
    /// one per blur rung, one surface pass — and nothing merged or culled, since
    /// every pass in a real frame writes a distinct attachment that something
    /// downstream samples (§16.1).
    #[test]
    fn the_pass_plan_counts_every_attachment_the_frame_writes() {
        let mut gpu = HeadlessRaster::new();
        let surface = gpu.create_surface(RawWindowHandle::Headless, 64, 64);
        let format = gpu.surface_format(surface);
        let mut r = Renderer::new(&mut gpu, format);
        r.set_surface_size([64.0, 64.0]);

        r.upload(&mut gpu, &[quad(4.0, 4.0)]);
        let flat = r.frame_stats();
        assert_eq!(
            flat.render_passes, 1,
            "a frame with no layers is the surface pass alone"
        );

        r.upload(
            &mut gpu,
            &[
                Primitive::Layer(LayerClip {
                    clip: Rect {
                        x: 4.0,
                        y: 4.0,
                        w: 24.0,
                        h: 24.0,
                    },
                    opacity: 1.0,
                    blur_sigma: 4.0,
                    backdrop_sigma: 0.0,
                }),
                quad(8.0, 8.0),
                Primitive::LayerEnd,
            ],
        );
        let blurred = r.frame_stats();
        assert_eq!(
            blurred.render_passes,
            blurred.offscreen_passes
                + blurred.blur_passes as usize
                + blurred.color_transform_passes as usize
                + 1,
            "every layer, every rung, every unfused color op, and the surface each get a pass"
        );
        assert_eq!(blurred.render_pass_merges, 0);
        assert_eq!(blurred.culled_render_passes, 0);

        // The same identity holds when a color chain is what forces the
        // offscreen, including the split case that does earn extra passes.
        r.upload(
            &mut gpu,
            &[
                layer_color(
                    Rect {
                        x: 4.0,
                        y: 4.0,
                        w: 24.0,
                        h: 24.0,
                    },
                    1.0,
                ),
                Primitive::ColorEffect(ColorEffect::Brightness(1.4)),
                Primitive::ColorEffect(ColorEffect::Gamma(2.2)),
                Primitive::ColorEffect(ColorEffect::Saturation(0.3)),
                quad(8.0, 8.0),
                Primitive::LayerEnd,
            ],
        );
        let split = r.frame_stats();
        assert_eq!(split.color_transform_passes, 1);
        assert_eq!(
            split.render_passes,
            split.offscreen_passes
                + split.blur_passes as usize
                + split.color_transform_passes as usize
                + 1,
        );
    }

    /// The headline property of E2.2 (§17.3): three mergeable effects are one
    /// fused op, that op rides the composite the layer already draws, and the
    /// frame therefore costs **one** offscreen pass and **zero** extra
    /// render-target passes — not three.
    #[test]
    fn a_mergeable_color_chain_costs_no_extra_passes() {
        let mut gpu = HeadlessRaster::new();
        let surface = gpu.create_surface(RawWindowHandle::Headless, 64, 64);
        let format = gpu.surface_format(surface);
        let mut r = Renderer::new(&mut gpu, format);
        r.set_surface_size([64.0, 64.0]);

        let clip = Rect {
            x: 4.0,
            y: 4.0,
            w: 24.0,
            h: 24.0,
        };
        r.upload(
            &mut gpu,
            &[
                layer_color(clip, 1.0),
                Primitive::ColorEffect(ColorEffect::Brightness(1.2)),
                Primitive::ColorEffect(ColorEffect::Contrast(0.8)),
                Primitive::ColorEffect(ColorEffect::Saturation(0.5)),
                quad(8.0, 8.0),
                Primitive::LayerEnd,
            ],
        );
        let s = r.frame_stats();
        assert_eq!(s.color_effect_ops, 1, "three mergeable effects fuse to one");
        assert_eq!(
            s.color_transform_passes, 0,
            "the one op rides the composite: no extra render-target pass"
        );
        assert_eq!(s.offscreen_passes, 1, "the chain forces exactly one layer");
        assert_eq!(s.blur_passes, 0);
        assert_eq!(
            s.render_passes, 2,
            "the layer and the surface, nothing else"
        );

        // The composite draws through the color-transform pipeline rather than
        // the plain image one — same quad, one extra matrix in the fragment.
        let batches = r.inspect_batches();
        assert_eq!(
            batches
                .batches
                .iter()
                .filter(|b| b.pipeline == crate::inspect::BatchPipeline::ColorTransform)
                .count(),
            1,
            "the composite is the single color-transform draw"
        );
        assert!(
            !batches
                .batches
                .iter()
                .any(|b| b.pipeline == crate::inspect::BatchPipeline::Image),
            "a recolored composite never also emits a plain image draw"
        );
    }

    /// Length is not what splits a chain — expressibility is. All nine affine
    /// effects at once still fuse to one op and zero extra passes.
    #[test]
    fn a_long_mergeable_chain_still_costs_no_extra_passes() {
        let mut gpu = HeadlessRaster::new();
        let surface = gpu.create_surface(RawWindowHandle::Headless, 64, 64);
        let format = gpu.surface_format(surface);
        let mut r = Renderer::new(&mut gpu, format);
        r.set_surface_size([64.0, 64.0]);

        let clip = Rect {
            x: 4.0,
            y: 4.0,
            w: 24.0,
            h: 24.0,
        };
        r.upload(
            &mut gpu,
            &[
                layer_color(clip, 1.0),
                Primitive::ColorEffect(ColorEffect::Brightness(1.1)),
                Primitive::ColorEffect(ColorEffect::Contrast(1.2)),
                Primitive::ColorEffect(ColorEffect::Saturation(0.9)),
                Primitive::ColorEffect(ColorEffect::HueRotate(0.4)),
                Primitive::ColorEffect(ColorEffect::Grayscale(0.25)),
                Primitive::ColorEffect(ColorEffect::Sepia(0.3)),
                Primitive::ColorEffect(ColorEffect::Invert(0.1)),
                Primitive::ColorEffect(ColorEffect::ColorMatrix(ColorMatrix::brightness(0.95))),
                Primitive::ColorEffect(ColorEffect::Tint {
                    color: [0.2, 0.4, 0.9],
                    amount: 0.35,
                }),
                quad(8.0, 8.0),
                Primitive::LayerEnd,
            ],
        );
        let s = r.frame_stats();
        assert_eq!(s.color_effect_ops, 1, "nine mergeable effects are one op");
        assert_eq!(s.color_transform_passes, 0);
        assert_eq!(s.render_passes, 2);
    }

    /// Only the stage the fused form cannot express earns a pass: a gamma in the
    /// middle of two affine runs splits the chain exactly once, so the frame pays
    /// one extra render-target pass — not one per effect.
    #[test]
    fn only_a_non_expressible_stage_earns_a_pass() {
        let mut gpu = HeadlessRaster::new();
        let surface = gpu.create_surface(RawWindowHandle::Headless, 64, 64);
        let format = gpu.surface_format(surface);
        let mut r = Renderer::new(&mut gpu, format);
        r.set_surface_size([64.0, 64.0]);

        let clip = Rect {
            x: 4.0,
            y: 4.0,
            w: 24.0,
            h: 24.0,
        };
        r.upload(
            &mut gpu,
            &[
                layer_color(clip, 1.0),
                Primitive::ColorEffect(ColorEffect::Brightness(1.2)),
                Primitive::ColorEffect(ColorEffect::Contrast(0.8)),
                Primitive::ColorEffect(ColorEffect::Gamma(2.2)),
                Primitive::ColorEffect(ColorEffect::Saturation(0.5)),
                Primitive::ColorEffect(ColorEffect::Invert(0.2)),
                quad(8.0, 8.0),
                Primitive::LayerEnd,
            ],
        );
        let s = r.frame_stats();
        assert_eq!(
            s.color_effect_ops, 2,
            "five effects around one gamma are two ops"
        );
        assert_eq!(
            s.color_transform_passes, 1,
            "the earlier op gets a pass; the last still rides the composite"
        );
        assert_eq!(s.offscreen_passes, 1);
        assert_eq!(
            s.render_passes, 3,
            "the layer, the one unfused op, and the surface"
        );
    }

    /// A chain of neutral parameters computes nothing, so it must not force an
    /// offscreen at all: the layer stays a plain in-pass scissor clip.
    #[test]
    fn a_neutral_color_chain_forces_no_offscreen() {
        let mut gpu = HeadlessRaster::new();
        let surface = gpu.create_surface(RawWindowHandle::Headless, 64, 64);
        let format = gpu.surface_format(surface);
        let mut r = Renderer::new(&mut gpu, format);
        r.set_surface_size([64.0, 64.0]);

        let clip = Rect {
            x: 4.0,
            y: 4.0,
            w: 24.0,
            h: 24.0,
        };
        r.upload(
            &mut gpu,
            &[
                layer_color(clip, 1.0),
                Primitive::ColorEffect(ColorEffect::Brightness(1.0)),
                Primitive::ColorEffect(ColorEffect::Gamma(1.0)),
                Primitive::ColorEffect(ColorEffect::Grayscale(0.0)),
                quad(8.0, 8.0),
                Primitive::LayerEnd,
            ],
        );
        let s = r.frame_stats();
        assert_eq!(s.color_effect_ops, 0);
        assert_eq!(s.color_transform_passes, 0);
        assert_eq!(
            s.offscreen_passes, 0,
            "nothing to compute, nothing to build"
        );
        assert_eq!(s.render_passes, 1, "the surface pass alone");
    }

    /// Two identical frames plan the same graph and reuse the same pooled
    /// scratch: a split color chain is steady-state stable (§16.3, §17.4).
    #[test]
    fn steady_state_color_chain_reuses_pooled_targets() {
        let mut gpu = HeadlessRaster::new();
        let surface = gpu.create_surface(RawWindowHandle::Headless, 64, 64);
        let format = gpu.surface_format(surface);
        let mut r = Renderer::new(&mut gpu, format);
        r.set_surface_size([64.0, 64.0]);

        let clip = Rect {
            x: 4.0,
            y: 4.0,
            w: 24.0,
            h: 24.0,
        };
        let scene = [
            layer_color(clip, 0.75),
            Primitive::ColorEffect(ColorEffect::Saturation(0.4)),
            Primitive::ColorEffect(ColorEffect::Gamma(1.8)),
            Primitive::ColorEffect(ColorEffect::Brightness(1.3)),
            quad(8.0, 8.0),
            Primitive::LayerEnd,
        ];

        r.upload(&mut gpu, &scene);
        let first = r.frame_stats();
        r.upload(&mut gpu, &scene);
        let second = r.frame_stats();

        assert_eq!(first.color_effect_ops, 2);
        assert_eq!(first.color_transform_passes, 1);
        assert_eq!(second.color_effect_ops, first.color_effect_ops);
        assert_eq!(
            second.color_transform_passes, first.color_transform_passes,
            "an identical frame plans an identical chain"
        );
        assert_eq!(second.render_passes, first.render_passes);
        assert_eq!(second.draw_calls, first.draw_calls);
        assert_eq!(
            second.transient_target_allocations, 0,
            "the second frame allocates no new scratch"
        );
        assert_eq!(
            second.render_graph_compiles, 0,
            "the topology hash hits, so the graph is not recompiled"
        );
    }

    /// The markers are an annotation on the layer, not a primitive of their own:
    /// a `ColorEffect` outside a layer open is inert, and a run that follows a
    /// layer stops at the first non-marker primitive.
    #[test]
    fn color_effect_markers_only_bind_to_the_layer_they_follow() {
        let mut gpu = HeadlessRaster::new();
        let surface = gpu.create_surface(RawWindowHandle::Headless, 64, 64);
        let format = gpu.surface_format(surface);
        let mut r = Renderer::new(&mut gpu, format);
        r.set_surface_size([64.0, 64.0]);

        // Markers with no layer to annotate: no ops, no passes, no draws beyond
        // the quad itself.
        r.upload(
            &mut gpu,
            &[
                Primitive::ColorEffect(ColorEffect::Invert(1.0)),
                quad(4.0, 4.0),
                Primitive::ColorEffect(ColorEffect::Invert(1.0)),
            ],
        );
        let loose = r.frame_stats();
        assert_eq!(loose.color_effect_ops, 0);
        assert_eq!(loose.offscreen_passes, 0);
        assert_eq!(loose.render_passes, 1);

        // A marker after the layer's content is past the run and does not join it.
        let clip = Rect {
            x: 4.0,
            y: 4.0,
            w: 24.0,
            h: 24.0,
        };
        r.upload(
            &mut gpu,
            &[
                layer_color(clip, 1.0),
                Primitive::ColorEffect(ColorEffect::Invert(1.0)),
                quad(8.0, 8.0),
                Primitive::ColorEffect(ColorEffect::Gamma(2.2)),
                Primitive::LayerEnd,
            ],
        );
        let s = r.frame_stats();
        assert_eq!(
            s.color_effect_ops, 1,
            "only the leading run fuses; the trailing marker is inert"
        );
        assert_eq!(s.color_transform_passes, 0);
    }

    /// Layer opacity rides the fused op's alpha row instead of a second tint, so
    /// a translucent recolored layer still costs one offscreen and no extra pass.
    #[test]
    fn layer_opacity_folds_into_the_fused_op() {
        let mut gpu = HeadlessRaster::new();
        let surface = gpu.create_surface(RawWindowHandle::Headless, 64, 64);
        let format = gpu.surface_format(surface);
        let mut r = Renderer::new(&mut gpu, format);
        r.set_surface_size([64.0, 64.0]);

        let clip = Rect {
            x: 4.0,
            y: 4.0,
            w: 24.0,
            h: 24.0,
        };
        r.upload(
            &mut gpu,
            &[
                layer_color(clip, 0.5),
                Primitive::ColorEffect(ColorEffect::Grayscale(1.0)),
                quad(8.0, 8.0),
                Primitive::LayerEnd,
            ],
        );
        let s = r.frame_stats();
        assert_eq!(s.color_effect_ops, 1);
        assert_eq!(s.color_transform_passes, 0);
        assert_eq!(s.offscreen_passes, 1);

        let op = r.offscreen_passes[0]
            .color
            .expect("a recolored layer carries its fused op");
        assert_eq!(
            op.matrix.rows[3],
            [0.0, 0.0, 0.0, 0.5, 0.0],
            "the alpha row carries the layer opacity"
        );
    }

    /// A blur and a color chain on one layer compose rather than fight: the
    /// ladder runs on the layer's content and the fused op recolors its result,
    /// still with no extra color pass.
    #[test]
    fn a_blur_and_a_color_chain_share_one_offscreen() {
        let mut gpu = HeadlessRaster::new();
        let surface = gpu.create_surface(RawWindowHandle::Headless, 64, 64);
        let format = gpu.surface_format(surface);
        let mut r = Renderer::new(&mut gpu, format);
        r.set_surface_size([64.0, 64.0]);

        r.upload(
            &mut gpu,
            &[
                Primitive::Layer(LayerClip {
                    clip: Rect {
                        x: 4.0,
                        y: 4.0,
                        w: 24.0,
                        h: 24.0,
                    },
                    opacity: 1.0,
                    blur_sigma: 3.0,
                    backdrop_sigma: 0.0,
                }),
                Primitive::ColorEffect(ColorEffect::Grayscale(1.0)),
                Primitive::ColorEffect(ColorEffect::Brightness(1.2)),
                quad(8.0, 8.0),
                Primitive::LayerEnd,
            ],
        );
        let s = r.frame_stats();
        assert_eq!(s.offscreen_passes, 1);
        assert!(s.blur_passes >= 2, "the ladder still runs");
        assert_eq!(s.color_effect_ops, 1);
        assert_eq!(
            s.color_transform_passes, 0,
            "the fused op rides the composite of the blurred texture"
        );
    }

    /// Topology alone drives a recompile (§16.1): a second identical frame reuses
    /// the plan, and so does a frame that only moves geometry, recolors it, resizes
    /// the surface, or nudges a sigma within its ladder tier — all of which change
    /// extents and payloads but not the pass graph.
    #[test]
    fn only_a_topology_change_recompiles_the_pass_plan() {
        let scene = |x: f32, sigma: f32| {
            vec![
                Primitive::Layer(LayerClip {
                    clip: Rect {
                        x,
                        y: 4.0,
                        w: 24.0,
                        h: 24.0,
                    },
                    opacity: 1.0,
                    blur_sigma: sigma,
                    backdrop_sigma: 0.0,
                }),
                quad(x + 4.0, 8.0),
                Primitive::LayerEnd,
            ]
        };

        let mut gpu = HeadlessRaster::new();
        let surface = gpu.create_surface(RawWindowHandle::Headless, 64, 64);
        let format = gpu.surface_format(surface);
        let mut r = Renderer::new(&mut gpu, format);
        r.set_surface_size([64.0, 64.0]);

        r.upload(&mut gpu, &scene(4.0, 4.0));
        let first = r.frame_stats();
        assert_eq!(
            first.render_graph_compiles, 1,
            "the first frame has no cached plan to reuse"
        );

        r.upload(&mut gpu, &scene(4.0, 4.0));
        assert_eq!(
            r.frame_stats().render_graph_compiles,
            0,
            "an identical frame reuses the compiled plan"
        );

        r.upload(&mut gpu, &scene(10.0, 4.6));
        let moved = r.frame_stats();
        assert_eq!(
            moved.render_graph_compiles, 0,
            "a transform and a sigma tweak inside one ladder tier are parameters, not topology"
        );
        assert_eq!(moved.render_passes, first.render_passes);

        r.set_surface_size([48.0, 96.0]);
        r.upload(&mut gpu, &scene(4.0, 4.0));
        assert_eq!(
            r.frame_stats().render_graph_compiles,
            0,
            "a resize changes every extent and no dependency"
        );

        let mut two = scene(4.0, 4.0);
        two.extend(scene(34.0, 4.0));
        r.upload(&mut gpu, &two);
        let grown = r.frame_stats();
        assert_eq!(
            grown.render_graph_compiles, 1,
            "a second layer is a genuinely different graph"
        );
        assert!(grown.render_passes > first.render_passes);
    }

    /// A sigma large enough to enter the downsampling tier plans more rungs than a
    /// small one, which *is* a topology change — the plan-reuse contract covers
    /// parameter drift, not a different ladder shape (§16.1, §16.3).
    #[test]
    fn crossing_a_blur_ladder_tier_recompiles_the_pass_plan() {
        let scene = |sigma: f32| {
            [
                Primitive::Layer(LayerClip {
                    clip: Rect {
                        x: 0.0,
                        y: 0.0,
                        w: 64.0,
                        h: 64.0,
                    },
                    opacity: 1.0,
                    blur_sigma: sigma,
                    backdrop_sigma: 0.0,
                }),
                quad(8.0, 8.0),
                Primitive::LayerEnd,
            ]
        };

        let mut gpu = HeadlessRaster::new();
        let surface = gpu.create_surface(RawWindowHandle::Headless, 64, 64);
        let format = gpu.surface_format(surface);
        let mut r = Renderer::new(&mut gpu, format);
        r.set_surface_size([64.0, 64.0]);

        r.upload(&mut gpu, &scene(2.0));
        let small = r.frame_stats();
        r.upload(&mut gpu, &scene(40.0));
        let large = r.frame_stats();

        assert!(
            large.blur_passes > small.blur_passes,
            "the large sigma has to downsample, which adds rungs"
        );
        assert_eq!(
            large.render_graph_compiles, 1,
            "more rungs is a different pass graph"
        );
    }
}
