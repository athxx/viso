//! Patching an uploaded frame in place (§9.7).
//!
//! A frame that only repaints — a hover recolors a card, a progress label
//! changes color — rewrites a handful of primitives of the stream it uploaded
//! last, each the same kind over the same box. [`Renderer::patch`] touches
//! only those: each is re-ingested at the store slot it holds, re-lowered into
//! the instance slots it was lowered to, uploaded alone, and its surface draw
//! re-keyed, so the frame costs the primitives it changed and not the scene.
//!
//! A layer that only fades — its opacity moves and its plan does not — is a
//! patch too: an offscreen layer re-tints its composite, a folded one rewrites
//! every child at the new factor.
//!
//! Anything that would move another entry — a new kind or count, a new box, a
//! new texture, a layer that changes how it is drawn, a backdrop capture
//! re-rendering what lies under it — is not a patch: the caller uploads the
//! stream whole.

use viso_gpu::GpuBackend;

use super::{CompositeLowering, LayerDrawn, Renderer, Splice};
use crate::effect_plan::plan_layer;
use crate::primitive::{GlyphRunDraw, LayerClip, Primitive};
use crate::scene::ingest::Instance;
use crate::scene::store::{DirtyPlanes, StoreRef, glyph_instances};

/// An entry a patch cannot rewrite in place, or one culled from its pass.
pub(super) const NOT_LOWERED: u32 = u32::MAX;

/// One validated rewrite.
#[derive(Debug, Clone, Copy)]
pub(super) enum Edit {
    /// Paint-order entry `entry` takes the primitive at stream index `source`
    /// under group opacity `fold`, as `instance` unless it is a glyph run.
    Draw {
        entry: u32,
        source: u32,
        instance: Option<Instance>,
        fold: f32,
    },
    /// Layer record `record` takes `layer`, whose opacity alone moved; an
    /// offscreen layer re-tints composite entry `composite`.
    Fade {
        record: u32,
        layer: LayerClip,
        composite: u32,
    },
}

/// What a layer primitive's rewrite asks of the layer it opens.
enum Faded {
    /// Nothing, or a fade drawn by one composite.
    Kept,
    /// Every entry before stream index `end` is redrawn at group opacity
    /// `fold`.
    Refolded { end: u32, fold: f32 },
}

/// The instance families a patch writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Family {
    Quad,
    AnalyticRRect,
    AnalyticEllipse,
    AnalyticCapsule,
    AnalyticLine,
    AnalyticShadow,
    Image,
    Glyph,
}

impl Family {
    fn of(store: StoreRef) -> Option<Family> {
        Some(match store {
            StoreRef::Quad(_) => Family::Quad,
            StoreRef::AnalyticRRect(_) => Family::AnalyticRRect,
            StoreRef::AnalyticEllipse(_) => Family::AnalyticEllipse,
            StoreRef::AnalyticCapsule(_) => Family::AnalyticCapsule,
            StoreRef::AnalyticLine(_) => Family::AnalyticLine,
            StoreRef::AnalyticShadow(_) => Family::AnalyticShadow,
            StoreRef::Image(_) | StoreRef::Composite { .. } => Family::Image,
            StoreRef::GlyphRun(_) => Family::Glyph,
            _ => return None,
        })
    }
}

