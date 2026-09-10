//! Glyph identity and residency metadata: the [`GlyphKey`] every glyph resolves
//! to, and the per-representation residency pools with page-age + CLOCK
//! eviction.
//!
//! This crate owns only identity and residency *metadata* — which glyph, in
//! which representation, at which resolution bucket, and whether it is resident.
//! The actual atlas texture, page memory, and upload live in `viso-render`.
//! There are four independent pools (A8 coverage, MTSDF, RGBA color, vector);
//! eviction is by page age with a CLOCK second-chance sweep, not per-glyph LRU,
//! and exact A8 coverage is always a correct fallback under pressure.
//!
//! # Four independent pools, never a shared queue
//!
//! Each [`GlyphImageKind`] resolves into its own [`Pool`]: the A8 coverage pool,
//! the MTSDF pool, the RGBA color pool, and the vector pool. Every pool carries
//! its own pages, eviction queue (CLOCK hand), and byte accounting, so a
//! pure-text run touches only the A8 pool and never allocates in, evicts from,
//! or re-uploads the color or MTSDF pools. There is no cross-pool clear: one
//! pool reaching its page budget never disturbs another. Because exact coverage
//! is always correct, a glyph whose promoted representation is under pool
//! pressure can fall back to A8 rather than force an eviction elsewhere.
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

/// How many glyph slots one page holds.
///
/// A pool tracks residency at page granularity in fixed-count slots;
/// pixel-level rectangle packing is a texture-memory concern that lives in
/// `viso-render`, not in this metadata layer. A page is full when it holds this
/// many glyphs.
const PAGE_GLYPH_CAP: usize = 256;

/// The result of asking a pool to make a glyph resident.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admission {
    /// The glyph was already resident; no upload is needed. `viso-render`
    /// reuses the existing page slot.
    Cached {
        /// Which representation pool the glyph is resident in. For a fallback
        /// admission this is [`GlyphImageKind::MaskA8`], not the requested kind.
        kind: GlyphImageKind,
        page: usize,
    },
    /// The glyph was newly admitted and must be uploaded. `page` is the atlas
    /// page it landed on within `kind`'s pool; `offset_bytes` is where its
    /// bitmap starts within the page's accumulated bytes.
    Admitted {
        /// The pool the glyph landed in. May differ from the requested kind when
        /// pool pressure forced a fall back to A8 coverage.
        kind: GlyphImageKind,
        page: usize,
        offset_bytes: usize,
    },
}

/// One atlas page's residency bookkeeping. Owns no pixels — only which glyphs
/// live here, the page's age and CLOCK bit, and its generation.
#[derive(Debug)]
struct Page {
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
    /// Accumulated bitmap bytes admitted onto this page: the upload candidate
    /// size, and the offset the next glyph is placed at.
    bytes: usize,
}

impl Page {
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
        self.resident.len() < PAGE_GLYPH_CAP
    }
}

/// One representation's residency pool: its own pages, CLOCK eviction hand,
/// byte accounting, and glyph index. Pools never share state, so eviction in
/// one pool never touches another and a whole-atlas reset is impossible.
#[derive(Debug)]
struct Pool {
    /// This pool's pages.
    pages: Vec<Page>,
    /// Where each resident glyph lives: `(page index, page generation at
    /// admission)`. A stale generation means the entry was invalidated by a
    /// page reclaim. Cold-path lookup only; never touched per glyph draw.
    index: HashMap<GlyphKey, (usize, u32)>,
    /// CLOCK hand: where the next eviction sweep resumes.
    clock_hand: usize,
    /// Maximum pages before eviction is forced for this pool.
    max_pages: usize,
    /// Accumulated bitmap bytes ever admitted into this pool: its upload
    /// candidate volume (counter 61 `gpu_upload_bytes` for the pool).
    upload_bytes_total: u64,
    /// Number of glyphs currently resident in this pool.
    resident_glyphs: usize,
    /// Whether the last [`Self::select_page`] reclaimed a page (an eviction),
    /// so callers can decide whether to fall back to A8 instead of evicting.
    last_select_evicted: bool,
}

