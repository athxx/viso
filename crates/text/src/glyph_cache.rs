//! Glyph identity and residency metadata: the [`GlyphKey`] every glyph resolves
//! to, and the per-representation residency pools with page-age + CLOCK
//! eviction.
//!
//! This crate owns only identity and residency *metadata* — which glyph, in
//! which representation, at which resolution bucket, and whether it is resident.
//! The actual atlas texture, page memory, and upload live in `viso-render`.
//! There are four independent pools (A8 coverage, MTSDF, RGBA color, vector);
//! eviction is by page age with a CLOCK second-chance sweep, not per-glyph LRU,
//! and exact coverage is always a correct fallback under pressure.
//!
//! # Page-level residency, never per-glyph LRU
//!
//! The unit of residency and eviction is an atlas *page*, not a glyph. A glyph
//! is admitted onto a page; the page carries its own age, a CLOCK reference
//! bit, and a generation. When space is needed the CLOCK hand sweeps pages,
//! gives a recently-referenced page one second chance, and reclaims the coldest
//! non-referenced page: its generation is bumped, invalidating only the entries
//! that lived on it, and it is reused. There is never a whole-atlas clear.
//!
//! # Recency is folded once per frame, never per glyph draw
//!
//! Drawing a glyph only records that its page was touched this frame; it does
//! not move any page age. [`GlyphResidency::advance_epoch`] folds the frame's
//! touched pages into recency exactly once, mirroring the Face Cache's
//! epoch-merge. A glyph drawn ten thousand times in a frame is one recency
//! update on its page, so glyph draw never mutates CLOCK/age metadata.

use std::collections::HashMap;
use std::collections::HashSet;

use crate::FontFaceId;
use crate::glyph_representation::GlyphImageKind;

/// The full identity of a resolved glyph image: the face, the glyph, the chosen
/// representation, and the resolution / size bucket it was resolved into. Color
/// glyphs quantize to a size bucket; scalable representations carry their
/// resolution bucket.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GlyphKey {
    pub face: FontFaceId,
    pub glyph: u16,
    pub kind: GlyphImageKind,
    /// Resolution / size bucket the representation was resolved into.
    pub bucket: u16,
}

/// How many glyph slots one A8 page holds.
///
/// The A8 pool tracks residency at page granularity in fixed-count slots;
/// pixel-level rectangle packing is a texture-memory concern that lives in
/// `viso-render`, not in this metadata layer. A page is full when it holds this
/// many glyphs.
const A8_PAGE_GLYPH_CAP: usize = 256;

/// The result of asking the A8 pool to make a glyph resident.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admission {
    /// The glyph was already resident; no upload is needed. `viso-render`
    /// reuses the existing page slot.
    Cached { page: usize },
    /// The glyph was newly admitted and must be uploaded. `page` is the atlas
    /// page it landed on; `offset_bytes` is where its bitmap starts within the
    /// page's accumulated bytes.
    Admitted { page: usize, offset_bytes: usize },
}

/// One A8 atlas page's residency bookkeeping. Owns no pixels — only which
/// glyphs live here, the page's age and CLOCK bit, and its generation.
#[derive(Debug)]
struct A8Page {
    /// The epoch this page was last used in, folded once per frame by
    /// [`GlyphResidency::advance_epoch`], for coldest-first CLOCK selection.
    last_used_epoch: u64,
    /// CLOCK second-chance bit: set when the page is touched, cleared to give a
    /// recently-referenced page one reprieve before reclamation.
    referenced: bool,
    /// Bumped when the page is reclaimed, so stale index entries pointing at an
    /// earlier generation are detected as invalidated.
    generation: u32,
    /// The glyph identities resident on this page (metadata, no pixels).
    resident: Vec<GlyphKey>,
    /// Accumulated A8 bitmap bytes admitted onto this page: the upload
    /// candidate size, and the offset the next glyph is placed at.
    bytes: usize,
}

impl A8Page {
    fn new(epoch: u64) -> Self {
        Self {
            last_used_epoch: epoch,
            referenced: false,
            generation: 0,
            resident: Vec::new(),
            bytes: 0,
        }
    }

    fn has_room(&self) -> bool {
        self.resident.len() < A8_PAGE_GLYPH_CAP
    }
}

