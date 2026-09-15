//! Read-only batch and primitive introspection for the inspector, Studio, and
//! tests.
//!
//! Architecture section 34/62 ask that a `BatchId`'s pipeline and resources, and
//! a `PrimitiveId`'s geometry ranges, be inspectable without unsafe memory
//! poking — through the same model the renderer itself uses, so a tool, a
//! headless golden test, and an AI automation all read one contract.
//! [`Renderer::inspect_batches`] snapshots the frame's
//! draw segments (built by [`upload`](crate::Renderer::upload), before
//! [`submit`](crate::Renderer::submit)) into a flat, self-contained
//! [`InspectBatches`], mirroring the UI node-tree introspection shape: one row
//! per batch, each naming its pipeline, resources, and range.
//!
//! This is a cold path (architecture section 7.2): it allocates a fresh snapshot
//! from `&self` and is never touched by the steady-state frame path (which reads
//! the private segment list directly). It only reads the renderer's existing
//! segment list and pipeline handles, so building a snapshot changes no renderer
//! state.
//!
//! The batch identity here is [`BatchId`] — a batch's index into the frame's
//! segment list. It is the stable handle architecture section 62 names
//! (`BatchId -> pipeline/resources`); the batch's true draw command is derived
//! from the segment exactly as the encoder derives it.
//!
//! [`Renderer::inspect_primitives`] is the retained-scene counterpart: it
//! replays the paint-order record the way the lowering does and reports, per
//! [`PrimitiveId`], which batch draws it and the geometry sub-range it occupies
//! (`PrimitiveId -> ranges`). The two views line up — a primitive's range is a
//! sub-range of its batch's range — so a tool can go from a scene primitive to
//! its exact draw and buffer slice.

use crate::Rect;
use crate::Renderer;
use crate::batch::{
    BatchFamily, BatchItem, BatchKey, BatchTarget, RenderChunk, RenderChunkId, joins,
};
use crate::renderer::{PassTarget, Segment, SegmentKind};
use crate::scene::ids::PrimitiveId;
use crate::scene::store::StoreRef;
use viso_gpu::{BindGroupId, PipelineId};

/// A batch's index into the frame's segment list — the stable handle
/// architecture section 62 names for `BatchId -> pipeline/resources`.
///
/// `BatchId(i)` addresses `inspect_batches().batches[i]`, the `i`-th draw
/// command the frame will encode, in submission order. (Distinct from
/// [`BatchKey`](crate::BatchKey), the packed GPU-state key the planner merges
/// on: many primitives sharing one `BatchKey` collapse into the one `BatchId`
/// that draws them.)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BatchId(pub u32);

/// Which built-in pipeline a batch draws through — the readable discriminator
/// for a batch dump, mirroring the UI `InspectKind::label`. The concrete
/// [`PipelineId`] is carried alongside it in [`InspectBatch`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BatchPipeline {
    /// A run of adjacent quads (the quad pipeline).
    Quad,
    /// A single textured image (the image pipeline).
    Image,
    /// One run of SDF glyphs (the glyph pipeline).
    GlyphRun,
    /// A run of triangle meshes — `Path`/`Mesh`, the direct-geometry pipeline.
    Mesh,
}

impl BatchPipeline {
    /// The lowercase label used in a batch dump (`quad`, `image`, …), mirroring
    /// the UI `InspectKind::label`.
    pub fn label(self) -> &'static str {
        match self {
            BatchPipeline::Quad => "quad",
            BatchPipeline::Image => "image",
            BatchPipeline::GlyphRun => "glyph",
            BatchPipeline::Mesh => "mesh",
        }
    }

    /// The batch-planner family this pipeline draws through. `inspect_primitives`
    /// packs its merge key from this so its adjacency decision routes through the
    /// same [`joins`](crate::batch::joins) predicate the lowering uses.
    fn family(self) -> BatchFamily {
        match self {
            BatchPipeline::Quad => BatchFamily::Quad,
            BatchPipeline::Image => BatchFamily::Image,
            BatchPipeline::GlyphRun => BatchFamily::GlyphRun,
            BatchPipeline::Mesh => BatchFamily::Mesh,
        }
    }
}