impl Pool {
    fn new(max_pages: usize) -> Self {
        Self {
            pages: Vec::new(),
            index: HashMap::new(),
            clock_hand: 0,
            max_pages: max_pages.max(1),
            upload_bytes_total: 0,
            resident_glyphs: 0,
            last_select_evicted: false,
        }
    }

    /// Whether the given key is resident (its index entry still points at a live
    /// generation). A hit here means admission is a no-upload cache hit.
    fn resident_page(&self, key: &GlyphKey) -> Option<usize> {
        self.index.get(key).and_then(|&(page, generation)| {
            (self.pages[page].generation == generation).then_some(page)
        })
    }

    /// Whether an admit of a new glyph would force an eviction: every page is
    /// full and the pool is at its page budget.
    fn would_evict(&self) -> bool {
        self.pages.len() >= self.max_pages && !self.pages.iter().any(Page::has_room)
    }

    /// Admit a new (known-not-resident) glyph, returning the page it landed on
    /// and the byte offset within that page. Grows a page, reuses a page with
    /// room, or CLOCK-reclaims a cold page under budget pressure.
    fn admit(&mut self, key: GlyphKey, bitmap_bytes: usize, epoch: u64) -> (usize, usize) {
        let page = self.select_page(epoch);
        let offset_bytes = self.pages[page].bytes;
        self.pages[page].resident.push(key);
        self.pages[page].bytes += bitmap_bytes;
        let generation = self.pages[page].generation;
        self.index.insert(key, (page, generation));
        self.pages[page].referenced = true;

        self.upload_bytes_total += bitmap_bytes as u64;
        self.resident_glyphs += 1;
        (page, offset_bytes)
    }

    /// Pick a page to admit onto: a page with room, else a fresh page while
    /// under budget, else a CLOCK-reclaimed cold page. Sets
    /// [`Self::last_select_evicted`] when a page had to be reclaimed.
    fn select_page(&mut self, epoch: u64) -> usize {
        self.last_select_evicted = false;
        if let Some(page) = self.pages.iter().position(Page::has_room) {
            return page;
        }
        if self.pages.len() < self.max_pages {
            self.pages.push(Page::new(epoch));
            return self.pages.len() - 1;
        }
        let victim = self.evict_one();
        self.reclaim_page(victim, epoch);
        self.last_select_evicted = true;
        victim
    }

    /// The CLOCK sweep: from the hand, give any referenced page one second
    /// chance (clear the bit, advance) and reclaim the first non-referenced
    /// page, preferring the coldest. Returns the page index to reuse.
    fn evict_one(&mut self) -> usize {
        let n = self.pages.len();
        // A full sweep clearing reference bits guarantees a non-referenced page
        // exists on the second lap; bound the scan to two laps.
        for _ in 0..(2 * n) {
            let idx = self.clock_hand % n;
            self.clock_hand = (self.clock_hand + 1) % n;
            if self.pages[idx].referenced {
                self.pages[idx].referenced = false;
            } else {
                return idx;
            }
        }
        // Fallback: after two laps every bit was cleared, so pick the coldest.
        (0..n)
            .min_by_key(|&i| self.pages[i].last_used_epoch)
            .unwrap_or(0)
    }

    /// Reclaim a page for reuse: bump its generation (invalidating only the
    /// entries that lived on it), drop its residents, and reset its bytes. The
    /// stale index entries are detected lazily by generation mismatch on
    /// lookup. This never clears any other page or any other pool.
    fn reclaim_page(&mut self, page: usize, epoch: u64) {
        let evicted = std::mem::take(&mut self.pages[page].resident);
        self.resident_glyphs -= evicted.len();
        for key in &evicted {
            // Only drop index entries still pointing at this page's old
            // generation; a key re-admitted elsewhere must not be removed.
            if let Some(&(p, g)) = self.index.get(key)
                && p == page
                && g == self.pages[page].generation
            {
                self.index.remove(key);
            }
        }
        self.pages[page].generation = self.pages[page].generation.wrapping_add(1);
        self.pages[page].bytes = 0;
        self.pages[page].last_used_epoch = epoch;
        self.pages[page].referenced = false;
    }
}

