//! Ingest-diff: fold a frame's primitive stream into the retained stores,
//! bumping only the revision planes a field-wise change actually moved (§8.4).
//!
//! The renderer's immediate walk resolves each primitive to its lowered
//! instance and lowering context, then hands it here through one `ingest_*`
//! call per primitive kind. Each call:
//!
//! 1. diffs the incoming value against the retained store entry at its
//!    **positional slot** (the Nth primitive of a kind → slot N, stable because
//!    the sole producer re-emits the whole tree in stable pre-order every frame,
//!    `ui::component::repaint_dirty`);
//! 2. mutates the store in place *only* where a field differs, and bumps *only*
//!    the affected revision plane(s) — an unchanged primitive touches neither
//!    the store nor any plane (this is what makes "0 primitive reconstruction"
//!    hold under a whole-tree re-emit, §8.4);
//! 3. records the slot, lowering context, and bounds in the paint-order spine,
//!    assigning the stable [`PrimitiveId`], and accumulates the per-frame dirty
//!    counters ([`super::store::DirtyPlanes`] → [`IngestStats`]).
//!
//! A kind/count/sequence change at a slot is a *structural* change: the store's
//! `ingest_*` appends a fresh entry (cold growth) or, on a shrink, `finish_frame`
//! trims the tail. Those are the cold paths (§9.5); the steady path — same tree,
//! same order — appends nothing and trims nothing.
//!
//! The field → plane mapping is the primitive semantics this module owns; the
//! stores own the comparison (they own the entry layout) and report *which*
//! fields moved, but the decision that a moved `rect_pos` is a transform-plane
//! event and a moved `color` a paint-plane event lives here.

use viso_gpu::{SamplerDesc, TextureId};

use crate::primitive::{
    AnalyticCapsuleInstance, AnalyticEllipseInstance, AnalyticLineInstance, AnalyticRRectInstance,
    GlyphInstance, GradientInstance, ImageInstance, MeshVertex, Path, QuadInstance, Rect,
    ShadowInstance,
};

use super::bounds::Bounds;
use super::ids::{ClipChainId, PrimitiveId};
use super::store::{Brush, ClipChainDescriptor, ClipMaskKey, DirtyPlanes, StoreRef};
use super::{EmitContext, Scene};

/// Per-frame ingest tallies (§61), accumulated as the frame's primitives are
/// folded in. Plain integer counters — no allocation, reset each `begin_frame`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct IngestStats {
    /// Primitives visited this frame (one per `ingest_*` call, composites
    /// excluded — they are per-frame derived draws, not retained slots).
    pub visible_primitives: u32,
    /// Primitives whose diff moved at least one revision plane (a cold append
    /// counts as dirty).
    pub dirty_primitives: u32,
    /// Quad instances retained this frame.
    pub quad_instances: u32,
    /// Analytic rounded-rectangle instances retained this frame.
    pub analytic_rrect_instances: u32,
    /// Analytic ellipse instances retained this frame.
    pub analytic_ellipse_instances: u32,
    /// Analytic capsule instances retained this frame.
    pub analytic_capsule_instances: u32,
    /// Analytic line instances retained this frame.
    pub analytic_line_instances: u32,
    /// Glyph instances retained this frame (summed across runs).
    pub glyph_instances: u32,
    /// Gradient fill instances retained this frame.
    pub gradient_instances: u32,
    /// Analytic soft-shadow instances retained this frame.
    pub analytic_shadow_instances: u32,
    /// Paths (re-)tessellated this frame — a geometry or paint change on a path,
    /// or a cold append. A cache hit does not count.
    pub path_tessellations: u32,
}

impl IngestStats {
    /// Reset to zero for a new frame.
    fn clear(&mut self) {
        *self = IngestStats::default();
    }
}

impl Scene {
    /// Reset the per-frame ingest counters. Called by [`Scene::begin_frame`].
    pub(super) fn begin_ingest(&mut self) {
        self.ingest_stats.clear();
    }