/// One batch's snapshot: its identity, which pipeline it draws through, the
/// resource it binds, the geometry range it covers, its clip, and whether it
/// belongs to an offscreen pass.
///
/// All fields are read straight from the corresponding draw segment; a snapshot
/// derives no new behavior, it only reports what the frame will encode.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InspectBatch {
    /// The batch this row snapshots (its index into the segment list).
    pub id: BatchId,
    /// Which built-in pipeline this batch draws through (the readable label).
    pub pipeline: BatchPipeline,
    /// The concrete pipeline handle, resolved by the exact `kind -> *_pipeline`
    /// mapping the encoder uses.
    pub pipeline_id: PipelineId,
    /// The bind group this batch samples — `Some` for image/glyph batches
    /// (their texture/atlas), `None` for quad/mesh batches (no bound resource).
    pub bind_group: Option<BindGroupId>,
    /// The half-open geometry range `(start, count)` this batch covers in its
    /// buffer. `count` is instances for quad/image/glyph batches, **indices**
    /// for mesh batches — the same caveat `FrameStats::instances` carries.
    pub range: (u32, u32),
    /// The effective clip rect, or `None` for an unclipped batch.
    pub clip: Option<Rect>,
    /// Whether this batch draws into an offscreen pass (a translucent layer's
    /// render-to-texture) rather than the main surface pass.
    pub offscreen: bool,
}

/// A flat snapshot of a frame's draw batches: `batches[i]` is the batch
/// addressed by `BatchId(i)`, in submission order. Flat storage keeps it
/// snapshot-friendly and mirrors the UI node-tree introspection convention.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct InspectBatches {
    /// The batches in submission order; `batches[i]` is `BatchId(i)`.
    pub batches: Vec<InspectBatch>,
}

impl InspectBatches {
    /// The number of batches in the snapshot.
    pub fn len(&self) -> usize {
        self.batches.len()
    }

    /// Whether the snapshot has no batches.
    pub fn is_empty(&self) -> bool {
        self.batches.is_empty()
    }

    /// Find a batch by its [`BatchId`], if present.
    pub fn get(&self, id: BatchId) -> Option<&InspectBatch> {
        self.batches.get(id.0 as usize)
    }

    /// The draw-call count this snapshot represents — one draw command per
    /// batch, so exactly `batches.len()`. Equal to
    /// [`FrameStats::draw_calls`](crate::FrameStats::draw_calls) read in the same
    /// window (both count every segment across all passes, composites included).
    pub fn draw_calls(&self) -> usize {
        self.batches.len()
    }

    /// The geometry-unit total this snapshot represents — the sum of every
    /// batch's `count`. Equal to
    /// [`FrameStats::instances`](crate::FrameStats::instances) read in the same
    /// window (instances for quad/image/glyph batches, indices for mesh
    /// batches).
    pub fn instances(&self) -> usize {
        self.batches.iter().map(|b| b.range.1 as usize).sum()
    }

    /// A stable, one-line-per-batch text rendering for a golden dump: each line
    /// carries the id, pipeline label, range, bind group, clip, and offscreen
    /// flag, so a snapshot test reads as a readable batch list rather than a
    /// debug blob.
    pub fn dump(&self) -> String {
        use core::fmt::Write as _;

        let mut out = String::new();
        for b in &self.batches {
            let _ = write!(
                out,
                "#{} {} range={}..{}",
                b.id.0,
                b.pipeline.label(),
                b.range.0,
                b.range.0 + b.range.1,
            );
            if let Some(bg) = b.bind_group {
                let _ = write!(out, " bind={}", bg.index);
            }
            if let Some(c) = b.clip {
                let _ = write!(out, " clip=[{:.0},{:.0} {:.0}x{:.0}]", c.x, c.y, c.w, c.h);
            }
            if b.offscreen {
                out.push_str(" offscreen");
            }
            out.push('\n');
        }
        out
    }
}

