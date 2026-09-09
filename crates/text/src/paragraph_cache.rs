//! Paragraph layout cache: memoize [`layout`](crate::layout::layout)'s output —
//! the shaped, line-broken, positioned glyphs of one paragraph — so a re-request
//! with identical inputs skips the reshape and re-linebreak entirely.
//!
//! This lowers the "do not reshape when nothing changed" boundary from the
//! facade's coarse node-level pending-request gate down into the text layer
//! itself: a run whose text, font, size, and wrap width are all unchanged
//! returns the same laid-out glyphs from the cache, no matter how many times it
//! is prepared (steady-state repaint, resize jitter that quantizes back to the
//! same width, a re-declared identical label).
//!
//! # Coverage-scoped invalidation
//!
//! The font store's coverage generation (bumped only when the fallback chain
//! *grows*) is deliberately **not** a key axis. Instead each entry records one
//! bit — whether its laid-out result boxed any `.notdef` (glyph id 0) — and the
//! generation it was computed against. A hit stays valid across generations
//! unless the entry boxed a `.notdef` *and* the chain has since grown: only then
//! could a newly-appended face cover the boxed character and change the layout,
//! so only then is the entry reshaped. A fully-covered paragraph (no `.notdef`)
//! is provably append-independent — shaping recurses into later chain faces only
//! for `.notdef` runs — so it stays a hit forever, no matter how many unrelated
//! scripts are later appended. This is the "adding unrelated coverage does not
//! invalidate" contract, resolved per entry rather than by clearing the cache.
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
/// [`layout`](crate::layout::layout) that can change its output. The font
/// store's coverage generation is intentionally absent: it is not a key axis but
/// a per-entry scope check (see the module docs and [`Entry`]).
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
}

/// The sentinel `wrap_bits` value standing for "no wrap width" (`max_width ==
/// None`). `f32::to_bits` of any finite width is an ordinary pattern; a signaling
/// NaN's bit pattern is never produced by `to_bits` of a real width, so it can
/// never collide with a `Some(w)` key.
const NO_WRAP: u32 = 0x7fc0_0000 | 0x1; // a NaN bit pattern, distinct from any width

impl ParagraphKey {
    fn new(font: FontId, text: &str, font_size_px: f32, max_width_px: Option<f32>) -> Self {
        Self {
            font,
            text: text.to_owned(),
            font_size_bits: font_size_px.to_bits(),
            wrap_bits: max_width_px.map_or(NO_WRAP, f32::to_bits),
        }
    }
}