impl Renderer {
    /// Rewrite the uploaded frame for `primitives`: the stream last uploaded,
    /// with the primitives at the ascending stream indices `changed` rewritten
    /// in place. Only those are re-ingested, re-lowered, uploaded and
    /// re-keyed for damage.
    ///
    /// A rewritten leaf that no longer fits its slot — another kind, glyph
    /// count, box or texture — is spliced in its place instead
    /// ([`Renderer::splice`]).
    ///
    /// Returns `false`, having changed nothing, when the rewrite is neither a
    /// patch nor a splice — the stream changed length, a layer changed how it
    /// draws, a leaf that does not fit lies inside a folded group or an
    /// offscreen pass, or the frame captures a backdrop. Upload the stream
    /// whole then.
    pub fn patch<B: GpuBackend>(
        &mut self,
        backend: &mut B,
        primitives: &[Primitive],
        changed: &[u32],
    ) -> bool {
        if self.surface_moved || primitives.len() != self.stream_len {
            return false;
        }
        if changed.is_empty() {
            self.uploaded_ranges = 0;
            self.gpu_upload_bytes = 0;
            return true;
        }
        if !self.backdrop_captures.is_empty() {
            return false;
        }
        let mut edits = std::mem::take(&mut self.edits);
        edits.clear();
        if !self.edits_for(primitives, changed, &mut edits) {
            self.edits = edits;
            return self.splice_rewrites(backend, primitives, changed);
        }
        self.scene.ingest_stats.dirty_primitives = 0;
        let mut slots = std::mem::take(&mut self.edit_slots);
        slots.clear();
        for &edit in &edits {
            let index = match edit {
                Edit::Draw {
                    entry,
                    source,
                    instance,
                    fold,
                } => {
                    let index = entry as usize;
                    let store = self.scene.paint_order[index].store;
                    self.scene.paint_order[index].context.fold = fold;
                    let dirty = match (instance, store) {
                        (Some(instance), store) => self.scene_set(store, instance),
                        (None, StoreRef::GlyphRun(run)) => match &primitives[source as usize] {
                            Primitive::GlyphRun(draw) => self
                                .scene
                                .glyph_runs
                                .set(run, glyph_instances(&draw.glyphs, color(draw))),
                            _ => DirtyPlanes::default(),
                        },
                        (None, _) => DirtyPlanes::default(),
                    };
                    if !dirty.any() {
                        continue;
                    }
                    self.scene.restamp(index, dirty);
                    index
                }
                Edit::Fade {
                    record,
                    layer,
                    composite,
                } => match self.fade(record, layer, composite) {
                    Some(index) => index,
                    None => continue,
                },
            };
            if let Some(slot) = self.relower(index) {
                slots.push(slot);
            }
            self.rekey(index);
        }
        // Each family's slots ascend in paint order already; group them by
        // family, keeping that order.
        slots.sort_by_key(|&(family, _, _)| family);
        let mut writes = 0;
        let mut bytes = 0;
        let mut at = 0;
        while at < slots.len() {
            let family = slots[at].0;
            let end = at + slots[at..].iter().take_while(|s| s.0 == family).count();
            let ranges = slots[at..end]
                .iter()
                .map(|&(_, start, len)| (start as usize, len as usize));
            let (w, b) = self.write_family(backend, family, ranges);
            writes += w;
            bytes += b;
            at = end;
        }
        self.uploaded_ranges = writes;
        self.gpu_upload_bytes = bytes;
        self.edits = edits;
        self.edit_slots = slots;
        true
    }

    /// Splice each run of rewritten primitives `changed` over the run it
    /// rewrote, one for one.
    fn splice_rewrites<B: GpuBackend>(
        &mut self,
        backend: &mut B,
        primitives: &[Primitive],
        changed: &[u32],
    ) -> bool {
        let mut runs = std::mem::take(&mut self.rewrites);
        runs.clear();
        for &at in changed {
            match runs.last_mut() {
                Some(run) if run.at + run.inserted == at => {
                    run.removed += 1;
                    run.inserted += 1;
                }
                _ => runs.push(Splice {
                    at,
                    removed: 1,
                    inserted: 1,
                }),
            }
        }
        let spliced = self.splice(backend, primitives, &runs);
        self.rewrites = runs;
        spliced
    }

    /// Every rewrite `changed` asks of the stream last uploaded, into
    /// `edits`; `false` when one of them is not a patch.
    fn edits_for(&self, primitives: &[Primitive], changed: &[u32], edits: &mut Vec<Edit>) -> bool {
        // A folded layer that fades rewrites its whole subtree at the new
        // factor, so a changed child under it is rewritten at that factor too.
        let mut refold: Option<(u32, f32)> = None;
        for &source in changed {
            let Some(prim) = primitives.get(source as usize) else {
                return false;
            };
            if let Primitive::Layer(layer) = prim {
                match self.fade_edit(primitives, source, layer, edits) {
                    Some(Faded::Kept) => {}
                    Some(Faded::Refolded { end, fold }) => refold = Some((end, fold)),
                    None => return false,
                }
                continue;
            }
            let fold = match refold {
                Some((end, fold)) if source < end => Some(fold),
                _ => None,
            };
            match self.draw_edit(primitives, source, fold) {
                Some(edit) => edits.push(edit),
                None => return false,
            }
        }
        true
    }

