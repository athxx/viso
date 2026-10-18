//! Splicing runs of leaf entries into the retained scene (§9.7).
//!
//! A leaf — a quad, an analytic shape or shadow, an image, a glyph run — is
//! one paint-order entry over one slot of its kind's store, and every store
//! holds its kind's slots in paint order. A splice keeps that true: it
//! replaces a run of entries and, in each store, the run of slots they held,
//! then moves every later entry's stream index and slot by the difference.
//! The scene it leaves is the one a whole upload of the new stream would
//! build, but for the content stamps of the entries it did not touch.

use std::ops::Range;

use viso_gpu::TextureId;

use super::bounds::Bounds;
use super::ids::{GeometryId, ImageId};
use super::ingest::Instance;
use super::store::{self, StoreRef};
use super::{EmitContext, PaintEntry, Scene};
use crate::primitive::{GlyphInstance, GlyphLane};

/// The kinds of leaf, each the store it is slotted in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Leaf {
    Quad,
    AnalyticRRect,
    AnalyticEllipse,
    AnalyticCapsule,
    AnalyticLine,
    AnalyticShadow,
    Image,
    GlyphRun,
}

const LEAVES: usize = 8;

impl Leaf {
    /// The leaf `store` names, and its slot; `None` for an entry that is not
    /// a leaf.
    pub(crate) fn of(store: StoreRef) -> Option<(Leaf, u32)> {
        Some(match store {
            StoreRef::Quad(id) => (Leaf::Quad, id.index()),
            StoreRef::AnalyticRRect(id) => (Leaf::AnalyticRRect, id.index()),
            StoreRef::AnalyticEllipse(id) => (Leaf::AnalyticEllipse, id.index()),
            StoreRef::AnalyticCapsule(id) => (Leaf::AnalyticCapsule, id.index()),
            StoreRef::AnalyticLine(id) => (Leaf::AnalyticLine, id.index()),
            StoreRef::AnalyticShadow(id) => (Leaf::AnalyticShadow, id.index()),
            StoreRef::Image(id) => (Leaf::Image, id.index()),
            StoreRef::GlyphRun(run) => (Leaf::GlyphRun, run),
            _ => return None,
        })
    }

    /// Slot `slot` of this leaf's store.
    fn at(self, slot: u32) -> StoreRef {
        let id = GeometryId::new(slot);
        match self {
            Leaf::Quad => StoreRef::Quad(id),
            Leaf::AnalyticRRect => StoreRef::AnalyticRRect(id),
            Leaf::AnalyticEllipse => StoreRef::AnalyticEllipse(id),
            Leaf::AnalyticCapsule => StoreRef::AnalyticCapsule(id),
            Leaf::AnalyticLine => StoreRef::AnalyticLine(id),
            Leaf::AnalyticShadow => StoreRef::AnalyticShadow(id),
            Leaf::Image => StoreRef::Image(ImageId::new(slot)),
            Leaf::GlyphRun => StoreRef::GlyphRun(slot),
        }
    }
}

/// What an inserted leaf draws.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Draw {
    Instance(Instance),
    /// A run of `count` glyphs, the next ones in the splice's glyph buffer.
    Glyphs {
        count: u32,
        atlas: TextureId,
        lane: GlyphLane,
    },
}

impl Draw {
    fn leaf(&self) -> Leaf {
        match self {
            Draw::Instance(Instance::Quad(_)) => Leaf::Quad,
            Draw::Instance(Instance::AnalyticRRect(_)) => Leaf::AnalyticRRect,
            Draw::Instance(Instance::AnalyticEllipse(_)) => Leaf::AnalyticEllipse,
            Draw::Instance(Instance::AnalyticCapsule(_)) => Leaf::AnalyticCapsule,
            Draw::Instance(Instance::AnalyticLine(_)) => Leaf::AnalyticLine,
            Draw::Instance(Instance::AnalyticShadow(_)) => Leaf::AnalyticShadow,
            Draw::Instance(Instance::Image(..)) => Leaf::Image,
            Draw::Glyphs { .. } => Leaf::GlyphRun,
        }
    }
}

/// A leaf entry a splice inserts: what it draws, under which context, over
/// which bounds, from which stream index.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Inserted {
    pub(crate) draw: Draw,
    pub(crate) context: EmitContext,
    pub(crate) bounds: Bounds,
    pub(crate) source: u32,
}

