//! Splicing an uploaded frame (§9.7).
//!
//! A frame whose shape changed in one place — a virtual list mounts a row and
//! unmounts another, a label's text gains a glyph — rewrites a run of the
//! stream it uploaded last and leaves the rest where it was, only moved.
//! [`Renderer::splice`] takes those runs: each replaced run's entries are
//! spliced out of the retained scene and the new primitives' spliced in,
//! every later entry keeping its retained content and moving its stream index
//! and store slot by the difference. Only the inserted primitives are
//! ingested.
//!
//! A run may only cross leaves — quads, analytic shapes and shadows, images,
//! glyph runs — drawn into the surface pass outside any folded group: what
//! opens a layer, a transform root or a capture, or decides how a layer is
//! drawn, is not spliced. The caller uploads the stream whole then.

use viso_gpu::GpuBackend;

use super::Renderer;
use crate::primitive::Primitive;
use crate::scene::EmitContext;
use crate::scene::bounds::Bounds;
use crate::scene::ingest::Instance;
use crate::scene::splice::{Draw, Inserted};
use crate::scene::store::glyph_instances;

/// One run of a stream a splice rewrote: the `inserted` primitives from `at`
/// in the new stream stand where `removed` primitives stood in the old one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Splice {
    pub at: u32,
    pub removed: u32,
    pub inserted: u32,
}

impl Splice {
    fn shift(&self) -> i64 {
        i64::from(self.inserted) - i64::from(self.removed)
    }
}

/// Whether `prim` is a leaf a splice can cross.
pub(super) fn is_leaf(prim: &Primitive) -> bool {
    matches!(
        prim,
        Primitive::Quad(_)
            | Primitive::AnalyticRRect(_)
            | Primitive::AnalyticEllipse(_)
            | Primitive::AnalyticCapsule(_)
            | Primitive::AnalyticLine(_)
            | Primitive::AnalyticShadow(_)
            | Primitive::Image(_)
            | Primitive::GlyphRun(_)
    )
}

impl Renderer {
    /// Bring the uploaded frame to `primitives`: the stream last uploaded with
    /// the runs `splices` — ascending, apart, in the new stream's indices —
    /// rewritten. Only the inserted primitives are ingested; the frame is
    /// lowered, uploaded and keyed for damage as an upload would.
    ///
    /// Returns `false`, having changed nothing, when a run crosses anything
    /// but leaves drawn straight into the surface pass outside a folded group,
    /// or the frame captures a backdrop. Upload the stream whole then.
    pub fn splice<B: GpuBackend>(
        &mut self,
        backend: &mut B,
        primitives: &[Primitive],
        splices: &[Splice],
    ) -> bool {
        if !self.splices(primitives, splices) {
            return false;
        }
        let mut new = std::mem::take(&mut self.spliced);
        let mut glyphs = std::mem::take(&mut self.spliced_glyphs);
        self.scene.ingest_stats.dirty_primitives = 0;
        // Right to left: a run's old indices are its new ones less what the
        // runs before it moved, and nothing before it has moved yet.
        let mut before: i64 = splices.iter().map(Splice::shift).sum();
        for s in splices.iter().rev() {
            before -= s.shift();
            let old = (i64::from(s.at) - before) as u32;
            let context = self.context_before(old);
            new.clear();
            glyphs.clear();
            for (j, prim) in primitives[s.at as usize..(s.at + s.inserted) as usize]
                .iter()
                .enumerate()
            {
                let source = old + j as u32;
                let (draw, bounds) = match prim {
                    Primitive::GlyphRun(run) => {
                        if run.glyphs.is_empty() {
                            continue;
                        }
                        let color = [run.color.r, run.color.g, run.color.b, run.color.a];
                        let from = glyphs.len();
                        glyphs.extend(glyph_instances(&run.glyphs, color));
                        let draw = Draw::Glyphs {
                            count: (glyphs.len() - from) as u32,
                            atlas: run.atlas,
                            lane: run.lane,
                        };
                        (draw, Bounds::default())
                    }
                    _ => match Instance::of(prim, context.fold) {
                        Some((instance, bounds)) => (Draw::Instance(instance), bounds),
                        None => continue,
                    },
                };
                new.push(Inserted {
                    draw,
                    context,
                    bounds,
                    source,
                });
            }
            let order = &self.scene.paint_order;
            let from = order.partition_point(|e| e.source < old);
            let to = order.partition_point(|e| e.source < old + s.removed);
            self.scene.splice(from..to, &new, &glyphs, s.shift());
            self.shift_marks(old + s.removed, s.shift());
        }
        self.spliced = new;
        self.spliced_glyphs = glyphs;
        self.stream_len = primitives.len();
        self.culled_this_frame = 0;
        self.program_draws_dropped = 0;
        self.lower_from_scene(backend);
        self.sync_pools(backend);
        self.collect_surface_draws();
        true
    }