    /// The rewrite of the entry stream index `source` recorded, at group
    /// opacity `fold` or the one it was uploaded under, if it is a patch.
    fn draw_edit(&self, primitives: &[Primitive], source: u32, fold: Option<f32>) -> Option<Edit> {
        let prim = primitives.get(source as usize)?;
        let order = &self.scene.paint_order;
        let index = order.partition_point(|e| e.source < source);
        let entry = order.get(index)?;
        if entry.source != source || order.get(index + 1).is_some_and(|e| e.source == source) {
            return None;
        }
        self.lowered.get(index)?;
        let fold = fold.unwrap_or(entry.context.fold);
        let instance = match (prim, entry.store) {
            (Primitive::GlyphRun(draw), StoreRef::GlyphRun(run)) => {
                let glyphs = glyph_instances(&draw.glyphs, color(draw));
                if fold != entry.context.fold
                    || !self
                        .scene
                        .glyph_runs
                        .fits(run, glyphs, draw.atlas, draw.lane)
                {
                    return None;
                }
                None
            }
            _ => {
                let (instance, bounds) = Instance::of(prim, fold)?;
                if bounds != entry.bounds || !self.holds(entry.store, &instance) {
                    return None;
                }
                Some(instance)
            }
        };
        Some(Edit::Draw {
            entry: index as u32,
            source,
            instance,
            fold,
        })
    }

    /// Push the rewrites the layer opening at stream index `source` asks for
    /// as `layer`: none when it is unchanged, else its fade, and for a folded
    /// layer every entry under it at the new factor. `None` when the change is
    /// not a fade its plan survives.
    fn fade_edit(
        &self,
        primitives: &[Primitive],
        source: u32,
        layer: &LayerClip,
        edits: &mut Vec<Edit>,
    ) -> Option<Faded> {
        let at = self.layers.partition_point(|r| r.source < source);
        let record = self
            .layers
            .get(at)
            .filter(|r| r.source == source && r.end > source)?;
        let was = record.layer;
        if *layer == was {
            return Some(Faded::Kept);
        }
        let same = LayerClip {
            opacity: was.opacity,
            ..*layer
        };
        if same != was {
            return None;
        }
        // The plan decided how the layer is drawn; a fade that would change it
        // draws the frame differently.
        let old = self.layer_plans.get(at)?;
        let new = plan_layer(&crate::effect_plan::LayerRequest {
            opacity: layer.opacity,
            ..record.request
        });
        if new.requested != old.requested
            || new.eliminated != old.eliminated
            || new.surviving != old.surviving
        {
            return None;
        }
        let fade = |composite| Edit::Fade {
            record: at as u32,
            layer: *layer,
            composite,
        };
        match record.drawn {
            LayerDrawn::InPass => None,
            LayerDrawn::Offscreen(pass) => {
                let p = self.offscreen_passes.get(pass)?;
                if p.blend.is_some() || p.color.is_some() {
                    return None;
                }
                let order = &self.scene.paint_order;
                let composite = order.partition_point(|e| e.source < record.end);
                match order.get(composite)?.store {
                    StoreRef::Composite { pass: of, .. } if of as usize == pass => {}
                    _ => return None,
                }
                if self.lowered.get(composite) == Some(&NOT_LOWERED) {
                    return None;
                }
                edits.push(fade(composite as u32));
                Some(Faded::Kept)
            }
            LayerDrawn::Folded => {
                let fold = record.parent_fold * new.fold_opacity;
                let order = &self.scene.paint_order;
                let start = order.partition_point(|e| e.source <= source);
                let end = order.partition_point(|e| e.source < record.end);
                for entry in &order[start..end] {
                    edits.push(self.draw_edit(primitives, entry.source, Some(fold))?);
                }
                edits.push(fade(NOT_LOWERED));
                Some(Faded::Refolded {
                    end: record.end,
                    fold,
                })
            }
        }
    }

    /// Apply a validated fade of layer record `record` to `layer`: the
    /// composite entry it re-tinted, if any.
    fn fade(&mut self, record: u32, layer: LayerClip, composite: u32) -> Option<usize> {
        let r = &mut self.layers[record as usize];
        r.layer = layer;
        r.request.opacity = layer.opacity;
        let (drawn, parent_fold) = (r.drawn, r.parent_fold);
        match drawn {
            LayerDrawn::Folded => {
                self.layer_plans[record as usize].fold_opacity = layer.opacity;
                None
            }
            LayerDrawn::InPass => None,
            LayerDrawn::Offscreen(pass) => {
                self.offscreen_passes[pass].opacity = layer.opacity;
                self.offscreen_passes[pass].key =
                    self.pass_key(pass, layer.blur_sigma, parent_fold, 0..0);
                let index = composite as usize;
                if let StoreRef::Composite { slot, .. } = self.scene.paint_order[index].store {
                    self.scene.composites[slot as usize].color[3] = layer.opacity;
                }
                self.scene.restamp_derived(index);
                Some(index)
            }
        }
    }

