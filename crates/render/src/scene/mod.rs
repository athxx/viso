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
    BrushStore, ClipStore, GlyphRunStore, ImageStore, MeshStore, SolidQuadStore, StoreRef,
    TransformStore, VectorPathStore,
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
    /// Image draws.
    pub images: ImageStore,
    /// Glyph runs and their packed instances.
    pub glyph_runs: GlyphRunStore,
    /// Vector paths with their cached tessellation.
    pub paths: VectorPathStore,
    /// Caller-supplied triangle meshes.
    pub meshes: MeshStore,
    /// Effective clip rects (identity-separated).
    pub clips: ClipStore,
    /// Transforms (identity-separated; a pure move bumps this plane alone).
    pub transforms: TransformStore,
    /// Brushes (identity-separated; a recolor bumps this plane alone).
    pub brushes: BrushStore,
    /// The seven independent revision planes.
    pub revisions: Revisions,
    /// One [`PaintEntry`] per emitted primitive, in paint order.
    pub paint_order: Vec<PaintEntry>,
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
        self.images.begin_frame();
        self.glyph_runs.begin_frame();
        self.paths.begin_frame();
        self.meshes.begin_frame();
        self.clips.begin_frame();
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
    }

    /// Record one emitted primitive: its store slot, lowering context, and
    /// bounds, assigning it the next positional [`PrimitiveId`]. Returns the id.
    pub fn record(&mut self, store: StoreRef, context: EmitContext, bounds: Bounds) -> PrimitiveId {
        let id = store::primitive_id(self.paint_order.len());
        self.paint_order.push(PaintEntry {
            id,
            store,
            context,
            bounds,
        });
        id
    }
}