/// The residency metadata across the four representation pools. Owns no GPU
/// memory; tracks which [`GlyphKey`]s are resident and their page ages.
///
/// The four pools (A8 coverage, MTSDF, RGBA color, vector) are structurally
/// distinct [`Pool`]s so their eviction queues and byte accounting never mix.
/// A single shared epoch and per-frame touched set drive recency across all
/// pools with one fold per frame.
#[derive(Debug)]
pub struct GlyphResidency {
    /// The A8 coverage pool: the default and the always-correct fallback.
    a8: Pool,
    /// The MTSDF pool: glyphs promoted under sustained transform.
    mtsdf: Pool,
    /// The RGBA color pool: bitmap-strike color glyphs.
    rgba: Pool,
    /// The vector pool: retained outline / color-vector glyphs.
    vector: Pool,
    /// Pages touched this frame, folded into recency by [`Self::advance_epoch`].
    /// Keyed by `(kind, page)` so all four pools share one per-frame bitset and
    /// recency stays off the per-glyph path.
    touched_this_frame: HashSet<(GlyphImageKind, usize)>,
    /// Monotonic epoch counter; advanced once per frame across all pools.
    epoch: u64,
}

impl Default for GlyphResidency {
    fn default() -> Self {
        // A stable default page budget per pool; render sizes real atlas memory.
        Self::new(64)
    }
}

impl GlyphResidency {
    /// A residency map with the given page budget applied to every pool.
    pub fn new(max_pages: usize) -> Self {
        Self {
            a8: Pool::new(max_pages),
            mtsdf: Pool::new(max_pages),
            rgba: Pool::new(max_pages),
            vector: Pool::new(max_pages),
            touched_this_frame: HashSet::new(),
            epoch: 0,
        }
    }

    /// A residency map with independent per-pool page budgets. A pool given a
    /// small budget evicts sooner without affecting the others.
    pub fn with_budgets(a8: usize, mtsdf: usize, rgba: usize, vector: usize) -> Self {
        Self {
            a8: Pool::new(a8),
            mtsdf: Pool::new(mtsdf),
            rgba: Pool::new(rgba),
            vector: Pool::new(vector),
            touched_this_frame: HashSet::new(),
            epoch: 0,
        }
    }

    /// The pool a representation kind resolves into.
    fn pool_mut(&mut self, kind: GlyphImageKind) -> &mut Pool {
        match kind {
            GlyphImageKind::MaskA8 => &mut self.a8,
            GlyphImageKind::ScalableMtsdf => &mut self.mtsdf,
            GlyphImageKind::ColorRgba8 => &mut self.rgba,
            GlyphImageKind::OutlineVector | GlyphImageKind::ColorVector => &mut self.vector,
        }
    }

    fn pool(&self, kind: GlyphImageKind) -> &Pool {
        match kind {
            GlyphImageKind::MaskA8 => &self.a8,
            GlyphImageKind::ScalableMtsdf => &self.mtsdf,
            GlyphImageKind::ColorRgba8 => &self.rgba,
            GlyphImageKind::OutlineVector | GlyphImageKind::ColorVector => &self.vector,
        }
    }

    /// Make a glyph resident in the pool its [`GlyphKey::kind`] selects.
    ///
    /// If the glyph is resident this is a [`Admission::Cached`] hit and no bytes
    /// are counted. Otherwise it is admitted onto a page in that pool, reusing a
    /// page with room, growing a page under budget, or CLOCK-reclaiming a cold
    /// page in *that pool only*. This is the only method that mutates a pool's
    /// CLOCK/page metadata, and it runs on the cold admit path, never on glyph
    /// draw. Eviction is confined to the target pool: no whole-atlas reset, no
    /// cross-pool clear.
    pub fn get_or_admit(&mut self, key: GlyphKey, bitmap_bytes: usize) -> Admission {
        let kind = key.kind;
        let epoch = self.epoch;
        let pool = self.pool_mut(kind);
        if let Some(page) = pool.resident_page(&key) {
            pool.pages[page].referenced = true;
            self.touched_this_frame.insert((kind, page));
            return Admission::Cached { kind, page };
        }
        let (page, offset_bytes) = pool.admit(key, bitmap_bytes, epoch);
        self.touched_this_frame.insert((kind, page));
        Admission::Admitted {
            kind,
            page,
            offset_bytes,
        }
    }