    /// Whether `store` is a slot `instance` can be written over: the same
    /// kind, and for an image the same texture and sampler.
    fn holds(&self, store: StoreRef, instance: &Instance) -> bool {
        match (store, instance) {
            (StoreRef::Quad(_), Instance::Quad(_))
            | (StoreRef::AnalyticRRect(_), Instance::AnalyticRRect(_))
            | (StoreRef::AnalyticEllipse(_), Instance::AnalyticEllipse(_))
            | (StoreRef::AnalyticCapsule(_), Instance::AnalyticCapsule(_))
            | (StoreRef::AnalyticLine(_), Instance::AnalyticLine(_))
            | (StoreRef::AnalyticShadow(_), Instance::AnalyticShadow(_)) => true,
            (StoreRef::Image(id), Instance::Image(_, texture, sampler)) => self
                .scene
                .images
                .get(id)
                .is_some_and(|e| e.texture == *texture && e.sampler == *sampler),
            _ => false,
        }
    }

    /// Write `instance` over its store slot; the planes it moved.
    fn scene_set(&mut self, store: StoreRef, instance: Instance) -> DirtyPlanes {
        let scene = &mut self.scene;
        match (store, instance) {
            (StoreRef::Quad(id), Instance::Quad(i)) => scene.quads.set(id, i),
            (StoreRef::AnalyticRRect(id), Instance::AnalyticRRect(i)) => {
                scene.analytic_rrects.set(id, i)
            }
            (StoreRef::AnalyticEllipse(id), Instance::AnalyticEllipse(i)) => {
                scene.analytic_ellipses.set(id, i)
            }
            (StoreRef::AnalyticCapsule(id), Instance::AnalyticCapsule(i)) => {
                scene.analytic_capsules.set(id, i)
            }
            (StoreRef::AnalyticLine(id), Instance::AnalyticLine(i)) => {
                scene.analytic_lines.set(id, i)
            }
            (StoreRef::AnalyticShadow(id), Instance::AnalyticShadow(i)) => {
                scene.analytic_shadows.set(id, i)
            }
            (StoreRef::Image(id), Instance::Image(i, texture, sampler)) => {
                scene.images.set(id, i, texture, sampler)
            }
            _ => DirtyPlanes::default(),
        }
    }

    /// Where entry `store`'s instances begin in its family's scratch were it
    /// lowered now, or [`NOT_LOWERED`] for one a patch cannot rewrite.
    pub(super) fn lowering_cursor(&self, store: StoreRef) -> u32 {
        if let StoreRef::Composite { pass, .. } = store
            && self.composite_lowering(pass as usize) != CompositeLowering::Image
        {
            return NOT_LOWERED;
        }
        let len = match Family::of(store) {
            Some(Family::Quad) => self.quad_scratch.len(),
            Some(Family::AnalyticRRect) => self.analytic_rrect_scratch.len(),
            Some(Family::AnalyticEllipse) => self.analytic_ellipse_scratch.len(),
            Some(Family::AnalyticCapsule) => self.analytic_capsule_scratch.len(),
            Some(Family::AnalyticLine) => self.analytic_line_scratch.len(),
            Some(Family::AnalyticShadow) => self.analytic_shadow_scratch.len(),
            Some(Family::Image) => self.image_scratch.len(),
            Some(Family::Glyph) => self.glyph_scratch.len(),
            None => return NOT_LOWERED,
        };
        len as u32
    }

