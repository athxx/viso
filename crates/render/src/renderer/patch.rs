//! Patching an uploaded frame in place (§9.7).
//!
//! A frame that only repaints — a hover recolors a card, a progress label
//! changes color — rewrites a handful of primitives of the stream it uploaded
//! last, each the same kind over the same box. [`Renderer::patch`] touches
//! only those: each is re-ingested at the store slot it holds, re-lowered into
//! the instance slots it was lowered to, uploaded alone, and its surface draw
//! re-keyed, so the frame costs the primitives it changed and not the scene.
//!
//! Anything that would move another entry — a new kind or count, a new box, a
//! new texture, a backdrop capture re-rendering what lies under it — is not a
//! patch: the caller uploads the stream whole.

use viso_gpu::GpuBackend;

use super::Renderer;
use crate::primitive::{GlyphRunDraw, Primitive};
use crate::scene::ingest::Instance;
use crate::scene::store::{DirtyPlanes, StoreRef, glyph_instances};

/// An entry a patch cannot rewrite in place, or one culled from its pass.
pub(super) const NOT_LOWERED: u32 = u32::MAX;

/// One validated rewrite: paint-order entry `entry` takes the primitive at
/// stream index `source`, as `instance` unless it is a glyph run.
#[derive(Debug, Clone, Copy)]
pub(super) struct Edit {
    entry: u32,
    source: u32,
    instance: Option<Instance>,
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
            StoreRef::Image(_) => Family::Image,
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
    /// Returns `false`, having changed nothing, when the rewrite is not a
    /// patch — a primitive changed kind, glyph count, bounds or texture, is
    /// not one instance in one store, or the frame captures a backdrop. Upload
    /// the stream whole then.
    pub fn patch<B: GpuBackend>(
        &mut self,
        backend: &mut B,
        primitives: &[Primitive],
        changed: &[u32],
    ) -> bool {
        if primitives.len() != self.stream_len {
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
        for &source in changed {
            match self.edit(primitives, source) {
                Some(edit) => edits.push(edit),
                None => {
                    self.edits = edits;
                    return false;
                }
            }
        }
        self.scene.ingest_stats.dirty_primitives = 0;
        let mut slots = std::mem::take(&mut self.edit_slots);
        slots.clear();
        for edit in &edits {
            let index = edit.entry as usize;
            let entry = self.scene.paint_order[index];
            let dirty = match (edit.instance, entry.store) {
                (Some(instance), store) => self.scene_set(store, instance),
                (None, StoreRef::GlyphRun(run)) => match &primitives[edit.source as usize] {
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

    /// The rewrite of the entry stream index `source` recorded, if it is a
    /// patch.
    fn edit(&self, primitives: &[Primitive], source: u32) -> Option<Edit> {
        let prim = primitives.get(source as usize)?;
        let order = &self.scene.paint_order;
        let index = order.partition_point(|e| e.source < source);
        let entry = order.get(index)?;
        if entry.source != source || order.get(index + 1).is_some_and(|e| e.source == source) {
            return None;
        }
        if self.lowered.get(index).is_none() {
            return None;
        }
        let instance = match (prim, entry.store) {
            (Primitive::GlyphRun(draw), StoreRef::GlyphRun(run)) => {
                let glyphs = glyph_instances(&draw.glyphs, color(draw));
                if !self
                    .scene
                    .glyph_runs
                    .fits(run, glyphs, draw.atlas, draw.lane)
                {
                    return None;
                }
                None
            }
            _ => {
                let (instance, bounds) = Instance::of(prim, entry.context.fold, None)?;
                let old = entry.bounds;
                let same_box = bounds.local == old.local
                    && bounds.world == old.world
                    && bounds.paint == old.paint
                    && bounds.effect == old.effect;
                if !same_box || !self.holds(entry.store, &instance) {
                    return None;
                }
                Some(instance)
            }
        };
        Some(Edit {
            entry: index as u32,
            source,
            instance,
        })
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
    use viso_gpu::{CpuRaster, GpuBackend, RawWindowHandle, SurfaceId, TextureDesc, TextureFormat};

    use super::*;
    use crate::primitive::{
        AnalyticCapsule, AnalyticEllipse, AnalyticLine, AnalyticRRect, AnalyticShadow, Border,
        Corners, GlyphInstanceData, GlyphLane, ImageDraw, LayerClip, LineCap, LineJoin, Point,
        Quad, Rect, Rgba, ShadowShape, TransformSlot,
    };
    use crate::transform::TransformUpdate;

    const SIZE: f32 = 128.0;

    struct Gpu {
        gpu: CpuRaster,
        surface: SurfaceId,
        r: Renderer,
    }

    impl Gpu {
        fn new() -> Gpu {
            let mut gpu = CpuRaster::new();
            let surface = gpu.create_surface(RawWindowHandle::Headless, SIZE as u32, SIZE as u32);
            let format = gpu.surface_format(surface);
            let mut r = Renderer::new(&mut gpu, format);
            r.set_surface_size([SIZE, SIZE]);
            Gpu { gpu, surface, r }
        }

        fn texture(&mut self) -> viso_gpu::TextureId {
            self.gpu.create_texture(&TextureDesc {
                width: 4,
                height: 4,
                format: TextureFormat::Bgra8Unorm,
                render_target: false,
                label: "t",
            })
        }

        fn upload(&mut self, scene: &[Primitive]) {
            self.r.upload(&mut self.gpu, scene);
        }

        fn patch(&mut self, scene: &[Primitive], changed: &[u32]) -> bool {
            self.r.patch(&mut self.gpu, scene, changed)
        }

        fn present(&mut self) -> Vec<u8> {
            self.r.submit(
                &mut self.gpu,
                self.surface,
                [0.0, 0.0, 0.0, 1.0],
                [SIZE, SIZE],
            );
            self.gpu.read_pixels_bgra8(self.surface)
        }
    }

    fn rect(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect { x, y, w, h }
    }

    fn rgba(r: f32, a: f32) -> Rgba {
        Rgba::new(r, 0.5, 0.25, a)
    }

    fn quad(r: Rect, color: Rgba) -> Primitive {
        Primitive::Quad(Quad {
            rect: r,
            color,
            radius: 2.0,
            border: Border::NONE,
        })
    }

    fn layer(clip: Rect, opacity: f32) -> Primitive {
        Primitive::Layer(LayerClip {
            clip,
            opacity,
            blur_sigma: 0.0,
            backdrop_sigma: 0.0,
        })
    }

    /// One primitive of every kind a patch rewrites, tinted `t`, over a
    /// background quad.
    fn kinds(texture: viso_gpu::TextureId, t: f32) -> Vec<Primitive> {
        let color = rgba(t, 0.8);
        vec![
            quad(rect(0.0, 0.0, SIZE, SIZE), rgba(0.1, 1.0)),
            Primitive::AnalyticRRect(AnalyticRRect {
                rect: rect(4.0, 4.0, 20.0, 12.0),
                color,
                radius: Corners::SHARP,
                border: Border::NONE,
            }),
            Primitive::AnalyticEllipse(AnalyticEllipse {
                rect: rect(28.0, 4.0, 12.0, 12.0),
                color,
                border: Border::NONE,
            }),
            Primitive::AnalyticCapsule(AnalyticCapsule {
                rect: rect(44.0, 4.0, 24.0, 8.0),
                color,
                border: Border::NONE,
            }),
            Primitive::AnalyticLine(AnalyticLine {
                p0: Point { x: 4.0, y: 30.0 },
                p1: Point { x: 60.0, y: 40.0 },
                width: 3.0,
                color,
                cap: LineCap::Round,
                join: LineJoin::Miter,
                miter_limit: 4.0,
                border: Border::NONE,
            }),
            Primitive::AnalyticShadow(AnalyticShadow {
                rect: rect(70.0, 10.0, 20.0, 20.0),
                color,
                radius: Corners::SHARP,
                offset: [2.0, 2.0],
                sigma: 3.0,
                spread: 0.0,
                shape: ShadowShape::RoundedBox,
                inner: false,
            }),
            Primitive::Image(ImageDraw {
                tint: color,
                ..ImageDraw::new(rect(4.0, 50.0, 16.0, 16.0), texture)
            }),
            Primitive::GlyphRun(GlyphRunDraw {
                glyphs: (0..4)
                    .map(|i| GlyphInstanceData {
                        rect: rect(30.0 + i as f32 * 6.0, 50.0, 5.0, 8.0),
                        uv: rect(0.0, 0.0, 0.5, 0.5),
                    })
                    .collect(),
                atlas: texture,
                color,
                lane: GlyphLane::CoverageA8,
            }),
        ]
    }

    /// Requires `patched` to hold exactly what `whole` holds after uploading
    /// the same stream whole.
    #[track_caller]
    fn same_state(patched: &Renderer, whole: &Renderer) {
        assert_eq!(patched.quad_scratch, whole.quad_scratch);
        assert_eq!(patched.analytic_rrect_scratch, whole.analytic_rrect_scratch);
        assert_eq!(
            patched.analytic_ellipse_scratch,
            whole.analytic_ellipse_scratch
        );
        assert_eq!(
            patched.analytic_capsule_scratch,
            whole.analytic_capsule_scratch
        );
        assert_eq!(patched.analytic_line_scratch, whole.analytic_line_scratch);
        assert_eq!(
            patched.analytic_shadow_scratch,
            whole.analytic_shadow_scratch
        );
        assert_eq!(patched.image_scratch, whole.image_scratch);
        assert_eq!(patched.glyph_scratch, whole.glyph_scratch);
        assert_eq!(patched.segments, whole.segments);
        assert_eq!(patched.quad_pool.shadow(), whole.quad_pool.shadow());
        assert_eq!(patched.glyph_pool.shadow(), whole.glyph_pool.shadow());
        assert_eq!(patched.image_pool.shadow(), whole.image_pool.shadow());
        let order = patched.scene.paint_order.len();
        assert_eq!(order, whole.scene.paint_order.len());
        for i in 0..order {
            assert_eq!(patched.scene.key(i), whole.scene.key(i), "entry {i} key");
            // A whole upload stamps every composite as changed; a patch
            // stamps only what it rewrote.
            if matches!(whole.scene.paint_order[i].store, StoreRef::Composite { .. }) {
                continue;
            }
            assert_eq!(
                patched.scene.content_stamp(i),
                whole.scene.content_stamp(i),
                "entry {i} stamp"
            );
        }
        assert_eq!(patched.present.snapshot(), whole.present.snapshot());
    }

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
    fn a_rewrite_that_moves_anything_else_is_not_a_patch() {
        let mut gpu = Gpu::new();
        let texture = gpu.texture();
        let other = gpu.texture();
        let before = kinds(texture, 0.2);
        let refused = |gpu: &mut Gpu, after: Vec<Primitive>, at: u32| {
            gpu.upload(&before);
            assert!(!gpu.patch(&after, &[at]), "entry {at} refused");
        };
        // A new box.
        let mut after = before.clone();
        after[1] = Primitive::AnalyticRRect(AnalyticRRect {
            rect: rect(4.0, 4.0, 22.0, 12.0),
            color: rgba(0.2, 0.8),
            radius: Corners::SHARP,
            border: Border::NONE,
        });
        refused(&mut gpu, after, 1);
        // A new kind.
        let mut after = before.clone();
        after[1] = quad(rect(4.0, 4.0, 20.0, 12.0), rgba(0.2, 0.8));
        refused(&mut gpu, after, 1);
        // A new texture.
        let mut after = before.clone();
        if let Primitive::Image(image) = &mut after[6] {
            image.texture = other;
        }
        refused(&mut gpu, after, 6);
        // Another glyph count.
        let mut after = before.clone();
        if let Primitive::GlyphRun(run) = &mut after[7] {
            run.glyphs.pop();
        }
        refused(&mut gpu, after, 7);
        // A stream of another length.
        let mut after = before.clone();
        after.push(quad(rect(0.0, 0.0, 4.0, 4.0), rgba(0.2, 1.0)));
        refused(&mut gpu, after, 1);
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
        let before = vec![
            quad(rect(4.0, 4.0, 20.0, 20.0), rgba(0.3, 1.0)),
            quad(rect(30.0, 4.0, 20.0, 20.0), rgba(0.3, 1.0)),
        ];
        let mut after = before.clone();
        after[0] = quad(rect(4.0, 4.0, 20.0, 20.0), rgba(0.9, 1.0));
        after[1] = quad(rect(30.0, 4.0, 24.0, 20.0), rgba(0.3, 1.0));
        patched.upload(&before);
        whole.upload(&before);
        assert!(!patched.patch(&after, &[0, 1]));
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
        let crate::damage::Region::Rects(rects) = kept.r.last_damage().clone() else {
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