    /// Make a glyph resident, falling back to exact A8 coverage rather than
    /// evicting from a promoted pool under pressure.
    ///
    /// When `key.kind` is already A8, or its pool has room, this is exactly
    /// [`Self::get_or_admit`]. When the promoted pool would have to evict a live
    /// page to admit, the glyph is instead admitted into the A8 pool as a
    /// [`GlyphImageKind::MaskA8`] entry — coverage is always a correct answer,
    /// so residency pressure degrades quality, never correctness, and never
    /// forces an eviction cascade. The returned [`Admission`]'s `kind` tells the
    /// caller which pool actually holds the glyph. Requires `a8_bitmap_bytes`
    /// for the fallback A8 upload alongside the promoted `bitmap_bytes`.
    pub fn get_or_admit_with_fallback(
        &mut self,
        key: GlyphKey,
        bitmap_bytes: usize,
        a8_bitmap_bytes: usize,
    ) -> Admission {
        let kind = key.kind;
        // Already resident in its own pool: a plain hit, no fallback needed.
        if self.pool(kind).resident_page(&key).is_some() {
            return self.get_or_admit(key, bitmap_bytes);
        }
        // A8 requests never fall back — A8 *is* the fallback.
        if kind == GlyphImageKind::MaskA8 || !self.pool(kind).would_evict() {
            return self.get_or_admit(key, bitmap_bytes);
        }
        // The promoted pool would evict: admit into A8 as coverage instead.
        let a8_key = GlyphKey {
            kind: GlyphImageKind::MaskA8,
            ..key
        };
        self.get_or_admit(a8_key, a8_bitmap_bytes)
    }

    /// Make an A8 glyph resident. Thin wrapper over [`Self::get_or_admit`] for
    /// the pure-coverage path, the common case for editor and CJK text.
    pub fn get_or_admit_a8(&mut self, key: GlyphKey, bitmap_bytes: usize) -> Admission {
        debug_assert_eq!(
            key.kind,
            GlyphImageKind::MaskA8,
            "get_or_admit_a8 is A8-only; use get_or_admit for other kinds"
        );
        self.get_or_admit(key, bitmap_bytes)
    }

    /// Mark a resident glyph used this frame, for the CLOCK sweep.
    ///
    /// This is the per-glyph-draw touch. It only records the page as touched;
    /// recency is applied by [`Self::advance_epoch`], so many draws of the same
    /// glyph collapse to one recency update per frame and nothing mutates page
    /// age here.
    pub fn touch(&mut self, key: GlyphKey) {
        let kind = key.kind;
        let pool = self.pool_mut(kind);
        if let Some(page) = pool.resident_page(&key) {
            pool.pages[page].referenced = true;
            self.touched_this_frame.insert((kind, page));
        }
    }

    /// Fold this frame's touched pages into recency and advance the epoch.
    ///
    /// Every page touched during the frame, in any pool, has its
    /// `last_used_epoch` set to the current epoch exactly once, regardless of
    /// how many glyphs were drawn on it. Call once per frame.
    pub fn advance_epoch(&mut self) {
        let epoch = self.epoch;
        for (kind, page) in self.touched_this_frame.drain() {
            let pool = match kind {
                GlyphImageKind::MaskA8 => &mut self.a8,
                GlyphImageKind::ScalableMtsdf => &mut self.mtsdf,
                GlyphImageKind::ColorRgba8 => &mut self.rgba,
                GlyphImageKind::OutlineVector | GlyphImageKind::ColorVector => &mut self.vector,
            };
            if let Some(p) = pool.pages.get_mut(page) {
                p.last_used_epoch = epoch;
            }
        }
        self.epoch += 1;
    }

    /// The accumulated upload candidate size in bytes across all four pools
    /// (counter 61 `gpu_upload_bytes`: grows only on admit, never on a hit).
    pub fn upload_bytes_total(&self) -> u64 {
        self.a8.upload_bytes_total
            + self.mtsdf.upload_bytes_total
            + self.rgba.upload_bytes_total
            + self.vector.upload_bytes_total
    }

    /// The accumulated upload candidate size in bytes for one pool.
    pub fn pool_upload_bytes(&self, kind: GlyphImageKind) -> u64 {
        self.pool(kind).upload_bytes_total
    }