    /// Lower paint-order entry `index` again over the scratch slots it was
    /// lowered to, as [`lower_entry`](Self::lower_entry) lowers it: its
    /// family and the slots `(start, len)`, or `None` for an entry its pass
    /// culled.
    fn relower(&mut self, index: usize) -> Option<(Family, u32, u32)> {
        let start = self.lowered[index];
        if start == NOT_LOWERED {
            return None;
        }
        let entry = self.scene.paint_order[index];
        let [ox, oy] = entry.context.origin;
        let at = start as usize;
        macro_rules! place {
            ($store:ident, $scratch:ident, $id:expr) => {{
                let mut inst = self.scene.$store.get($id)?.instance;
                inst.rect_pos[0] -= ox;
                inst.rect_pos[1] -= oy;
                *self.$scratch.get_mut(at)? = inst;
                1
            }};
        }
        let family = Family::of(entry.store)?;
        let len = match entry.store {
            StoreRef::Quad(id) => place!(quads, quad_scratch, id),
            StoreRef::AnalyticRRect(id) => place!(analytic_rrects, analytic_rrect_scratch, id),
            StoreRef::AnalyticEllipse(id) => {
                place!(analytic_ellipses, analytic_ellipse_scratch, id)
            }
            StoreRef::AnalyticCapsule(id) => {
                place!(analytic_capsules, analytic_capsule_scratch, id)
            }
            StoreRef::AnalyticShadow(id) => {
                place!(analytic_shadows, analytic_shadow_scratch, id)
            }
            StoreRef::Image(id) => place!(images, image_scratch, id),
            StoreRef::Composite { slot, .. } => {
                let mut instance = *self.scene.composites.get(slot as usize)?;
                instance.rect_pos[0] -= ox;
                instance.rect_pos[1] -= oy;
                *self.image_scratch.get_mut(at)? = instance;
                1
            }
            StoreRef::AnalyticLine(id) => {
                let mut inst = self.scene.analytic_lines.get(id)?.instance;
                inst.p0[0] -= ox;
                inst.p0[1] -= oy;
                inst.p1[0] -= ox;
                inst.p1[1] -= oy;
                *self.analytic_line_scratch.get_mut(at)? = inst;
                1
            }
            StoreRef::GlyphRun(run) => {
                let e = *self.scene.glyph_runs.run(run)?;
                let glyphs = self.scene.glyph_runs.glyphs(&e);
                let out = self.glyph_scratch.get_mut(at..at + glyphs.len())?;
                for (slot, g) in out.iter_mut().zip(glyphs) {
                    let mut inst = *g;
                    inst.rect_pos[0] -= ox;
                    inst.rect_pos[1] -= oy;
                    *slot = inst;
                }
                glyphs.len() as u32
            }
            _ => return None,
        };
        Some((family, start, len))
    }

    /// Upload `ranges` of `family`'s scratch: the writes issued and the
    /// bytes they carried.
    fn write_family<B: GpuBackend>(
        &mut self,
        backend: &mut B,
        family: Family,
        ranges: impl Iterator<Item = (usize, usize)>,
    ) -> (usize, usize) {
        macro_rules! write {
            ($pool:ident, $scratch:ident) => {{
                let writes = self.$pool.write_slots(backend, &self.$scratch, ranges);
                (writes, self.$pool.last_upload_bytes())
            }};
        }
        match family {
            Family::Quad => write!(quad_pool, quad_scratch),
            Family::AnalyticRRect => write!(analytic_rrect_pool, analytic_rrect_scratch),
            Family::AnalyticEllipse => write!(analytic_ellipse_pool, analytic_ellipse_scratch),
            Family::AnalyticCapsule => write!(analytic_capsule_pool, analytic_capsule_scratch),
            Family::AnalyticLine => write!(analytic_line_pool, analytic_line_scratch),
            Family::AnalyticShadow => write!(analytic_shadow_pool, analytic_shadow_scratch),
            Family::Image => write!(image_pool, image_scratch),
            Family::Glyph => write!(glyph_pool, glyph_scratch),
        }
    }
}

/// A glyph run's color, as its instances carry it.
fn color(draw: &GlyphRunDraw) -> [f32; 4] {
    [draw.color.r, draw.color.g, draw.color.b, draw.color.a]
}

#[cfg(test)]
mod tests {
    use super::super::testing::*;
    use crate::primitive::{AnalyticRRect, Border, Corners, LayerClip, Primitive, TransformSlot};
    use crate::transform::TransformUpdate;

    /// Uploads `before`, then brings one renderer to `after` by patching
    /// `changed` and another by uploading `after` whole; both must agree.
    #[track_caller]
    fn agree(before: &[Primitive], after: &[Primitive], changed: &[u32]) -> (Gpu, Gpu) {
        let mut patched = Gpu::new();
        let mut whole = Gpu::new();
        patched.upload(before);
        whole.upload(before);
        assert!(patched.patch(after, changed), "a patch");
        whole.upload(after);
        same_state(&patched.r, &whole.r);
        (patched, whole)
    }

    #[test]
    fn a_patch_leaves_what_a_whole_upload_leaves() {
        let mut gpu = Gpu::new();
        let texture = gpu.texture();
        let before = kinds(texture, 0.2);
        let after = kinds(texture, 0.9);
        let changed: Vec<u32> = (1..after.len() as u32).collect();
        agree(&before, &after, &changed);
    }