/// One retained primitive's contribution to the frame's geometry: which
/// [`BatchId`] draws it, through which pipeline, and the half-open geometry
/// sub-range it occupies inside that batch's buffer.
///
/// This is the `PrimitiveId -> ranges` view architecture section 62 names,
/// the retained-scene counterpart to [`InspectBatch`]'s `BatchId -> resources`.
/// A tool can ask "where did this primitive end up?" and get the exact draw and
/// buffer slice, without unsafe memory poking.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PrimitiveRange {
    /// The retained primitive this row describes (its paint-order identity).
    /// A composite (a per-frame derived layer draw, not a retained store slot)
    /// still occupies a paint-order position and is reported with that
    /// positional id.
    pub id: PrimitiveId,
    /// Which built-in pipeline this primitive lowers through (the readable
    /// label), matching the [`BatchPipeline`] of the batch it lands in.
    pub pipeline: BatchPipeline,
    /// The batch this primitive was merged into — the draw command that emits
    /// it. Adjacent same-kind primitives under one clip/target share a batch.
    pub batch: BatchId,
    /// The half-open geometry range `(start, count)` this primitive occupies in
    /// its buffer, in the same units as [`InspectBatch::range`]: instances for
    /// quad/image/glyph/composite primitives, **indices** for path/mesh
    /// primitives. Always a sub-range of its batch's range.
    pub range: (u32, u32),
}

/// A flat snapshot mapping each retained primitive, in paint order, to the
/// batch and geometry range that draws it: `primitives[i]` is the `i`-th
/// emitted primitive. The retained-scene analogue of [`InspectBatches`].
#[derive(Debug, Clone, PartialEq, Default)]
pub struct InspectPrimitives {
    /// The primitives in paint order; `primitives[i]` is the `i`-th emit.
    pub primitives: Vec<PrimitiveRange>,
}

impl InspectPrimitives {
    /// The number of primitives in the snapshot.
    pub fn len(&self) -> usize {
        self.primitives.len()
    }

    /// Whether the snapshot has no primitives.
    pub fn is_empty(&self) -> bool {
        self.primitives.is_empty()
    }

    /// Find a primitive's range by its [`PrimitiveId`], if present. Resolves by
    /// the positional paint-order index the id carries.
    pub fn get(&self, id: PrimitiveId) -> Option<&PrimitiveRange> {
        self.primitives.get(id.index() as usize)
    }

    /// A stable, one-line-per-primitive text rendering for a golden dump: each
    /// line carries the primitive index, pipeline label, owning batch, and
    /// geometry range, so a snapshot test reads as a readable primitive list.
    pub fn dump(&self) -> String {
        use core::fmt::Write as _;

        let mut out = String::new();
        for p in &self.primitives {
            let _ = writeln!(
                out,
                "#{} {} batch=#{} range={}..{}",
                p.id.index(),
                p.pipeline.label(),
                p.batch.0,
                p.range.0,
                p.range.0 + p.range.1,
            );
        }
        out
    }
}

impl Renderer {
    /// Snapshot the frame's draw batches into an [`InspectBatches`].
    ///
    /// A cold-path introspection surface (architecture section 34/62): it reads
    /// the segment list [`upload`](Self::upload) built and maps each segment to
    /// an [`InspectBatch`] — deriving its pipeline label and concrete
    /// [`PipelineId`] from the segment kind exactly as the encoder does, and
    /// carrying the segment's bind group, range, clip, and pass. It reads only
    /// `&self`, so it mutates no renderer state and does not touch the steady
    /// frame path.
    ///
    /// Read it in the same window as [`frame_stats`](Self::frame_stats): after
    /// [`upload`](Self::upload) has built the segments and before
    /// [`submit`](Self::submit) consumes them. `inspect_batches().draw_calls()`
    /// and `.instances()` then equal the corresponding `FrameStats` fields.
    pub fn inspect_batches(&self) -> InspectBatches {
        let batches = self
            .segments_snapshot()
            .iter()
            .enumerate()
            .map(|(i, seg)| self.inspect_segment(BatchId(i as u32), seg))
            .collect();
        InspectBatches { batches }
    }

    /// Map one segment to its [`InspectBatch`], deriving pipeline + resource the
    /// same way [`command_for`](Self::command_for) derives its `DrawCommand`.
    fn inspect_segment(&self, id: BatchId, seg: &Segment) -> InspectBatch {
        let (pipeline, pipeline_id, bind_group) = match seg.kind {
            SegmentKind::Quad => (BatchPipeline::Quad, self.quad_pipeline_id(), None),
            SegmentKind::Image { bind_group } => (
                BatchPipeline::Image,
                self.image_pipeline_id(),
                Some(bind_group),
            ),
            SegmentKind::GlyphRun { bind_group } => (
                BatchPipeline::GlyphRun,
                self.glyph_pipeline_id(),
                Some(bind_group),
            ),
            SegmentKind::Mesh => (BatchPipeline::Mesh, self.mesh_pipeline_id(), None),
        };
        InspectBatch {
            id,
            pipeline,
            pipeline_id,
            bind_group,
            range: (seg.start, seg.count),
            clip: seg.clip,
            offscreen: matches!(seg.target, PassTarget::Offscreen(_)),
        }
    }