    /// Whether `splices` bring the stream last uploaded to `primitives` by
    /// crossing leaves alone.
    fn splices(&self, primitives: &[Primitive], splices: &[Splice]) -> bool {
        if !self.backdrop_captures.is_empty() {
            return false;
        }
        let mut len = self.stream_len as i64;
        let mut end = 0;
        let mut before = 0;
        for s in splices {
            if s.at < end {
                return false;
            }
            end = s.at + s.inserted;
            len += s.shift();
            let old = (i64::from(s.at) - before) as u32;
            before += s.shift();
            let mark = self.marks.partition_point(|&(at, _)| at < old);
            if self
                .marks
                .get(mark)
                .is_some_and(|&(at, _)| at < old + s.removed)
            {
                return false;
            }
            let context = self.context_before(old);
            if context.offscreen.is_some() || context.fold.to_bits() != 1.0f32.to_bits() {
                return false;
            }
            let Some(run) = primitives.get(s.at as usize..end as usize) else {
                return false;
            };
            if !run.iter().all(is_leaf) {
                return false;
            }
        }
        len == primitives.len() as i64
    }

    /// The context of a leaf at old stream index `at`: the one the last mark
    /// before it left.
    fn context_before(&self, at: u32) -> EmitContext {
        match self.marks.partition_point(|&(m, _)| m < at) {
            0 => self.root_context,
            i => self.marks[i - 1].1,
        }
    }