/// The residency metadata across the four representation pools. Owns no GPU
/// memory; tracks which [`GlyphKey`]s are resident and their page ages.
///
/// Only the A8 coverage pool is implemented here; MTSDF, RGBA color, and vector
/// pools are separate residency structures added in later phases and are kept
/// structurally distinct so their eviction queues and accounting never mix.
#[derive(Debug)]
pub struct GlyphResidency {
    /// The A8 coverage pool's pages.
    a8_pages: Vec<A8Page>,
    /// Where each resident A8 glyph lives: `(page index, page generation at
    /// admission)`. A stale generation means the entry was invalidated by a
    /// page reclaim. Cold-path lookup only; never touched per glyph draw.
    a8_index: HashMap<GlyphKey, (usize, u32)>,
    /// A8 pages touched this frame, folded into recency by
    /// [`Self::advance_epoch`]. This is the per-frame page bitset that keeps
    /// recency off the per-glyph path.
    touched_this_frame: HashSet<usize>,
    /// CLOCK hand: where the next eviction sweep resumes.
    clock_hand: usize,
    /// Monotonic epoch counter; advanced once per frame.
    epoch: u64,
    /// Maximum A8 pages before eviction is forced.
    max_pages: usize,
    /// Accumulated A8 bitmap bytes ever admitted: the upload candidate volume.
    upload_bytes_total: u64,
    /// Number of glyphs currently resident in the A8 pool.
    resident_glyphs: usize,
    // TODO(TF-P1+): mtsdf_pages / rgba_pages / vector_cache — separate pools
    // with their own eviction queues and byte accounting.
}

impl Default for GlyphResidency {
    fn default() -> Self {
        // A stable default page budget; render sizes real atlas memory.
        Self::new(64)
    }
}

impl GlyphResidency {
    /// A residency map with the given A8 page budget.
    pub fn new(max_pages: usize) -> Self {
        Self {
            a8_pages: Vec::new(),
            a8_index: HashMap::new(),
            touched_this_frame: HashSet::new(),
            clock_hand: 0,
            epoch: 0,
            max_pages: max_pages.max(1),
            upload_bytes_total: 0,
            resident_glyphs: 0,
        }
    }

    /// Make an A8 glyph resident, admitting it onto a page if it is not already.
    ///
    /// If the glyph is resident this is a [`Admission::Cached`] hit and no bytes
    /// are counted. Otherwise it is placed on a page with room, or — if every
    /// page is full and the budget is reached — the CLOCK sweep reclaims a cold
    /// page for it; either way `bitmap_bytes` is added to the upload total. This
    /// is the only method that mutates CLOCK/page metadata, and it runs on the
    /// cold admit path, never on glyph draw.
    pub fn get_or_admit_a8(&mut self, key: GlyphKey, bitmap_bytes: usize) -> Admission {
        if let Some(&(page, generation)) = self.a8_index.get(&key)
            && self.a8_pages[page].generation == generation
        {
            self.mark_touched(page);
            return Admission::Cached { page };
        }

        let page = self.select_a8_page();
        let offset_bytes = self.a8_pages[page].bytes;
        self.a8_pages[page].resident.push(key);
        self.a8_pages[page].bytes += bitmap_bytes;
        let generation = self.a8_pages[page].generation;
        self.a8_index.insert(key, (page, generation));
        self.mark_touched(page);

        self.upload_bytes_total += bitmap_bytes as u64;
        self.resident_glyphs += 1;

        Admission::Admitted { page, offset_bytes }
    }

    /// Pick an A8 page to admit onto: a page with room, else a fresh page while
    /// under budget, else a CLOCK-reclaimed cold page.
    fn select_a8_page(&mut self) -> usize {
        if let Some(page) = self.a8_pages.iter().position(A8Page::has_room) {
            return page;
        }
        if self.a8_pages.len() < self.max_pages {
            self.a8_pages.push(A8Page::new(self.epoch));
            return self.a8_pages.len() - 1;
        }
        let victim = self.evict_one();
        self.reclaim_a8_page(victim);
        victim
    }

    /// The CLOCK sweep: from the hand, give any referenced page one second
    /// chance (clear the bit, advance) and reclaim the first non-referenced
    /// page, preferring the coldest. Returns the page index to reuse.
    fn evict_one(&mut self) -> usize {
        let n = self.a8_pages.len();
        // A full sweep clearing reference bits guarantees a non-referenced page
        // exists on the second lap; bound the scan to two laps.
        for _ in 0..(2 * n) {
            let idx = self.clock_hand % n;
            self.clock_hand = (self.clock_hand + 1) % n;
            if self.a8_pages[idx].referenced {
                self.a8_pages[idx].referenced = false;
            } else {
                return idx;
            }
        }
        // Fallback: after two laps every bit was cleared, so pick the coldest.
        (0..n)
            .min_by_key(|&i| self.a8_pages[i].last_used_epoch)
            .unwrap_or(0)
    }

