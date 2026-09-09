//! Paragraph layout cache: memoize [`layout`](crate::layout::layout)'s output —
//! the shaped, line-broken, positioned glyphs of one paragraph — so a re-request
//! with identical inputs skips the reshape and re-linebreak entirely.
//!
//! This lowers the "do not reshape when nothing changed" boundary from the
//! facade's coarse node-level pending-request gate down into the text layer
//! itself: a run whose text, font, size, wrap width, and font revision are all
//! unchanged returns the same laid-out glyphs from the cache, no matter how many
//! times it is prepared (steady-state repaint, resize jitter that quantizes back
//! to the same width, a re-declared identical label).
//!
//! # What is *not* cached here
//!
//! Glyph rasterization and atlas packing are already deduplicated one layer down
//! by [`Atlas`](crate::atlas::Atlas)'s own glyph map (a repeated glyph is a hash
//! hit, never re-rastered), so the cache boundary is drawn exactly at the
//! reshape + re-linebreak work — [`layout`](crate::layout::layout)'s
//! `Vec<PositionedGlyph>` — and nothing below it. The positions are in logical
//! pixels and independent of the surface's DPI density (density only affects the
//! rasterized bitmap, not glyph placement), so DPI is deliberately absent from
//! the key: the same paragraph at 1x and 2x shares one cache entry.
//!
//! # Bound and eviction
//!
//! The cache is bounded to `capacity` entries with least-recently-used eviction:
//! each lookup or insertion stamps the entry with a monotonic tick, and once the
//! map is full the lowest-tick (oldest-touched) entry is dropped. A hot working
//! set of paragraphs (the labels currently on screen) survives, while a burst of
//! one-off strings (scrolling through varied text) ages out. `capacity == 0`
//! disables caching (every call reshapes), which the microbenchmarks use to
//! measure the uncached cost.

use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;

use crate::FontId;
use crate::layout::PositionedGlyph;

/// The identity of one laid-out paragraph — every input to
/// [`layout`](crate::layout::layout) that can change its output, plus the font
/// store's revision so a stale entry from before the chain grew never matches.
///
/// `font_size` and `wrap_width` are `f32`, which is neither [`Eq`] nor [`Hash`];
/// they are stored as their raw bit patterns (`f32::to_bits`) so the key is a
/// plain hashable value. Bit equality is exactly the right notion here: the
/// upstream layout pass has already quantized an assigned width to integer
/// physical pixels before it reaches shaping (so continuous drag-resize does not
/// produce a new width every sub-pixel frame), so two requests that should share
/// a layout arrive with bit-identical widths, and any genuine width change
/// differs in the bits and misses correctly.
///
/// `wrap_width` folds `None` (unconstrained, single-line) and `Some(w)` into one
/// field: `None` is stored as a sentinel bit pattern (`NO_WRAP`) that no finite
/// positive width can collide with, so an unconstrained layout and a
/// width-limited one never alias.
///
/// There is no OpenType-feature axis yet — the shaper applies no per-run features
/// (`rustybuzz::shape(&rb, &[], buffer)`), so a feature field would be dead. When
/// features become a live input this key gains one more component; the doc marks
/// the seam.
#[derive(Clone, PartialEq, Eq, Hash)]
struct ParagraphKey {
    font: FontId,
    text: String,
    font_size_bits: u32,
    wrap_bits: u32,
    revision: u64,
}

/// The sentinel `wrap_bits` value standing for "no wrap width" (`max_width ==
/// None`). `f32::to_bits` of any finite width is an ordinary pattern; a signaling
/// NaN's bit pattern is never produced by `to_bits` of a real width, so it can
/// never collide with a `Some(w)` key.
const NO_WRAP: u32 = 0x7fc0_0000 | 0x1; // a NaN bit pattern, distinct from any width

impl ParagraphKey {
    fn new(
        font: FontId,
        text: &str,
        font_size_px: f32,
        max_width_px: Option<f32>,
        revision: u64,
    ) -> Self {
        Self {
            font,
            text: text.to_owned(),
            font_size_bits: font_size_px.to_bits(),
            wrap_bits: max_width_px.map_or(NO_WRAP, f32::to_bits),
            revision,
        }
    }
}