    #[test]
    fn a_patch_uploads_only_the_slots_it_rewrote() {
        let rows = |hot: usize| -> Vec<Primitive> {
            (0..12)
                .map(|i| {
                    let t = if i == hot { 0.9 } else { 0.3 };
                    quad(rect(4.0, 4.0 + i as f32 * 10.0, 100.0, 8.0), rgba(t, 1.0))
                })
                .collect()
        };
        let (patched, _) = agree(&rows(1), &rows(10), &[1, 10]);
        let stats = patched.r.frame_stats();
        assert_eq!(stats.uploaded_ranges, 2, "the two rows, apart");
        assert_eq!(stats.gpu_upload_bytes, 2 * size_of::<crate::QuadInstance>());
    }

    #[test]
    fn a_patch_inside_a_translucent_layer_rekeys_its_composite() {
        let scene = |t: f32| {
            vec![
                quad(rect(0.0, 0.0, SIZE, SIZE), rgba(0.1, 1.0)),
                layer(rect(10.0, 10.0, 60.0, 60.0), 0.5),
                quad(rect(10.0, 10.0, 40.0, 40.0), rgba(t, 1.0)),
                quad(rect(30.0, 30.0, 40.0, 40.0), rgba(0.4, 1.0)),
                Primitive::LayerEnd,
            ]
        };
        let (patched, _) = agree(&scene(0.2), &scene(0.8), &[2]);
        assert!(
            !patched.r.offscreen_passes.is_empty(),
            "the layer is a pass"
        );
    }

    #[test]
    fn a_patch_inside_a_folded_layer_keeps_the_fold() {
        let scene = |t: f32| {
            vec![
                layer(rect(0.0, 0.0, 100.0, 40.0), 0.5),
                quad(rect(0.0, 0.0, 40.0, 40.0), rgba(t, 1.0)),
                quad(rect(50.0, 0.0, 40.0, 40.0), rgba(0.4, 1.0)),
                Primitive::LayerEnd,
            ]
        };
        let (patched, _) = agree(&scene(0.2), &scene(0.8), &[1]);
        assert!(
            patched.r.offscreen_passes.is_empty(),
            "disjoint children fold"
        );
    }

    /// Two overlapping children under a layer of `opacity`, which therefore
    /// draws through an offscreen pass, blurred by `blur`.
    fn isolated(opacity: f32, blur: f32, t: f32) -> Vec<Primitive> {
        vec![
            quad(rect(0.0, 0.0, SIZE, SIZE), rgba(0.1, 1.0)),
            Primitive::Layer(LayerClip {
                clip: rect(10.0, 10.0, 60.0, 60.0),
                opacity,
                blur_sigma: blur,
                backdrop_sigma: 0.0,
            }),
            quad(rect(10.0, 10.0, 40.0, 40.0), rgba(t, 1.0)),
            quad(rect(30.0, 30.0, 40.0, 40.0), rgba(0.4, 1.0)),
            Primitive::LayerEnd,
        ]
    }

    /// Two disjoint children under a layer of `opacity`, which therefore
    /// folds into them.
    fn folded(opacity: f32, t: f32) -> Vec<Primitive> {
        vec![
            quad(rect(0.0, 0.0, SIZE, SIZE), rgba(0.1, 1.0)),
            layer(rect(0.0, 0.0, 100.0, 40.0), opacity),
            quad(rect(0.0, 0.0, 40.0, 40.0), rgba(t, 1.0)),
            quad(rect(50.0, 0.0, 40.0, 40.0), rgba(0.4, 1.0)),
            Primitive::LayerEnd,
        ]
    }

    #[test]
    fn a_fade_of_an_isolated_layer_retints_its_composite() {
        let (patched, _) = agree(&isolated(0.5, 0.0, 0.2), &isolated(0.7, 0.0, 0.2), &[1]);
        assert_eq!(patched.r.offscreen_passes.len(), 1, "the layer is a pass");
        assert_eq!(patched.r.frame_stats().uploaded_ranges, 1, "its composite");
        agree(&isolated(0.5, 0.0, 0.2), &isolated(0.7, 0.0, 0.9), &[1, 2]);
        agree(&isolated(0.5, 3.0, 0.2), &isolated(0.3, 3.0, 0.2), &[1]);
    }

    #[test]
    fn a_fade_of_a_folded_layer_refolds_its_children() {
        let (patched, _) = agree(&folded(0.5, 0.2), &folded(0.3, 0.2), &[1]);
        assert!(patched.r.offscreen_passes.is_empty(), "the layer folds");
        assert_eq!(patched.r.frame_stats().uploaded_ranges, 1, "both children");
        agree(&folded(0.5, 0.2), &folded(0.3, 0.9), &[1, 2]);
    }