    /// Reclaim a page for reuse: bump its generation (invalidating only the
    /// entries that lived on it), drop its residents, and reset its bytes. The
    /// stale index entries are detected lazily by generation mismatch on
    /// lookup. This never clears any other page.
    fn reclaim_a8_page(&mut self, page: usize) {
        let evicted = std::mem::take(&mut self.a8_pages[page].resident);
        self.resident_glyphs -= evicted.len();
        for key in &evicted {
            // Only drop index entries still pointing at this page's old
            // generation; a key re-admitted elsewhere must not be removed.
            if let Some(&(p, g)) = self.a8_index.get(key)
                && p == page
                && g == self.a8_pages[page].generation
            {
                self.a8_index.remove(key);
            }
        }
        self.a8_pages[page].generation = self.a8_pages[page].generation.wrapping_add(1);
        self.a8_pages[page].bytes = 0;
        self.a8_pages[page].last_used_epoch = self.epoch;
        self.a8_pages[page].referenced = false;
    }

    /// Record that an A8 page was used this frame: set its CLOCK bit and note it
    /// for the epoch fold. Does not move `last_used_epoch`.
    fn mark_touched(&mut self, page: usize) {
        self.a8_pages[page].referenced = true;
        self.touched_this_frame.insert(page);
    }

    /// Mark a resident glyph used this frame, for the CLOCK sweep.
    ///
    /// This is the per-glyph-draw touch. It only records the page as touched;
    /// recency is applied by [`Self::advance_epoch`], so many draws of the same
    /// glyph collapse to one recency update per frame and nothing mutates page
    /// age here.
    pub fn touch(&mut self, key: GlyphKey) {
        if let Some(&(page, generation)) = self.a8_index.get(&key)
            && self.a8_pages[page].generation == generation
        {
            self.mark_touched(page);
        }
    }

    /// Fold this frame's touched A8 pages into recency and advance the epoch.
    ///
    /// Every page touched during the frame has its `last_used_epoch` set to the
    /// current epoch exactly once, regardless of how many glyphs were drawn on
    /// it. Call once per frame.
    pub fn advance_epoch(&mut self) {
        for page in self.touched_this_frame.drain() {
            if let Some(p) = self.a8_pages.get_mut(page) {
                p.last_used_epoch = self.epoch;
            }
        }
        self.epoch += 1;
    }

    /// The accumulated A8 upload candidate size in bytes (counter 61
    /// `gpu_upload_bytes` for this pool: grows only on admit, never on a hit).
    pub fn upload_bytes_total(&self) -> u64 {
        self.upload_bytes_total
    }

    /// The number of glyphs currently resident in the A8 pool.
    pub fn resident_glyphs(&self) -> usize {
        self.resident_glyphs
    }