/// One cache entry: the laid-out glyphs (shared by `Rc` so a hit clones a handle,
/// not the vector) and the tick under which it currently sits in the LRU order.
struct Entry {
    glyphs: Rc<Vec<PositionedGlyph>>,
    last_used: u64,
}

/// A bounded, LRU-evicting cache mapping a paragraph's identity to its laid-out
/// glyphs. See the module docs for the boundary rationale.
pub(crate) struct ParagraphCache {
    capacity: usize,
    tick: u64,
    entries: HashMap<ParagraphKey, Entry>,
    /// Recency index: `tick -> key`, so the oldest-touched entry is the map's
    /// first element and eviction is an `O(log n)` `pop_first`.
    lru: BTreeMap<u64, ParagraphKey>,
}

impl ParagraphCache {
    /// A cache holding at most `capacity` paragraphs. `capacity == 0` disables
    /// caching: every lookup misses and nothing is stored.
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            capacity,
            tick: 0,
            entries: HashMap::new(),
            lru: BTreeMap::new(),
        }
    }

    /// Return the laid-out glyphs for this paragraph, computing them with `miss`
    /// only on a cache miss. On a hit the entry's recency is refreshed so the hot
    /// working set survives eviction; on a miss the freshly computed result is
    /// stored (evicting the least-recently-used entry first if full) and shared.
    ///
    /// `miss` is the reshape + re-linebreak path (`layout`); it runs at most once
    /// per distinct paragraph identity between evictions.
    pub(crate) fn get_or_compute(
        &mut self,
        font: FontId,
        text: &str,
        font_size_px: f32,
        max_width_px: Option<f32>,
        revision: u64,
        miss: impl FnOnce() -> Vec<PositionedGlyph>,
    ) -> Rc<Vec<PositionedGlyph>> {
        if self.capacity == 0 {
            return Rc::new(miss());
        }

        let key = ParagraphKey::new(font, text, font_size_px, max_width_px, revision);

        if let Some(entry) = self.entries.get_mut(&key) {
            // Hit: bump recency. Remove the old tick from the LRU index and
            // reinsert under a fresh (larger) tick so this entry is now the most
            // recently used.
            self.lru.remove(&entry.last_used);
            self.tick += 1;
            entry.last_used = self.tick;
            self.lru.insert(self.tick, key);
            return entry.glyphs.clone();
        }

        // Miss: make room, then compute and store.
        while self.entries.len() >= self.capacity {
            let Some((_, oldest)) = self.lru.pop_first() else {
                break;
            };
            self.entries.remove(&oldest);
        }

        let glyphs = Rc::new(miss());
        self.tick += 1;
        self.lru.insert(self.tick, key.clone());
        self.entries.insert(
            key,
            Entry {
                glyphs: glyphs.clone(),
                last_used: self.tick,
            },
        );
        glyphs
    }

    /// The number of paragraphs currently cached. For tests and profiling.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        debug_assert_eq!(
            self.entries.len(),
            self.lru.len(),
            "LRU index and map agree"
        );
        self.entries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A distinguishable dummy glyph so different `miss` results are visibly
    /// different in the cache without needing a real font.
    fn glyph(id: u16) -> PositionedGlyph {
        PositionedGlyph {
            font: FontId(0),
            id,
            origin_px: [id as f32, 0.0],
        }
    }

    #[test]
    fn identical_request_is_a_hit_and_skips_recompute() {
        let mut cache = ParagraphCache::new(8);
        let mut computes = 0;

        let a = cache.get_or_compute(FontId(0), "hello", 16.0, None, 0, || {
            computes += 1;
            vec![glyph(1)]
        });
        let b = cache.get_or_compute(FontId(0), "hello", 16.0, None, 0, || {
            computes += 1;
            vec![glyph(99)] // never runs on a hit
        });

        assert_eq!(
            computes, 1,
            "the second identical request hits, no recompute"
        );
        assert_eq!(*a, *b, "the hit returns the first result");
        assert!(
            Rc::ptr_eq(&a, &b),
            "the hit shares the same Rc, no clone of the vec"
        );
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn each_key_axis_misses_independently() {
        use std::cell::Cell;
        let mut cache = ParagraphCache::new(64);
        let computes = Cell::new(0);
        let go = |c: &mut ParagraphCache,
                  font: FontId,
                  text: &str,
                  size: f32,
                  w: Option<f32>,
                  rev: u64| {
            c.get_or_compute(font, text, size, w, rev, || {
                computes.set(computes.get() + 1);
                vec![glyph(1)]
            });
        };

        // A distinct value on each axis is a distinct paragraph → a miss each.
        go(&mut cache, FontId(0), "hello", 16.0, None, 0); // baseline
        go(&mut cache, FontId(1), "hello", 16.0, None, 0); // font differs
        go(&mut cache, FontId(0), "world", 16.0, None, 0); // text differs
        go(&mut cache, FontId(0), "hello", 18.0, None, 0); // size differs
        go(&mut cache, FontId(0), "hello", 16.0, Some(120.0), 0); // wrap differs
        go(&mut cache, FontId(0), "hello", 16.0, None, 1); // revision differs

        assert_eq!(
            computes.get(),
            6,
            "every axis of the key misses independently"
        );
        assert_eq!(cache.len(), 6);

        // Re-requesting the baseline still hits — the other misses did not evict it.
        go(&mut cache, FontId(0), "hello", 16.0, None, 0);
        assert_eq!(computes.get(), 6, "the baseline is still cached");
    }

    #[test]
    fn wrap_none_and_some_do_not_alias() {
        let mut cache = ParagraphCache::new(8);
        let mut computes = 0;

        cache.get_or_compute(FontId(0), "t", 16.0, None, 0, || {
            computes += 1;
            vec![glyph(1)]
        });
        cache.get_or_compute(FontId(0), "t", 16.0, Some(100.0), 0, || {
            computes += 1;
            vec![glyph(2)]
        });

        assert_eq!(
            computes, 2,
            "unconstrained and width-limited are distinct keys"
        );
        assert_eq!(cache.len(), 2);
    }

    #[test]
    fn revision_bump_forces_a_recompute() {
        // The freshness contract: after a font mutation bumps the store revision,
        // the same text/font/size/width reshapes once (the old-revision entry is
        // still resident but no longer matches) rather than serving a stale
        // layout that predates the grown fallback chain.
        let mut cache = ParagraphCache::new(8);
        let mut computes = 0;

        let old = cache.get_or_compute(FontId(0), "t", 16.0, None, 0, || {
            computes += 1;
            vec![glyph(1)]
        });
        let new = cache.get_or_compute(FontId(0), "t", 16.0, None, 1, || {
            computes += 1;
            vec![glyph(2)]
        });

        assert_eq!(computes, 2, "a new revision misses and recomputes");
        assert_ne!(*old, *new, "the recompute produced the fresh layout");
    }

    #[test]
    fn lru_evicts_the_least_recently_used() {
        // Capacity 2. Insert A, B (full). Touch A (now B is oldest). Insert C →
        // B is evicted, A and C remain. Re-request B → a miss; A → still a hit.
        use std::cell::Cell;
        let mut cache = ParagraphCache::new(2);
        let computes = Cell::new(0);
        let go = |c: &mut ParagraphCache, text: &str| {
            c.get_or_compute(FontId(0), text, 16.0, None, 0, || {
                computes.set(computes.get() + 1);
                vec![glyph(1)]
            });
        };

        go(&mut cache, "A"); // miss → {A}
        go(&mut cache, "B"); // miss → {A, B} (full)
        go(&mut cache, "A"); // hit, refreshes A → B is now the oldest
        go(&mut cache, "C"); // miss, evicts the oldest (B) → {A, C}
        assert_eq!(cache.len(), 2, "capacity is respected");

        go(&mut cache, "B"); // miss (B was evicted), evicts the oldest (A) → {C, B}
        assert_eq!(computes.get(), 4, "B had to be recomputed after eviction");

        go(&mut cache, "A"); // miss: A was evicted by B's insertion above
        assert_eq!(computes.get(), 5, "A was evicted in turn and recomputed");
    }

    #[test]
    fn zero_capacity_disables_caching() {
        let mut cache = ParagraphCache::new(0);
        let mut computes = 0;

        cache.get_or_compute(FontId(0), "t", 16.0, None, 0, || {
            computes += 1;
            vec![glyph(1)]
        });
        cache.get_or_compute(FontId(0), "t", 16.0, None, 0, || {
            computes += 1;
            vec![glyph(1)]
        });

        assert_eq!(
            computes, 2,
            "capacity 0 never caches; every call recomputes"
        );
        assert_eq!(cache.len(), 0);
    }
}