    /// Bump the revision planes flagged by a store's field-wise diff, and tally
    /// the primitive as dirty if anything moved. Shared by every `ingest_*`.
    fn apply_planes(&mut self, dirty: DirtyPlanes) {
        if dirty.appended {
            // A cold structural append: the slot did not exist last frame, so
            // its geometry is new. Its paint/transform ride along with the new
            // geometry; the structural generation is the cold path's concern.
            self.revisions.bump_geometry();
        } else {
            if dirty.geometry {
                self.revisions.bump_geometry();
            }
            if dirty.paint {
                self.revisions.bump_paint();
            }
            if dirty.transform {
                self.revisions.bump_transform();
            }
            if dirty.resource {
                self.revisions.bump_resource();
            }
        }
        if dirty.any() {
            self.ingest_stats.dirty_primitives += 1;
        }
        self.ingest_stats.visible_primitives += 1;
    }

    /// Ingest a solid/bordered quad: diff into the quad store, bump the moved
    /// planes, and record its paint-order slot. Returns the stable id.
    pub fn ingest_quad(
        &mut self,
        instance: QuadInstance,
        context: EmitContext,
        bounds: Bounds,
    ) -> PrimitiveId {
        let (slot, dirty) = self.quads.ingest(instance);
        self.apply_planes(dirty);
        self.ingest_stats.quad_instances += 1;
        self.record(StoreRef::Quad(slot), context, bounds)
    }

    /// Ingest an analytic rounded rectangle: diff into the rrect store, bump the
    /// moved planes, and record its paint-order slot. Returns the stable id.
    pub fn ingest_analytic_rrect(
        &mut self,
        instance: AnalyticRRectInstance,
        context: EmitContext,
        bounds: Bounds,
    ) -> PrimitiveId {
        let (slot, dirty) = self.analytic_rrects.ingest(instance);
        self.apply_planes(dirty);
        self.ingest_stats.analytic_rrect_instances += 1;
        self.record(StoreRef::AnalyticRRect(slot), context, bounds)
    }

    /// Ingest an analytic ellipse: diff into the ellipse store, bump the moved
    /// planes, and record its paint-order slot. Returns the stable id.
    pub fn ingest_analytic_ellipse(
        &mut self,
        instance: AnalyticEllipseInstance,
        context: EmitContext,
        bounds: Bounds,
    ) -> PrimitiveId {
        let (slot, dirty) = self.analytic_ellipses.ingest(instance);
        self.apply_planes(dirty);
        self.ingest_stats.analytic_ellipse_instances += 1;
        self.record(StoreRef::AnalyticEllipse(slot), context, bounds)
    }

    /// Ingest an analytic capsule: diff into the capsule store, bump the moved
    /// planes, and record its paint-order slot. Returns the stable id.
    pub fn ingest_analytic_capsule(
        &mut self,
        instance: AnalyticCapsuleInstance,
        context: EmitContext,
        bounds: Bounds,
    ) -> PrimitiveId {
        let (slot, dirty) = self.analytic_capsules.ingest(instance);
        self.apply_planes(dirty);
        self.ingest_stats.analytic_capsule_instances += 1;
        self.record(StoreRef::AnalyticCapsule(slot), context, bounds)
    }

    /// Ingest an analytic line: diff into the line store, bump the moved
    /// planes, and record its paint-order slot. Returns the stable id.
    pub fn ingest_analytic_line(
        &mut self,
        instance: AnalyticLineInstance,
        context: EmitContext,
        bounds: Bounds,
    ) -> PrimitiveId {
        let (slot, dirty) = self.analytic_lines.ingest(instance);
        self.apply_planes(dirty);
        self.ingest_stats.analytic_line_instances += 1;
        self.record(StoreRef::AnalyticLine(slot), context, bounds)
    }

    /// Ingest an analytic soft shadow: diff into the shadow store, bump the moved
    /// planes, and record its paint-order slot. Returns the stable id.
    pub fn ingest_analytic_shadow(
        &mut self,
        instance: ShadowInstance,
        context: EmitContext,
        bounds: Bounds,
    ) -> PrimitiveId {
        let (slot, dirty) = self.analytic_shadows.ingest(instance);
        self.apply_planes(dirty);
        self.ingest_stats.analytic_shadow_instances += 1;
        self.record(StoreRef::AnalyticShadow(slot), context, bounds)
    }

    /// Ingest an image draw: diff instance + texture + sampler, bump the moved
    /// planes, record its slot.
    pub fn ingest_image(
        &mut self,
        instance: ImageInstance,
        texture: TextureId,
        sampler: SamplerDesc,
        context: EmitContext,
        bounds: Bounds,
    ) -> PrimitiveId {
        let (slot, dirty) = self.images.ingest(instance, texture, sampler);
        self.apply_planes(dirty);
        self.record(StoreRef::Image(slot), context, bounds)
    }