    #[test]
    fn a_presented_fade_matches_a_fresh_frame() {
        for (before, after) in [
            (isolated(0.5, 0.0, 0.2), isolated(0.8, 0.0, 0.2)),
            (folded(0.5, 0.2), folded(0.8, 0.2)),
        ] {
            let mut kept = Gpu::new();
            kept.upload(&before);
            kept.present();
            kept.present();
            assert!(kept.patch(&after, &[1]));
            let shown = kept.present();
            assert!(
                matches!(kept.r.last_damage(), crate::damage::Region::Rects(_)),
                "a partial redraw, not {:?}",
                kept.r.last_damage()
            );
            let mut fresh = Gpu::new();
            fresh.upload(&after);
            assert!(shown == fresh.present(), "pixel-identical to a fresh frame");
        }
    }

    #[test]
    fn a_fade_that_changes_how_the_layer_draws_is_not_a_patch() {
        let mut gpu = Gpu::new();
        let mut refused = |before: Vec<Primitive>, after: Vec<Primitive>| {
            gpu.upload(&before);
            assert!(!gpu.patch(&after, &[1]));
        };
        // Opaque, or invisible: the plan changes.
        refused(isolated(0.5, 0.0, 0.2), isolated(1.0, 0.0, 0.2));
        refused(isolated(0.5, 0.0, 0.2), isolated(0.0, 0.0, 0.2));
        refused(folded(0.5, 0.2), folded(1.0, 0.2));
        refused(isolated(1.0, 0.0, 0.2), isolated(0.5, 0.0, 0.2));
        refused(isolated(1.0, 3.0, 0.2), isolated(0.5, 3.0, 0.2));
        // Another blur, or another clip.
        refused(isolated(0.5, 2.0, 0.2), isolated(0.5, 3.0, 0.2));
        let mut moved = isolated(0.5, 0.0, 0.2);
        moved[1] = layer(rect(10.0, 10.0, 62.0, 60.0), 0.5);
        refused(isolated(0.5, 0.0, 0.2), moved);
    }

    #[test]
    fn a_patch_under_a_moved_root_agrees() {
        let scene = |t: f32| {
            vec![
                quad(rect(0.0, 0.0, SIZE, SIZE), rgba(0.1, 1.0)),
                Primitive::Transform(TransformSlot(0)),
                quad(rect(10.0, 10.0, 20.0, 20.0), rgba(t, 1.0)),
                Primitive::TransformEnd,
            ]
        };
        let mut patched = Gpu::new();
        let mut whole = Gpu::new();
        for g in [&mut patched, &mut whole] {
            g.r.upload_with_transforms(&mut g.gpu, &scene(0.2), &[[0.0, 0.0]]);
            assert_eq!(g.r.set_transforms(&[[0.0, 12.0]]), TransformUpdate::Moved);
        }
        assert!(patched.patch(&scene(0.8), &[2]));
        whole
            .r
            .upload_with_transforms(&mut whole.gpu, &scene(0.8), &[[0.0, 0.0]]);
        whole.r.set_transforms(&[[0.0, 12.0]]);
        same_state(&patched.r, &whole.r);
        assert_eq!(patched.present(), whole.present());
    }

    #[test]
    fn an_unchanged_rewrite_patches_nothing() {
        let scene = vec![quad(rect(4.0, 4.0, 20.0, 20.0), rgba(0.3, 1.0))];
        let (patched, _) = agree(&scene, &scene, &[0]);
        assert_eq!(patched.r.frame_stats().uploaded_ranges, 0);
    }