    /// Snapshot every retained primitive's geometry range into an
    /// [`InspectPrimitives`] — the `PrimitiveId -> ranges` view (architecture
    /// section 34/62).
    ///
    /// A cold-path introspection surface: it replays the retained scene's
    /// paint-order record exactly as [`lower_from_scene`](Self::lower_from_scene)
    /// does — same per-kind buffer cursors, same same-kind/clip/target segment
    /// merge — but attributes the buffer range to each primitive instead of
    /// building GPU scratch. The result therefore lines up 1:1 with
    /// [`inspect_batches`](Self::inspect_batches): a primitive's `range` is a
    /// sub-range of `batches[primitive.batch].range`, and reading both in the
    /// same window (after [`upload`](Self::upload), before
    /// [`submit`](Self::submit)) yields consistent numbers.
    ///
    /// It reads only `&self`, mutating no renderer state, and never runs on the
    /// steady frame path.
    pub fn inspect_primitives(&self) -> InspectPrimitives {
        // Per-kind running buffer cursors, mirroring `lower_from_scene`: quads,
        // images (also composites), and glyphs count in instances; paths and
        // meshes share the mesh index buffer, counted in indices.
        let mut quad_cursor: u32 = 0;
        let mut image_cursor: u32 = 0;
        let mut glyph_cursor: u32 = 0;
        let mut index_cursor: u32 = 0;

        // The batch item the last emit landed in, so a mergeable run can
        // recognise the next primitive joins it and reuse its BatchId. `None`
        // before the first emit.
        let mut last_item: Option<BatchItem> = None;
        let mut batch: i64 = -1;

        let mut primitives = Vec::with_capacity(self.scene_snapshot().paint_order.len());
        for entry in &self.scene_snapshot().paint_order {
            let clip = entry.context.clip;
            let target = match entry.context.offscreen {
                None => PassTarget::Main,
                Some(idx) => PassTarget::Offscreen(idx),
            };
            let (pipeline, kind, start, count) = match entry.store {
                StoreRef::Quad(_) => {
                    let start = quad_cursor;
                    quad_cursor += 1;
                    (BatchPipeline::Quad, SegmentKind::Quad, start, 1)
                }
                StoreRef::Image(_) | StoreRef::Composite { .. } => {
                    let start = image_cursor;
                    image_cursor += 1;
                    // Image kind carries a bind group in the segment list, but
                    // range attribution ignores it — each image is its own draw,
                    // so it never merges regardless. Use a placeholder that the
                    // merge test below treats as non-mergeable.
                    (BatchPipeline::Image, SegmentKind::Quad, start, 1)
                }
                StoreRef::GlyphRun(run) => {
                    let e = self
                        .scene_snapshot()
                        .glyph_runs
                        .run(run)
                        .expect("glyph run slot");
                    let start = glyph_cursor;
                    glyph_cursor += e.count;
                    (BatchPipeline::GlyphRun, SegmentKind::Quad, start, e.count)
                }
                StoreRef::Path(id) => {
                    let e = self.scene_snapshot().paths.get(id).expect("path slot");
                    let start = index_cursor;
                    let count = e.indices.len() as u32;
                    index_cursor += count;
                    (BatchPipeline::Mesh, SegmentKind::Mesh, start, count)
                }
                StoreRef::Mesh(id) => {
                    let e = self.scene_snapshot().meshes.get(id).expect("mesh slot");
                    let start = index_cursor;
                    let count = e.indices.len() as u32;
                    index_cursor += count;
                    (BatchPipeline::Mesh, SegmentKind::Mesh, start, count)
                }
            };

            // A zero-count mesh (empty path/mesh) emits no segment at all
            // (`push_mesh_segment` early-returns), leaving the batch state
            // untouched. Attribute the empty range to the current batch without
            // opening a new one, so `primitive.batch` always indexes a real
            // batch (or stays −1 when nothing has been emitted yet).
            if kind == SegmentKind::Mesh && count == 0 {
                primitives.push(PrimitiveRange {
                    id: entry.id,
                    pipeline,
                    batch: BatchId(batch.max(0) as u32),
                    range: (start, 0),
                });
                continue;
            }

            // A primitive merges into the previous batch exactly when the
            // planner's `joins` predicate holds for their batch items — the same
            // predicate `lower_from_scene` routes every segment through. The
            // resource is left `None`: it never affects the decision (quads and
            // meshes bind none; images and glyph runs are unmergeable regardless).
            // Images, glyph runs, and composites are always their own draw.
            let family = pipeline.family();
            let target_field = match target {
                PassTarget::Main => BatchTarget::Main,
                PassTarget::Offscreen(i) => BatchTarget::Offscreen(i),
            };
            let item = BatchItem {
                key: BatchKey::pack(family, target_field, None),
                clip,
                mergeable: family.mergeable(),
            };
            let merges = last_item.is_some_and(|prev| joins(&prev, &item));
            if !merges {
                batch += 1;
            }
            last_item = Some(item);

            primitives.push(PrimitiveRange {
                id: entry.id,
                pipeline,
                batch: BatchId(batch as u32),
                range: (start, count),
            });
        }
        InspectPrimitives { primitives }
    }