    /// Move every mark and layer bound from old stream index `from` on by
    /// `shift`.
    fn shift_marks(&mut self, from: u32, shift: i64) {
        if shift == 0 {
            return;
        }
        let moved = |at: &mut u32| {
            if *at >= from {
                *at = (i64::from(*at) + shift) as u32;
            }
        };
        let first = self.marks.partition_point(|&(at, _)| at < from);
        for (at, _) in &mut self.marks[first..] {
            moved(at);
        }
        for layer in &mut self.layers {
            moved(&mut layer.source);
            moved(&mut layer.end);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::testing::*;
    use super::*;
    use crate::primitive::{GlyphInstanceData, GlyphLane, GlyphRunDraw, LayerClip, TransformSlot};

    /// A run of `n` glyphs at `(x, y)`.
    fn text(texture: viso_gpu::TextureId, x: f32, y: f32, n: usize) -> Primitive {
        Primitive::GlyphRun(GlyphRunDraw {
            glyphs: (0..n)
                .map(|i| GlyphInstanceData {
                    rect: rect(x + i as f32 * 5.0, y, 4.0, 6.0),
                    uv: rect(0.0, 0.0, 0.5, 0.5),
                })
                .collect(),
            atlas: texture,
            color: rgba(0.9, 1.0),
            lane: GlyphLane::CoverageA8,
        })
    }

    /// Row `i` of a list: a quad and a label of `glyphs` glyphs.
    fn row(texture: viso_gpu::TextureId, i: usize, glyphs: usize) -> [Primitive; 2] {
        let y = 4.0 + i as f32 * 10.0;
        [
            quad(rect(4.0, y, 100.0, 8.0), rgba(0.1 * (i % 7) as f32, 1.0)),
            text(texture, 8.0, y + 1.0, glyphs),
        ]
    }

    /// Rows `rows` of a list scrolled in a viewport, between a background and
    /// a footer; row `long` has a longer label.
    fn list(
        texture: viso_gpu::TextureId,
        rows: std::ops::Range<usize>,
        long: usize,
    ) -> Vec<Primitive> {
        let mut out = vec![
            quad(rect(0.0, 0.0, SIZE, SIZE), rgba(0.05, 1.0)),
            layer(rect(0.0, 0.0, SIZE, 100.0), 1.0),
            Primitive::Transform(TransformSlot(0)),
        ];
        for i in rows {
            out.extend(row(texture, i, if i == long { 6 } else { 3 }));
        }
        out.extend([
            Primitive::TransformEnd,
            Primitive::LayerEnd,
            quad(rect(0.0, 110.0, SIZE, 10.0), rgba(0.3, 1.0)),
        ]);
        out
    }

    /// Uploads `before`, then brings one renderer to `after` by splicing
    /// `splices` and another by uploading `after` whole; both must agree.
    #[track_caller]
    fn agree(before: &[Primitive], after: &[Primitive], splices: &[Splice]) -> (Gpu, Gpu) {
        let mut spliced = Gpu::new();
        let mut whole = Gpu::new();
        spliced.upload(before);
        whole.upload(before);
        assert!(spliced.splice(after, splices), "a splice");
        whole.upload(after);
        same_scene(&spliced.r, &whole.r);
        (spliced, whole)
    }

    fn at(at: u32, removed: u32, inserted: u32) -> Splice {
        Splice {
            at,
            removed,
            inserted,
        }
    }

    #[test]
    fn a_list_scrolling_a_row_splices_one_row_out_and_one_in() {
        let mut gpu = Gpu::new();
        let t = gpu.texture();
        let before = list(t, 0..8, 99);
        let after = list(t, 1..9, 99);
        // Row 0 leaves at 3; row 8 lands after rows 1..8, at 3 + 7 * 2.
        agree(&before, &after, &[at(3, 2, 0), at(17, 0, 2)]);
        agree(&after, &before, &[at(3, 0, 2), at(19, 2, 0)]);
    }

    #[test]
    fn a_label_that_grows_splices_its_run() {
        let mut gpu = Gpu::new();
        let t = gpu.texture();
        // Row 2's label is the sixth primitive's neighbor: 3 + 2 * 2 + 1.
        agree(&list(t, 0..8, 99), &list(t, 0..8, 2), &[at(8, 1, 1)]);
        agree(&list(t, 0..8, 2), &list(t, 0..8, 99), &[at(8, 1, 1)]);
    }

    #[test]
    fn every_leaf_kind_splices_in_and_out() {
        let mut gpu = Gpu::new();
        let t = gpu.texture();
        let leaves = kinds(t, 0.4);
        let base = list(t, 0..4, 99);
        let mut after = base.clone();
        // Every kind lands between rows 1 and 2, and again at the very end.
        after.splice(7..7, leaves.iter().cloned());
        let n = leaves.len() as u32;
        after.extend(leaves.iter().cloned());
        let end = after.len() as u32 - n;
        agree(&base, &after, &[at(7, 0, n), at(end, 0, n)]);
        agree(&after, &base, &[at(7, n, 0), at(base.len() as u32, n, 0)]);
        // At the very start too.
        let mut first = leaves.clone();
        first.extend(base.iter().cloned());
        agree(&base, &first, &[at(0, 0, n)]);
    }

    #[test]
    fn a_spliced_frame_presents_what_a_fresh_frame_draws() {
        let mut gpu = Gpu::new();
        let t = gpu.texture();
        let mut kept = Gpu::new();
        let before = list(t, 0..8, 99);
        let after = list(t, 0..8, 4);
        kept.upload(&before);
        kept.present();
        kept.present();
        assert!(kept.splice(&after, &[at(12, 1, 1)]));
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

    #[test]
    fn a_patch_after_a_splice_agrees() {
        let mut gpu = Gpu::new();
        let t = gpu.texture();
        let before = list(t, 0..8, 99);
        let after = list(t, 1..9, 99);
        let (mut spliced, mut whole) = agree(&before, &after, &[at(3, 2, 0), at(17, 0, 2)]);
        let mut recolored = after.clone();
        recolored[5] = quad(rect(4.0, 24.0, 100.0, 8.0), rgba(0.95, 1.0));
        assert!(spliced.patch(&recolored, &[5]));
        whole.upload(&recolored);
        same_scene(&spliced.r, &whole.r);
    }

    #[test]
    fn a_run_across_anything_but_leaves_is_not_a_splice() {
        let mut gpu = Gpu::new();
        let t = gpu.texture();
        let before = list(t, 0..4, 99);
        let refused = |gpu: &mut Gpu, before: &[Primitive], after: &[Primitive], s: &[Splice]| {
            gpu.upload(before);
            assert!(!gpu.splice(after, s), "{s:?} refused");
        };
        // Across the viewport's layer and transform.
        let mut after = before.clone();
        after.drain(1..3);
        refused(&mut gpu, &before, &after, &[at(1, 2, 0)]);
        // Inserting a layer.
        let mut after = before.clone();
        after.insert(5, layer(rect(0.0, 0.0, 10.0, 10.0), 1.0));
        refused(&mut gpu, &before, &after, &[at(5, 0, 1)]);
        // A length the runs do not account for, and runs out of order.
        refused(&mut gpu, &before, &before, &[at(3, 1, 0)]);
        refused(&mut gpu, &before, &before, &[at(5, 1, 1), at(3, 1, 1)]);
        // Inside a translucent layer, isolated or folded.
        for (opacity, overlap) in [(0.5, 20.0), (0.5, 50.0)] {
            let group = vec![
                layer(rect(0.0, 0.0, 100.0, 40.0), opacity),
                quad(rect(0.0, 0.0, 40.0, 40.0), rgba(0.2, 1.0)),
                quad(rect(overlap, 0.0, 40.0, 40.0), rgba(0.4, 1.0)),
                Primitive::LayerEnd,
            ];
            let mut after = group.clone();
            after[1] = quad(rect(0.0, 0.0, 40.0, 40.0), rgba(0.9, 1.0));
            refused(&mut gpu, &group, &after, &[at(1, 1, 1)]);
        }
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
        let mut after = frosted.clone();
        after.push(quad(rect(0.0, 0.0, 4.0, 4.0), rgba(0.2, 1.0)));
        refused(&mut gpu, &frosted, &after, &[at(4, 0, 1)]);
    }

    #[test]
    fn a_refused_splice_changes_nothing() {
        let mut gpu = Gpu::new();
        let t = gpu.texture();
        let before = list(t, 0..4, 99);
        let mut after = before.clone();
        after.drain(1..3);
        let mut refused = Gpu::new();
        let mut whole = Gpu::new();
        refused.upload(&before);
        whole.upload(&before);
        assert!(!refused.splice(&after, &[at(1, 2, 0)]));
        same_scene(&refused.r, &whole.r);
    }
}