    #[test]
    fn a_rewrite_that_does_not_fit_its_slot_splices() {
        let mut gpu = Gpu::new();
        let texture = gpu.texture();
        let other = gpu.texture();
        let before = kinds(texture, 0.2);
        let spliced = |after: Vec<Primitive>, at: u32| {
            let mut patched = Gpu::new();
            let mut whole = Gpu::new();
            patched.upload(&before);
            whole.upload(&before);
            assert!(patched.patch(&after, &[at]), "entry {at} spliced");
            whole.upload(&after);
            same_scene(&patched.r, &whole.r);
        };
        // A new box.
        let mut after = before.clone();
        after[1] = Primitive::AnalyticRRect(AnalyticRRect {
            rect: rect(4.0, 4.0, 22.0, 12.0),
            color: rgba(0.2, 0.8),
            radius: Corners::SHARP,
            border: Border::NONE,
        });
        spliced(after, 1);
        // A new kind.
        let mut after = before.clone();
        after[1] = quad(rect(4.0, 4.0, 20.0, 12.0), rgba(0.2, 0.8));
        spliced(after, 1);
        // A new texture.
        let mut after = before.clone();
        if let Primitive::Image(image) = &mut after[6] {
            image.texture = other;
        }
        spliced(after, 6);
        // Another glyph count, beside a recolored box.
        let mut after = before.clone();
        if let Primitive::GlyphRun(run) = &mut after[7] {
            run.glyphs.pop();
        }
        after[6] = before[6].clone();
        after[1] = kinds(texture, 0.9)[1].clone();
        let mut patched = Gpu::new();
        let mut whole = Gpu::new();
        patched.upload(&before);
        whole.upload(&before);
        assert!(patched.patch(&after, &[1, 6, 7]));
        whole.upload(&after);
        same_scene(&patched.r, &whole.r);
    }

    #[test]
    fn a_rewrite_neither_in_place_nor_of_surface_leaves_is_refused() {
        let mut gpu = Gpu::new();
        let texture = gpu.texture();
        let before = kinds(texture, 0.2);
        // A stream of another length.
        let mut after = before.clone();
        after.push(quad(rect(0.0, 0.0, 4.0, 4.0), rgba(0.2, 1.0)));
        gpu.upload(&before);
        assert!(!gpu.patch(&after, &[1]), "another length");
        // A leaf that no longer fits, inside a folded group.
        let group = folded(0.5, 0.2);
        let mut after = group.clone();
        after[2] = quad(rect(0.0, 0.0, 44.0, 40.0), rgba(0.2, 1.0));
        gpu.upload(&group);
        assert!(!gpu.patch(&after, &[2]), "inside a folded group");
        // A frame capturing a backdrop.
        let frosted = vec![
            quad(rect(0.0, 0.0, SIZE, SIZE), rgba(0.1, 1.0)),
            Primitive::Layer(LayerClip {
                clip: rect(10.0, 10.0, 50.0, 50.0),
                opacity: 1.0,
                blur_sigma: 0.0,
                backdrop_sigma: 6.0,
            }),
            quad(rect(20.0, 20.0, 10.0, 10.0), rgba(0.5, 1.0)),
            Primitive::LayerEnd,
        ];
        gpu.upload(&frosted);
        let mut after = frosted.clone();
        after[2] = quad(rect(20.0, 20.0, 10.0, 10.0), rgba(0.9, 1.0));
        assert!(!gpu.patch(&after, &[2]), "a capture refuses");
    }

    #[test]
    fn a_refused_patch_changes_nothing() {
        let mut patched = Gpu::new();
        let mut whole = Gpu::new();
        // One child of a folded group recolored, the other grown.
        let before = folded(0.5, 0.2);
        let mut after = folded(0.5, 0.9);
        after[3] = quad(rect(50.0, 0.0, 44.0, 40.0), rgba(0.4, 1.0));
        patched.upload(&before);
        whole.upload(&before);
        assert!(!patched.patch(&after, &[2, 3]));
        same_state(&patched.r, &whole.r);
    }

    #[test]
    fn a_presented_patch_redraws_only_its_draw_and_matches_a_fresh_frame() {
        let rows = |hot: usize| -> Vec<Primitive> {
            (0..10)
                .map(|i| {
                    let a = if i == hot { 0.9 } else { 0.5 };
                    quad(rect(4.0, 4.0 + i as f32 * 12.0, 100.0, 9.0), rgba(0.7, a))
                })
                .collect()
        };
        let mut kept = Gpu::new();
        kept.upload(&rows(99));
        kept.present();
        kept.present();
        assert!(kept.patch(&rows(4), &[4]));
        let shown = kept.present();
        let crate::damage::Region::Rects(rects) = *kept.r.last_damage() else {
            panic!("a partial redraw, not {:?}", kept.r.last_damage());
        };
        assert_eq!(rects.len(), 1);
        assert!(rects[0].h < 14.0, "one row: {:?}", rects[0]);
        let mut fresh = Gpu::new();
        fresh.upload(&rows(4));
        assert!(shown == fresh.present(), "pixel-identical to a fresh frame");
        // And a second patch back, without anything else in between.
        assert!(kept.patch(&rows(99), &[4]));
        let shown = kept.present();
        let mut fresh = Gpu::new();
        fresh.upload(&rows(99));
        assert!(shown == fresh.present());
    }
}
