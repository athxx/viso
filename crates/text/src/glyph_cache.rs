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
//! A full pool is therefore a normal steady state, not a reset event: the
//! coldest page is reclaimed and immediately re-admitted into, and every other
//! page keeps its live glyphs and its pixels. Memory pressure uses the same
//! sweep through [`GlyphResidency::shed_pool_to_pressure`], confined to the one
//! pool named — it can never cascade into a text-cache-wide clear.
//!
//! # The pixel owner is told exactly which pages died
//!
//! Reclaiming a page invalidates pixels this crate does not own, so every
//! reclaim is recorded and handed to the pixel owner through
//! [`GlyphResidency::take_reclaims`]: `viso-render` resets that page's packer
//! and drops the placements it held, and nothing else. The reverse direction
//! exists too — when the owner's packer cannot fit a glyph the metadata layer
//! thought would fit (rectangle fragmentation is invisible from here), the owner
//! calls [`GlyphResidency::revoke`], which undoes the admission and seals the
//! page so no further glyph is aimed at it until it is reclaimed.
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
/// many glyphs, or when its byte capacity is reached, whichever comes first.
const PAGE_GLYPH_CAP: usize = 256;

/// The default byte capacity of one page: a 256×256 single-channel page.
///
/// The pixel owner overrides this per pool through [`PoolBudget`] — an RGBA page
/// of the same edge length holds four times the bytes — so the metadata layer's
/// notion of a full page tracks real texture memory.
pub const DEFAULT_PAGE_BYTES: usize = 256 * 256;

/// One pool's independent budget: how many pages it may hold, and how many
/// bitmap bytes fit on one of its pages.
///
/// [`Self::bytes`] is the pool's byte ceiling. Budgets are per pool and never
/// shared: a pool at its ceiling evicts within itself, so filling the color pool
/// cannot evict a coverage glyph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PoolBudget {
    /// Maximum pages this pool may allocate before eviction is forced.
    pub pages: usize,
    /// Maximum admitted bitmap bytes one page holds.
    pub page_bytes: usize,
}

impl PoolBudget {
    /// A budget of `pages` pages of `page_bytes` bytes each.
    pub const fn new(pages: usize, page_bytes: usize) -> Self {
        Self { pages, page_bytes }
    }

    /// The pool's byte ceiling: every page full.
    pub const fn bytes(&self) -> usize {
        self.pages * self.page_bytes
    }
}

impl Default for PoolBudget {
    fn default() -> Self {
        Self::new(64, DEFAULT_PAGE_BYTES)
    }
}

/// A page whose contents were dropped by a reclaim, reported to the owner of the
/// pixels.
///
/// The pixel owner must reset that page's rectangle packer and drop the
/// placements it was holding for that page — and nothing else: no other page, no
/// other pool, and never the whole texture. `kind` is the pool's canonical kind,
/// so the vector pool reports [`GlyphImageKind::OutlineVector`] for both vector
/// representations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reclaimed {
    pub kind: GlyphImageKind,
    pub page: usize,
}

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
    /// One past the epoch this page was last drawn from or admitted to. While
    /// that epoch is current the frame holds placements on the page, so
    /// reclaiming it would repaint glyphs already handed out; CLOCK skips it
    /// unless the frame alone needs every page.
    busy_until: u64,
    /// Set by [`GlyphResidency::revoke`] when the pixel owner's packer could not
    /// fit a glyph this page's byte accounting said would fit. A sealed page
    /// accepts no further glyphs until it is reclaimed.
    sealed: bool,
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
            busy_until: 0,
            sealed: false,
            generation: 0,
            resident: Vec::new(),
            bytes: 0,
        }
    }

    /// Whether a `need`-byte glyph still fits: a free slot, byte room under the
    /// page's capacity, and not sealed by the pixel owner.
    fn has_room(&self, need: usize, page_bytes: usize) -> bool {
        !self.sealed
            && self.resident.len() < PAGE_GLYPH_CAP
            && self.bytes.saturating_add(need) <= page_bytes
    }
}