    /// Ingest a gradient fill: diff the lowered instance + its baked LUT texture,
    /// bump the moved planes, record its slot. The instance is finalized at
    /// lowering (it carries the resolved `lut_v`/`use_lut`), so a recolor that
    /// re-bakes to a different LUT row moves the geometry/paint fields the same as
    /// any other field change; a new LUT texture handle moves the resource plane.
    pub fn ingest_gradient(
        &mut self,
        instance: GradientInstance,
        lut_texture: TextureId,
        context: EmitContext,
        bounds: Bounds,
    ) -> PrimitiveId {
        let (slot, dirty) = self.gradients.ingest(instance, lut_texture);
        self.apply_planes(dirty);
        self.ingest_stats.gradient_instances += 1;
        self.record(StoreRef::Gradient(slot), context, bounds)
    }

    /// Ingest a glyph run: pack its instances, diff against last frame's run,
    /// bump the moved planes, record its slot.
    pub fn ingest_glyph_run(
        &mut self,
        glyphs: impl Iterator<Item = GlyphInstance>,
        atlas: TextureId,
        context: EmitContext,
        bounds: Bounds,
    ) -> PrimitiveId {
        let (slot, dirty) = self.glyph_runs.ingest_run(glyphs, atlas);
        self.apply_planes(dirty);
        if let Some(entry) = self.glyph_runs.run(slot) {
            self.ingest_stats.glyph_instances += entry.count;
        }
        self.record(StoreRef::GlyphRun(slot), context, bounds)
    }

    /// Ingest a vector path: diff against the retained entry, re-tessellating
    /// only on a geometry change (a transform-only, paint-only, or fully-equal
    /// revisit re-uses the cached tessellation, §13.4), bump the moved planes,
    /// record its slot.
    pub fn ingest_path(
        &mut self,
        path: &Path,
        context: EmitContext,
        bounds: Bounds,
    ) -> PrimitiveId {
        // The device scale drives the tessellation quality bucket. Surface/layer
        // DPI is not yet threaded to lowering, so this round tessellates at the
        // identity scale (bucket 0); the hysteresis machinery is exercised by the
        // store's unit tests. DPI wiring lands with the surface/layer scale (§13.4).
        let (slot, dirty) = self.paths.ingest(path, 1.0);
        // The tessellator runs only on a geometry rebuild (or a cold append);
        // transform-only and paint-only reuse the cached geometry (§13.4).
        if dirty.geometry || dirty.appended {
            self.ingest_stats.path_tessellations += 1;
        }
        self.apply_planes(dirty);
        self.record(StoreRef::Path(slot), context, bounds)
    }

    /// Ingest a caller-supplied mesh: diff vertices/indices, bump the moved
    /// planes, record its slot.
    pub fn ingest_mesh(
        &mut self,
        vertices: &[MeshVertex],
        indices: &[u32],
        context: EmitContext,
        bounds: Bounds,
    ) -> PrimitiveId {
        let (slot, dirty) = self.meshes.ingest(vertices, indices);
        self.apply_planes(dirty);
        self.record(StoreRef::Mesh(slot), context, bounds)
    }

    /// Record a translucent layer's composite draw. A composite is a per-frame
    /// derived draw (it samples an offscreen texture created this frame), not a
    /// retained store slot, so it bumps no plane and is not counted as a visible
    /// retained primitive; it only takes a paint-order position so re-derivation
    /// reproduces it.
    pub fn ingest_composite(
        &mut self,
        instance: ImageInstance,
        pass: usize,
        context: EmitContext,
        bounds: Bounds,
    ) -> PrimitiveId {
        self.record(StoreRef::Composite { instance, pass }, context, bounds)
    }