/// One cache entry: the laid-out glyphs (shared by `Rc` so a hit clones a handle,
/// not the vector), the tick under which it sits in the LRU order, and the scope
/// bits that decide whether a later chain growth invalidates it.
struct Entry {
    glyphs: Rc<Vec<PositionedGlyph>>,
    last_used: u64,
    /// Whether the laid-out result boxed any `.notdef` (a glyph with `id == 0`).
    /// A boxed entry can change when the fallback chain grows (a newly-appended
    /// face may cover the boxed character); a fully-covered entry (`false`) never
    /// can, so it stays a hit across every future coverage generation.
    had_notdef: bool,
    /// The coverage generation this entry was laid out against. Only consulted
    /// when `had_notdef` is true: if the store's generation has advanced past it,
    /// the chain grew and the boxed run is reshaped once against the wider chain.
    generation: u64,
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
    /// `generation` is the font store's current coverage generation (bumped only
    /// on chain growth). A resident entry that boxed a `.notdef` is treated as a
    /// miss when `generation` has advanced past the one it was laid out against —
    /// the chain grew, so its boxed run is reshaped once against the wider chain.
    /// A fully-covered entry ignores `generation` entirely and stays a hit.
    ///
    /// Returns the shared glyphs and whether this call was a miss (the caller
    /// records the reshape in its profiler counters). `miss` is the reshape +
    /// re-linebreak path (`layout`); it runs at most once per distinct paragraph
    /// identity between evictions and coverage-scoped invalidations.
    pub(crate) fn get_or_compute(
        &mut self,
        font: FontId,
        text: &str,
        font_size_px: f32,
        max_width_px: Option<f32>,
        generation: u64,
        miss: impl FnOnce() -> Vec<PositionedGlyph>,
    ) -> (Rc<Vec<PositionedGlyph>>, bool) {
        if self.capacity == 0 {
            return (Rc::new(miss()), true);
        }

        let key = ParagraphKey::new(font, text, font_size_px, max_width_px);

        if let Some(entry) = self.entries.get_mut(&key) {
            // A boxed entry is stale once the chain has grown past its generation:
            // a newly-appended face may now cover the boxed `.notdef`. A
            // fully-covered entry is append-independent and always valid.
            let stale = entry.had_notdef && entry.generation != generation;
            if !stale {
                // Hit: bump recency. Remove the old tick from the LRU index and
                // reinsert under a fresh (larger) tick so this entry is now the
                // most recently used.
                self.lru.remove(&entry.last_used);
                self.tick += 1;
                entry.last_used = self.tick;
                self.lru.insert(self.tick, key);
                return (entry.glyphs.clone(), false);
            }
            // Stale: drop the old entry and fall through to recompute in place.
            self.lru.remove(&entry.last_used);
            self.entries.remove(&key);
        }

        // Miss: make room, then compute and store.
        while self.entries.len() >= self.capacity {
            let Some((_, oldest)) = self.lru.pop_first() else {
                break;
            };
            self.entries.remove(&oldest);
        }

        let laid_out = miss();
        let had_notdef = laid_out.iter().any(|g| g.id == 0);
        let glyphs = Rc::new(laid_out);
        self.tick += 1;
        self.lru.insert(self.tick, key.clone());
        self.entries.insert(
            key,
            Entry {
                glyphs: glyphs.clone(),
                last_used: self.tick,
                had_notdef,
                generation,
            },
        );
        (glyphs, true)
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

    /// A `.notdef` glyph (id 0) — a boxed character shaping could not cover. Its
    /// presence in a laid-out result is the scope bit the cache records.
    fn notdef() -> PositionedGlyph {
        PositionedGlyph {
            font: FontId(0),
            id: 0,
            origin_px: [0.0, 0.0],
        }
    }

    #[test]
    fn identical_request_is_a_hit_and_skips_recompute() {
        let mut cache = ParagraphCache::new(8);
        let mut computes = 0;

        let (a, a_miss) = cache.get_or_compute(FontId(0), "hello", 16.0, None, 0, || {
            computes += 1;
            vec![glyph(1)]
        });
        let (b, b_miss) = cache.get_or_compute(FontId(0), "hello", 16.0, None, 0, || {
            computes += 1;
            vec![glyph(99)] // never runs on a hit
        });

        assert_eq!(
            computes, 1,
            "the second identical request hits, no recompute"
        );
        assert!(a_miss, "the first request was a miss");
        assert!(!b_miss, "the second identical request was a hit");
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
        let go = |c: &mut ParagraphCache, font: FontId, text: &str, size: f32, w: Option<f32>| {
            c.get_or_compute(font, text, size, w, 0, || {
                computes.set(computes.get() + 1);
                vec![glyph(1)]
            });
        };

        // A distinct value on each axis is a distinct paragraph → a miss each.
        go(&mut cache, FontId(0), "hello", 16.0, None); // baseline
        go(&mut cache, FontId(1), "hello", 16.0, None); // font differs
        go(&mut cache, FontId(0), "world", 16.0, None); // text differs
        go(&mut cache, FontId(0), "hello", 18.0, None); // size differs
        go(&mut cache, FontId(0), "hello", 16.0, Some(120.0)); // wrap differs

        assert_eq!(
            computes.get(),
            5,
            "every axis of the key misses independently"
        );
        assert_eq!(cache.len(), 5);

        // Re-requesting the baseline still hits — the other misses did not evict it.
        go(&mut cache, FontId(0), "hello", 16.0, None);
        assert_eq!(computes.get(), 5, "the baseline is still cached");
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
    fn unrelated_coverage_does_not_invalidate() {
        // The headline scoped-invalidation contract: a fully-covered paragraph
        // (no `.notdef` boxed) laid out at generation 0 stays a hit after the
        // chain grows (generation 1) — a face appended for a script this run
        // never used cannot change its layout, so it must not reshape.
        let mut cache = ParagraphCache::new(8);
        let mut computes = 0;

        let (_, first_miss) = cache.get_or_compute(FontId(0), "hello", 16.0, None, 0, || {
            computes += 1;
            vec![glyph(1)] // fully covered: no id-0 glyph
        });
        // Generation advances (a fallback face was appended for another script).
        let (_, second_miss) = cache.get_or_compute(FontId(0), "hello", 16.0, None, 1, || {
            computes += 1;
            vec![glyph(99)] // must not run
        });

        assert!(first_miss, "the first request was a miss");
        assert!(
            !second_miss,
            "a fully-covered paragraph hits across coverage generations"
        );
        assert_eq!(computes, 1, "no reshape after unrelated coverage grew");
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn boxed_paragraph_reshapes_on_new_generation() {
        // Scope is per entry, not global. A boxed paragraph (`had_notdef`)
        // reshapes once when the chain grows, while a fully-covered sibling laid
        // out at the same generation keeps hitting across the growth.
        let mut cache = ParagraphCache::new(8);
        let mut boxed_computes = 0;
        let mut covered_computes = 0;

        // Two entries at generation 0: one boxes a `.notdef`, one is fully covered.
        cache.get_or_compute(FontId(0), "boxed", 16.0, None, 0, || {
            boxed_computes += 1;
            vec![glyph(1), notdef()]
        });
        cache.get_or_compute(FontId(0), "covered", 16.0, None, 0, || {
            covered_computes += 1;
            vec![glyph(2)]
        });

        // Chain grows to generation 1.
        let (_, boxed_miss) = cache.get_or_compute(FontId(0), "boxed", 16.0, None, 1, || {
            boxed_computes += 1;
            vec![glyph(3)] // the appended face now covers the boxed char
        });
        let (_, covered_miss) = cache.get_or_compute(FontId(0), "covered", 16.0, None, 1, || {
            covered_computes += 1;
            vec![glyph(99)] // must not run
        });

        assert!(
            boxed_miss,
            "the boxed paragraph reshaped at the new generation"
        );
        assert!(!covered_miss, "the fully-covered sibling still hit");
        assert_eq!(boxed_computes, 2, "the boxed paragraph recomputed once");
        assert_eq!(
            covered_computes, 1,
            "the covered paragraph never recomputed"
        );
        // The reshaped boxed entry replaced the stale one in place, not a growth.
        assert_eq!(cache.len(), 2, "stale entry replaced, not accumulated");

        // The reshaped boxed entry (now fully covered at gen 1) keeps hitting.
        let (_, again) = cache.get_or_compute(FontId(0), "boxed", 16.0, None, 1, || {
            boxed_computes += 1;
            vec![glyph(99)]
        });
        assert!(!again, "the reshaped entry is now a hit at its generation");
        assert_eq!(boxed_computes, 2, "no further reshape");
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

        let (_, first_miss) = cache.get_or_compute(FontId(0), "t", 16.0, None, 0, || {
            computes += 1;
            vec![glyph(1)]
        });
        let (_, second_miss) = cache.get_or_compute(FontId(0), "t", 16.0, None, 0, || {
            computes += 1;
            vec![glyph(1)]
        });

        assert!(first_miss && second_miss, "capacity 0 is always a miss");
        assert_eq!(
            computes, 2,
            "capacity 0 never caches; every call recomputes"
        );
        assert_eq!(cache.len(), 0);
    }
}