/// One representation's residency pool: its own pages, CLOCK eviction hand,
/// byte accounting, and glyph index. Pools never share state, so eviction in
/// one pool never touches another and a whole-atlas reset is impossible.
#[derive(Debug)]
struct Pool {
    /// The pool's canonical representation kind, reported in [`Reclaimed`].
    kind: GlyphImageKind,
    /// This pool's pages.
    pages: Vec<Page>,
    /// Where each resident glyph lives: `(page index, page generation at
    /// admission)`. A stale generation means the entry was invalidated by a
    /// page reclaim. Cold-path lookup only; never touched per glyph draw.
    index: HashMap<GlyphKey, (usize, u32)>,
    /// CLOCK hand: where the next eviction sweep resumes.
    clock_hand: usize,
    /// This pool's independent budget: page count and per-page bytes.
    budget: PoolBudget,
    /// Accumulated bitmap bytes ever admitted into this pool: its upload
    /// candidate volume (counter 61 `gpu_upload_bytes` for the pool).
    upload_bytes_total: u64,
    /// Number of glyphs currently resident in this pool.
    resident_glyphs: usize,
    /// Pages reclaimed in this pool over its lifetime (counter 61).
    evictions: u64,
    /// Admissions the pixel owner could not place, undone by
    /// [`GlyphResidency::revoke`] (counter 61).
    admission_failures: u64,
}

impl Pool {
    fn new(kind: GlyphImageKind, budget: PoolBudget) -> Self {
        Self {
            kind,
            pages: Vec::new(),
            index: HashMap::new(),
            clock_hand: 0,
            budget: PoolBudget::new(budget.pages.max(1), budget.page_bytes.max(1)),
            upload_bytes_total: 0,
            resident_glyphs: 0,
            evictions: 0,
            admission_failures: 0,
        }
    }

    /// Whether the given key is resident (its index entry still points at a live
    /// generation). A hit here means admission is a no-upload cache hit.
    fn resident_page(&self, key: &GlyphKey) -> Option<usize> {
        self.index.get(key).and_then(|&(page, generation)| {
            (self.pages[page].generation == generation).then_some(page)
        })
    }

    /// The first page a `need`-byte glyph fits on, if any.
    fn room_for(&self, need: usize) -> Option<usize> {
        let page_bytes = self.budget.page_bytes;
        self.pages
            .iter()
            .position(|page| page.has_room(need, page_bytes))
    }

    /// Whether an admit of a new `need`-byte glyph would force an eviction:
    /// no page has room and the pool is at its page budget.
    fn would_evict(&self, need: usize) -> bool {
        self.pages.len() >= self.budget.pages && self.room_for(need).is_none()
    }

    /// Bytes currently held by this pool's resident glyphs.
    fn resident_bytes(&self) -> usize {
        self.pages.iter().map(|page| page.bytes).sum()
    }

    /// Admit a new (known-not-resident) glyph, returning the page it landed on
    /// and the byte offset within that page. Grows a page, reuses a page with
    /// room, or CLOCK-reclaims a cold page under budget pressure, logging any
    /// reclaim for the pixel owner.
    fn admit(
        &mut self,
        key: GlyphKey,
        bitmap_bytes: usize,
        epoch: u64,
        reclaims: &mut Vec<Reclaimed>,
    ) -> (usize, usize) {
        let page = self.select_page(bitmap_bytes, epoch, reclaims);
        let offset_bytes = self.pages[page].bytes;
        self.pages[page].resident.push(key);
        self.pages[page].bytes += bitmap_bytes;
        let generation = self.pages[page].generation;
        self.index.insert(key, (page, generation));
        self.pages[page].referenced = true;
        self.pages[page].busy_until = epoch + 1;

        self.upload_bytes_total += bitmap_bytes as u64;
        self.resident_glyphs += 1;
        (page, offset_bytes)
    }