    /// The number of A8 pages currently allocated.
    pub fn page_count(&self) -> usize {
        self.a8_pages.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(glyph: u16, bucket: u16) -> GlyphKey {
        GlyphKey {
            face: FontFaceId(0),
            glyph,
            kind: GlyphImageKind::MaskA8,
            bucket,
        }
    }

    #[test]
    fn admit_then_hit_is_cached_and_counts_bytes_once() {
        let mut res = GlyphResidency::new(4);
        let k = key(34, 0);

        let first = res.get_or_admit_a8(k, 100);
        assert!(matches!(first, Admission::Admitted { .. }));
        assert_eq!(res.upload_bytes_total(), 100);
        assert_eq!(res.resident_glyphs(), 1);

        // A second request for the same key is a cached hit that counts no bytes.
        let second = res.get_or_admit_a8(k, 100);
        assert!(matches!(second, Admission::Cached { .. }));
        assert_eq!(res.upload_bytes_total(), 100);
        assert_eq!(res.resident_glyphs(), 1);
    }

    #[test]
    fn touch_does_not_move_epoch_per_glyph() {
        let mut res = GlyphResidency::new(4);
        let k = key(34, 0);
        let Admission::Admitted { page, .. } = res.get_or_admit_a8(k, 100) else {
            panic!("first admit");
        };
        res.advance_epoch();
        let after_admit = res.a8_pages[page].last_used_epoch;

        // 10_000 draws of the same glyph in one frame must not move page age.
        for _ in 0..10_000 {
            res.touch(k);
        }
        assert_eq!(
            res.a8_pages[page].last_used_epoch, after_admit,
            "touch must not update recency per glyph"
        );

        res.advance_epoch();
        assert!(
            res.a8_pages[page].last_used_epoch > after_admit,
            "advance_epoch folds the frame's touches into one recency update"
        );
    }

    #[test]
    fn clock_second_chance_spares_recently_referenced() {
        // Two single-slot pages, budget of two; fill both, then force eviction.
        let mut res = GlyphResidency::new(2);
        // Fill page 0 to capacity and page 1 to capacity across distinct epochs.
        for g in 0..A8_PAGE_GLYPH_CAP as u16 {
            res.get_or_admit_a8(key(g, 0), 10);
        }
        res.advance_epoch();
        for g in 0..A8_PAGE_GLYPH_CAP as u16 {
            res.get_or_admit_a8(key(g, 1), 10);
        }
        res.advance_epoch();
        assert_eq!(res.page_count(), 2);

        // Reference page 1's glyphs this frame so it earns a second chance.
        res.touch(key(0, 1));

        // Admitting a new glyph forces eviction of a page. Page 0 (unreferenced,
        // colder) must be reclaimed, not page 1, and page 1's residents survive.
        let before_page1_residents = res.a8_pages[1].resident.len();
        res.get_or_admit_a8(key(999, 2), 10);

        assert_eq!(
            res.page_count(),
            2,
            "no new page: a page was reused, not grown"
        );
        assert_eq!(
            res.a8_pages[1].resident.len(),
            before_page1_residents.min(A8_PAGE_GLYPH_CAP),
            "the referenced page's residents were not cleared"
        );
        // The whole atlas was not cleared: page 1 still holds a real key.
        assert!(res.a8_pages[1].resident.contains(&key(0, 1)));
    }

    #[test]
    fn evicted_page_generation_bumps_and_invalidates_only_its_entries() {
        let mut res = GlyphResidency::new(2);
        // Page 0: one glyph. Page 1: fill so page 0 is the only room-less
        // candidate for a forced eviction later.
        let victim_key = key(1, 0);
        res.get_or_admit_a8(victim_key, 10);
        // Fill page 0 the rest of the way and all of page 1.
        for g in 2..=A8_PAGE_GLYPH_CAP as u16 {
            res.get_or_admit_a8(key(g, 0), 10);
        }
        let survivor_key = key(500, 1);
        res.get_or_admit_a8(survivor_key, 10);
        for g in 501..(500 + A8_PAGE_GLYPH_CAP as u16) {
            res.get_or_admit_a8(key(g, 1), 10);
        }
        assert_eq!(res.page_count(), 2);

        let gen_before = res.a8_pages[0].generation;
        // Force an eviction; page 0 (colder, unreferenced) is reclaimed.
        res.get_or_admit_a8(key(9999, 2), 10);
        assert_eq!(
            res.a8_pages[0].generation,
            gen_before.wrapping_add(1),
            "reclaimed page bumps its generation"
        );

        // The victim page's entries are invalidated; the survivor page's are not.
        assert!(matches!(
            res.get_or_admit_a8(survivor_key, 10),
            Admission::Cached { .. }
        ));
        // The reclaimed key is gone: re-admitting it is a fresh admission.
        // (It may or may not land on the reused page; either way it is Admitted.)
        let readmit = res.get_or_admit_a8(victim_key, 10);
        assert!(matches!(readmit, Admission::Admitted { .. }));
    }

    #[test]
    fn upload_bytes_counter_is_readable_and_monotonic_under_admit() {
        let mut res = GlyphResidency::new(8);
        assert_eq!(res.upload_bytes_total(), 0);

        res.get_or_admit_a8(key(1, 0), 40);
        res.get_or_admit_a8(key(2, 0), 60);
        assert_eq!(res.upload_bytes_total(), 100);

        // A steady-state frame of pure hits adds no bytes.
        res.advance_epoch();
        for _ in 0..1_000 {
            res.touch(key(1, 0));
            res.touch(key(2, 0));
        }
        res.get_or_admit_a8(key(1, 0), 40);
        assert_eq!(
            res.upload_bytes_total(),
            100,
            "cached hits do not grow the upload counter"
        );
    }
}