    /// Record a backdrop layer's composite draw, emitted when the layer *opens*
    /// so the blurred backdrop lands under the layer's own content. Like
    /// [`ingest_composite`] it is a per-frame derived draw that bumps no plane,
    /// but its instance is left unresolved: the capture's ROI can still grow as
    /// later layers join its group, so only the destination `rect` and `opacity`
    /// are recorded and the UVs are derived once the capture is realized.
    ///
    /// [`ingest_composite`]: Self::ingest_composite
    pub fn ingest_backdrop_composite(
        &mut self,
        capture: usize,
        rect: Rect,
        opacity: f32,
        context: EmitContext,
        bounds: Bounds,
    ) -> PrimitiveId {
        self.record(
            StoreRef::BackdropComposite {
                capture,
                rect,
                opacity,
            },
            context,
            bounds,
        )
    }

    /// Fold an emit's clip rect into the clip store, bumping the clip plane on a
    /// change. The identity-separated clip lets a scroll that only shifts a clip
    /// bump `ClipRevision` without disturbing geometry/paint (§8.5). Returns
    /// whether the clip moved. Does not itself record — the owning primitive's
    /// `ingest_*` records the paint-order entry.
    pub fn ingest_clip(&mut self, rect: Option<Rect>) -> bool {
        let (_id, changed) = self.clips.ingest(rect);
        if changed {
            self.revisions.bump_clip();
        }
        changed
    }

    /// Resolve a nested clip stack into a retained [`ClipChainId`] (§14.2).
    ///
    /// `rects` is the ordered axis-aligned clip stack (outermost first), folded
    /// once into the returned descriptor's box; `mask` is the complex tail's
    /// key or `None`; `input` fingerprints the stack so an unchanged chain is
    /// recognised and its descriptor returned without refolding or re-rastering.
    /// A changed chain bumps the clip plane (the same plane a moved clip rect
    /// bumps — a chain is a pre-resolution of clips, not a new revision axis).
    ///
    /// Returns the chain's id and its descriptor. The descriptor's
    /// [`is_empty`](ClipChainDescriptor::is_empty) is the subtree-reject signal
    /// (§14.3): an empty clip means every primitive under the chain is discarded.
    pub fn resolve_clip_chain(
        &mut self,
        rects: &[Rect],
        mask: Option<ClipMaskKey>,
        input: u64,
    ) -> (ClipChainId, ClipChainDescriptor) {
        let (id, descriptor, changed) = self.clip_chains.resolve(rects, mask, input);
        if changed {
            self.revisions.bump_clip();
        }
        (id, descriptor)
    }

    /// Fold an emit's world-space origin into the transform store, bumping the
    /// transform plane on a change. A pure move (origin shift, unchanged
    /// geometry/paint) dirties `TransformRevision` alone (§8.5). Returns whether
    /// the transform moved.
    pub fn ingest_transform(&mut self, origin: [f32; 2]) -> bool {
        let (_id, changed) = self.transforms.ingest(origin);
        if changed {
            self.revisions.bump_transform();
        }
        changed
    }

    /// Fold a resolved fill/stroke brush into the brush store, bumping the paint
    /// plane on a change (§8.5). Returns whether the brush moved. The deferred
    /// [`Brush::ImagePattern`]/[`Brush::ShaderBrush`] variants are rejected inside
    /// the store rather than stored as no-ops.
    pub fn ingest_brush(&mut self, brush: Brush) -> bool {
        let (_id, changed) = self.brushes.ingest(brush);
        if changed {
            self.revisions.bump_paint();
        }
        changed
    }

    /// Trim every store's tail past the frame's cursor (the scene shrank) and
    /// bump the visibility plane if any store lost entries. Called after the
    /// ingest walk by [`Scene::finish_frame`].
    pub(super) fn finish_ingest(&mut self) {
        let mut shrank = self.quads.finish_frame();
        shrank |= self.analytic_rrects.finish_frame();
        shrank |= self.analytic_ellipses.finish_frame();
        shrank |= self.analytic_capsules.finish_frame();
        shrank |= self.analytic_lines.finish_frame();
        shrank |= self.images.finish_frame();
        shrank |= self.gradients.finish_frame();
        shrank |= self.analytic_shadows.finish_frame();
        shrank |= self.glyph_runs.finish_frame();
        shrank |= self.paths.finish_frame();
        shrank |= self.meshes.finish_frame();
        shrank |= self.clips.finish_frame();
        shrank |= self.clip_chains.finish_frame();
        shrank |= self.transforms.finish_frame();
        shrank |= self.brushes.finish_frame();
        if shrank {
            self.revisions.bump_visibility();
        }
    }
}