    /// Pick a page to admit a `need`-byte glyph onto: a page with room, else a
    /// fresh page while under budget, else a CLOCK-reclaimed cold page (logged
    /// as a [`Reclaimed`] so the pixel owner can reset just that page).
    fn select_page(&mut self, need: usize, epoch: u64, reclaims: &mut Vec<Reclaimed>) -> usize {
        if let Some(page) = self.room_for(need) {
            return page;
        }
        if self.pages.len() < self.budget.pages {
            self.pages.push(Page::new(epoch));
            return self.pages.len() - 1;
        }
        let victim = self.evict_one(epoch);
        self.reclaim_page(victim, epoch, reclaims);
        victim
    }

    /// Reclaim the coldest pages, one at a time, until the pool's resident bytes
    /// fit `pressure_bytes`. Confined to this pool: it reclaims pages here and
    /// reports them, and touches no other pool and no other cache.
    fn shed(&mut self, pressure_bytes: usize, epoch: u64, reclaims: &mut Vec<Reclaimed>) {
        while self.resident_bytes() > pressure_bytes {
            let Some(victim) = (0..self.pages.len())
                .filter(|&i| self.pages[i].bytes > 0)
                .min_by_key(|&i| self.pages[i].last_used_epoch)
            else {
                return;
            };
            self.reclaim_page(victim, epoch, reclaims);
        }
    }

    /// Undo the most recent admission of `key` on `page` — the pixel owner could
    /// not place it — and seal the page so nothing else is aimed at it until it
    /// is reclaimed. `bitmap_bytes` must be what the admission was charged.
    fn revoke(&mut self, key: &GlyphKey, page: usize, bitmap_bytes: usize) {
        let Some(slot) = self.pages.get_mut(page) else {
            return;
        };
        slot.sealed = true;
        self.admission_failures += 1;
        debug_assert_eq!(
            slot.resident.last(),
            Some(key),
            "revoke must name the admission that just happened on this page"
        );
        if slot.resident.last() != Some(key) {
            return;
        }
        slot.resident.pop();
        slot.bytes = slot.bytes.saturating_sub(bitmap_bytes);
        self.index.remove(key);
        self.upload_bytes_total = self.upload_bytes_total.saturating_sub(bitmap_bytes as u64);
        self.resident_glyphs -= 1;
    }

    /// Drop every glyph of `face` from this pool, returning how many were
    /// resident. Their texels stay packed until the page is next reclaimed —
    /// the packer frees whole pages, not rectangles — so each page keeps its
    /// byte charge and nothing else in the pool moves.
    fn forget_face(&mut self, face: FontFaceId) -> usize {
        let mut dropped = 0;
        for page in &mut self.pages {
            let before = page.resident.len();
            page.resident.retain(|key| key.face != face);
            dropped += before - page.resident.len();
        }
        if dropped > 0 {
            self.index.retain(|key, _| key.face != face);
            self.resident_glyphs -= dropped;
        }
        dropped
    }

    /// The CLOCK sweep: from the hand, give any referenced page one second
    /// chance (clear the bit, advance) and reclaim the first non-referenced
    /// page the frame at `epoch` holds nothing on. Returns the page index to
    /// reuse.
    fn evict_one(&mut self, epoch: u64) -> usize {
        let n = self.pages.len();
        // A full sweep clearing reference bits guarantees a non-referenced page
        // exists on the second lap if any page is idle; bound the scan to two
        // laps.
        for _ in 0..(2 * n) {
            let idx = self.clock_hand % n;
            self.clock_hand = (self.clock_hand + 1) % n;
            let page = &mut self.pages[idx];
            if page.busy_until > epoch {
                continue;
            }
            if page.referenced {
                page.referenced = false;
            } else {
                return idx;
            }
        }
        // The frame holds every page: its working set exceeds the pool, so the
        // coldest page goes regardless.
        (0..n)
            .min_by_key(|&i| self.pages[i].last_used_epoch)
            .unwrap_or(0)
    }

