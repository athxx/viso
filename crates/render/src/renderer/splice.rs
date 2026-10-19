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
//!
//! Lowering splices too. A draw reads its instances wherever its segment
//! points, so the inserted leaves are lowered at the tail of each family's
//! scratch and only that tail is uploaded; every other instance keeps its
//! slot, and the slots the replaced leaves held are left for the next whole
//! lowering to reclaim. The segments around the run are lowered again with
//! it, so they join as a whole lowering would join them; one too long to
//! lower again is cut where the run meets it, a join lost until the next
//! whole lowering. Past [`FRAGMENTS`] lost joins, or once the slots left
//! behind outnumber half the live ones, a splice lowers whole.

use std::ops::Range;

use viso_gpu::GpuBackend;

use super::patch::NOT_LOWERED;
use super::{PassTarget, Renderer, Segment, SegmentKind};
use crate::batch::joins;
use crate::primitive::Primitive;
use crate::scene::EmitContext;
use crate::scene::bounds::Bounds;
use crate::scene::ingest::Instance;
use crate::scene::splice::{Draw, Inserted};
use crate::scene::store::glyph_instances;

/// A segment beside a spliced run is lowered again with it while it draws
/// at most this many instances; a longer one is cut where the run meets it.
const RELOWER: u32 = 256;

/// How many joins splices may lose before one lowers whole.
const FRAGMENTS: u32 = 64;

/// What lowering a run reuses from splice to splice.
#[derive(Debug, Default)]
pub(super) struct Lowering {
    /// The segments replacing the ones the run touched, and the entry each
    /// opens at.
    segments: Vec<Segment>,
    entries: Vec<u32>,
    /// The segments the run's entries were lowered into, and the entry each
    /// opens at.
    run: Vec<Segment>,
    opens: Vec<u32>,
    /// Where each entry lowered again starts in its family's scratch.
    cursors: Vec<u32>,
}

/// The leaf families' scratch lengths, by [`super::patch::Family`].
type Tails = [usize; 8];

/// Whether a segment of `kind` draws leaves only.
fn draws_leaves(kind: SegmentKind) -> bool {
    matches!(
        kind,
        SegmentKind::Quad
            | SegmentKind::AnalyticRRect
            | SegmentKind::AnalyticEllipse
            | SegmentKind::AnalyticCapsule
            | SegmentKind::AnalyticLine
            | SegmentKind::AnalyticShadow
            | SegmentKind::Image { .. }
            | SegmentKind::GlyphRun { .. }
            | SegmentKind::MtsdfRun { .. }
    )
}

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