    /// The number of glyphs currently resident across all four pools.
    pub fn resident_glyphs(&self) -> usize {
        self.a8.resident_glyphs
            + self.mtsdf.resident_glyphs
            + self.rgba.resident_glyphs
            + self.vector.resident_glyphs
    }

    /// The number of glyphs currently resident in one pool.
    pub fn pool_resident_glyphs(&self, kind: GlyphImageKind) -> usize {
        self.pool(kind).resident_glyphs
    }

    /// The number of pages currently allocated across all four pools.
    pub fn page_count(&self) -> usize {
        self.a8.pages.len()
            + self.mtsdf.pages.len()
            + self.rgba.pages.len()
            + self.vector.pages.len()
    }

    /// The number of pages currently allocated in one pool.
    pub fn pool_page_count(&self, kind: GlyphImageKind) -> usize {
        self.pool(kind).pages.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key_kind(glyph: u16, bucket: u16, kind: GlyphImageKind) -> GlyphKey {
        GlyphKey {
            face: FontFaceId(0),
            glyph,
            kind,
            bucket,
        }
    }

    fn key(glyph: u16, bucket: u16) -> GlyphKey {
        key_kind(glyph, bucket, GlyphImageKind::MaskA8)
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
        let after_admit = res.a8.pages[page].last_used_epoch;

        // 10_000 draws of the same glyph in one frame must not move page age.
        for _ in 0..10_000 {
            res.touch(k);
        }
        assert_eq!(
            res.a8.pages[page].last_used_epoch, after_admit,
            "touch must not update recency per glyph"
        );

        res.advance_epoch();
        assert!(
            res.a8.pages[page].last_used_epoch > after_admit,
            "advance_epoch folds the frame's touches into one recency update"
        );
    }

    #[test]
    fn clock_second_chance_spares_recently_referenced() {
        // Two single-slot pages, budget of two; fill both, then force eviction.
        let mut res = GlyphResidency::new(2);
        // Fill page 0 to capacity and page 1 to capacity across distinct epochs.
        for g in 0..PAGE_GLYPH_CAP as u16 {
            res.get_or_admit_a8(key(g, 0), 10);
        }
        res.advance_epoch();
        for g in 0..PAGE_GLYPH_CAP as u16 {
            res.get_or_admit_a8(key(g, 1), 10);
        }
        res.advance_epoch();
        assert_eq!(res.pool_page_count(GlyphImageKind::MaskA8), 2);

        // Reference page 1's glyphs this frame so it earns a second chance.
        res.touch(key(0, 1));

        // Admitting a new glyph forces eviction of a page. Page 0 (unreferenced,
        // colder) must be reclaimed, not page 1, and page 1's residents survive.
        let before_page1_residents = res.a8.pages[1].resident.len();
        res.get_or_admit_a8(key(999, 2), 10);

        assert_eq!(
            res.pool_page_count(GlyphImageKind::MaskA8),
            2,
            "no new page: a page was reused, not grown"
        );
        assert_eq!(
            res.a8.pages[1].resident.len(),
            before_page1_residents.min(PAGE_GLYPH_CAP),
            "the referenced page's residents were not cleared"
        );
        // The whole atlas was not cleared: page 1 still holds a real key.
        assert!(res.a8.pages[1].resident.contains(&key(0, 1)));
    }

    #[test]
    fn evicted_page_generation_bumps_and_invalidates_only_its_entries() {
        let mut res = GlyphResidency::new(2);
        // Page 0: one glyph. Page 1: fill so page 0 is the only room-less
        // candidate for a forced eviction later.
        let victim_key = key(1, 0);
        res.get_or_admit_a8(victim_key, 10);
        // Fill page 0 the rest of the way and all of page 1.
        for g in 2..=PAGE_GLYPH_CAP as u16 {
            res.get_or_admit_a8(key(g, 0), 10);
        }
        let survivor_key = key(500, 1);
        res.get_or_admit_a8(survivor_key, 10);
        for g in 501..(500 + PAGE_GLYPH_CAP as u16) {
            res.get_or_admit_a8(key(g, 1), 10);
        }
        assert_eq!(res.pool_page_count(GlyphImageKind::MaskA8), 2);

        let gen_before = res.a8.pages[0].generation;
        // Force an eviction; page 0 (colder, unreferenced) is reclaimed.
        res.get_or_admit_a8(key(9999, 2), 10);
        assert_eq!(
            res.a8.pages[0].generation,
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

    #[test]
    fn four_pools_are_independent_no_cross_pool_eviction() {
        // Each kind admits into its own pool; filling and evicting one leaves the
        // others untouched. Give each pool a one-page budget's worth so pressure
        // on one cannot spill.
        let mut res = GlyphResidency::new(1);

        // Admit one glyph into each of the four pools.
        res.get_or_admit(key_kind(1, 0, GlyphImageKind::MaskA8), 10);
        res.get_or_admit(key_kind(1, 0, GlyphImageKind::ScalableMtsdf), 20);
        res.get_or_admit(key_kind(1, 0, GlyphImageKind::ColorRgba8), 40);
        res.get_or_admit(key_kind(1, 0, GlyphImageKind::OutlineVector), 80);

        assert_eq!(res.pool_resident_glyphs(GlyphImageKind::MaskA8), 1);
        assert_eq!(res.pool_resident_glyphs(GlyphImageKind::ScalableMtsdf), 1);
        assert_eq!(res.pool_resident_glyphs(GlyphImageKind::ColorRgba8), 1);
        assert_eq!(res.pool_resident_glyphs(GlyphImageKind::OutlineVector), 1);

        // Per-pool byte accounting never mixes.
        assert_eq!(res.pool_upload_bytes(GlyphImageKind::MaskA8), 10);
        assert_eq!(res.pool_upload_bytes(GlyphImageKind::ScalableMtsdf), 20);
        assert_eq!(res.pool_upload_bytes(GlyphImageKind::ColorRgba8), 40);
        assert_eq!(res.pool_upload_bytes(GlyphImageKind::OutlineVector), 80);

        // Fill the MTSDF pool to capacity and force it to evict; the other three
        // pools' residency and bytes are unchanged.
        for g in 2..=PAGE_GLYPH_CAP as u16 {
            res.get_or_admit(key_kind(g, 0, GlyphImageKind::ScalableMtsdf), 20);
        }
        res.get_or_admit(key_kind(9999, 0, GlyphImageKind::ScalableMtsdf), 20);

        assert_eq!(
            res.pool_page_count(GlyphImageKind::ScalableMtsdf),
            1,
            "MTSDF stayed at its one-page budget: it evicted, did not grow"
        );
        // The other pools are undisturbed by MTSDF eviction.
        assert_eq!(res.pool_resident_glyphs(GlyphImageKind::MaskA8), 1);
        assert_eq!(res.pool_resident_glyphs(GlyphImageKind::ColorRgba8), 1);
        assert_eq!(res.pool_resident_glyphs(GlyphImageKind::OutlineVector), 1);
        assert_eq!(res.pool_upload_bytes(GlyphImageKind::MaskA8), 10);
        assert_eq!(res.pool_upload_bytes(GlyphImageKind::ColorRgba8), 40);
        assert_eq!(res.pool_upload_bytes(GlyphImageKind::OutlineVector), 80);
    }

    #[test]
    fn atlas_full_evicts_a_page_never_whole_resets() {
        // A single-page pool held to capacity: admitting one more must reclaim
        // exactly that page (bump one generation, drop only its residents), not
        // clear the pool or grow unboundedly.
        let mut res = GlyphResidency::new(1);
        for g in 0..PAGE_GLYPH_CAP as u16 {
            res.get_or_admit_a8(key(g, 0), 10);
        }
        assert_eq!(res.pool_page_count(GlyphImageKind::MaskA8), 1);
        assert_eq!(
            res.pool_resident_glyphs(GlyphImageKind::MaskA8),
            PAGE_GLYPH_CAP
        );
        let gen_before = res.a8.pages[0].generation;

        // Overflow the full single page.
        res.get_or_admit_a8(key(9999, 0), 10);

        // Exactly one generation bump: the page was reclaimed once, not reset in
        // a loop, and no second page was grown.
        assert_eq!(
            res.a8.pages[0].generation,
            gen_before.wrapping_add(1),
            "the full page was reclaimed exactly once (no whole-atlas reset loop)"
        );
        assert_eq!(
            res.pool_page_count(GlyphImageKind::MaskA8),
            1,
            "still one page: reclaimed and reused, not grown or cleared to zero"
        );
        // The new glyph is resident on the reclaimed page.
        assert!(matches!(
            res.get_or_admit_a8(key(9999, 0), 10),
            Admission::Cached { .. }
        ));
    }

    #[test]
    fn promoted_pool_under_pressure_falls_back_to_a8_not_eviction() {
        // A one-page MTSDF pool held full. A new MTSDF glyph, admitted through
        // the fallback path, must land in A8 as coverage rather than evict a live
        // MTSDF page — coverage is always correct, so pressure degrades quality
        // not correctness, and the MTSDF residents survive.
        let mut res = GlyphResidency::new(1);
        for g in 0..PAGE_GLYPH_CAP as u16 {
            res.get_or_admit(key_kind(g, 0, GlyphImageKind::ScalableMtsdf), 20);
        }
        assert_eq!(
            res.pool_resident_glyphs(GlyphImageKind::ScalableMtsdf),
            PAGE_GLYPH_CAP
        );
        let mtsdf_bytes_before = res.pool_upload_bytes(GlyphImageKind::ScalableMtsdf);

        // This MTSDF glyph would force an MTSDF eviction; fall back to A8.
        let admission = res.get_or_admit_with_fallback(
            key_kind(9999, 0, GlyphImageKind::ScalableMtsdf),
            20,
            10,
        );
        assert!(
            matches!(
                admission,
                Admission::Admitted {
                    kind: GlyphImageKind::MaskA8,
                    ..
                }
            ),
            "under MTSDF pressure the glyph fell back to A8 coverage"
        );
        // The MTSDF pool did not evict: same resident count and bytes.
        assert_eq!(
            res.pool_resident_glyphs(GlyphImageKind::ScalableMtsdf),
            PAGE_GLYPH_CAP,
            "no MTSDF eviction: the promoted pool's residents are intact"
        );
        assert_eq!(
            res.pool_upload_bytes(GlyphImageKind::ScalableMtsdf),
            mtsdf_bytes_before,
            "fallback added no MTSDF bytes"
        );
        // A8 gained the fallback glyph.
        assert_eq!(res.pool_resident_glyphs(GlyphImageKind::MaskA8), 1);
        assert_eq!(res.pool_upload_bytes(GlyphImageKind::MaskA8), 10);
    }

    #[test]
    fn fallback_is_noop_when_pool_has_room() {
        // With room in the MTSDF pool, the fallback path admits into MTSDF as
        // requested — no gratuitous downgrade to A8.
        let mut res = GlyphResidency::new(4);
        let admission =
            res.get_or_admit_with_fallback(key_kind(7, 0, GlyphImageKind::ScalableMtsdf), 20, 10);
        assert!(matches!(
            admission,
            Admission::Admitted {
                kind: GlyphImageKind::ScalableMtsdf,
                ..
            }
        ));
        assert_eq!(res.pool_resident_glyphs(GlyphImageKind::ScalableMtsdf), 1);
        assert_eq!(res.pool_resident_glyphs(GlyphImageKind::MaskA8), 0);
    }

    #[test]
    fn independent_budgets_evict_at_their_own_thresholds() {
        // A tiny A8 budget and a large MTSDF budget: A8 evicts while MTSDF grows.
        let mut res = GlyphResidency::with_budgets(1, 4, 1, 1);
        // Fill A8's single page and overflow it: it must stay at one page.
        for g in 0..=(PAGE_GLYPH_CAP as u16) {
            res.get_or_admit_a8(key(g, 0), 10);
        }
        assert_eq!(res.pool_page_count(GlyphImageKind::MaskA8), 1);

        // MTSDF has room to grow a second page under its larger budget.
        for g in 0..(PAGE_GLYPH_CAP as u16 + 1) {
            res.get_or_admit(key_kind(g, 0, GlyphImageKind::ScalableMtsdf), 20);
        }
        assert_eq!(
            res.pool_page_count(GlyphImageKind::ScalableMtsdf),
            2,
            "MTSDF grew a second page under its own budget, independent of A8"
        );
    }
}