    /// Reclaim a page for reuse: bump its generation (invalidating only the
    /// entries that lived on it), drop its residents, and reset its bytes. The
    /// stale index entries are detected lazily by generation mismatch on
    /// lookup. This never clears any other page or any other pool.
    ///
    /// The reclaim is logged so the pixel owner resets exactly this page.
    fn reclaim_page(&mut self, page: usize, epoch: u64, reclaims: &mut Vec<Reclaimed>) {
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
        self.pages[page].sealed = false;
        self.evictions += 1;
        reclaims.push(Reclaimed {
            kind: self.kind,
            page,
        });
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
    /// Pages reclaimed since the pixel owner last drained them, in reclaim order.
    /// Empty in steady state, so draining costs nothing per frame.
    reclaims: Vec<Reclaimed>,
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
    /// A residency map with the given page budget and the default page byte
    /// capacity applied to every pool.
    pub fn new(max_pages: usize) -> Self {
        let budget = PoolBudget::new(max_pages, DEFAULT_PAGE_BYTES);
        Self::with_pool_budgets(budget, budget, budget, budget)
    }

    /// A residency map with independent per-pool page budgets. A pool given a
    /// small budget evicts sooner without affecting the others.
    pub fn with_budgets(a8: usize, mtsdf: usize, rgba: usize, vector: usize) -> Self {
        Self::with_pool_budgets(
            PoolBudget::new(a8, DEFAULT_PAGE_BYTES),
            PoolBudget::new(mtsdf, DEFAULT_PAGE_BYTES),
            PoolBudget::new(rgba, DEFAULT_PAGE_BYTES),
            PoolBudget::new(vector, DEFAULT_PAGE_BYTES),
        )
    }

    /// A residency map whose four pools each carry their own page count and
    /// per-page byte capacity — the form the pixel owner uses, since an RGBA page
    /// of a given edge length holds four times the bytes of a coverage page.
    pub fn with_pool_budgets(
        a8: PoolBudget,
        mtsdf: PoolBudget,
        rgba: PoolBudget,
        vector: PoolBudget,
    ) -> Self {
        Self {
            a8: Pool::new(GlyphImageKind::MaskA8, a8),
            mtsdf: Pool::new(GlyphImageKind::ScalableMtsdf, mtsdf),
            rgba: Pool::new(GlyphImageKind::ColorRgba8, rgba),
            vector: Pool::new(GlyphImageKind::OutlineVector, vector),
            touched_this_frame: HashSet::new(),
            reclaims: Vec::new(),
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

    /// Drop every glyph of `face` from every pool — the face itself was
    /// dropped — returning how many were resident. No page is reclaimed and no
    /// other face's glyph moves; the dead texels are recovered when their page
    /// next turns over.
    pub fn forget_face(&mut self, face: FontFaceId) -> usize {
        self.a8.forget_face(face)
            + self.mtsdf.forget_face(face)
            + self.rgba.forget_face(face)
            + self.vector.forget_face(face)
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
        let reclaims = &mut self.reclaims;
        let pool = match kind {
            GlyphImageKind::MaskA8 => &mut self.a8,
            GlyphImageKind::ScalableMtsdf => &mut self.mtsdf,
            GlyphImageKind::ColorRgba8 => &mut self.rgba,
            GlyphImageKind::OutlineVector | GlyphImageKind::ColorVector => &mut self.vector,
        };
        let pool_kind = pool.kind;
        if let Some(page) = pool.resident_page(&key) {
            pool.pages[page].referenced = true;
            pool.pages[page].busy_until = epoch + 1;
            self.touched_this_frame.insert((pool_kind, page));
            return Admission::Cached { kind, page };
        }
        let (page, offset_bytes) = pool.admit(key, bitmap_bytes, epoch, reclaims);
        self.touched_this_frame.insert((pool_kind, page));
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
        if kind == GlyphImageKind::MaskA8 || !self.pool(kind).would_evict(bitmap_bytes) {
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
        let epoch = self.epoch;
        let pool = self.pool_mut(key.kind);
        let pool_kind = pool.kind;
        if let Some(page) = pool.resident_page(&key) {
            pool.pages[page].referenced = true;
            pool.pages[page].busy_until = epoch + 1;
            self.touched_this_frame.insert((pool_kind, page));
        }
    }

    /// Mark a page used this frame, for callers that already know which page a
    /// glyph lives on (the pixel owner's placement record carries it).
    ///
    /// Same effect as [`Self::touch`] without the residency index lookup, so the
    /// per-glyph-draw path costs no hash of a [`GlyphKey`].
    pub fn touch_page(&mut self, kind: GlyphImageKind, page: usize) {
        let epoch = self.epoch;
        let pool = self.pool_mut(kind);
        let pool_kind = pool.kind;
        if let Some(slot) = pool.pages.get_mut(page) {
            slot.referenced = true;
            slot.busy_until = epoch + 1;
            self.touched_this_frame.insert((pool_kind, page));
        }
    }

    /// Undo an admission the pixel owner could not place, and seal its page.
    ///
    /// The metadata layer accounts bytes; the owner packs rectangles, and
    /// fragmentation can defeat a placement this layer thought would fit. Then
    /// the owner calls this with the same `bitmap_bytes` the admission was
    /// charged: the glyph stops being resident, the bytes are refunded, one
    /// admission failure is counted, and the page is sealed so the next admit
    /// aims elsewhere (or reclaims a cold page) instead of retrying forever.
    pub fn revoke(&mut self, key: GlyphKey, page: usize, bitmap_bytes: usize) {
        self.pool_mut(key.kind).revoke(&key, page, bitmap_bytes);
    }

    /// Move the pages reclaimed since the last drain into `out`.
    ///
    /// The pixel owner calls this after admitting and after shedding, resets each
    /// reported page's packer, and drops the placements it held for that page —
    /// only those. Empty in steady state, so a warm frame drains nothing.
    pub fn take_reclaims(&mut self, out: &mut Vec<Reclaimed>) {
        out.append(&mut self.reclaims);
    }

    /// Move only `kind`'s pool reclaims into `out`, leaving the other pools'
    /// queued for their own owners.
    ///
    /// For a pool whose contents one owner holds exclusively — the vector pool's
    /// retained outlines — so that owner settles its reclaims the moment they
    /// happen, before a later admission can reuse the page they name.
    pub fn take_pool_reclaims(&mut self, kind: GlyphImageKind, out: &mut Vec<Reclaimed>) {
        let kind = self.pool(kind).kind;
        self.reclaims.retain(|reclaimed| {
            let theirs = reclaimed.kind != kind;
            if !theirs {
                out.push(*reclaimed);
            }
            theirs
        });
    }

    /// Shed one pool down to `pressure_bytes` of resident bytes, reclaiming its
    /// coldest pages first, and return how many pages were reclaimed.
    ///
    /// Memory pressure is answered inside the named pool: the other three pools
    /// keep every glyph, no other text cache is consulted, and nothing is
    /// cleared wholesale. The reclaimed pages are reported through
    /// [`Self::take_reclaims`] like any other eviction.
    pub fn shed_pool_to_pressure(&mut self, kind: GlyphImageKind, pressure_bytes: usize) -> u64 {
        let epoch = self.epoch;
        let reclaims = &mut self.reclaims;
        let pool = match kind {
            GlyphImageKind::MaskA8 => &mut self.a8,
            GlyphImageKind::ScalableMtsdf => &mut self.mtsdf,
            GlyphImageKind::ColorRgba8 => &mut self.rgba,
            GlyphImageKind::OutlineVector | GlyphImageKind::ColorVector => &mut self.vector,
        };
        let before = pool.evictions;
        pool.shed(pressure_bytes, epoch, reclaims);
        pool.evictions - before
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

    /// Bytes currently held by one pool's resident glyphs, against
    /// [`PoolBudget::bytes`].
    pub fn pool_resident_bytes(&self, kind: GlyphImageKind) -> usize {
        self.pool(kind).resident_bytes()
    }

    /// One pool's independent budget.
    pub fn pool_budget(&self, kind: GlyphImageKind) -> PoolBudget {
        self.pool(kind).budget
    }

    /// Pages reclaimed across all four pools (counter 61: evictions).
    pub fn evictions(&self) -> u64 {
        self.a8.evictions + self.mtsdf.evictions + self.rgba.evictions + self.vector.evictions
    }

    /// Pages reclaimed in one pool.
    pub fn pool_evictions(&self, kind: GlyphImageKind) -> u64 {
        self.pool(kind).evictions
    }

    /// Admissions the pixel owner could not place, across all four pools
    /// (counter 61: admission failures).
    pub fn admission_failures(&self) -> u64 {
        self.a8.admission_failures
            + self.mtsdf.admission_failures
            + self.rgba.admission_failures
            + self.vector.admission_failures
    }

    /// Admissions the pixel owner could not place in one pool.
    pub fn pool_admission_failures(&self, kind: GlyphImageKind) -> u64 {
        self.pool(kind).admission_failures
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
    fn forgetting_a_face_drops_only_its_glyphs() {
        let mut res = GlyphResidency::new(4);
        let other = key(1, 0);
        let doomed = GlyphKey {
            face: FontFaceId(7),
            ..key(2, 0)
        };
        res.get_or_admit_a8(other, 100);
        res.get_or_admit_a8(doomed, 100);
        res.get_or_admit_a8(GlyphKey { glyph: 3, ..doomed }, 100);

        assert_eq!(res.forget_face(doomed.face), 2);

        assert_eq!(res.resident_glyphs(), 1);
        assert!(matches!(
            res.get_or_admit_a8(other, 100),
            Admission::Cached { .. }
        ));
        assert!(matches!(
            res.get_or_admit_a8(doomed, 100),
            Admission::Admitted { .. }
        ));
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
    fn clock_spares_a_page_drawn_this_frame_when_every_bit_is_set() {
        let mut res = GlyphResidency::new(2);
        for g in 0..PAGE_GLYPH_CAP as u16 {
            res.get_or_admit_a8(key(g, 0), 10);
        }
        for g in 0..PAGE_GLYPH_CAP as u16 {
            res.get_or_admit_a8(key(g, 1), 10);
        }
        res.advance_epoch();
        // Both pages carry a set bit, but only page 0 is drawn from this frame.
        res.a8.pages[1].referenced = true;
        res.touch(key(0, 0));

        res.get_or_admit_a8(key(999, 2), 10);

        assert!(
            res.a8.pages[0].resident.contains(&key(0, 0)),
            "the page this frame draws from survives"
        );
        assert!(res.a8.pages[1].resident.contains(&key(999, 2)));
        assert_eq!(res.a8.pages[0].generation, 0);
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
    fn a_byte_budget_fills_a_page_before_its_slots_do() {
        // A page that holds 100 bytes takes two 40-byte glyphs and grows a second
        // page for the third, long before the 256-slot cap is in play.
        let mut res = GlyphResidency::with_pool_budgets(
            PoolBudget::new(4, 100),
            PoolBudget::default(),
            PoolBudget::default(),
            PoolBudget::default(),
        );
        let pages = |r: &GlyphResidency| r.pool_page_count(GlyphImageKind::MaskA8);
        res.get_or_admit_a8(key(1, 0), 40);
        res.get_or_admit_a8(key(2, 0), 40);
        assert_eq!(pages(&res), 1, "80 of 100 bytes: still one page");
        res.get_or_admit_a8(key(3, 0), 40);
        assert_eq!(
            pages(&res),
            2,
            "the third glyph does not fit the byte budget"
        );
        assert_eq!(res.pool_resident_bytes(GlyphImageKind::MaskA8), 120);
        assert_eq!(res.pool_budget(GlyphImageKind::MaskA8).bytes(), 400);
    }

    #[test]
    fn a_reclaim_is_reported_to_the_pixel_owner_and_nothing_else_is() {
        // One page of 100 bytes: the third glyph forces that page's reclaim, and
        // exactly one page is reported — never a whole-atlas reset.
        let mut res = GlyphResidency::with_pool_budgets(
            PoolBudget::new(1, 100),
            PoolBudget::default(),
            PoolBudget::default(),
            PoolBudget::default(),
        );
        let mut reclaims = Vec::new();
        res.get_or_admit_a8(key(1, 0), 40);
        res.get_or_admit_a8(key(2, 0), 40);
        res.take_reclaims(&mut reclaims);
        assert!(reclaims.is_empty(), "admitting with room reclaims nothing");

        res.get_or_admit_a8(key(3, 0), 40);
        res.take_reclaims(&mut reclaims);
        assert_eq!(
            reclaims,
            vec![Reclaimed {
                kind: GlyphImageKind::MaskA8,
                page: 0,
            }]
        );
        assert_eq!(res.evictions(), 1);
        // Draining is idempotent: the same reclaim is not reported twice.
        reclaims.clear();
        res.take_reclaims(&mut reclaims);
        assert!(reclaims.is_empty());
        // The re-admitted glyph is resident on the reclaimed page.
        assert!(matches!(
            res.get_or_admit_a8(key(3, 0), 40),
            Admission::Cached { .. }
        ));
    }

    #[test]
    fn a_revoked_admission_seals_its_page_and_is_not_resident() {
        // The pixel owner's packer could not fit the glyph the byte accounting
        // admitted: the admission is undone and the page takes nothing more.
        let mut res = GlyphResidency::with_pool_budgets(
            PoolBudget::new(2, 1_000),
            PoolBudget::default(),
            PoolBudget::default(),
            PoolBudget::default(),
        );
        let Admission::Admitted { page, .. } = res.get_or_admit_a8(key(1, 0), 40) else {
            panic!("first admit");
        };
        res.revoke(key(1, 0), page, 40);

        assert_eq!(res.pool_resident_glyphs(GlyphImageKind::MaskA8), 0);
        assert_eq!(res.pool_resident_bytes(GlyphImageKind::MaskA8), 0);
        assert_eq!(res.upload_bytes_total(), 0, "the bytes were refunded");
        assert_eq!(res.admission_failures(), 1);

        // The sealed page takes nothing more, so the next admit grows a page
        // instead of aiming at it again.
        let Admission::Admitted { page: next, .. } = res.get_or_admit_a8(key(2, 0), 40) else {
            panic!("second admit");
        };
        assert_ne!(next, page, "a sealed page is not admitted onto");
        assert_eq!(res.pool_page_count(GlyphImageKind::MaskA8), 2);
    }

    #[test]
    fn a_reclaim_unseals_the_page_it_reuses() {
        // Sealing is a property of a page's current contents, not of the page:
        // reclaiming it makes it admissible again.
        let mut res = GlyphResidency::with_pool_budgets(
            PoolBudget::new(1, 1_000),
            PoolBudget::default(),
            PoolBudget::default(),
            PoolBudget::default(),
        );
        let Admission::Admitted { page, .. } = res.get_or_admit_a8(key(1, 0), 40) else {
            panic!("first admit");
        };
        res.revoke(key(1, 0), page, 40);
        // The only page is sealed and the pool is at its page budget, so this
        // admit must reclaim it — and then succeed on it.
        let Admission::Admitted { page: reused, .. } = res.get_or_admit_a8(key(2, 0), 40) else {
            panic!("admit after seal");
        };
        assert_eq!(reused, page);
        assert_eq!(res.pool_resident_glyphs(GlyphImageKind::MaskA8), 1);
        assert_eq!(res.evictions(), 1);
    }

    #[test]
    fn memory_pressure_sheds_one_pool_and_leaves_the_others_whole() {
        // Fill all four pools, then shed only the RGBA pool. No other pool loses
        // a glyph, a page, or a byte: pressure never cascades into a text-cache
        // clear (§13.10 DoD).
        let page = PoolBudget::new(8, 100);
        let mut res = GlyphResidency::with_pool_budgets(page, page, page, page);
        for g in 0..6u16 {
            res.get_or_admit(key_kind(g, 0, GlyphImageKind::MaskA8), 40);
            res.get_or_admit(key_kind(g, 0, GlyphImageKind::ScalableMtsdf), 40);
            res.get_or_admit(key_kind(g, 0, GlyphImageKind::ColorRgba8), 40);
            res.get_or_admit(key_kind(g, 0, GlyphImageKind::OutlineVector), 40);
            // Spread page ages so the shed has a coldest page to pick.
            res.advance_epoch();
        }
        let mut reclaims = Vec::new();
        res.take_reclaims(&mut reclaims);
        reclaims.clear();

        let a8_before = res.pool_resident_glyphs(GlyphImageKind::MaskA8);
        let mtsdf_before = res.pool_resident_bytes(GlyphImageKind::ScalableMtsdf);
        let vector_pages = res.pool_page_count(GlyphImageKind::OutlineVector);
        let rgba_before = res.pool_resident_bytes(GlyphImageKind::ColorRgba8);
        assert!(rgba_before > 80);

        let evicted = res.shed_pool_to_pressure(GlyphImageKind::ColorRgba8, 80);
        assert!(evicted > 0, "pressure reclaimed pages");
        assert!(res.pool_resident_bytes(GlyphImageKind::ColorRgba8) <= 80);

        // Every reported reclaim is an RGBA page and nothing else.
        res.take_reclaims(&mut reclaims);
        assert_eq!(reclaims.len() as u64, evicted);
        assert!(
            reclaims
                .iter()
                .all(|r| r.kind == GlyphImageKind::ColorRgba8)
        );
        // The other three pools are untouched.
        assert_eq!(res.pool_resident_glyphs(GlyphImageKind::MaskA8), a8_before);
        assert_eq!(
            res.pool_resident_bytes(GlyphImageKind::ScalableMtsdf),
            mtsdf_before
        );
        assert_eq!(
            res.pool_page_count(GlyphImageKind::OutlineVector),
            vector_pages
        );
        assert_eq!(res.pool_evictions(GlyphImageKind::MaskA8), 0);
        assert_eq!(res.pool_evictions(GlyphImageKind::ScalableMtsdf), 0);
        assert_eq!(res.pool_evictions(GlyphImageKind::OutlineVector), 0);
    }

    #[test]
    fn shedding_to_a_budget_that_already_fits_reclaims_nothing() {
        let mut res = GlyphResidency::new(4);
        res.get_or_admit_a8(key(1, 0), 40);
        let evicted = res.shed_pool_to_pressure(GlyphImageKind::MaskA8, 1_000);
        assert_eq!(evicted, 0);
        assert_eq!(res.pool_resident_glyphs(GlyphImageKind::MaskA8), 1);
    }

    #[test]
    fn touch_page_folds_recency_without_an_index_lookup() {
        let mut res = GlyphResidency::new(2);
        let Admission::Admitted { kind, page, .. } = res.get_or_admit_a8(key(7, 0), 40) else {
            panic!("admit");
        };
        res.advance_epoch();
        let after_admit = res.a8.pages[page].last_used_epoch;

        for _ in 0..10_000 {
            res.touch_page(kind, page);
        }
        assert_eq!(res.a8.pages[page].last_used_epoch, after_admit);
        res.advance_epoch();
        assert!(res.a8.pages[page].last_used_epoch > after_admit);
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
