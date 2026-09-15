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
//!
//! # Staged migration
//!
//! F3.1 (this stage) runs the scene as a **shadow**: the immediate walk in
//! `Renderer::upload` stays authoritative and produces the scratch buffers that
//! actually drive the GPU, while [`Scene::ingest`] rebuilds the retained stores
//! from the same primitives and records, per emitted primitive, the store slot
//! it landed in and the lowering context (`clip`, `target`, `origin`) resolved
//! for it. A debug-only check ([`Scene::rederive_into`]) then replays that
//! paint-order record back into scratch + segments and asserts it is
//! byte-identical to what the immediate walk produced — proving the retained
//! model is a faithful mirror before F3.2/F3.3 make it load-bearing.
//!
//! The paint-order record is the bridge: it captures paint order (a correctness
//! contract, §8.6) and the per-emit context the stores deliberately do *not*
//! bake into their entries, so one geometry can be re-lowered under different
//! layer origins across frames.

pub mod bounds;
pub mod ids;
pub mod revision;
pub mod store;

use crate::primitive::Rect;

use bounds::Bounds;
use ids::PrimitiveId;
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
/// The record is the retained scene's paint-ordered spine. F3.1 replays it to
/// re-derive scratch; F3.2 diffs it against the previous frame to assign stable
/// [`PrimitiveId`]s and bump only the revision planes a change touched.
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
/// Owned by the [`Renderer`](crate::renderer::Renderer) and rebuilt each frame
/// from the ingested primitive stream. Storage is cleared-not-freed via
/// [`Scene::begin_frame`], so a steady-state scene of the same shape reuses last
/// frame's allocations and the counting-allocator bench stays flat (§28).
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
}

impl Scene {
    /// A fresh, empty scene.
    pub fn new() -> Scene {
        Scene::default()
    }

    /// Clear every store and the paint-order record for a new frame, keeping
    /// backing capacity. Revision planes persist across frames (they are the
    /// running history a consumer compares against).
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
