//! The retained scene (§8): the renderer's internal, frame-persistent model of
//! what is on screen, built *under* the frozen immediate-mode `upload`/`submit`
//! API. The renderer still ingests the same flat `&[Primitive]` stream every
//! frame; the scene is where that stream is retained so a later stage can diff
//! it and touch only what changed instead of rebuilding from scratch.
//!
//! # Structure
//!
//! - [`ids`] — typed dense generational handles (`PrimitiveId`, `TransformId`, …)
//!   addressing store slots, mirroring the GPU RHI handle shape.
//! - [`store`] — per-kind compact stores (quads, images, glyph runs, paths,
//!   meshes) plus the identity-separated transform/brush/clip stores.
//! - [`revision`] — the seven independent monotone revision planes a diff bumps.
//! - [`bounds`] — the five-stage bounds a primitive carries.
//! - [`ingest`] — the per-frame diff walk that folds the primitive stream into
//!   the stores, bumping only the revision planes a change moved.
//!
//! # Source of truth
//!
//! The scene is authoritative. `Renderer::upload` walks the primitive stream only
//! to resolve each primitive's lowering context from the layer stack and hand it
//! to the scene through one [`ingest`] call per kind ([`Scene::ingest_quad`], …),
//! which diffs it against the retained store entry at its positional slot, mutates
//! the store only where a field moved, bumps only the revision planes that change
//! touched (§8.4), and records the store slot + lowering context (`clip`,
//! `target`, `origin`) in paint order. The GPU scratch and draw segments are then
//! derived from the retained scene by replaying the paint-order record — the walk
//! no longer produces them directly (§8).
//!
//! The stores **persist across frames**: [`Scene::begin_frame`] resets each
//! store's cursor to zero (it does not clear entries), the ingest walk re-visits
//! the slots in paint order, and [`Scene::finish_frame`] trims any tail the walk
//! did not reach (the scene shrank). An unchanged scene therefore mutates no
//! store and bumps no plane — the "0 primitive reconstruction" guarantee under a
//! whole-tree re-emit (§8.4).
//!
//! The paint-order record is the bridge: it captures paint order (a correctness
//! contract, §8.6) and the per-emit context the stores deliberately do *not*
//! bake into their entries, so one geometry can be re-lowered under different
//! layer origins across frames.

pub mod bounds;
pub mod ids;
pub mod ingest;
pub mod revision;
pub mod store;

use crate::primitive::Rect;

use bounds::Bounds;
use ids::PrimitiveId;
use ingest::IngestStats;
use revision::Revisions;
use store::{
    AnalyticCapsuleStore, AnalyticEllipseStore, AnalyticLineStore, AnalyticRRectStore,
    AnalyticShadowStore, BrushStore, ClipChainStore, ClipStore, GlyphRunStore, GradientStore,
    ImageStore, MeshStore, SolidQuadStore, StoreRef, TransformStore, VectorPathStore,
};

/// Where an emitted primitive's geometry is routed and how it is offset — the
/// lowering context the immediate walk resolves from its layer stack, captured
/// per emit so re-derivation reproduces the same origin subtraction, clip, and
/// pass assignment the immediate walk applied.
///
/// This is `render`-local and mirrors the renderer's own `(clip, target,
/// origin)`; `target` is stored as its raw discriminant so the scene module
/// need not depend on the renderer's private `PassTarget` type.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EmitContext {
    /// The effective clip in the emit's own space (world space for the main
    /// pass, texture-local for an offscreen pass), or `None` for unclipped.
    pub clip: Option<Rect>,
    /// The pass this emit routes to: `None` for the main surface pass, or
    /// `Some(idx)` for offscreen pass `idx`.
    pub offscreen: Option<usize>,
    /// The world-space origin subtracted from this emit's geometry.
    pub origin: [f32; 2],
}

/// One entry in the paint-order record: which store slot the Nth emitted
/// primitive landed in, the context it was lowered under, and its bounds.
///
/// The record is the retained scene's paint-ordered spine: it is replayed each
/// frame to derive the GPU scratch + draw segments, and diffed against the
/// previous frame to assign stable [`PrimitiveId`]s and bump only the revision
/// planes a change touched.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PaintEntry {
    /// This primitive's stable identity (positional this stage).
    pub id: PrimitiveId,
    /// The store slot holding this primitive's retained data.
    pub store: StoreRef,
    /// The lowering context resolved for this emit.
    pub context: EmitContext,
    /// The primitive's five-stage bounds.
    pub bounds: Bounds,
}