    /// Project the frame's draw segments into the paint-order `RenderChunk`
    /// stream — the `RenderChunkId -> ranges` view (architecture section 62).
    ///
    /// A cold-path introspection surface, produced only when tooling asks. It is
    /// not a hot-path structure and does not replace the segment list: the
    /// segments stay the sole draw carrier the encoder reads. Each emitted draw
    /// becomes one [`RenderChunk`] carrying the packed [`BatchKey`] the encoder
    /// fixes — including the real bound resource for image/glyph chunks, so
    /// chunks that bind distinct textures hold distinct keys — its geometry
    /// range and clip, and the half-open paint-order span it absorbed. That span
    /// is the one piece [`InspectBatch`] cannot carry: it maps a changed
    /// paint-order position back to the single chunk that must be rebuilt.
    ///
    /// `chunks[i]` is `RenderChunkId(i)` and corresponds one-to-one with
    /// `inspect_batches().batches[i]`: identical count and boundaries, since both
    /// route every merge through the same [`joins`](crate::batch::joins)
    /// predicate. It reads only `&self`, mutates no renderer state, and never
    /// runs on the steady frame path.
    pub fn render_chunks(&self) -> Vec<RenderChunk> {
        // The paint-order walk yields, per emitted batch, the half-open span of
        // paint-order positions that batch absorbed. Batch boundaries here match
        // the segment list exactly (both merge through `joins`), so the i-th span
        // pairs with the i-th segment. An empty mesh (zero indices) emits no
        // segment and opens no batch — it is attributed to the current batch
        // without extending the order span past what a real emit already covers.
        let order_spans = self.chunk_order_spans();

        let segments = self.segments_snapshot();
        debug_assert_eq!(
            segments.len(),
            order_spans.len(),
            "one paint-order span per emitted segment"
        );

        segments
            .iter()
            .zip(order_spans)
            .map(|(seg, order)| RenderChunk {
                key: BatchKey::pack(
                    seg.kind.family(),
                    seg.target.batch_target(),
                    seg.kind.resource(),
                ),
                family: seg.kind.family(),
                clip: seg.clip,
                geometry: (seg.start, seg.count),
                order,
            })
            .collect()
    }

    /// Find a chunk by its [`RenderChunkId`], if present.
    pub fn render_chunk(&self, id: RenderChunkId) -> Option<RenderChunk> {
        self.render_chunks().get(id.0 as usize).copied()
    }