/// Whether a paint-order entry over `store` is a leaf.
fn is_leaf_entry(store: crate::scene::store::StoreRef) -> bool {
    crate::scene::splice::Leaf::of(store).is_some()
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
        let mut in_place = self.lowers_in_place();
        let tails = self.tails();
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
            in_place = in_place
                && self
                    .lower_run(backend, from, to - from, new.len())
                    .is_some();
            self.splice_surface_draws(from, to - from, new.len());
            self.shift_marks(old + s.removed, s.shift());
        }
        self.spliced = new;
        self.spliced_glyphs = glyphs;
        self.stream_len = primitives.len();
        if in_place {
            self.append_pools(backend, tails);
        } else {
            self.culled_this_frame = 0;
            self.program_draws_dropped = 0;
            self.lower_from_scene(backend);
            self.sync_pools(backend);
        }
        true
    }

    /// Whether a splice may lower in place: the segments index the paint
    /// order, and the joins lost and slots left behind since the last whole
    /// lowering are few.
    fn lowers_in_place(&self) -> bool {
        let live = self.tails().iter().sum::<usize>() - self.holes;
        self.segment_entries.len() == self.segments.len()
            && self.fragments < FRAGMENTS
            && self.holes <= live / 2
    }

    /// The leaf families' scratch lengths.
    fn tails(&self) -> Tails {
        [
            self.quad_scratch.len(),
            self.analytic_rrect_scratch.len(),
            self.analytic_ellipse_scratch.len(),
            self.analytic_capsule_scratch.len(),
            self.analytic_line_scratch.len(),
            self.analytic_shadow_scratch.len(),
            self.image_scratch.len(),
            self.glyph_scratch.len(),
        ]
    }

    /// Upload what each leaf family's scratch gained past `tails`.
    fn append_pools<B: GpuBackend>(&mut self, backend: &mut B, tails: Tails) {
        let mut writes = 0;
        let mut bytes = 0;
        macro_rules! append {
            ($k:expr, $pool:ident, $scratch:ident) => {
                if self.$scratch.len() > tails[$k] {
                    writes += self.$pool.append(backend, &self.$scratch);
                    bytes += self.$pool.last_upload_bytes();
                }
            };
        }
        append!(0, quad_pool, quad_scratch);
        append!(1, analytic_rrect_pool, analytic_rrect_scratch);
        append!(2, analytic_ellipse_pool, analytic_ellipse_scratch);
        append!(3, analytic_capsule_pool, analytic_capsule_scratch);
        append!(4, analytic_line_pool, analytic_line_scratch);
        append!(5, analytic_shadow_pool, analytic_shadow_scratch);
        append!(6, image_pool, image_scratch);
        append!(7, glyph_pool, glyph_scratch);
        self.uploaded_ranges = writes;
        self.gpu_upload_bytes = bytes;
    }

    /// Lower the run of paint-order entries a splice just replaced: the
    /// `removed` entries from `at` the last lowering drew are now the `added`
    /// ones there. The run, and each segment beside it short enough to lower
    /// again, is lowered at the scratch tails into segments of its own; a
    /// longer one beside it keeps the instances outside the run. `None`,
    /// having changed nothing, when the run meets a segment that does not
    /// draw leaves.
    fn lower_run<B: GpuBackend>(
        &mut self,
        backend: &mut B,
        at: usize,
        removed: usize,
        added: usize,
    ) -> Option<()> {
        let firsts = &self.segment_entries;
        let entries = self.lowered.len();
        let end = at + removed;
        let shift = added as i64 - removed as i64;
        let moved = |i: usize| (i as i64 + shift) as usize;
        let entry_end = |s: usize| firsts.get(s + 1).map_or(entries, |&f| f as usize);
        let before = firsts.partition_point(|&f| (f as usize) < at);
        let after = firsts.partition_point(|&f| (f as usize) < end);

        // The segments replaced are `x..y`; the entries lowered again are
        // `a..b`, numbered as the paint order now is.
        let (mut x, mut a, mut prefix) = (before, at, None);
        if let Some(s) = before.checked_sub(1) {
            let cut = match entry_end(s) > at {
                true => self.cut(s, at..entry_end(s))?,
                false => None,
            };
            if self.relowers(s, firsts[s] as usize..at) {
                (x, a) = (s, firsts[s] as usize);
            } else if let Some(c) = cut {
                let seg = self.segments[s];
                x = s;
                prefix = Some(Segment {
                    count: c - seg.start,
                    ..seg
                });
            }
        }
        let (mut y, mut b, mut suffix) = (after, at + added, None);
        match after.checked_sub(1).filter(|&s| entry_end(s) > end) {
            Some(s) => {
                let cut = self.cut(s, end..entry_end(s))?;
                y = s + 1;
                if self.relowers(s, moved(end)..moved(entry_end(s))) {
                    b = moved(entry_end(s));
                } else if let Some(c) = cut {
                    let seg = self.segments[s];
                    suffix = Some(Segment {
                        start: c,
                        count: seg.start + seg.count - c,
                        ..seg
                    });
                }
            }
            None => {
                if after < self.segments.len()
                    && self.relowers(after, moved(end)..moved(entry_end(after)))
                {
                    (y, b) = (after + 1, moved(entry_end(after)));
                }
            }
        }

        let mut l = std::mem::take(&mut self.lowering);
        l.cursors.clear();
        l.opens.clear();
        l.run.clear();
        std::mem::swap(&mut self.segments, &mut l.run);
        let order = std::mem::take(&mut self.scene.paint_order);
        for (i, entry) in order.iter().enumerate().take(b).skip(a) {
            l.cursors.push(self.lowering_cursor(entry.store));
            let opened = self.segments.len();
            let context = entry.context;
            self.lower_entry(
                backend,
                entry,
                context.clip,
                PassTarget::Main,
                context.origin,
            );
            for _ in opened..self.segments.len() {
                l.opens.push(i as u32);
            }
        }
        self.scene.paint_order = order;
        std::mem::swap(&mut self.segments, &mut l.run);

        let drew: u32 = self.segments[x..y].iter().map(|s| s.count).sum();
        let kept = prefix.map_or(0, |p| p.count) + suffix.map_or(0, |s| s.count);
        self.holes += (drew - kept) as usize;
        l.segments.clear();
        l.entries.clear();
        if let Some(p) = prefix {
            l.segments.push(p);
            l.entries.push(self.segment_entries[x]);
        }
        l.segments.extend_from_slice(&l.run);
        l.entries.extend_from_slice(&l.opens);
        if let Some(s) = suffix {
            l.segments.push(s);
            l.entries.push(moved(end) as u32);
        }
        let len = l.segments.len();
        self.segments.splice(x..y, l.segments.iter().copied());
        self.segment_entries.splice(x..y, l.entries.iter().copied());
        if shift != 0 {
            for f in &mut self.segment_entries[x + len..] {
                *f = (i64::from(*f) + shift) as u32;
            }
        }
        let old_b = (b as i64 - shift) as usize;
        self.lowered.splice(a..old_b, l.cursors.iter().copied());

        // A join a whole lowering would make between segments now side by
        // side is lost until it runs.
        let lo = x.saturating_sub(1);
        let hi = (x + len + 1).min(self.segments.len());
        for w in self.segments[lo..hi].windows(2) {
            if joins(&w[0].batch_item(), &w[1].batch_item()) {
                self.fragments += 1;
            }
        }
        self.lowering = l;
        Some(())
    }

    /// Where segment `s` draws the first of the old entries `entries` it
    /// draws, `None` when it draws none of them; `None` outright when it
    /// does not draw leaves, so where it draws them is not known.
    fn cut(&self, s: usize, entries: Range<usize>) -> Option<Option<u32>> {
        if !draws_leaves(self.segments[s].kind) {
            return None;
        }
        Some(
            self.lowered[entries]
                .iter()
                .copied()
                .find(|&c| c != NOT_LOWERED),
        )
    }

    /// Whether segment `s` may be lowered again over the paint-order
    /// entries `entries`: it is short, drawn into the surface pass, and every
    /// one of them is a leaf there.
    fn relowers(&self, s: usize, entries: Range<usize>) -> bool {
        let seg = &self.segments[s];
        seg.count <= RELOWER
            && seg.target == PassTarget::Main
            && draws_leaves(seg.kind)
            && self.scene.paint_order[entries]
                .iter()
                .all(|e| is_leaf_entry(e.store) && e.context.offscreen.is_none())
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

    /// A row of the randomized list: a quad and a label of `glyphs` glyphs,
    /// or, with no label, `glyphs` quads in a run.
    #[derive(Debug, Clone, Copy)]
    struct Row {
        glyphs: usize,
        label: bool,
        tint: f32,
        y: f32,
    }

    fn row_prims(t: viso_gpu::TextureId, r: Row) -> Vec<Primitive> {
        let y = r.y;
        if r.label {
            vec![
                quad(rect(4.0, y, 100.0, 3.0), rgba(r.tint, 1.0)),
                text(t, 8.0, y, r.glyphs),
            ]
        } else {
            (0..r.glyphs)
                .map(|i| quad(rect(i as f32 * 0.25, y, 0.2, 2.0), rgba(r.tint, 1.0)))
                .collect()
        }
    }

    /// The randomized list's stream, and where each row starts in it.
    fn rows_stream(t: viso_gpu::TextureId, rows: &[Row], tint: f32) -> (Vec<Primitive>, Vec<u32>) {
        let mut out = vec![quad(rect(0.0, 0.0, SIZE, SIZE), rgba(0.05, 1.0))];
        out.extend(group(0.0));
        out.extend([
            layer(rect(0.0, 0.0, SIZE, 100.0), 1.0),
            Primitive::Transform(TransformSlot(0)),
        ]);
        let mut starts = Vec::new();
        for r in rows {
            starts.push(out.len() as u32);
            out.extend(row_prims(t, *r));
        }
        starts.push(out.len() as u32);
        out.extend([
            Primitive::TransformEnd,
            Primitive::LayerEnd,
            quad(rect(0.0, 110.0, SIZE, 10.0), rgba(0.3, 1.0)),
        ]);
        out.extend(group(tint));
        (out, starts)
    }

    /// A translucent group of two overlapping quads, drawn through an
    /// offscreen pass, the first tinted `t`.
    fn group(t: f32) -> [Primitive; 4] {
        [
            layer(rect(10.0, 100.0, 60.0, 20.0), 0.5),
            quad(rect(10.0, 100.0, 40.0, 20.0), rgba(t, 1.0)),
            quad(rect(30.0, 100.0, 40.0, 20.0), rgba(0.6, 1.0)),
            Primitive::LayerEnd,
        ]
    }

    #[test]
    fn every_splice_lowers_into_what_a_whole_upload_draws() {
        let mut lcg = 0x2545_f491_4f6c_dd1du64;
        let mut below = |n: u32| {
            lcg = lcg
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((lcg >> 33) % u64::from(n.max(1))) as u32
        };
        let (mut cut, mut lost, mut compacted) = (false, false, false);
        for seed in 0..6 {
            let mut spliced = Gpu::new();
            let t = spliced.texture();
            let mut whole = Gpu::new();
            whole.texture();
            let row = |below: &mut dyn FnMut(u32) -> u32| {
                let label = below(4) != 0;
                Row {
                    glyphs: if label {
                        1 + below(8) as usize
                    } else {
                        1 + below(400) as usize
                    },
                    label,
                    tint: below(10) as f32 / 10.0,
                    y: below(90) as f32,
                }
            };
            let mut rows: Vec<Row> = (0..12 + seed).map(|_| row(&mut below)).collect();
            let mut tint = 0.4;
            let (stream, _) = rows_stream(t, &rows, tint);
            spliced.upload(&stream);
            whole.upload(&stream);
            for frame in 0..150 {
                // Each old row is kept, rewritten or removed; rows land
                // before some.
                let mut next = Vec::new();
                let mut splices: Vec<Splice> = Vec::new();
                let (_, starts) = rows_stream(t, &rows, tint);
                let mut at = 7u32;
                let edit =
                    |splices: &mut Vec<Splice>, at: u32, removed: u32, inserted: u32| match splices
                        .last_mut()
                    {
                        Some(s) if s.at + s.inserted == at => {
                            s.removed += removed;
                            s.inserted += inserted;
                        }
                        _ => splices.push(Splice {
                            at,
                            removed,
                            inserted,
                        }),
                    };
                for (i, r) in rows.iter().enumerate() {
                    let old = starts[i + 1] - starts[i];
                    if below(16) == 0 {
                        let n = row(&mut below);
                        let len = row_prims(t, n).len() as u32;
                        edit(&mut splices, at, 0, len);
                        next.push(n);
                        at += len;
                    }
                    match below(12) {
                        0 => edit(&mut splices, at, old, 0),
                        1 | 2 => {
                            let mut n = *r;
                            if n.label {
                                n.glyphs = 1 + below(8) as usize;
                            } else {
                                n = row(&mut below);
                            }
                            let len = row_prims(t, n).len() as u32;
                            edit(&mut splices, at, old, len);
                            next.push(n);
                            at += len;
                        }
                        _ => {
                            next.push(*r);
                            at += old;
                        }
                    }
                }
                rows = next;
                let (stream, _) = rows_stream(t, &rows, tint);
                let holes = spliced.r.holes;
                assert!(
                    spliced.splice(&stream, &splices),
                    "seed {seed} frame {frame}"
                );
                whole.upload(&stream);
                same_scene(&spliced.r, &whole.r);
                if below(3) == 0 {
                    // The later group's first quad, recolored in its pass.
                    tint = below(10) as f32 / 10.0;
                    let (tinted, _) = rows_stream(t, &rows, tint);
                    let at = tinted.len() as u32 - 3;
                    assert!(spliced.patch(&tinted, &[at]));
                    whole.upload(&tinted);
                    same_state_but_stamps(&spliced.r, &whole.r);
                }
                cut |= spliced.r.holes > holes && spliced.r.fragments > 0;
                lost |= spliced.r.segments.len() > whole.r.segments.len();
                compacted |= holes > 0 && spliced.r.holes == 0;
            }
        }
        assert!(
            cut && lost && compacted,
            "cut {cut}, lost {lost}, compacted {compacted}"
        );
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