/// The retained scene: the per-kind stores, the revision planes, and the
/// paint-order record tying them together (§8).
///
/// Owned by the [`Renderer`](crate::renderer::Renderer) and diffed each frame
/// against the ingested primitive stream. Storage persists across frames;
/// [`Scene::begin_frame`] only resets the per-store cursors, so a steady-state
/// scene of the same shape mutates nothing, reuses last frame's allocations, and
/// keeps the counting-allocator bench flat (§28).
#[derive(Debug, Default)]
pub struct Scene {
    /// Solid/bordered quads, in paint order.
    pub quads: SolidQuadStore,
    /// Analytic rounded rectangles (per-corner radius), in paint order.
    pub analytic_rrects: AnalyticRRectStore,
    /// Analytic ellipses, in paint order.
    pub analytic_ellipses: AnalyticEllipseStore,
    /// Analytic capsules, in paint order.
    pub analytic_capsules: AnalyticCapsuleStore,
    /// Analytic lines, in paint order.
    pub analytic_lines: AnalyticLineStore,
    /// Image draws.
    pub images: ImageStore,
    /// Gradient fills, each carrying its baked 1D LUT texture.
    pub gradients: GradientStore,
    /// Analytic soft drop shadows (rounded box / ellipse / capsule), in paint order.
    pub analytic_shadows: AnalyticShadowStore,
    /// Glyph runs and their packed instances.
    pub glyph_runs: GlyphRunStore,
    /// Vector paths with their cached tessellation.
    pub paths: VectorPathStore,
    /// Caller-supplied triangle meshes.
    pub meshes: MeshStore,
    /// Effective clip rects (identity-separated).
    pub clips: ClipStore,
    /// Resolved clip chains (§14.2): nested clips pre-intersected once into a
    /// descriptor keyed so an unchanged chain skips re-resolution.
    pub clip_chains: ClipChainStore,
    /// Transforms (identity-separated; a pure move bumps this plane alone).
    pub transforms: TransformStore,
    /// Brushes (identity-separated; a recolor bumps this plane alone).
    pub brushes: BrushStore,
    /// The seven independent revision planes.
    pub revisions: Revisions,
    /// One [`PaintEntry`] per emitted primitive, in paint order.
    pub paint_order: Vec<PaintEntry>,
    /// One content stamp per paint-order slot: the value of
    /// [`Revisions::content`] at the last frame in which *that slot's* primitive
    /// changed (§3202).
    ///
    /// Unlike `paint_order`, this vector is **retained across frames** — it is the
    /// history a ROI-scoped consumer diffs against. A backdrop capture's
    /// dependency is "the pixels under this rect", so it takes the maximum stamp
    /// over the slots whose paint bounds hit its ROI: the stamp moves when the
    /// content behind *that* panel moves and stays put when some unrelated part of
    /// the scene changes, which is precisely the property that keeps one dirty
    /// backdrop from invalidating every other one.
    ///
    /// Positional, like the stores themselves: removing a primitive shifts every
    /// later slot, and the shifted slots diff dirty against their new contents, so
    /// the stamps follow the shift rather than aliasing across it.
    content_stamps: Vec<u64>,
    /// Whether the primitive currently being recorded had any dirty plane. `None`
    /// means no plane report arrived for it — the case for a composite the
    /// renderer synthesizes, which is conservatively treated as dirty because its
    /// pixels come from a pass rather than from a diffed store.
    pending_dirty: Option<bool>,
    /// Per-frame ingest tallies (§61), reset at [`Scene::begin_frame`].
    pub ingest_stats: IngestStats,
}

impl Scene {
    /// A fresh, empty scene.
    pub fn new() -> Scene {
        Scene::default()
    }

    /// Start a new frame: reset every store's cursor (entries persist to diff
    /// against), reset the per-frame ingest counters, and clear the paint-order
    /// record. Revision planes persist across frames — they are the running
    /// history a consumer compares against.
    pub fn begin_frame(&mut self) {
        self.quads.begin_frame();
        self.analytic_rrects.begin_frame();
        self.analytic_ellipses.begin_frame();
        self.analytic_capsules.begin_frame();
        self.analytic_lines.begin_frame();
        self.images.begin_frame();
        self.gradients.begin_frame();
        self.analytic_shadows.begin_frame();
        self.glyph_runs.begin_frame();
        self.paths.begin_frame();
        self.meshes.begin_frame();
        self.clips.begin_frame();
        self.clip_chains.begin_frame();
        self.transforms.begin_frame();
        self.brushes.begin_frame();
        self.paint_order.clear();
        self.begin_ingest();
    }

    /// Finish the frame's ingest walk: trim every store's tail past its cursor
    /// (the scene shrank) and bump the visibility plane if anything was trimmed.
    /// Called by the renderer after the immediate walk has ingested every
    /// primitive.
    pub fn finish_frame(&mut self) {
        self.finish_ingest();
        // The scene shrank: drop the stamps of slots that no longer exist, so a
        // later frame that grows back into them sees new content rather than a
        // stale stamp from two shapes ago.
        self.content_stamps.truncate(self.paint_order.len());
        self.pending_dirty = None;
    }

    /// Record one emitted primitive: its store slot, lowering context, and
    /// bounds, assigning it the next positional [`PrimitiveId`]. Returns the id.
    pub fn record(&mut self, store: StoreRef, context: EmitContext, bounds: Bounds) -> PrimitiveId {
        let index = self.paint_order.len();
        let id = store::primitive_id(index);
        self.paint_order.push(PaintEntry {
            id,
            store,
            context,
            bounds,
        });
        // A slot that did not exist last frame is new content by definition; an
        // existing slot keeps its old stamp unless something about it moved. The
        // `None` case is a synthesized composite — no store diffed it, so it is
        // assumed to have changed.
        let stamp = self.revisions.content();
        match self.content_stamps.get_mut(index) {
            Some(slot) => {
                if self.pending_dirty.take().unwrap_or(true) {
                    *slot = stamp;
                }
            }
            None => {
                self.pending_dirty = None;
                self.content_stamps.push(stamp);
            }
        }
        id
    }

    /// The content stamp of paint-order slot `index` (§3202): the
    /// [`Revisions::content`] value as of the last frame that slot's primitive
    /// changed. Out-of-range slots report `0`, which compares as "older than
    /// anything", so a consumer never mistakes a missing slot for fresh content.
    #[inline]
    pub fn content_stamp(&self, index: usize) -> u64 {
        self.content_stamps.get(index).copied().unwrap_or(0)
    }

    /// Record that the primitive about to be recorded had (or had not) a dirty
    /// plane this frame. Called by the ingest lane; a composite recorded without
    /// one is conservatively dirty.
    #[inline]
    pub(crate) fn set_pending_dirty(&mut self, dirty: bool) {
        self.pending_dirty = Some(dirty);
    }
}