    /// Walk the paint-order record the way [`inspect_primitives`](Self::inspect_primitives)
    /// does, folding it into one half-open `(order_start, order_end)` span per
    /// emitted batch. The merge decision routes through the same
    /// [`joins`](crate::batch::joins) predicate the lowering uses, so the spans
    /// line up one-to-one with the segment list.
    fn chunk_order_spans(&self) -> Vec<(u32, u32)> {
        let mut spans: Vec<(u32, u32)> = Vec::new();
        let mut last_item: Option<BatchItem> = None;

        for (pos, entry) in self.scene_snapshot().paint_order.iter().enumerate() {
            let pos = pos as u32;
            let clip = entry.context.clip;
            let target = match entry.context.offscreen {
                None => BatchTarget::Main,
                Some(idx) => BatchTarget::Offscreen(idx),
            };

            // The family, and whether this entry emits geometry at all. An empty
            // path/mesh (zero indices) produces no segment, so it neither opens a
            // batch nor extends the current one's order span — mirroring
            // `push_mesh_segment`'s early return.
            let (family, emits) = match entry.store {
                StoreRef::Quad(_) => (BatchFamily::Quad, true),
                StoreRef::Image(_) | StoreRef::Composite { .. } => (BatchFamily::Image, true),
                StoreRef::GlyphRun(run) => {
                    let e = self
                        .scene_snapshot()
                        .glyph_runs
                        .run(run)
                        .expect("glyph run slot");
                    (BatchFamily::GlyphRun, e.count > 0)
                }
                StoreRef::Path(id) => {
                    let e = self.scene_snapshot().paths.get(id).expect("path slot");
                    (BatchFamily::Mesh, !e.indices.is_empty())
                }
                StoreRef::Mesh(id) => {
                    let e = self.scene_snapshot().meshes.get(id).expect("mesh slot");
                    (BatchFamily::Mesh, !e.indices.is_empty())
                }
            };
            if !emits {
                continue;
            }

            // The resource never affects the boundary: quads/meshes bind none, and
            // images/glyph runs are unmergeable regardless. Pack `None` here so
            // the merge decision matches `inspect_primitives`; the emitted chunk
            // carries the real resource from its segment.
            let item = BatchItem {
                key: BatchKey::pack(family, target, None),
                clip,
                mergeable: family.mergeable(),
            };
            let merges = last_item.is_some_and(|prev| joins(&prev, &item));
            if merges {
                if let Some(span) = spans.last_mut() {
                    span.1 = pos + 1;
                }
            } else {
                spans.push((pos, pos + 1));
            }
            last_item = Some(item);
        }
        spans
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitive::{
        Border, GlyphInstanceData, GlyphRunDraw, ImageDraw, LayerClip, Primitive, Quad, Rgba,
    };
    use viso_gpu::{
        GpuBackend, HeadlessRaster, RawWindowHandle, TextureDesc, TextureFormat, TextureId,
    };

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

    /// Build a renderer over a headless surface, upload `prims`, and hand the
    /// renderer to `f` so it can read `inspect_batches`/`frame_stats` in the same
    /// window (after `upload`, before `submit`).
    fn with_upload(prims: &[Primitive], f: impl FnOnce(&Renderer)) {
        let mut gpu = HeadlessRaster::new();
        let surface = gpu.create_surface(RawWindowHandle::Headless, 128, 128);
        let format = gpu.surface_format(surface);
        let mut r = Renderer::new(&mut gpu, format);
        r.upload(&mut gpu, prims);
        f(&r);
    }

    /// A 2×2 BGRA test texture, so image batches have a real texture to bind.
    fn make_texture(gpu: &mut HeadlessRaster) -> TextureId {
        gpu.create_texture(&TextureDesc {
            width: 2,
            height: 2,
            format: TextureFormat::Bgra8Unorm,
            render_target: false,
            label: "inspect-test",
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

    fn glyph_run(atlas: TextureId) -> Primitive {
        Primitive::GlyphRun(GlyphRunDraw {
            glyphs: vec![GlyphInstanceData {
                rect: Rect {
                    x: 0.0,
                    y: 0.0,
                    w: 6.0,
                    h: 8.0,
                },
                uv: Rect {
                    x: 0.0,
                    y: 0.0,
                    w: 0.1,
                    h: 0.1,
                },
            }],
            atlas,
            color: Rgba {
                r: 1.0,
                g: 1.0,
                b: 1.0,
                a: 1.0,
            },
        })
    }

    #[test]
    fn batches_snapshot_matches_frame_stats() {
        // A no-translucent-layer scene: two adjacent quads (one batch, two
        // instances). The snapshot's cross-check helpers equal `FrameStats`.
        with_upload(&[quad(0.0, 0.0), quad(20.0, 20.0)], |r| {
            let batches = r.inspect_batches();
            let stats = r.frame_stats();
            assert_eq!(batches.draw_calls(), stats.draw_calls);
            assert_eq!(batches.instances(), stats.instances);
            assert_eq!(batches.len(), 1);
            assert_eq!(batches.get(BatchId(0)).unwrap().range, (0, 2));
        });
    }

    #[test]
    fn pipeline_is_derived_per_kind() {
        let mut gpu = HeadlessRaster::new();
        let surface = gpu.create_surface(RawWindowHandle::Headless, 128, 128);
        let format = gpu.surface_format(surface);
        let tex = make_texture(&mut gpu);
        let atlas = gpu.create_texture(&TextureDesc {
            width: 4,
            height: 4,
            format: TextureFormat::R8Unorm,
            render_target: false,
            label: "inspect-atlas",
        });
        let mut r = Renderer::new(&mut gpu, format);
        // Quad, then glyph, then image — three distinct pipelines in order.
        r.upload(&mut gpu, &[quad(0.0, 0.0), glyph_run(atlas), image(tex)]);

        let batches = r.inspect_batches();
        assert_eq!(batches.len(), 3);

        let q = batches.get(BatchId(0)).unwrap();
        assert_eq!(q.pipeline, BatchPipeline::Quad);
        assert_eq!(q.pipeline_id, r.quad_pipeline_id());
        assert_eq!(q.bind_group, None);

        let g = batches.get(BatchId(1)).unwrap();
        assert_eq!(g.pipeline, BatchPipeline::GlyphRun);
        assert_eq!(g.pipeline_id, r.glyph_pipeline_id());
        assert!(g.bind_group.is_some());

        let im = batches.get(BatchId(2)).unwrap();
        assert_eq!(im.pipeline, BatchPipeline::Image);
        assert_eq!(im.pipeline_id, r.image_pipeline_id());
        assert!(im.bind_group.is_some());
    }

    #[test]
    fn composite_is_an_image_batch() {
        // A translucent layer wrapping a quad: the subtree renders offscreen,
        // then composites back as a real Image batch on the main pass. The
        // cross-check with `FrameStats` still holds.
        let prims = vec![
            quad(0.0, 0.0),
            Primitive::Layer(LayerClip {
                clip: Rect {
                    x: 4.0,
                    y: 4.0,
                    w: 20.0,
                    h: 20.0,
                },
                opacity: 0.5,
            }),
            quad(4.0, 4.0),
            Primitive::LayerEnd,
        ];
        with_upload(&prims, |r| {
            let batches = r.inspect_batches();
            let stats = r.frame_stats();
            assert_eq!(batches.draw_calls(), stats.draw_calls);
            assert_eq!(batches.instances(), stats.instances);

            // Exactly one batch draws into an offscreen pass (the layer's quad),
            // and at least one Image batch lands on the main pass (the composite).
            assert!(batches.batches.iter().any(|b| b.offscreen));
            assert!(
                batches
                    .batches
                    .iter()
                    .any(|b| !b.offscreen && b.pipeline == BatchPipeline::Image)
            );
        });
    }

    #[test]
    fn primitives_map_into_their_batches() {
        // Two adjacent quads merge into one batch; each primitive owns a
        // one-instance sub-range of it, in order.
        with_upload(&[quad(0.0, 0.0), quad(20.0, 20.0)], |r| {
            let prims = r.inspect_primitives();
            let batches = r.inspect_batches();
            assert_eq!(prims.len(), 2);

            let p0 = &prims.primitives[0];
            let p1 = &prims.primitives[1];
            assert_eq!(p0.pipeline, BatchPipeline::Quad);
            assert_eq!(p0.batch, BatchId(0));
            assert_eq!(p0.range, (0, 1));
            assert_eq!(p1.batch, BatchId(0));
            assert_eq!(p1.range, (1, 1));

            // Every primitive's range is a sub-range of its batch's range, and
            // the batch's instance count is the sum of its primitives'.
            for p in &prims.primitives {
                let b = batches.get(p.batch).unwrap();
                assert!(p.range.0 >= b.range.0);
                assert!(p.range.0 + p.range.1 <= b.range.0 + b.range.1);
            }
            let summed: u32 = prims
                .primitives
                .iter()
                .filter(|p| p.batch == BatchId(0))
                .map(|p| p.range.1)
                .sum();
            assert_eq!(summed, batches.get(BatchId(0)).unwrap().range.1);
        });
    }

    #[test]
    fn primitive_get_resolves_by_id() {
        with_upload(&[quad(0.0, 0.0), quad(20.0, 20.0)], |r| {
            let prims = r.inspect_primitives();
            let id = prims.primitives[1].id;
            assert_eq!(prims.get(id), Some(&prims.primitives[1]));
        });
    }

    #[test]
    fn distinct_kinds_are_distinct_batches() {
        let mut gpu = HeadlessRaster::new();
        let surface = gpu.create_surface(RawWindowHandle::Headless, 128, 128);
        let format = gpu.surface_format(surface);
        let tex = make_texture(&mut gpu);
        let atlas = gpu.create_texture(&TextureDesc {
            width: 4,
            height: 4,
            format: TextureFormat::R8Unorm,
            render_target: false,
            label: "inspect-atlas",
        });
        let mut r = Renderer::new(&mut gpu, format);
        r.upload(&mut gpu, &[quad(0.0, 0.0), glyph_run(atlas), image(tex)]);

        let prims = r.inspect_primitives();
        assert_eq!(prims.len(), 3);
        assert_eq!(prims.primitives[0].pipeline, BatchPipeline::Quad);
        assert_eq!(prims.primitives[0].batch, BatchId(0));
        assert_eq!(prims.primitives[1].pipeline, BatchPipeline::GlyphRun);
        assert_eq!(prims.primitives[1].batch, BatchId(1));
        assert_eq!(prims.primitives[2].pipeline, BatchPipeline::Image);
        assert_eq!(prims.primitives[2].batch, BatchId(2));

        // Each primitive resolves to the batch of the matching pipeline.
        let batches = r.inspect_batches();
        for p in &prims.primitives {
            assert_eq!(batches.get(p.batch).unwrap().pipeline, p.pipeline);
        }
    }

    #[test]
    fn composite_takes_a_primitive_row() {
        // A translucent layer wrapping a quad: the offscreen quad is one
        // primitive, and the composite that samples the pass takes its own
        // paint-order row as an Image primitive on the main pass.
        let prims = vec![
            quad(0.0, 0.0),
            Primitive::Layer(LayerClip {
                clip: Rect {
                    x: 4.0,
                    y: 4.0,
                    w: 20.0,
                    h: 20.0,
                },
                opacity: 0.5,
            }),
            quad(4.0, 4.0),
            Primitive::LayerEnd,
        ];
        with_upload(&prims, |r| {
            let p = r.inspect_primitives();
            let batches = r.inspect_batches();
            // Every primitive row points at a real batch.
            for pr in &p.primitives {
                assert!(batches.get(pr.batch).is_some());
            }
            // At least one row is an Image (the composite) drawn by an
            // image batch on the main pass.
            assert!(p.primitives.iter().any(|pr| {
                pr.pipeline == BatchPipeline::Image && !batches.get(pr.batch).unwrap().offscreen
            }));
        });
    }

    #[test]
    fn primitive_dump_is_stable() {
        let prims = vec![
            quad(0.0, 0.0),
            Primitive::Layer(LayerClip {
                clip: Rect {
                    x: 5.0,
                    y: 5.0,
                    w: 30.0,
                    h: 30.0,
                },
                opacity: 1.0,
            }),
            quad(10.0, 10.0),
            Primitive::LayerEnd,
        ];
        with_upload(&prims, |r| {
            let dump = r.inspect_primitives().dump();
            let expected = "\
#0 quad batch=#0 range=0..1
#1 quad batch=#1 range=1..2
";
            assert_eq!(dump, expected);
        });
    }

    #[test]
    fn dump_is_stable() {
        // A quad then a clipped quad: one unclipped batch, one clipped batch.
        let prims = vec![
            quad(0.0, 0.0),
            Primitive::Layer(LayerClip {
                clip: Rect {
                    x: 5.0,
                    y: 5.0,
                    w: 30.0,
                    h: 30.0,
                },
                opacity: 1.0,
            }),
            quad(10.0, 10.0),
            Primitive::LayerEnd,
        ];
        with_upload(&prims, |r| {
            let dump = r.inspect_batches().dump();
            let expected = "\
#0 quad range=0..1
#1 quad range=1..2 clip=[5,5 30x30]
";
            assert_eq!(dump, expected);
        });
    }
}