impl Scene {
    /// Replace the leaf entries `range` of the paint order with `new`, whose
    /// glyph runs' glyphs are `glyphs` back to back, and move the stream index
    /// of every entry after them by `shift`.
    pub(crate) fn splice(
        &mut self,
        range: Range<usize>,
        new: &[Inserted],
        glyphs: &[GlyphInstance],
        shift: i64,
    ) {
        let mut removed = [0u32; LEAVES];
        let mut at = [u32::MAX; LEAVES];
        let mut gone_glyphs = 0u32;
        for e in &self.paint_order[range.clone()] {
            let (leaf, slot) = Leaf::of(e.store).expect("a splice removes leaves only");
            removed[leaf as usize] += 1;
            at[leaf as usize] = at[leaf as usize].min(slot);
            if let StoreRef::GlyphRun(run) = e.store {
                gone_glyphs += self.glyph_runs.run(run).map_or(0, |r| r.count);
            }
        }
        let mut added = [0u32; LEAVES];
        for n in new {
            added[n.draw.leaf() as usize] += 1;
        }
        // A kind with nothing removed starts its run where its first slot
        // after the range is, or at its store's end.
        let mut missing = (0..LEAVES)
            .filter(|&k| at[k] == u32::MAX && added[k] > 0)
            .count();
        if missing > 0 {
            for e in &self.paint_order[range.end..] {
                if let Some((leaf, slot)) = Leaf::of(e.store)
                    && at[leaf as usize] == u32::MAX
                {
                    at[leaf as usize] = slot;
                    if added[leaf as usize] > 0 {
                        missing -= 1;
                        if missing == 0 {
                            break;
                        }
                    }
                }
            }
        }
        let lens = [
            self.quads.len(),
            self.analytic_rrects.len(),
            self.analytic_ellipses.len(),
            self.analytic_capsules.len(),
            self.analytic_lines.len(),
            self.analytic_shadows.len(),
            self.images.len(),
            self.glyph_runs.len(),
        ];
        for k in 0..LEAVES {
            if at[k] == u32::MAX {
                at[k] = lens[k] as u32;
            }
        }

        self.splice_stores(&at, &removed, &added, new, glyphs);

        let (added_n, removed_n) = (new.len(), range.len());
        if added_n > 0 {
            self.revisions.bump_geometry();
        }
        if removed_n > 0 {
            self.revisions.bump_visibility();
        }
        let stamp = self.revisions.content();
        let mut next = at;
        let start = range.start;
        let entries = new.iter().enumerate().map(|(j, n)| {
            let leaf = n.draw.leaf();
            let slot = next[leaf as usize];
            next[leaf as usize] += 1;
            PaintEntry {
                id: store::primitive_id(start + j),
                store: leaf.at(slot),
                context: n.context,
                bounds: n.bounds,
                source: n.source,
            }
        });
        self.paint_order.splice(range.clone(), entries);
        self.content_stamps
            .splice(range.clone(), std::iter::repeat_n(stamp, added_n));
        self.keys.splice(
            range,
            std::iter::repeat_n((StoreRef::Quad(GeometryId::new(0)), 0, None), added_n),
        );
        for i in start..start + added_n {
            let store = self.paint_order[i].store;
            let (key, extent) = self.entry_key(store);
            self.keys[i] = (store, key, extent);
        }

        let mut delta = [0i64; LEAVES];
        for k in 0..LEAVES {
            delta[k] = i64::from(added[k]) - i64::from(removed[k]);
        }
        if shift != 0 || added_n != removed_n || delta.iter().any(|&d| d != 0) {
            for i in start + added_n..self.paint_order.len() {
                let e = &mut self.paint_order[i];
                e.id = store::primitive_id(i);
                e.source = (i64::from(e.source) + shift) as u32;
                if let Some((leaf, slot)) = Leaf::of(e.store) {
                    let d = delta[leaf as usize];
                    if d != 0 {
                        e.store = leaf.at((i64::from(slot) + d) as u32);
                        self.keys[i].0 = e.store;
                    }
                }
            }
        }

        let stats = &mut self.ingest_stats;
        let moved = |count: &mut u32, k: Leaf| {
            *count = (i64::from(*count) + delta[k as usize]) as u32;
        };
        moved(&mut stats.quad_instances, Leaf::Quad);
        moved(&mut stats.analytic_rrect_instances, Leaf::AnalyticRRect);
        moved(&mut stats.analytic_ellipse_instances, Leaf::AnalyticEllipse);
        moved(&mut stats.analytic_capsule_instances, Leaf::AnalyticCapsule);
        moved(&mut stats.analytic_line_instances, Leaf::AnalyticLine);
        moved(&mut stats.analytic_shadow_instances, Leaf::AnalyticShadow);
        stats.glyph_instances = (i64::from(stats.glyph_instances) + glyphs.len() as i64
            - i64::from(gone_glyphs)) as u32;
        stats.visible_primitives =
            (i64::from(stats.visible_primitives) + added_n as i64 - removed_n as i64) as u32;
        stats.dirty_primitives += added_n as u32;
    }

    /// Replace, in each leaf store, the `removed[k]` slots from `at[k]` with
    /// the `new` leaves of that kind.
    fn splice_stores(
        &mut self,
        at: &[u32; LEAVES],
        removed: &[u32; LEAVES],
        added: &[u32; LEAVES],
        new: &[Inserted],
        glyphs: &[GlyphInstance],
    ) {
        macro_rules! splice {
            ($leaf:ident, $store:ident) => {{
                let k = Leaf::$leaf as usize;
                if removed[k] > 0 || added[k] > 0 {
                    let new = new.iter().filter_map(|n| match n.draw {
                        Draw::Instance(Instance::$leaf(i)) => Some(i),
                        _ => None,
                    });
                    self.$store.splice(at[k] as usize, removed[k] as usize, new);
                }
            }};
        }
        splice!(Quad, quads);
        splice!(AnalyticRRect, analytic_rrects);
        splice!(AnalyticEllipse, analytic_ellipses);
        splice!(AnalyticCapsule, analytic_capsules);
        splice!(AnalyticLine, analytic_lines);
        splice!(AnalyticShadow, analytic_shadows);
        let k = Leaf::Image as usize;
        if removed[k] > 0 || added[k] > 0 {
            let new = new.iter().filter_map(|n| match n.draw {
                Draw::Instance(Instance::Image(i, texture, sampler)) => Some((i, texture, sampler)),
                _ => None,
            });
            self.images.splice(at[k] as usize, removed[k] as usize, new);
        }
        let k = Leaf::GlyphRun as usize;
        if removed[k] > 0 || added[k] > 0 {
            let new = new.iter().filter_map(|n| match n.draw {
                Draw::Glyphs { count, atlas, lane } => Some((count, atlas, lane)),
                _ => None,
            });
            self.glyph_runs
                .splice(at[k] as usize, removed[k] as usize, new, glyphs);
        }
    }
}
