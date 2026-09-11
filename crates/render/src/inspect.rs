//! Read-only batch introspection for the inspector, Studio, and tests.
//!
//! Architecture section 34/62 ask that a `BatchId`'s pipeline and resources be
//! inspectable without unsafe memory poking — through the same model the
//! renderer itself uses, so a tool, a headless golden test, and an AI automation
//! all read one contract. [`Renderer::inspect_batches`] snapshots the frame's
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

use crate::Rect;
use crate::Renderer;
use crate::renderer::{PassTarget, Segment, SegmentKind};
use viso_gpu::{BindGroupId, PipelineId};

/// A batch's index into the frame's segment list — the stable handle
/// architecture section 62 names for `BatchId -> pipeline/resources`.
///
/// `BatchId(i)` addresses `inspect_batches().batches[i]`, the `i`-th draw
/// command the frame will encode, in submission order. (Distinct from the
/// unused [`BatchKey`](crate::BatchKey), which is not the batch identity in
/// play.)
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
                let _ = write!(out, " bind={}", bg.0);
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
