//! Facade-owned text preparation: resolves faces, keeps every text node's last
//! good layout, hands shaping and exact-coverage rasterization to the text
//! worker, and uploads raster products into renderer-owned representation
//! pools.
//!
//! The main thread never shapes. A node whose text, wrap width, or locale
//! changed keeps drawing its last good lines while the worker lays the new
//! ones out; the finished layout is committed in one step once every glyph it
//! draws has coverage, within a per-frame commit budget (§12.20, §12.21,
//! §14.4).

use std::cell::Cell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::mpsc::TryRecvError;
use std::sync::{Arc, OnceLock};
use std::thread;
use std::time::Duration;

use viso_gpu::{GpuBackend, TextureDesc};
use viso_platform::Instant;
use viso_render::{
    AtlasAlloc, ColorAlloc, ColorAtlas, GlyphAtlas, GlyphInstanceData, Rect, TextureId,
};
use viso_text::fallback::{FallbackPlan, FontFallback};
use viso_text::font_manifest::{AssetRef, FontManifest};
use viso_text::inspect::{MissLedger, TextInspection};
use viso_text::paragraph::{LineLayout, line_index_at};
use viso_text::raster_color::rasterize_color;
use viso_text::system_fonts::ColorGlyph;
use viso_text::text_work::Priority;
use viso_text::{
    Admission, ColorGlyphRasterizer, CoverageBitmap, FontCache, FontFaceId, FontRequest,
    FontResolver, FontRole, GlyphImageKind, GlyphKey, GlyphResidency, LineBreakTailoring,
    MemoryClass, OUTLINE_POOL, PoolBudget, Reclaimed, Resolved, TextBudgets, TextPosition,
    inspect_face,
};
use viso_ui::{Content, EditGeometry, EditLayout, NodeId, TextRequest, Vec2};

use crate::system_fonts::{LiveFontRegistry, PlatformColorRaster, PlatformFontProvider};
use crate::text_worker::{
    ClusterKey, FromWorker, LayoutDone, LayoutJob, Rastered, SpanStats, TextWorker, ToWorker,
    starts_emoji,
};

const ATLAS_SIZE: u32 = 1024;
/// Page edge length: the atlas plane is cut into `256 × 256` pages, and the page
/// is the unit of residency and of eviction (§13.9). Sixteen pages per plane is
/// enough granularity that reclaiming the coldest one costs a small fraction of
/// the working set, and few enough that the CLOCK sweep is trivially cheap.
const ATLAS_PAGE: u32 = 256;
/// Bytes per texel of the color plane, so an RGBA page is budgeted for the four
/// times the bytes an A8 page of the same edge length holds.
const COLOR_BYTES_PER_TEXEL: usize = 4;
/// How many times one glyph may be re-aimed at a different page before it is
/// given up on for this frame.
///
/// Residency accounts bytes; the packer places rectangles, so fragmentation can
/// defeat a page the byte budget said would fit. Each refusal seals that page and
/// reclaims elsewhere, so the retry always makes progress; the bound only keeps a
/// pathological glyph from walking the whole plane in one frame.
const PLACEMENT_RETRIES: u32 = 4;
/// Where fallback face ids start: above every id the resolver hands out, so the
/// two id spaces never collide.
const FALLBACK_FACE_IDS: u32 = 1 << 30;
/// The frame interval the commit budget is derived from until the display's
/// own refresh period reaches the frame loop: 60Hz.
pub(crate) const DEFAULT_FRAME_INTERVAL: Duration = Duration::from_nanos(16_666_667);

/// The main thread's per-frame budget for committing worker results at a
/// display refresh of one frame per `interval`: `min(500µs, 5% of the frame)`
/// — 500µs at 60Hz, 416µs at 120Hz, 347µs at 144Hz, 208µs at 240Hz (§14.4).
/// At least one result commits per frame, so a backlog always drains.
pub(crate) fn text_commit_budget(interval: Duration) -> Duration {
    (interval / 20).min(Duration::from_micros(500))
}

#[derive(Debug, Default)]
pub(crate) struct TextCounters {
    /// Paragraph layouts committed that ran — a pure cache hit does not count.
    reshapes: Cell<u64>,
    /// The subset of `reshapes` that broke lines to a wrap width.
    relinebreaks: Cell<u64>,
    /// Spans handed to the shaper, on whichever thread shaped them: misses of
    /// both the paragraph's per-pass memo and the cross-paragraph span cache.
    shaped_runs: Cell<u64>,
    /// The subset of `shaped_runs` shaped on the main thread; nonzero only
    /// where the worker cannot run on a thread of its own.
    main_shaped_runs: Cell<u64>,
    /// Worker results committed this frame.
    commits: Cell<u64>,
    /// Worker results the commit budget left for a later frame.
    deferred: Cell<u64>,
    /// Main-thread time spent committing worker results this frame.
    main_commit_micros: Cell<u64>,
    rasters: Cell<u64>,
    atlas_upload_bytes: Cell<u64>,
    /// Pages reclaimed this frame, across all pools (§25 eviction visibility).
    evictions: Cell<u64>,
    /// Admissions the packer refused this frame, forcing a re-aim.
    admission_failures: Cell<u64>,
}

impl TextCounters {
    pub(crate) fn reshapes(&self) -> u64 {
        self.reshapes.get()
    }

    pub(crate) fn relinebreaks(&self) -> u64 {
        self.relinebreaks.get()
    }

    pub(crate) fn shaped_runs(&self) -> u64 {
        self.shaped_runs.get()
    }

    pub(crate) fn main_shaped_runs(&self) -> u64 {
        self.main_shaped_runs.get()
    }

    pub(crate) fn commits(&self) -> u64 {
        self.commits.get()
    }

    pub(crate) fn deferred(&self) -> u64 {
        self.deferred.get()
    }

    pub(crate) fn main_commit_micros(&self) -> u64 {
        self.main_commit_micros.get()
    }

    pub(crate) fn rasters(&self) -> u64 {
        self.rasters.get()
    }

    pub(crate) fn atlas_upload_bytes(&self) -> u64 {
        self.atlas_upload_bytes.get()
    }

    pub(crate) fn evictions(&self) -> u64 {
        self.evictions.get()
    }

    pub(crate) fn admission_failures(&self) -> u64 {
        self.admission_failures.get()
    }

    fn record_shape(&self, wrapped: bool) {
        self.reshapes.set(self.reshapes.get() + 1);
        if wrapped {
            self.relinebreaks.set(self.relinebreaks.get() + 1);
        }
    }

    fn record_shaped_runs(&self, runs: u64, on_main: bool) {
        self.shaped_runs.set(self.shaped_runs.get() + runs);
        if on_main {
            self.main_shaped_runs
                .set(self.main_shaped_runs.get() + runs);
        }
    }

    fn record_commits(&self, commits: u64, deferred: usize, spent: Duration) {
        self.commits.set(self.commits.get() + commits);
        self.deferred.set(deferred as u64);
        let micros = u64::try_from(spent.as_micros()).unwrap_or(u64::MAX);
        self.main_commit_micros
            .set(self.main_commit_micros.get().saturating_add(micros));
    }

    fn record_raster(&self) {
        self.rasters.set(self.rasters.get() + 1);
    }

    fn record_upload(&self, bytes: usize) {
        self.atlas_upload_bytes
            .set(self.atlas_upload_bytes.get() + bytes as u64);
    }

    fn record_eviction(&self) {
        self.evictions.set(self.evictions.get() + 1);
    }

    fn record_admission_failure(&self) {
        self.admission_failures
            .set(self.admission_failures.get() + 1);
    }

    fn reset(&self) {
        self.reshapes.set(0);
        self.relinebreaks.set(0);
        self.shaped_runs.set(0);
        self.main_shaped_runs.set(0);
        self.commits.set(0);
        self.deferred.set(0);
        self.main_commit_micros.set(0);
        self.rasters.set(0);
        self.atlas_upload_bytes.set(0);
        self.evictions.set(0);
        self.admission_failures.set(0);
    }
}

#[derive(Debug, Clone, Copy)]
struct PositionedGlyph {
    face: FontFaceId,
    glyph: u16,
    cluster: usize,
    origin: [f32; 2],
}

/// A paragraph's lines placed in logical pixels at one font size.
#[derive(Debug, Clone, Default)]
struct PreparedLayout {
    glyphs: Vec<PositionedGlyph>,
    natural: Vec2,
    baseline: f32,
    line_height: f32,
}

/// Which retained paragraph a request belongs to: the node that draws it,
/// generation included, so a recycled node index never inherits another
/// node's lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct ParagraphSlot {
    index: u32,
    generation: u32,
}

impl ParagraphSlot {
    pub(crate) fn index(self) -> u32 {
        self.index
    }

    /// The slot packed into one integer, as the worker keys paragraphs.
    fn key(self) -> u64 {
        (u64::from(self.generation) << 32) | u64::from(self.index)
    }

    pub(crate) fn from_key(key: u64) -> Self {
        Self {
            index: key as u32,
            generation: (key >> 32) as u32,
        }
    }
}

impl From<NodeId> for ParagraphSlot {
    fn from(id: NodeId) -> Self {
        Self {
            index: id.index(),
            generation: id.generation(),
        }
    }
}

/// What a paragraph is laid out from: everything its lines depend on. The
/// font size is not part of it — shaping is in em, so a size only scales the
/// placed lines.
#[derive(Debug, Clone, PartialEq)]
struct LayoutTarget {
    text: Arc<str>,
    /// The wrap width in em, as bits; `0` for unwrapped text.
    width_em: u32,
    tailoring: LineBreakTailoring,
    base: FontFaceId,
    locale: String,
}

impl LayoutTarget {
    fn matches(
        &self,
        text: &str,
        width_em: u32,
        tailoring: LineBreakTailoring,
        base: FontFaceId,
        locale: &str,
    ) -> bool {
        *self.text == *text
            && self.width_em == width_em
            && self.tailoring == tailoring
            && self.base == base
            && self.locale == locale
    }
}

/// A layout the worker returned, placed at one font size. It holds one
/// face-cache pin on each face its lines draw with.
#[derive(Debug)]
struct Committed {
    target: LayoutTarget,
    lines: Vec<LineLayout>,
    /// The font size bits `placed` was computed at.
    font_size: u32,
    placed: PreparedLayout,
    faces: Vec<FontFaceId>,
}

/// One text node as the runtime retains it between frames.
///
/// `drawn` is the last good layout and what draws until a newer one is
/// complete. A newer layout waits in `staged` until every glyph it draws has
/// coverage, then replaces `drawn` in one step, so the node never shows half
/// of an edit or a glyph missing from the new lines.
#[derive(Debug)]
struct TextSlot {
    /// What the node asks for now.
    target: LayoutTarget,
    /// The wrap width of `target`, in logical pixels; `None` for unwrapped
    /// text.
    wrap: Option<f32>,
    font_size: f32,
    /// The raster bucket the node draws at.
    ppem: u16,
    drawn: Option<Committed>,
    staged: Option<Committed>,
    /// Layouts sent to the worker and not yet returned, oldest first.
    laying: Vec<(u64, LayoutTarget)>,
    committed_seq: u64,
    /// Waiting in the pending list for the next dispatch.
    queued: bool,
    /// The next layout reshapes every run: a cluster the last one could not
    /// cover has a face now.
    full: bool,
    /// The worker paragraph's cumulative shape invocations.
    shape_calls: u64,
}

impl TextSlot {
    fn new(target: LayoutTarget) -> Self {
        Self {
            target,
            wrap: None,
            font_size: 0.0,
            ppem: 1,
            drawn: None,
            staged: None,
            laying: Vec::new(),
            committed_seq: 0,
            queued: false,
            full: false,
            shape_calls: 0,
        }
    }
}

/// What became of a coverage glyph the atlas does not hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fate {
    /// Asked of the worker; its coverage has not come back yet.
    Requested,
    /// The worker's parser reads no outline for it, so the platform
    /// rasterizes it on the main thread when it draws.
    Platform,
    /// It has no ink.
    Blank,
    /// The atlas could not place it; it is tried again once a page is
    /// reclaimed.
    Refused,
}

/// Where one glyph's pixels live, as the pixel owner records it.
///
/// `page` is what makes residency and pixels one model: a cache hit touches that
/// page for the CLOCK sweep without hashing a key, and a reclaim of that page
/// drops exactly the placements that pointed into it.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Placement {
    uv: Rect,
    bearing: [f32; 2],
    size: [u32; 2],
    page: usize,
}

pub(crate) struct TextShaper {
    resolver: FontResolver,
    fallback: FontFallback,
    manifest: FontManifest,
    primary: Option<FontFaceId>,
    /// System queries made to resolve the primary face; one at most once it
    /// resolved, so a steady frame never asks the system for a font.
    primary_queries: u64,
    next_asset: u32,
    /// Every mounted text node, pruned with the tree.
    slots: HashMap<ParagraphSlot, TextSlot>,
    /// Slots whose target changed since the last dispatch.
    pending: Vec<ParagraphSlot>,
    worker: TextWorker,
    /// Faces whose bytes the worker holds.
    worker_faces: HashSet<FontFaceId>,
    /// Messages for the worker, sent as one batch per dispatch.
    outbox: Vec<ToWorker>,
    /// Worker results received and not yet committed.
    ready: VecDeque<FromWorker>,
    /// Jobs sent that have not replied.
    in_flight: u32,
    next_seq: u64,
    next_raster: u64,
    /// Coverage glyphs to ask of the worker at the next dispatch.
    raster_wanted: Vec<GlyphKey>,
    /// Slots that draw, or are staged with, a glyph whose coverage was asked
    /// for, redrawn when a raster result arrives.
    raster_waiters: Vec<ParagraphSlot>,
    fate: HashMap<GlyphKey, Fate>,
    /// The worker's shaping-cache counters, as of its last layout.
    span_stats: SpanStats,
    /// The worker's shaping-cache explanations, as of the last layout that
    /// changed them; empty without the inspector.
    span_ledger: MissLedger,
    /// Face parses the worker's coverage sets have cost, as of its last layout.
    layout_coverage_parses: u64,
    /// The worker's coverage explanations, as of the last layout that changed
    /// them; empty without the inspector.
    layout_coverage_ledger: MissLedger,
    /// Face residency under its own byte budget. Pins hold the primary, app,
    /// and CJK fallback faces for the process and each committed layout's
    /// faces while it lives; an evicted face is dropped with everything keyed
    /// by it at the frame boundary.
    faces: FontCache,
    /// Faces pinned for the process, so each is pinned once.
    hot_faces: Vec<FontFaceId>,
    /// Scratch for draining face evictions; empty in steady state.
    evicted_faces: Vec<FontFaceId>,
    coverage_uv: HashMap<GlyphKey, Placement>,
    color_uv: HashMap<GlyphKey, Placement>,
    coverage_atlas: Option<GlyphAtlas>,
    color_atlas: Option<ColorAtlas>,
    /// Which glyph is resident on which page, and which page is coldest. The
    /// metadata half of the atlas lives in `viso-text`; the atlases above own the
    /// pixels of the pages it names (§13.9, §13.10).
    residency: GlyphResidency,
    /// Scratch for draining reclaimed pages; reused so a reclaim allocates
    /// nothing after warm-up and a steady-state frame drains an empty vec.
    reclaims: Vec<Reclaimed>,
    /// Atlas plane geometry in texels: edge length and page edge length. Fixed
    /// for the process; the pool budgets above are derived from it, so the
    /// metadata layer's "page full" and the packer's agree by construction.
    atlas_size: u32,
    atlas_page: u32,
    provider: PlatformFontProvider,
    color_raster: PlatformColorRaster,
    counters: TextCounters,
}

impl TextShaper {
    pub(crate) fn new() -> Self {
        Self::with_atlas_geometry(
            ATLAS_SIZE,
            ATLAS_PAGE,
            MemoryClass::for_target().text_budgets(),
        )
    }

    /// A shaper over atlas planes of `size × size` texels cut into
    /// `page × page` pages, with each pool budgeted to match, and face and
    /// shaping caches held to `budgets`.
    pub(crate) fn with_atlas_geometry(size: u32, page: u32, budgets: TextBudgets) -> Self {
        // One live-font registry, shared between the provider (which records the
        // handles CoreText resolves) and the color/coverage raster (which
        // rasterizes through them). See `system_fonts::LiveFontRegistry`.
        let live = LiveFontRegistry::new();
        let per_axis = (size / page.max(1)).max(1) as usize;
        let pages = per_axis * per_axis;
        let page_bytes = (page as usize) * (page as usize);
        Self {
            resolver: FontResolver::new(),
            fallback: FontFallback::new(FALLBACK_FACE_IDS),
            manifest: FontManifest::default(),
            primary: None,
            primary_queries: 0,
            next_asset: 0,
            slots: HashMap::new(),
            pending: Vec::new(),
            worker: TextWorker::spawn(budgets.shaping_cache_bytes),
            worker_faces: HashSet::new(),
            outbox: Vec::new(),
            ready: VecDeque::new(),
            in_flight: 0,
            next_seq: 0,
            next_raster: 0,
            raster_wanted: Vec::new(),
            raster_waiters: Vec::new(),
            fate: HashMap::new(),
            span_stats: SpanStats::empty(budgets.shaping_cache_bytes),
            span_ledger: MissLedger::default(),
            layout_coverage_parses: 0,
            layout_coverage_ledger: MissLedger::default(),
            faces: FontCache::with_budget(budgets.face_cache_bytes),
            hot_faces: Vec::new(),
            evicted_faces: Vec::new(),
            coverage_uv: HashMap::new(),
            color_uv: HashMap::new(),
            coverage_atlas: None,
            color_atlas: None,
            // Each pool is budgeted from the geometry of the plane that holds it,
            // so "this page is full" means the same thing to the metadata layer
            // and to the packer. The MTSDF pool keeps its default until its plane
            // exists; the vector pool holds retained outlines, not texels, and
            // carries the outline cache's own budget.
            residency: GlyphResidency::with_pool_budgets(
                PoolBudget::new(pages, page_bytes),
                PoolBudget::default(),
                PoolBudget::new(pages, page_bytes * COLOR_BYTES_PER_TEXEL),
                OUTLINE_POOL,
            ),
            reclaims: Vec::new(),
            atlas_size: size,
            atlas_page: page,
            provider: PlatformFontProvider::new(live.clone()),
            color_raster: PlatformColorRaster::new(live),
            counters: TextCounters::default(),
        }
    }

    pub(crate) fn load_font(
        &mut self,
        bytes: impl Into<Box<[u8]>>,
        index: u32,
    ) -> Option<FontFaceId> {
        let bytes = bytes.into().into_vec();
        inspect_face(&bytes, index)?;
        let asset = AssetRef(self.next_asset);
        self.next_asset = self.next_asset.wrapping_add(1);
        let cost = bytes.len() as u64;
        let face = self.resolver.register_app_face(asset, index, bytes);
        self.primary.get_or_insert(face);
        self.pin_hot(face, cost);
        Some(face)
    }

    /// Admit `face` and pin it for the process: the primary face, app faces,
    /// and CJK fallback faces are too expensive to reload to ever evict.
    fn pin_hot(&mut self, face: FontFaceId, cost: u64) {
        if self.hot_faces.contains(&face) {
            return;
        }
        self.faces.pin(face);
        self.faces.admit(face, cost);
        self.hot_faces.push(face);
    }

    /// The face cache, for its counters and resident bytes.
    pub(crate) fn face_cache(&self) -> &FontCache {
        &self.faces
    }

    /// The worker's shaping cache, for its counters and resident bytes.
    pub(crate) fn shaping_cache(&self) -> &SpanStats {
        &self.span_stats
    }

    pub(crate) fn counters(&self) -> &TextCounters {
        &self.counters
    }

    /// Fallback resolution, for its system-query, plan, and coverage
    /// counters.
    pub(crate) fn fallback(&self) -> &FontFallback {
        &self.fallback
    }

    /// System queries made to resolve the primary face.
    /// Face parses the worker's coverage sets have cost, as of its last layout.
    pub(crate) fn layout_coverage_parses(&self) -> u64 {
        self.layout_coverage_parses
    }

    pub(crate) fn primary_queries(&self) -> u64 {
        self.primary_queries
    }

    /// The cumulative shape invocations of `slot`'s worker paragraph, and the
    /// lines it draws; `None` until it draws a layout.
    pub(crate) fn drawn_layout(&self, slot: ParagraphSlot) -> Option<(u64, &[LineLayout])> {
        let entry = self.slots.get(&slot)?;
        let drawn = entry.drawn.as_ref()?;
        Some((entry.shape_calls, &drawn.lines))
    }

    /// Why each fallback chose its face, why each cache missed, and where each
    /// resident glyph lives. Everything is empty without the inspector.
    pub(crate) fn inspect(&self) -> TextInspection {
        TextInspection {
            fallbacks: self.fallback.traces().iter().cloned().collect(),
            faces: self.faces.ledger().clone(),
            shaping: self.span_ledger.clone(),
            coverage: self.fallback.coverage_ledger().clone(),
            layout_coverage: self.layout_coverage_ledger.clone(),
            glyphs: [
                GlyphImageKind::MaskA8,
                GlyphImageKind::ScalableMtsdf,
                GlyphImageKind::ColorRgba8,
                GlyphImageKind::OutlineVector,
            ]
            .map(|kind| self.residency.pool_ledger(kind).clone())
            .into(),
            resident: self.residency.explain_resident(),
        }
    }

    /// Residency itself, for the per-pool counters (resident glyphs, pages, and
    /// upload bytes) — read straight from the owner rather than mirrored.
    pub(crate) fn residency(&self) -> &GlyphResidency {
        &self.residency
    }

    /// Close the frame: fold the pages drawn this frame into page recency once
    /// (not once per glyph draw) and zero the per-frame counters.
    pub(crate) fn end_frame(&mut self) {
        self.residency.advance_epoch();
        self.faces.advance_epoch();
        self.drop_evicted_faces();
        self.counters.reset();
    }

    /// Drop every face the face cache evicted, unless it was re-admitted
    /// since: by then it is resident again and in use.
    fn drop_evicted_faces(&mut self) {
        let mut evicted = std::mem::take(&mut self.evicted_faces);
        self.faces.take_evicted(&mut evicted);
        for face in evicted.drain(..) {
            if !self.faces.contains(face) {
                self.drop_face(face);
            }
        }
        self.evicted_faces = evicted;
    }

    /// Drop exactly what is keyed by `face`: its bytes and fallback plans, the
    /// worker's copy of its bytes with its coverage sets and the spans shaped
    /// with it, its glyph residency and placements, and its color-raster
    /// binding. Every other face keeps all of its state. The dropped glyphs'
    /// texels stay on their pages until the page is reclaimed.
    fn drop_face(&mut self, face: FontFaceId) {
        self.fallback.forget(face);
        if self.worker_faces.remove(&face) {
            self.outbox.push(ToWorker::Forget(face));
        }
        self.fate.retain(|key, _| key.face != face);
        self.residency.forget_face(face);
        self.coverage_uv.retain(|key, _| key.face != face);
        self.color_uv.retain(|key, _| key.face != face);
        self.color_raster.forget_face(face);
    }

    /// Answer an OS memory warning: reclaim every page of every glyph pool,
    /// drop the worker's span cache, shed every face no live scope holds, and
    /// hand the atlas planes to `retire` so the caller can release them with
    /// their bindings. Placements die with their pages, so every retained text
    /// payload is stale afterwards — the caller reshapes the mounted text,
    /// which re-admits exactly the live working set into freshly created
    /// planes. The last good layouts are kept: they hold no pixels, and
    /// keeping them makes that reshape a re-raster only. Returns the plane
    /// bytes retired.
    pub(crate) fn trim(&mut self, mut retire: impl FnMut(TextureId)) -> usize {
        for kind in [
            GlyphImageKind::MaskA8,
            GlyphImageKind::ScalableMtsdf,
            GlyphImageKind::ColorRgba8,
            GlyphImageKind::OutlineVector,
        ] {
            self.residency.shed_pool_to_pressure(kind, 0);
        }
        self.drain_reclaims();
        self.faces.shed_to_pressure_budget(0);
        self.faces.restore_budget();
        self.drop_evicted_faces();
        let plane = (self.atlas_size as usize) * (self.atlas_size as usize);
        let mut bytes = 0;
        if let Some(atlas) = self.coverage_atlas.take() {
            retire(atlas.texture());
            bytes += plane;
        }
        if let Some(atlas) = self.color_atlas.take() {
            retire(atlas.texture());
            bytes += plane * COLOR_BYTES_PER_TEXEL;
        }
        self.coverage_uv = HashMap::new();
        self.color_uv = HashMap::new();
        self.fate = HashMap::new();
        self.outbox.push(ToWorker::Clear);
        bytes
    }

    /// Drop the slots of every node `live` rejects — nodes freed, or whose
    /// index now names a newer node — with the worker's paragraphs for them.
    pub(crate) fn retain_paragraphs(&mut self, mut live: impl FnMut(ParagraphSlot) -> bool) {
        let faces = &mut self.faces;
        let outbox = &mut self.outbox;
        self.slots.retain(|slot, entry| {
            let keep = live(*slot);
            if !keep {
                for committed in [&entry.drawn, &entry.staged].into_iter().flatten() {
                    for &face in &committed.faces {
                        faces.unpin(face);
                    }
                }
                outbox.push(ToWorker::DropSlot(slot.key()));
            }
            keep
        });
    }

    /// The wrap width `slot`'s text is laid out to, if it wraps.
    ///
    /// Reshaping edited text at this width keeps the last good line structure
    /// on screen until layout decides whether the box itself changed, instead
    /// of flashing the text unwrapped for a frame.
    pub(crate) fn wrap_width(&self, slot: ParagraphSlot) -> Option<f32> {
        self.slots.get(&slot).and_then(|entry| entry.wrap)
    }

    /// The caret box at `position` in `slot`'s text, relative to the run's
    /// origin: a zero-width rect at the caret's inline position spanning its
    /// line. Reads the lines the text is drawn with, so asking costs no
    /// layout; `None` until the text drawn is `request`'s.
    pub(crate) fn caret(
        &self,
        slot: ParagraphSlot,
        request: &TextRequest,
        position: TextPosition,
    ) -> Option<Rect> {
        let drawn = self.slots.get(&slot)?.drawn.as_ref()?;
        if *drawn.target.text != *request.text {
            return None;
        }
        let lines = &drawn.lines;
        let row = line_index_at(lines, position);
        let x = lines
            .get(row)
            .and_then(|line: &LineLayout| line.caret_x(position))
            .unwrap_or(0.0);
        let line_height = drawn.placed.line_height;
        Some(Rect {
            x: x * f32::from_bits(drawn.font_size),
            y: row as f32 * line_height,
            w: 0.0,
            h: line_height,
        })
    }

    /// Position every glyph of `lines` in logical pixels: each run starts at
    /// its own inline offset, so BiDi order and fallback splits come from the
    /// paragraph, not from re-walking the text here.
    fn place(&self, base: FontFaceId, lines: &[LineLayout], font_size: f32) -> PreparedLayout {
        let metrics = self
            .resolver
            .face_metrics(base)
            .unwrap_or_else(default_metrics);
        let baseline = metrics.ascender_em * font_size;
        let line_height = metrics.line_height_em.max(1.0) * font_size;
        let mut glyphs = Vec::new();
        let mut natural = Vec2::ZERO;
        for (row, line) in lines.iter().enumerate() {
            let y = baseline + row as f32 * line_height;
            for run in &line.runs {
                let mut pen = run.visual_inline_range.0;
                for glyph in &run.glyphs {
                    glyphs.push(PositionedGlyph {
                        face: run.face,
                        glyph: glyph.glyph_id,
                        cluster: run.logical_range.0.0 + glyph.cluster as usize,
                        origin: [
                            (pen + glyph.x_offset) * font_size,
                            y - glyph.y_offset * font_size,
                        ],
                    });
                    pen += glyph.x_advance;
                }
            }
            natural.x = natural.x.max(line.width * font_size);
        }
        natural.y = lines.len() as f32 * line_height;
        PreparedLayout {
            glyphs,
            natural,
            baseline,
            line_height,
        }
    }

    /// `slot`'s content for `request`: its last good layout, drawn now.
    ///
    /// A request its layout does not match yet is queued for the worker, and
    /// `slot` is reported by [`Self::pump`] once the new layout is committed;
    /// until then the last good one keeps drawing, and a node with none draws
    /// nothing.
    pub(crate) fn shape<B: GpuBackend>(
        &mut self,
        backend: &mut B,
        slot: ParagraphSlot,
        request: &TextRequest,
        dpi_factor: f32,
        max_width: Option<f32>,
    ) -> Content {
        let wrap = request.soft_wrap.then_some(max_width).flatten();
        let Some(base) = self.resolve_primary() else {
            return empty_content(request, max_width);
        };
        self.ensure_worker_face(base);
        let locale = match request.locale.as_deref() {
            Some(locale) => locale,
            None => process_locale(),
        };
        let tailoring = LineBreakTailoring::for_locale(locale);
        // Shaping is in em, so only the wrap width depends on the font size.
        let width_em = match wrap {
            Some(width) if request.font_size > 0.0 => (width / request.font_size).to_bits(),
            _ => 0,
        };
        let mut entry = match self.slots.remove(&slot) {
            Some(entry)
                if entry
                    .target
                    .matches(&request.text, width_em, tailoring, base, locale) =>
            {
                entry
            }
            stale => {
                let target = LayoutTarget {
                    text: Arc::from(request.text.as_str()),
                    width_em,
                    tailoring,
                    base,
                    locale: locale.to_owned(),
                };
                let mut entry = match stale {
                    Some(mut entry) => {
                        entry.target = target;
                        entry
                    }
                    None => TextSlot::new(target),
                };
                if !entry.queued {
                    entry.queued = true;
                    self.pending.push(slot);
                }
                entry
            }
        };
        entry.wrap = wrap;
        entry.font_size = request.font_size;
        entry.ppem = (request.font_size * dpi_factor)
            .round()
            .clamp(1.0, f32::from(u16::MAX)) as u16;
        let size = request.font_size.to_bits();
        for committed in [&mut entry.drawn, &mut entry.staged].into_iter().flatten() {
            if committed.font_size != size {
                committed.placed =
                    self.place(committed.target.base, &committed.lines, request.font_size);
                committed.font_size = size;
            }
        }
        self.try_promote(slot, &mut entry);
        let content = self.render(backend, slot, &entry, request, dpi_factor);
        self.slots.insert(slot, entry);
        content
    }

    /// Draw `entry`'s last good layout. A coverage glyph the atlas does not
    /// hold yet is skipped, and `at` is redrawn once it arrives.
    fn render<B: GpuBackend>(
        &mut self,
        backend: &mut B,
        at: ParagraphSlot,
        entry: &TextSlot,
        request: &TextRequest,
        dpi_factor: f32,
    ) -> Content {
        let wrap = entry.wrap;
        let Some(drawn) = &entry.drawn else {
            return empty_content(request, wrap);
        };
        // Once per draw, not per glyph, and folded into recency once per face
        // at the frame boundary.
        for &face in &drawn.faces {
            self.faces.touch(face);
        }
        let ppem = entry.ppem;
        let layout = &drawn.placed;
        let mut glyphs = Vec::with_capacity(layout.glyphs.len());
        let mut color_glyphs = Vec::new();
        for &glyph in &layout.glyphs {
            if starts_emoji(&drawn.target.text, glyph.cluster) {
                let key = GlyphKey {
                    face: glyph.face,
                    glyph: glyph.glyph,
                    bucket: ppem,
                    kind: GlyphImageKind::ColorRgba8,
                };
                let packed = match self.color_uv.get(&key) {
                    Some(&placement) => {
                        self.residency
                            .touch_page(GlyphImageKind::ColorRgba8, placement.page);
                        Some(placement)
                    }
                    None => self.color_miss(backend, key),
                };
                if let Some(Placement {
                    uv, bearing, size, ..
                }) = packed
                {
                    let inv = 1.0 / dpi_factor;
                    // `origin_px` is the ink bbox's *bottom-left* in y-up strike
                    // px: `bearing[1]` is the bottom of the ink above the baseline
                    // (negative for a glyph that descends). The quad's top edge is
                    // `bearing[1] + size.height` px above the baseline, so in
                    // y-down screen space it sits at `baseline - (bottom + h)` —
                    // the same "top of ink above baseline" the coverage path gets
                    // directly from `top = y_max`.
                    color_glyphs.push(GlyphInstanceData {
                        rect: Rect {
                            x: glyph.origin[0] + bearing[0] * inv,
                            y: glyph.origin[1] - (bearing[1] + size[1] as f32) * inv,
                            w: size[0] as f32 * inv,
                            h: size[1] as f32 * inv,
                        },
                        uv,
                    });
                    continue;
                }
            }

            let key = GlyphKey {
                face: glyph.face,
                glyph: glyph.glyph,
                bucket: ppem,
                kind: GlyphImageKind::MaskA8,
            };
            let packed = match self.coverage_uv.get(&key) {
                Some(&cached) => {
                    self.residency
                        .touch_page(GlyphImageKind::MaskA8, cached.page);
                    Some(cached)
                }
                None => self.coverage_miss(backend, at, key),
            };
            let Some(Placement {
                uv, bearing, size, ..
            }) = packed
            else {
                continue;
            };
            let inv = 1.0 / dpi_factor;
            glyphs.push(GlyphInstanceData {
                rect: Rect {
                    x: glyph.origin[0] + bearing[0] * inv,
                    y: glyph.origin[1] - bearing[1] * inv,
                    w: size[0] as f32 * inv,
                    h: size[1] as f32 * inv,
                },
                uv,
            });
        }

        let atlas = self
            .coverage_atlas
            .as_ref()
            .map_or(TextureId::new(0), GlyphAtlas::texture);
        self.upload_dirty(backend);
        Content::Text {
            glyphs,
            atlas,
            color_glyphs,
            color_atlas: self.color_atlas.as_ref().map(ColorAtlas::texture),
            color: request.color,
            natural: layout.natural,
            baseline: layout.baseline,
            shaped_at_width: wrap,
            soft_wrap: request.soft_wrap,
        }
    }

    /// A color glyph the atlas does not hold: painted from the face's own
    /// color tables when it carries them, otherwise by the platform raster,
    /// and remembered as blank when neither draws it so the glyph falls to
    /// coverage without asking again.
    fn color_miss<B: GpuBackend>(&mut self, backend: &mut B, key: GlyphKey) -> Option<Placement> {
        if matches!(self.fate.get(&key), Some(Fate::Blank | Fate::Refused)) {
            return None;
        }
        let portable = self
            .resolver
            .face_bytes(key.face)
            .or_else(|| self.fallback.face_bytes(key.face))
            .and_then(|(bytes, index)| rasterize_color(bytes, index, key.glyph, key.bucket));
        let color = portable.or_else(|| {
            self.color_raster
                .rasterize_color_glyph(key.face, key.glyph, key.bucket)
        });
        let Some(color) = color else {
            self.fate.insert(key, Fate::Blank);
            return None;
        };
        let placed = self.admit_color(backend, key, &color);
        if placed.is_none() {
            self.fate.insert(key, Fate::Refused);
        }
        placed
    }

    /// A coverage glyph the atlas does not hold: rasterized here when only
    /// the platform can, otherwise asked of the worker, with `at` redrawn when
    /// it arrives.
    fn coverage_miss<B: GpuBackend>(
        &mut self,
        backend: &mut B,
        at: ParagraphSlot,
        key: GlyphKey,
    ) -> Option<Placement> {
        match self.fate.get(&key) {
            Some(Fate::Platform) => self.platform_coverage(backend, key),
            Some(Fate::Blank | Fate::Refused) => None,
            Some(Fate::Requested) => {
                self.wait_for_raster(at);
                None
            }
            None => {
                self.request_raster(key);
                self.wait_for_raster(at);
                None
            }
        }
    }

    /// Recover a glyph the parser reads no outline for (Apple's proprietary
    /// `hvgl`, e.g. PingFang, or a CFF2 variable outline) as grayscale A8
    /// through CoreText, the same path emoji uses for color.
    fn platform_coverage<B: GpuBackend>(
        &mut self,
        backend: &mut B,
        key: GlyphKey,
    ) -> Option<Placement> {
        let bitmap = self
            .color_raster
            .rasterize_coverage_glyph(key.face, key.glyph, key.bucket)
            .filter(|bitmap| !bitmap.is_empty());
        let Some(bitmap) = bitmap else {
            self.fate.insert(key, Fate::Blank);
            return None;
        };
        let placed = self.admit_coverage(backend, key, &bitmap);
        if placed.is_none() {
            self.fate.insert(key, Fate::Refused);
        }
        placed
    }

    fn request_raster(&mut self, key: GlyphKey) {
        self.fate.insert(key, Fate::Requested);
        self.raster_wanted.push(key);
    }

    fn wait_for_raster(&mut self, at: ParagraphSlot) {
        if !self.raster_waiters.contains(&at) {
            self.raster_waiters.push(at);
        }
    }

    /// Replace `entry`'s drawn layout with its staged one if every glyph the
    /// staged one draws can draw now, asking the worker for the coverage of
    /// those that cannot. `true` when it did.
    fn try_promote(&mut self, at: ParagraphSlot, entry: &mut TextSlot) -> bool {
        let Some(staged) = &entry.staged else {
            return false;
        };
        let mut ready = true;
        for glyph in &staged.placed.glyphs {
            // Color glyphs rasterize on the main thread as they draw.
            if starts_emoji(&staged.target.text, glyph.cluster) {
                continue;
            }
            let key = GlyphKey {
                face: glyph.face,
                glyph: glyph.glyph,
                bucket: entry.ppem,
                kind: GlyphImageKind::MaskA8,
            };
            if self.coverage_uv.contains_key(&key) {
                continue;
            }
            match self.fate.get(&key) {
                Some(Fate::Platform | Fate::Blank | Fate::Refused) => {}
                Some(Fate::Requested) => ready = false,
                None => {
                    self.request_raster(key);
                    ready = false;
                }
            }
        }
        if !ready {
            self.wait_for_raster(at);
            return false;
        }
        let staged = entry.staged.take();
        if let Some(old) = std::mem::replace(&mut entry.drawn, staged) {
            self.unpin(&old.faces);
        }
        true
    }

    fn unpin(&mut self, faces: &[FontFaceId]) {
        for &face in faces {
            self.faces.unpin(face);
        }
    }

    /// Hand `face`'s bytes to the worker once, ahead of any job that uses it.
    /// `false` when the face has no bytes to hand over.
    fn ensure_worker_face(&mut self, face: FontFaceId) -> bool {
        if self.worker_faces.contains(&face) {
            return true;
        }
        let data = self
            .resolver
            .face_data(face)
            .or_else(|| self.fallback.face_data(face));
        let Some(data) = data else {
            return false;
        };
        self.worker_faces.insert(face);
        self.outbox.push(ToWorker::Face { face, data });
        true
    }

    /// Send every queued layout and coverage request to the worker in one
    /// batch.
    pub(crate) fn dispatch(&mut self) {
        let mut pending = std::mem::take(&mut self.pending);
        for at in pending.drain(..) {
            let Some(entry) = self.slots.get_mut(&at) else {
                continue;
            };
            if !std::mem::take(&mut entry.queued) {
                continue;
            }
            self.next_seq += 1;
            let seq = self.next_seq;
            entry.laying.push((seq, entry.target.clone()));
            let target = &entry.target;
            self.outbox.push(ToWorker::Layout(Box::new(LayoutJob {
                slot: at.key(),
                seq,
                text: target.text.clone(),
                width_em: f32::from_bits(target.width_em),
                tailoring: target.tailoring,
                base: target.base,
                locale: target.locale.clone(),
                full: std::mem::take(&mut entry.full),
                ppem: entry.ppem,
                // A node with nothing drawn yet shows a hole until it lands.
                priority: if entry.drawn.is_none() {
                    Priority::CriticalVisible
                } else {
                    Priority::InteractiveEdit
                },
            })));
            self.in_flight += 1;
        }
        self.pending = pending;
        if !self.raster_wanted.is_empty() {
            self.next_raster += 1;
            self.outbox.push(ToWorker::Raster {
                id: self.next_raster,
                keys: std::mem::take(&mut self.raster_wanted),
            });
            self.in_flight += 1;
        }
        if !self.outbox.is_empty() {
            self.worker.send(std::mem::take(&mut self.outbox));
        }
    }

    /// Whether the worker owes results or anything waits to be sent or
    /// committed.
    pub(crate) fn has_pending_work(&self) -> bool {
        self.in_flight > 0
            || !self.ready.is_empty()
            || !self.pending.is_empty()
            || !self.outbox.is_empty()
            || !self.raster_wanted.is_empty()
    }

    /// Commit the worker's results, oldest first, until `budget` is spent —
    /// at least one per call, so a backlog always drains — and push every slot
    /// whose drawn content changed onto `updated`, for the caller to redraw.
    /// What the budget leaves waits for the next frame.
    pub(crate) fn pump<B: GpuBackend>(
        &mut self,
        backend: &mut B,
        budget: Duration,
        updated: &mut Vec<ParagraphSlot>,
    ) {
        self.dispatch();
        loop {
            match self.worker.try_recv() {
                Ok(message) => self.receive(message),
                Err(TryRecvError::Empty) => break,
                // The worker died: nothing more will reply.
                Err(TryRecvError::Disconnected) => {
                    self.in_flight = 0;
                    break;
                }
            }
        }
        if self.ready.is_empty() {
            return;
        }
        let start = Instant::now();
        let mut commits = 0;
        while let Some(message) = self.ready.pop_front() {
            self.commit(backend, message, updated);
            commits += 1;
            if start.elapsed() >= budget {
                break;
            }
        }
        self.counters
            .record_commits(commits, self.ready.len(), start.elapsed());
        self.upload_dirty(backend);
        self.dispatch();
    }

    fn receive(&mut self, message: FromWorker) {
        match message {
            FromWorker::Cancelled(jobs) => self.in_flight = self.in_flight.saturating_sub(jobs),
            #[cfg(test)]
            FromWorker::Flushed => {}
            message => {
                self.in_flight = self.in_flight.saturating_sub(1);
                self.ready.push_back(message);
            }
        }
    }

    /// Block until the worker ran every job sent so far, keeping its results
    /// for [`Self::pump`].
    #[cfg(test)]
    fn wait_idle(&mut self) {
        self.dispatch();
        self.worker.send(vec![ToWorker::Flush]);
        while let Some(message) = self.worker.recv() {
            if matches!(message, FromWorker::Flushed) {
                break;
            }
            self.receive(message);
        }
    }

    fn commit<B: GpuBackend>(
        &mut self,
        backend: &mut B,
        message: FromWorker,
        updated: &mut Vec<ParagraphSlot>,
    ) {
        match message {
            FromWorker::Laid(done) => self.commit_layout(backend, *done, updated),
            FromWorker::Rastered(glyphs) => {
                self.admit_rastered(backend, glyphs);
                let mut waiters = std::mem::take(&mut self.raster_waiters);
                for at in waiters.drain(..) {
                    let Some(mut entry) = self.slots.remove(&at) else {
                        continue;
                    };
                    self.try_promote(at, &mut entry);
                    self.slots.insert(at, entry);
                    push_once(updated, at);
                }
                waiters.append(&mut self.raster_waiters);
                self.raster_waiters = waiters;
            }
            FromWorker::Cancelled(_) => {}
            #[cfg(test)]
            FromWorker::Flushed => {}
        }
    }

    /// Commit one finished layout: admit the coverage that came with it,
    /// resolve the fallback faces it asked for, and stage its lines — or lay
    /// the paragraph out again when it could not draw every cluster.
    fn commit_layout<B: GpuBackend>(
        &mut self,
        backend: &mut B,
        done: LayoutDone,
        updated: &mut Vec<ParagraphSlot>,
    ) {
        self.admit_rastered(backend, done.glyphs);
        let needs = !done.needs.is_empty();
        for need in done.needs {
            let face = match self
                .fallback
                .plan_run(&need.key, &need.cluster, &self.provider)
            {
                FallbackPlan::Mapped { face, .. } => {
                    self.use_fallback_face(face, FontFallback::is_cjk(need.key.script));
                    if self.ensure_worker_face(face) {
                        self.outbox.push(ToWorker::Route {
                            key: need.key.clone(),
                            face,
                        });
                        face
                    } else {
                        self.fallback.decline_not_resident(face);
                        need.key.base
                    }
                }
                // Nothing covers it: it draws with the base face's notdef.
                FallbackPlan::Unresolved => need.key.base,
            };
            self.outbox.push(ToWorker::Assign {
                cluster: ClusterKey {
                    base: need.key.base,
                    locale: need.key.locale,
                    cluster: need.cluster,
                },
                face,
            });
        }
        let at = ParagraphSlot::from_key(done.slot);
        let Some(mut entry) = self.slots.remove(&at) else {
            return;
        };
        self.counters
            .record_shaped_runs(done.shaped_runs, done.thread == thread::current().id());
        self.span_stats = done.spans;
        if let Some(ledger) = done.span_ledger {
            self.span_ledger = ledger;
        }
        self.layout_coverage_parses = done.coverage_parses;
        if let Some(ledger) = done.coverage_ledger {
            self.layout_coverage_ledger = ledger;
        }
        entry.shape_calls = done.shape_calls;
        // Replies arrive in order, so every older layout still listed was
        // superseded on the worker before it ran.
        let target = entry
            .laying
            .iter()
            .position(|&(seq, _)| seq == done.seq)
            .and_then(|i| entry.laying.drain(..=i).next_back())
            .map(|(_, target)| target);
        if let Some(target) = target {
            // A face the lines draw with was dropped since they were laid out.
            let dropped = done
                .lines
                .iter()
                .flat_map(|line| &line.runs)
                .any(|run| !self.worker_faces.contains(&run.face));
            if needs || dropped {
                entry.full = true;
                if !entry.queued {
                    entry.queued = true;
                    self.pending.push(at);
                }
            } else if done.seq > entry.committed_seq {
                entry.committed_seq = done.seq;
                if done.relaid {
                    self.counters.record_shape(target.width_em != 0);
                }
                let staged = self.stage(target, done.lines, entry.font_size);
                if let Some(old) = entry.staged.replace(staged) {
                    self.unpin(&old.faces);
                }
                if self.try_promote(at, &mut entry) {
                    push_once(updated, at);
                }
            }
        }
        self.slots.insert(at, entry);
    }

    /// Place `lines` at `font_size`, pinning every face they draw with. The
    /// pins are taken before the layout they replace releases its own, so a
    /// face both use is never unpinned in between.
    fn stage(&mut self, target: LayoutTarget, lines: Vec<LineLayout>, font_size: f32) -> Committed {
        let mut faces = Vec::new();
        for run in lines.iter().flat_map(|line| &line.runs) {
            if !faces.contains(&run.face) {
                faces.push(run.face);
            }
        }
        for &face in &faces {
            self.faces.pin(face);
        }
        let placed = self.place(target.base, &lines, font_size);
        Committed {
            target,
            lines,
            font_size: font_size.to_bits(),
            placed,
            faces,
        }
    }

    /// Admit the coverage the worker rasterized. A glyph it has no outline
    /// for is left to the platform raster.
    fn admit_rastered<B: GpuBackend>(&mut self, backend: &mut B, glyphs: Vec<Rastered>) {
        for (key, bitmap) in glyphs {
            // Its face was dropped while the raster was in flight.
            if !self.worker_faces.contains(&key.face) || self.coverage_uv.contains_key(&key) {
                self.fate.remove(&key);
                continue;
            }
            match bitmap {
                Some(bitmap) if !bitmap.is_empty() => {
                    if self.admit_coverage(backend, key, &bitmap).is_some() {
                        self.fate.remove(&key);
                    } else {
                        self.fate.insert(key, Fate::Refused);
                    }
                }
                _ => {
                    self.fate.insert(key, Fate::Platform);
                }
            }
        }
    }

    fn resolve_primary(&mut self) -> Option<FontFaceId> {
        if self.primary.is_none() {
            self.primary_queries += 1;
            let request = FontRequest::role(FontRole::Ui);
            if let Resolved::Face(face) =
                self.resolver
                    .resolve(&request, &self.manifest, &self.provider, "")
            {
                self.primary = Some(face);
                self.register_color_face(face);
                let cost = self
                    .resolver
                    .face_bytes(face)
                    .map_or(0, |(bytes, _)| bytes.len() as u64);
                self.pin_hot(face, cost);
            }
        }
        self.primary
    }

    fn register_color_face(&self, face: FontFaceId) {
        if let Some(metrics) = self.resolver.face_metrics(face)
            && let Some(name) = metrics.postscript_name
        {
            self.color_raster
                .register_face(face, &name, metrics.glyph_count);
        }
    }

    #[cfg(test)]
    fn face_bytes(&self, face: FontFaceId) -> Option<(&[u8], u32)> {
        self.resolver
            .face_bytes(face)
            .or_else(|| self.fallback.face_bytes(face))
    }

    /// Account one use of a resolved fallback face: a touch when resident,
    /// otherwise an admission charged its bytes and coverage — pinned for the
    /// process when it serves a CJK script.
    fn use_fallback_face(&mut self, face: FontFaceId, cjk: bool) {
        if self.faces.contains(face) {
            self.faces.touch(face);
            return;
        }
        self.register_fallback_color_face(face);
        let cost = self.fallback.face_cost(face).unwrap_or(0);
        if cjk {
            self.pin_hot(face, cost);
        } else {
            self.faces.admit(face, cost);
        }
    }

    /// Register a resolved fallback face with the CoreText raster so its color /
    /// proprietary-outline (`hvgl`) glyphs can be re-opened by name.
    ///
    /// The glyph count comes from the reassembled sfnt (`maxp` parses fine), but
    /// the re-open name must be the *platform*-reported PostScript name: an
    /// AppleColorEmoji / PingFang sfnt reassembled from CoreText tables keeps only
    /// Macintosh-platform `name` records, and `ttf-parser` returns `None` for the
    /// PostScript name of those — so reading it from the bytes leaves the color
    /// face unregistered and every emoji falls through to the monochrome A8 path
    /// (a tofu-like blob). Take the name the provider captured from CoreText.
    fn register_fallback_color_face(&self, face: FontFaceId) {
        let Some((bytes, index)) = self.fallback.face_bytes(face) else {
            return;
        };
        let Some(metrics) = inspect_face(bytes, index) else {
            return;
        };
        if let Some(name) = self.fallback.face_postscript_name(face) {
            self.color_raster
                .register_face(face, name, metrics.glyph_count);
        }
    }

    /// Admit a freshly rasterized coverage bitmap and pack its pixels onto the
    /// page residency chose, re-aiming if the packer refuses that page.
    ///
    /// `None` when the glyph cannot be placed at all — larger than a page, empty,
    /// or it exhausted its re-aims this frame. Nothing is cleared on the way:
    /// filling the pool reclaims the coldest page and re-admits into it.
    fn admit_coverage<B: GpuBackend>(
        &mut self,
        backend: &mut B,
        key: GlyphKey,
        bitmap: &CoverageBitmap,
    ) -> Option<Placement> {
        let bytes = bitmap.coverage.len();
        for _ in 0..PLACEMENT_RETRIES {
            let (page, fresh) = self.page_for(key, bytes);
            let atlas = ensure_coverage_atlas(
                &mut self.coverage_atlas,
                backend,
                self.atlas_size,
                self.atlas_page,
            );
            match atlas.alloc_in_page(page, bitmap) {
                AtlasAlloc::Placed(uv) => {
                    let placement = Placement {
                        uv,
                        bearing: [bitmap.left, bitmap.top],
                        size: [bitmap.width, bitmap.height],
                        page,
                    };
                    self.coverage_uv.insert(key, placement);
                    self.counters.record_raster();
                    return Some(placement);
                }
                AtlasAlloc::PageFull if fresh => self.revoke(key, page, bytes),
                AtlasAlloc::PageFull | AtlasAlloc::Empty | AtlasAlloc::TooLarge => return None,
            }
        }
        None
    }

    /// The color-glyph twin of [`Self::admit_coverage`], against the RGBA pool and
    /// the color plane.
    fn admit_color<B: GpuBackend>(
        &mut self,
        backend: &mut B,
        key: GlyphKey,
        color: &ColorGlyph,
    ) -> Option<Placement> {
        let bytes = color.rgba.len();
        for _ in 0..PLACEMENT_RETRIES {
            let (page, fresh) = self.page_for(key, bytes);
            let atlas = ensure_color_atlas(
                &mut self.color_atlas,
                backend,
                self.atlas_size,
                self.atlas_page,
            );
            match atlas.alloc_in_page(page, color) {
                ColorAlloc::Placed(uv) => {
                    let placement = Placement {
                        uv,
                        bearing: color.origin_px,
                        size: [color.width, color.height],
                        page,
                    };
                    self.color_uv.insert(key, placement);
                    self.counters.record_raster();
                    return Some(placement);
                }
                ColorAlloc::PageFull if fresh => self.revoke(key, page, bytes),
                ColorAlloc::PageFull | ColorAlloc::Empty | ColorAlloc::TooLarge => return None,
            }
        }
        None
    }

    /// Ask residency which page a glyph belongs on, then reopen every page the
    /// decision reclaimed before anything is packed into it.
    ///
    /// The flag says whether this call created the admission; only then may a
    /// refused placement be revoked (revoking a placement someone else owns would
    /// corrupt the byte accounting).
    fn page_for(&mut self, key: GlyphKey, bytes: usize) -> (usize, bool) {
        let admission = self.residency.get_or_admit(key, bytes);
        self.drain_reclaims();
        match admission {
            Admission::Admitted { page, .. } => (page, true),
            Admission::Cached { page, .. } => (page, false),
        }
    }

    /// Apply the reclaims residency has reported: reopen each named page for
    /// packing and drop the placements that pointed into it — that page and
    /// nothing else. Every other page keeps its pixels and its UVs, so no reclaim
    /// can cascade into a cache-wide clear.
    fn drain_reclaims(&mut self) {
        let mut reclaims = std::mem::take(&mut self.reclaims);
        self.residency.take_reclaims(&mut reclaims);
        // A reclaimed page is room a glyph the atlas refused may fit into.
        if !reclaims.is_empty() {
            self.fate.retain(|_, fate| *fate != Fate::Refused);
        }
        for reclaimed in reclaims.drain(..) {
            self.counters.record_eviction();
            match reclaimed.kind {
                GlyphImageKind::MaskA8 => {
                    if let Some(atlas) = self.coverage_atlas.as_mut() {
                        atlas.reset_page(reclaimed.page);
                    }
                    self.coverage_uv
                        .retain(|_, placement| placement.page != reclaimed.page);
                }
                GlyphImageKind::ColorRgba8 => {
                    if let Some(atlas) = self.color_atlas.as_mut() {
                        atlas.reset_page(reclaimed.page);
                    }
                    self.color_uv
                        .retain(|_, placement| placement.page != reclaimed.page);
                }
                // The MTSDF and vector pools hold no plane yet, so they own no
                // pixels to reopen.
                GlyphImageKind::ScalableMtsdf
                | GlyphImageKind::OutlineVector
                | GlyphImageKind::ColorVector => {}
            }
        }
        self.reclaims = reclaims;
    }

    /// Undo an admission the packer refused, counting the failure.
    fn revoke(&mut self, key: GlyphKey, page: usize, bytes: usize) {
        self.residency.revoke(key, page, bytes);
        self.counters.record_admission_failure();
    }

    fn upload_dirty<B: GpuBackend>(&mut self, backend: &mut B) {
        if let Some(atlas) = self.coverage_atlas.as_mut()
            && let Some((x, y, width, height, bytes)) = atlas.take_dirty()
        {
            self.counters.record_upload(bytes.len());
            backend.write_texture(atlas.texture(), x, y, width, height, &bytes);
        }
        if let Some(atlas) = self.color_atlas.as_mut()
            && let Some((x, y, width, height, bytes)) = atlas.take_dirty()
        {
            self.counters.record_upload(bytes.len());
            backend.write_texture(atlas.texture(), x, y, width, height, &bytes);
        }
    }
}

/// A text control's drawn geometry is its last good layout: the lines, the
/// size they were placed at, and their line height, all in the node's
/// logical-pixel space. The text is the one those lines were laid out from,
/// so an edit the worker has not laid out yet is not reconciled against them.
impl EditGeometry for TextShaper {
    fn layout(&self, node: NodeId) -> Option<EditLayout<'_>> {
        let drawn = self.slots.get(&ParagraphSlot::from(node))?.drawn.as_ref()?;
        Some(EditLayout {
            text: &drawn.target.text,
            lines: &drawn.lines,
            font_size: f32::from_bits(drawn.font_size),
            line_height: drawn.placed.line_height,
        })
    }
}

fn ensure_coverage_atlas<'a, B: GpuBackend>(
    atlas: &'a mut Option<GlyphAtlas>,
    backend: &mut B,
    size: u32,
    page: u32,
) -> &'a mut GlyphAtlas {
    atlas.get_or_insert_with(|| {
        let texture = backend.create_texture(&TextureDesc {
            width: size,
            height: size,
            format: GlyphAtlas::FORMAT,
            render_target: false,
            label: "ui-glyph-coverage",
        });
        GlyphAtlas::new(size, page, texture)
    })
}

fn ensure_color_atlas<'a, B: GpuBackend>(
    atlas: &'a mut Option<ColorAtlas>,
    backend: &mut B,
    size: u32,
    page: u32,
) -> &'a mut ColorAtlas {
    atlas.get_or_insert_with(|| {
        let texture = backend.create_texture(&TextureDesc {
            width: size,
            height: size,
            format: ColorAtlas::FORMAT,
            render_target: false,
            label: "ui-glyph-color",
        });
        ColorAtlas::new(size, page, texture)
    })
}

/// The process content locale as a BCP-47 tag, from the POSIX locale
/// variables (`zh_CN.UTF-8` → `zh-CN`); empty when none is set or it is the
/// `C`/`POSIX` locale. Read once: the locale of a running process is fixed.
fn process_locale() -> &'static str {
    static LOCALE: OnceLock<String> = OnceLock::new();
    LOCALE.get_or_init(|| {
        ["LC_ALL", "LC_CTYPE", "LANG"]
            .into_iter()
            .find_map(|name| std::env::var(name).ok().filter(|v| !v.is_empty()))
            .map_or_else(String::new, |value| posix_to_bcp47(&value))
    })
}

fn posix_to_bcp47(value: &str) -> String {
    let tag = value.split(['.', '@']).next().unwrap_or_default();
    if tag == "C" || tag == "POSIX" {
        return String::new();
    }
    tag.replace('_', "-")
}

fn empty_content(request: &TextRequest, shaped_at_width: Option<f32>) -> Content {
    Content::Text {
        glyphs: Vec::new(),
        atlas: TextureId::new(0),
        color_glyphs: Vec::new(),
        color_atlas: None,
        color: request.color,
        natural: Vec2::ZERO,
        baseline: 0.0,
        shaped_at_width,
        soft_wrap: request.soft_wrap,
    }
}

fn default_metrics() -> viso_text::FaceMetrics {
    viso_text::FaceMetrics {
        ascender_em: 0.8,
        descender_em: -0.2,
        line_height_em: 1.2,
        units_per_em: 1000,
        glyph_count: 0,
        postscript_name: None,
    }
}

fn push_once(slots: &mut Vec<ParagraphSlot>, slot: ParagraphSlot) {
    if !slots.contains(&slot) {
        slots.push(slot);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text_worker::{FaceData, WorkerState};
    use viso_gpu::{HeadlessRaster, RawWindowHandle};
    use viso_render::Rgba;
    use viso_text::fallback::{FallbackPlanKey, FallbackStyle};
    use viso_text::{TextOffset, rasterize_coverage};

    const TEST_FONT: &[u8] = include_bytes!("../fixtures/DejaVuSans-subset.ttf");
    const WHITE: Rgba = Rgba {
        r: 1.0,
        g: 1.0,
        b: 1.0,
        a: 1.0,
    };

    fn shaper() -> TextShaper {
        let mut shaper = TextShaper::new();
        shaper.load_font(TEST_FONT, 0).expect("fixture parses");
        shaper
    }

    /// A deliberately tiny plane for the residency tests: 96 × 96 texels cut into
    /// nine 32-texel pages. At the sizes used below one padded glyph fills a page,
    /// so every distinct glyph owns a page and page turnover is reachable within a
    /// handful of frames — the same code path a 1024 × 1024 plane reaches after a
    /// few thousand.
    const TINY_PLANE: u32 = 96;
    const TINY_PAGE: u32 = 32;
    /// Two ascenders: at ~30px each is padded past half a page on both axes, so no
    /// two of them ever share a page.
    const PAIR: &str = "bd";

    fn slot(index: u32) -> ParagraphSlot {
        ParagraphSlot {
            index,
            generation: 0,
        }
    }

    fn tiny_shaper() -> TextShaper {
        let mut shaper = TextShaper::with_atlas_geometry(
            TINY_PLANE,
            TINY_PAGE,
            MemoryClass::for_target().text_budgets(),
        );
        shaper.load_font(TEST_FONT, 0).expect("fixture parses");
        shaper
    }

    #[test]
    fn the_caret_walks_the_pen_positions_of_a_shaped_run() {
        let mut shaper = tiny_shaper();
        let request = TextRequest {
            text: PAIR.to_owned(),
            font_size: 30.0,
            color: Rgba::TRANSPARENT,
            soft_wrap: false,
            locale: None,
        };
        assert!(
            shaper
                .caret(slot(0), &request, TextPosition::downstream(TextOffset(0)))
                .is_none(),
            "nothing is drawn yet"
        );
        settle(&mut shaper, &mut headless(), slot(0), &request, None);
        let first = shaper
            .caret(slot(0), &request, TextPosition::downstream(TextOffset(0)))
            .expect("drawn");
        let middle = shaper
            .caret(slot(0), &request, TextPosition::downstream(TextOffset(1)))
            .expect("drawn");
        let end = shaper
            .caret(slot(0), &request, TextPosition::downstream(TextOffset(2)))
            .expect("drawn");
        assert_eq!(first.x, 0.0);
        assert!(first.x < middle.x && middle.x < end.x);
        assert_eq!((first.y, first.w), (0.0, 0.0));
        assert!(first.h >= 30.0, "the caret spans the line, not the ink");
        assert_eq!(
            shaper.counters().reshapes(),
            1,
            "every query reads one layout"
        );
    }

    #[test]
    fn edit_geometry_is_the_placed_paragraph_and_resolves_a_click() {
        let mut shaper = tiny_shaper();
        let request = TextRequest {
            text: PAIR.to_owned(),
            font_size: 30.0,
            color: Rgba::TRANSPARENT,
            soft_wrap: false,
            locale: None,
        };
        let node = viso_ui::NodeArena::new().alloc();
        let at = ParagraphSlot::from(node);
        let mut gpu = headless();
        shaper.shape(&mut gpu, at, &request, 1.0, None);
        assert!(shaper.layout(node).is_none(), "nothing drawn yet");
        settle(&mut shaper, &mut gpu, at, &request, None);
        let middle = shaper
            .caret(at, &request, TextPosition::downstream(TextOffset(1)))
            .expect("drawn");
        let layout = shaper.layout(node).expect("drawn");
        assert_eq!(layout.text, PAIR);
        assert_eq!(layout.font_size, 30.0);
        assert_eq!(layout.line_height, middle.h);
        // The stop the caret query drew at offset 1 is where a click resolves.
        let line = &layout.lines[0];
        let hit = viso_text::hit_test::HitTester::new(line)
            .position_at_inline((middle.x + 0.5) / layout.font_size);
        assert_eq!(hit.offset, TextOffset(1));
    }

    /// A platform stand-in that answers every fallback query with the fixture
    /// face, padded so each answer interns as a distinct face.
    struct FixtureFaces {
        answered: Cell<usize>,
    }

    impl viso_text::SystemFontProvider for FixtureFaces {
        fn resolve_system_face(
            &self,
            _query: &viso_text::SystemFontQuery,
        ) -> Option<viso_text::SystemFontResult> {
            let answered = self.answered.get() + 1;
            self.answered.set(answered);
            let mut bytes = TEST_FONT.to_vec();
            bytes.resize(bytes.len() + answered, 0);
            Some(viso_text::SystemFontResult {
                bytes,
                index: 0,
                postscript_name: None,
            })
        }
    }

    /// Resolve a fresh fallback face for "b" through `faces` and account its use
    /// the way shaping does.
    fn fallback_face(shaper: &mut TextShaper, faces: &FixtureFaces, cjk: bool) -> FontFaceId {
        let key = FallbackPlanKey {
            base: FontFaceId(u32::MAX),
            script: FontFallback::run_script("b"),
            locale: format!("x-{}", faces.answered.get()),
            style: FallbackStyle::default(),
            source_revision: 0,
        };
        let FallbackPlan::Mapped { face, .. } = shaper.fallback.plan_run(&key, "b", faces) else {
            panic!("the fixture covers the run");
        };
        shaper.use_fallback_face(face, cjk);
        face
    }

    fn budgeted_shaper(face_cache_bytes: u64, shaping_cache_bytes: u64) -> TextShaper {
        let budgets = TextBudgets {
            face_cache_bytes,
            shaping_cache_bytes,
        };
        let mut shaper = TextShaper::with_atlas_geometry(TINY_PLANE, TINY_PAGE, budgets);
        shaper.load_font(TEST_FONT, 0).expect("fixture parses");
        shaper
    }

    /// One fallback face's charge: the fixture's bytes plus its coverage set.
    fn fallback_cost() -> u64 {
        let mut shaper = budgeted_shaper(u64::MAX, u64::MAX);
        let faces = FixtureFaces {
            answered: Cell::new(0),
        };
        let face = fallback_face(&mut shaper, &faces, false);
        shaper.fallback.face_cost(face).expect("resident")
    }

    #[test]
    fn the_face_budget_drops_cold_fallback_faces_and_keeps_pinned_ones() {
        let cost = fallback_cost();
        // The pinned app face plus room for about two fallback faces.
        let mut shaper = budgeted_shaper(TEST_FONT.len() as u64 + cost * 5 / 2, u64::MAX);
        let app = shaper.primary.expect("loaded");
        let faces = FixtureFaces {
            answered: Cell::new(0),
        };
        let cjk = fallback_face(&mut shaper, &faces, true);
        shaper.end_frame();
        let mut latin = Vec::new();
        for _ in 0..4 {
            latin.push(fallback_face(&mut shaper, &faces, false));
            shaper.end_frame();
        }
        let cache = shaper.face_cache();
        assert!(cache.evictions() >= 2, "cold faces were evicted");
        assert!(cache.total_bytes() <= cache.budget_bytes() + cost);
        assert!(cache.contains(app) && cache.contains(cjk));
        assert!(shaper.resolver.face_bytes(app).is_some());
        assert!(
            shaper.fallback.face_bytes(cjk).is_some(),
            "CJK stays pinned"
        );
        let newest = *latin.last().expect("admitted");
        assert!(shaper.fallback.face_bytes(newest).is_some());
        for &face in &latin[..2] {
            assert!(!cache.contains(face));
            assert!(shaper.fallback.face_bytes(face).is_none(), "bytes dropped");
            assert!(shaper.fallback.face_cost(face).is_none());
        }
    }

    #[test]
    fn a_face_in_use_this_frame_is_not_dropped_before_the_frame_ends() {
        let cost = fallback_cost();
        let mut shaper = budgeted_shaper(TEST_FONT.len() as u64 + cost / 2, u64::MAX);
        let faces = FixtureFaces {
            answered: Cell::new(0),
        };
        let first = fallback_face(&mut shaper, &faces, false);
        let second = fallback_face(&mut shaper, &faces, false);
        // Both overshoot the budget, but both are in flight this frame.
        assert!(shaper.fallback.face_bytes(first).is_some());
        assert!(shaper.fallback.face_bytes(second).is_some());
        shaper.end_frame();
        shaper.end_frame();
        assert!(shaper.face_cache().total_bytes() <= shaper.face_cache().budget_bytes());
        assert!(shaper.fallback.face_bytes(first).is_none());
    }

    #[test]
    fn dropping_a_face_leaves_every_other_face_intact() {
        let mut shaper = tiny_shaper();
        let mut gpu = headless();
        let app = shaper.primary.expect("loaded");
        let faces = FixtureFaces {
            answered: Cell::new(0),
        };
        let other = fallback_face(&mut shaper, &faces, false);
        let doomed = fallback_face(&mut shaper, &faces, false);
        settle(&mut shaper, &mut gpu, slot(0), &request(PAIR, 30.0), None);
        // The fixture faces are one font padded apart, so glyph ids agree.
        let glyph = shaper.slots[&slot(0)]
            .drawn
            .as_ref()
            .expect("drawn")
            .placed
            .glyphs[0]
            .glyph;
        for face in [other, doomed] {
            assert!(shaper.ensure_worker_face(face));
            let key = GlyphKey {
                face,
                glyph,
                bucket: 30,
                kind: GlyphImageKind::MaskA8,
            };
            let bitmap = rasterize_coverage(TEST_FONT, 0, glyph, 30.0).expect("outline");
            assert!(shaper.admit_coverage(&mut gpu, key, &bitmap).is_some());
        }
        let placements = shaper.coverage_uv.len();
        let glyphs = shaper
            .residency
            .pool_resident_glyphs(GlyphImageKind::MaskA8);

        shaper.drop_face(doomed);

        assert_eq!(shaper.coverage_uv.len(), placements - 1);
        assert_eq!(
            shaper
                .residency
                .pool_resident_glyphs(GlyphImageKind::MaskA8),
            glyphs - 1
        );
        assert!(shaper.coverage_uv.keys().all(|key| key.face != doomed));
        assert!(shaper.fallback.face_bytes(doomed).is_none());
        assert!(
            shaper
                .outbox
                .iter()
                .any(|message| matches!(message, ToWorker::Forget(face) if *face == doomed)),
            "the worker forgets it too"
        );
        for face in [app, other] {
            assert!(shaper.coverage_uv.keys().any(|key| key.face == face));
            assert!(shaper.worker_faces.contains(&face));
        }
        assert!(shaper.fallback.face_bytes(other).is_some());
    }

    #[test]
    fn repeated_drawing_folds_face_recency_once_per_frame() {
        let mut shaper = tiny_shaper();
        let mut gpu = headless();
        settle(&mut shaper, &mut gpu, slot(0), &request(PAIR, 30.0), None);
        shaper.end_frame();
        let before = shaper.face_cache().recency_updates();
        let hits = shaper.face_cache().hits();
        for _ in 0..1000 {
            shaper.shape(&mut gpu, slot(0), &request(PAIR, 30.0), 1.0, None);
        }
        shaper.end_frame();
        assert!(shaper.face_cache().hits() >= hits + 1000);
        assert_eq!(shaper.face_cache().recency_updates(), before + 1);
    }

    #[test]
    fn a_face_budget_turnover_leaves_the_span_cache_alone() {
        let cost = fallback_cost();
        let mut shaper = budgeted_shaper(TEST_FONT.len() as u64 + cost, u64::MAX);
        let mut gpu = headless();
        settle(&mut shaper, &mut gpu, slot(0), &request(PAIR, 30.0), None);
        let faces = FixtureFaces {
            answered: Cell::new(0),
        };
        for _ in 0..3 {
            fallback_face(&mut shaper, &faces, false);
            shaper.end_frame();
        }
        assert!(shaper.face_cache().evictions() > 0);
        // A second paragraph of the same text reads the span the first cached.
        settle(&mut shaper, &mut gpu, slot(1), &request(PAIR, 30.0), None);
        let spans = shaper.shaping_cache();
        assert_eq!(spans.evictions(), 0);
        assert_eq!(spans.len(), 1, "the app face's span stays");
        assert!(spans.hits() > 0);
    }

    #[test]
    fn span_budget_turnover_and_face_drops_are_independent() {
        let mut worker = WorkerState::new(4 * 1024);
        let app = FontFaceId(1);
        let other = FontFaceId(2);
        let data: FaceData = (Arc::from(TEST_FONT), 0);
        worker.add_face(app, data.clone());
        worker.add_face(other, data);
        worker.shape_text(app, PAIR);
        worker.shape_text(other, PAIR);

        // Distinct spans from one face turn the generations over.
        for n in 0..200 {
            worker.shape_text(app, &format!("{PAIR} {n}"));
        }
        let turned = worker.span_stats();
        assert!(turned.evictions() > 0);
        assert!(turned.bytes() <= turned.budget_bytes());

        // A span in use survives turnover by moving to the live generation.
        let misses = worker.span_stats().misses();
        worker.shape_text(app, "keep");
        worker.shape_text(app, "keep");
        assert_eq!(worker.span_stats().misses(), misses + 1);

        // Dropping a face takes its spans, and is no eviction.
        worker.shape_text(other, "keep");
        let before = worker.span_stats();
        worker.drop_face(other);
        let after = worker.span_stats();
        assert_eq!(after.evictions(), before.evictions());
        assert_eq!(after.len(), before.len() - 1);
        worker.shape_text(app, "keep");
        assert_eq!(
            worker.span_stats().misses(),
            after.misses(),
            "the app face's span stays"
        );
    }

    #[cfg(feature = "inspector")]
    #[test]
    fn a_span_miss_names_the_turnover_or_face_drop_that_evicted_it() {
        use viso_text::inspect::{CacheKey, MissCause};

        let mut worker = WorkerState::new(4 * 1024);
        let app = FontFaceId(1);
        let other = FontFaceId(2);
        let data: FaceData = (Arc::from(TEST_FONT), 0);
        worker.add_face(app, data.clone());
        worker.add_face(other, data);
        worker.shape_text(app, PAIR);
        worker.shape_text(other, PAIR);
        let cold = worker.span_ledger().misses().last().expect("a cold miss");
        assert_eq!(cold.cause, MissCause::Cold);
        assert_eq!(cold.budget.budget_bytes, Some(4 * 1024));

        for n in 0..200 {
            worker.shape_text(app, &format!("{PAIR} {n}"));
        }
        worker.shape_text(app, PAIR);
        let miss = worker.span_ledger().misses().last().expect("a miss");
        let CacheKey::Shaping { face, text, .. } = &miss.key else {
            panic!("a shaping key");
        };
        assert_eq!((*face, text.as_str()), (app, PAIR));
        let MissCause::Evicted {
            by: Some(CacheKey::Shaping { text: by, .. }),
        } = &miss.cause
        else {
            panic!("evicted by a turnover: {:?}", miss.cause);
        };
        assert!(by.starts_with(PAIR), "the admission that turned it over");

        worker.shape_text(other, "keep");
        worker.drop_face(other);
        worker.add_face(other, (Arc::from(TEST_FONT), 0));
        worker.shape_text(other, "keep");
        let miss = worker.span_ledger().misses().last().expect("a miss");
        assert_eq!(
            miss.cause,
            MissCause::Evicted {
                by: Some(CacheKey::Face(other))
            }
        );
    }

    #[cfg(feature = "inspector")]
    #[test]
    fn the_worker_s_coverage_builds_reach_the_inspection() {
        use viso_text::inspect::{CacheKey, MissCause};

        let mut gpu = headless();
        let mut shaper = shaper();
        // Devanagari the fixture does not cover: the worker routes the cluster
        // and builds the primary face's coverage set to do it.
        settle(
            &mut shaper,
            &mut gpu,
            slot(0),
            &request("a\u{0915}", 16.0),
            None,
        );
        assert!(shaper.layout_coverage_parses() >= 1);
        let inspection = shaper.inspect();
        let miss = inspection
            .layout_coverage
            .misses()
            .last()
            .expect("a coverage build");
        assert!(matches!(miss.key, CacheKey::Coverage(_)));
        assert_eq!(miss.cause, MissCause::Cold);
    }

    #[cfg(feature = "inspector")]
    #[test]
    fn the_inspection_explains_a_reclaimed_glyph_and_what_it_draws_from() {
        use viso_text::inspect::{CacheKey, MissCause};

        let mut gpu = headless();
        let mut shaper = tiny_shaper();
        let probe = request("q", 26.0);
        settle(&mut shaper, &mut gpu, slot(2), &probe, None);
        settle(&mut shaper, &mut gpu, slot(3), &request("aceo", 26.0), None);
        shaper.end_frame();
        assert!(flood(&mut shaper, &mut gpu) > 0);
        settle(&mut shaper, &mut gpu, slot(2), &probe, None);

        let inspection = shaper.inspect();
        let coverage = &inspection.glyphs[0];
        let miss = coverage
            .misses()
            .iter()
            .rev()
            .find(|miss| matches!(&miss.key, CacheKey::Glyph(key) if key.bucket == 26))
            .expect("the probe missed again");
        let MissCause::Evicted {
            by: Some(CacheKey::Glyph(by)),
        } = &miss.cause
        else {
            panic!("reclaimed for an admission: {:?}", miss.cause);
        };
        assert_ne!(by.bucket, 26, "a flood glyph took its page");
        assert!(miss.budget.budget_bytes.is_some());
        assert!(
            inspection
                .shaping
                .misses()
                .iter()
                .any(|miss| miss.cause == MissCause::Cold),
            "the worker's shaping misses reach the main thread"
        );
        let CacheKey::Glyph(probe_key) = miss.key else {
            unreachable!();
        };
        let resident = inspection
            .resident
            .iter()
            .find(|glyph| glyph.key == probe_key)
            .expect("the probe is resident again");
        assert_eq!(resident.key.kind, GlyphImageKind::MaskA8);
        assert!(resident.generation > 0, "it lives on a reclaimed page");
        assert_eq!(resident.promotion, None);
    }

    #[test]
    fn an_edit_cadence_shapes_nothing_on_the_main_thread() {
        let mut shaper = tiny_shaper();
        let mut gpu = headless();
        let mut text = String::from("tick tock ");
        let first = settle(&mut shaper, &mut gpu, slot(0), &wrapped(&text), Some(120.0));
        let mut last = natural(&first);
        for n in 0..20 {
            text.push(if n % 2 == 0 { 'a' } else { ' ' });
            // The edited text draws the last good layout at once.
            let drawn = shaper.shape(&mut gpu, slot(0), &wrapped(&text), 1.0, Some(120.0));
            assert_eq!(natural(&drawn), last);
            assert!(shaper.has_pending_work());
            drain(&mut shaper, &mut gpu);
            let committed = shaper.shape(&mut gpu, slot(0), &wrapped(&text), 1.0, Some(120.0));
            assert_eq!(drawn_text(&shaper, slot(0)), text);
            last = natural(&committed);
        }
        let counters = shaper.counters();
        assert_eq!(counters.main_shaped_runs(), 0);
        assert!(counters.shaped_runs() > 0);
        assert!(counters.commits() >= 21);
    }

    #[test]
    fn an_over_budget_frame_defers_the_rest_of_the_commits() {
        let mut shaper = tiny_shaper();
        let mut gpu = headless();
        let texts = ["ab", "ba", "aab", "bba"];
        for (n, text) in (0..).zip(texts) {
            shaper.shape(&mut gpu, slot(n), &request(text, 30.0), 1.0, None);
        }
        shaper.wait_idle();
        let mut updated = Vec::new();
        for (frame, left) in [3, 2, 1, 0].into_iter().enumerate() {
            shaper.pump(&mut gpu, Duration::ZERO, &mut updated);
            let counters = shaper.counters();
            assert_eq!(counters.commits(), frame as u64 + 1, "one commit a frame");
            assert_eq!(counters.deferred(), left);
            assert_eq!(updated.len(), frame + 1);
        }
        assert!(!shaper.has_pending_work());
        for (n, text) in (0..).zip(texts) {
            let content = shaper.shape(&mut gpu, slot(n), &request(text, 30.0), 1.0, None);
            assert_eq!(glyph_count(&content), text.len());
        }
    }

    #[test]
    fn deferred_work_completes_the_same_under_any_budget() {
        let texts = [
            "the quick brown fox jumps over the lazy dog",
            "tick tock tick tock",
            PAIR,
        ];
        let run = |budget: Duration| {
            let mut shaper = tiny_shaper();
            let mut gpu = headless();
            for (n, text) in (0..).zip(texts) {
                shaper.shape(&mut gpu, slot(n), &wrapped(text), 1.0, Some(90.0));
            }
            let mut updated = Vec::new();
            let mut frames = 0;
            while shaper.has_pending_work() {
                shaper.wait_idle();
                shaper.pump(&mut gpu, budget, &mut updated);
                frames += 1;
            }
            let drawn: Vec<_> = (0..)
                .zip(texts)
                .map(|(n, text)| {
                    let content = shaper.shape(&mut gpu, slot(n), &wrapped(text), 1.0, Some(90.0));
                    (
                        natural(&content),
                        glyph_count(&content),
                        lines_of(&shaper, slot(n)),
                    )
                })
                .collect();
            (drawn, frames)
        };
        let (slow, slow_frames) = run(Duration::ZERO);
        let (fast, fast_frames) = run(Duration::MAX);
        assert_eq!(slow, fast);
        assert!(slow_frames > fast_frames);
    }

    /// Run the worker until it owes nothing, committing everything it
    /// returns; the slots whose drawn layout changed.
    fn drain(shaper: &mut TextShaper, gpu: &mut HeadlessRaster) -> Vec<ParagraphSlot> {
        let mut updated = Vec::new();
        while shaper.has_pending_work() {
            shaper.wait_idle();
            shaper.pump(gpu, Duration::MAX, &mut updated);
        }
        updated
    }

    /// `request`'s content once the worker laid it out and every glyph it
    /// draws is resident.
    fn settle(
        shaper: &mut TextShaper,
        gpu: &mut HeadlessRaster,
        at: ParagraphSlot,
        request: &TextRequest,
        width: Option<f32>,
    ) -> Content {
        shaper.shape(gpu, at, request, 1.0, width);
        drain(shaper, gpu);
        shaper.shape(gpu, at, request, 1.0, width)
    }

    fn glyph_count(content: &Content) -> usize {
        let Content::Text { glyphs, .. } = content else {
            panic!("text content");
        };
        glyphs.len()
    }

    fn natural(content: &Content) -> Vec2 {
        let Content::Text { natural, .. } = content else {
            panic!("text content");
        };
        *natural
    }

    fn headless() -> HeadlessRaster {
        let mut gpu = HeadlessRaster::new();
        let _ = gpu.create_surface(RawWindowHandle::Headless, 128, 128);
        gpu
    }

    fn request(text: &str, font_size: f32) -> TextRequest {
        TextRequest {
            text: text.into(),
            font_size,
            color: WHITE,
            soft_wrap: false,
            locale: None,
        }
    }

    /// Draw the hot pair plus one never-before-seen resolution bucket per frame,
    /// closing every frame the way the runtime does. Eighteen glyphs are demanded
    /// of nine pages, so the pool must turn over. Returns the pages reclaimed.
    fn flood(shaper: &mut TextShaper, gpu: &mut HeadlessRaster) -> u64 {
        let mut evictions = 0;
        for size in [27.0, 28.0, 29.0, 31.0, 32.0, 33.0, 34.0] {
            shaper.shape(gpu, slot(0), &request(PAIR, 30.0), 1.0, None);
            shaper.shape(gpu, slot(1), &request(PAIR, size), 1.0, None);
            drain(shaper, gpu);
            shaper.shape(gpu, slot(0), &request(PAIR, 30.0), 1.0, None);
            shaper.shape(gpu, slot(1), &request(PAIR, size), 1.0, None);
            evictions += shaper.counters().evictions();
            shaper.end_frame();
        }
        evictions
    }

    #[test]
    fn a_memory_trim_retires_the_atlas_and_reshaping_readmits_only_the_live_runs() {
        let mut gpu = headless();
        let mut shaper = tiny_shaper();
        let first = settle(&mut shaper, &mut gpu, slot(0), &request(PAIR, 30.0), None);
        settle(&mut shaper, &mut gpu, slot(1), &request(PAIR, 27.0), None);
        shaper.end_frame();
        let Content::Text { atlas: old, .. } = first else {
            panic!("text content");
        };

        let mut retired = Vec::new();
        let bytes = shaper.trim(|texture| retired.push(texture));
        assert_eq!(retired, vec![old], "only the coverage plane existed");
        assert_eq!(bytes, (TINY_PLANE * TINY_PLANE) as usize);
        assert_eq!(
            shaper
                .residency()
                .pool_resident_bytes(GlyphImageKind::MaskA8),
            0
        );

        // Reshaping the one run still mounted re-rasterizes into a fresh plane
        // and admits its two glyphs only; the dropped run stays out.
        let again = settle(&mut shaper, &mut gpu, slot(0), &request(PAIR, 30.0), None);
        let Content::Text { atlas, glyphs, .. } = &again else {
            panic!("text content");
        };
        assert_ne!(*atlas, old, "a retired plane is never sampled again");
        assert_eq!(glyphs.len(), 2);
        assert_eq!(shaper.counters().rasters(), 2);
        let mut fresh = tiny_shaper();
        settle(
            &mut fresh,
            &mut headless(),
            slot(0),
            &request(PAIR, 30.0),
            None,
        );
        assert_eq!(
            shaper
                .residency()
                .pool_resident_bytes(GlyphImageKind::MaskA8),
            fresh
                .residency()
                .pool_resident_bytes(GlyphImageKind::MaskA8),
            "the trimmed atlas holds exactly what a cold start would",
        );
    }

    #[test]
    fn filling_the_pool_reclaims_cold_pages_and_keeps_the_hot_glyphs() {
        let mut gpu = headless();
        let mut shaper = tiny_shaper();
        settle(&mut shaper, &mut gpu, slot(0), &request(PAIR, 30.0), None);
        shaper.end_frame();

        let evicted = flood(&mut shaper, &mut gpu);
        assert!(evicted > 0, "nine pages must turn over under this load");
        assert_eq!(
            shaper.residency().pool_page_count(GlyphImageKind::MaskA8),
            9,
            "eviction reuses pages; it never grows the plane",
        );

        // The hot pair was drawn in every single frame, so CLOCK found its pages
        // referenced every time it passed them: they are still resident, and this
        // frame therefore rasterizes nothing and uploads nothing.
        let content = shaper.shape(&mut gpu, slot(0), &request(PAIR, 30.0), 1.0, None);
        let Content::Text { glyphs, .. } = &content else {
            panic!("text content");
        };
        assert_eq!(glyphs.len(), 2);
        assert_eq!(
            shaper.counters().rasters(),
            0,
            "the hot glyphs must survive the flood",
        );
        assert_eq!(shaper.counters().atlas_upload_bytes(), 0);
    }

    #[test]
    fn a_reclaimed_glyph_readmits_without_a_whole_atlas_upload() {
        let mut gpu = headless();
        let mut shaper = tiny_shaper();
        // Drawn once, in the first frame, beside cold filler that fills its page,
        // and never touched again: no hot glyph lands on its page, so CLOCK
        // finds it idle and reclaims it.
        let probe = request("q", 26.0);
        settle(&mut shaper, &mut gpu, slot(2), &probe, None);
        settle(&mut shaper, &mut gpu, slot(3), &request("aceo", 26.0), None);
        shaper.end_frame();
        assert!(flood(&mut shaper, &mut gpu) > 0);

        let content = settle(&mut shaper, &mut gpu, slot(2), &probe, None);
        let Content::Text { glyphs, .. } = &content else {
            panic!("text content");
        };
        assert_eq!(
            glyphs.len(),
            1,
            "a reclaimed glyph re-admits, it is not dropped"
        );
        assert!(
            shaper.counters().rasters() > 0,
            "its page was reclaimed during the flood",
        );
        // Only the re-admitted glyph's own texels move. The generational wipe this
        // replaced re-uploaded the entire plane every time it fired.
        let uploaded = shaper.counters().atlas_upload_bytes();
        assert!(
            uploaded > 0 && uploaded < (TINY_PAGE * TINY_PAGE) as u64,
            "re-admission uploaded {uploaded} bytes",
        );
    }

    /// Pool budgets are independent: filling the RGBA pool reclaims color pages
    /// and nothing else. The A8 coverage pool keeps every glyph and every page —
    /// §13.10's `memory pressure 不引发全 Text cache 连锁清空`.
    ///
    /// macOS-only: only the CoreText provider yields real color-emoji bitmaps, so
    /// only there can the RGBA pool be filled at all.
    #[cfg(target_os = "macos")]
    #[test]
    fn filling_the_color_pool_reclaims_nothing_from_the_coverage_pool() {
        let mut gpu = headless();
        let mut shaper = tiny_shaper();
        let latin = request(PAIR, 30.0);
        settle(&mut shaper, &mut gpu, slot(0), &latin, None);
        shaper.end_frame();
        let coverage_glyphs = shaper
            .residency()
            .pool_resident_glyphs(GlyphImageKind::MaskA8);
        let coverage_bytes = shaper
            .residency()
            .pool_resident_bytes(GlyphImageKind::MaskA8);
        assert!(coverage_glyphs > 0, "the coverage pool is warm");

        // One fresh emoji bucket per frame, never re-requested, until the nine-page
        // color pool turns over. The Latin pair is not drawn again at all.
        for size in 12..=26 {
            settle(
                &mut shaper,
                &mut gpu,
                slot(1),
                &request("🥟", size as f32),
                None,
            );
            shaper.end_frame();
        }
        let residency = shaper.residency();
        assert!(
            residency.pool_evictions(GlyphImageKind::ColorRgba8) > 0,
            "the color pool must have turned over",
        );
        assert_eq!(
            residency.pool_evictions(GlyphImageKind::MaskA8),
            0,
            "color pressure must not reclaim a coverage page",
        );
        assert_eq!(
            residency.pool_resident_glyphs(GlyphImageKind::MaskA8),
            coverage_glyphs,
        );
        assert_eq!(
            residency.pool_resident_bytes(GlyphImageKind::MaskA8),
            coverage_bytes,
        );

        // And the untouched coverage glyphs are still usable: no raster, no upload.
        shaper.shape(&mut gpu, slot(0), &latin, 1.0, None);
        assert_eq!(shaper.counters().rasters(), 0);
        assert_eq!(shaper.counters().atlas_upload_bytes(), 0);
    }

    #[test]
    fn a_warm_working_set_admits_nothing_and_uploads_zero_bytes() {
        let mut gpu = headless();
        let mut shaper = tiny_shaper();
        let frame = ["Viso", "steady", "state"];
        for (n, text) in (0..).zip(frame) {
            shaper.shape(&mut gpu, slot(n), &request(text, 18.0), 1.0, None);
        }
        drain(&mut shaper, &mut gpu);
        for (n, text) in (0..).zip(frame) {
            shaper.shape(&mut gpu, slot(n), &request(text, 18.0), 1.0, None);
        }
        shaper.end_frame();
        let residency = shaper.residency();
        let resident = residency.pool_resident_glyphs(GlyphImageKind::MaskA8);
        let pages = residency.pool_page_count(GlyphImageKind::MaskA8);
        let uploaded = residency.pool_upload_bytes(GlyphImageKind::MaskA8);
        assert!(
            resident > 0 && uploaded > 0,
            "the first frame admitted glyphs"
        );

        for (n, text) in (0..).zip(frame) {
            shaper.shape(&mut gpu, slot(n), &request(text, 18.0), 1.0, None);
        }
        assert!(
            !shaper.has_pending_work(),
            "a warm frame asks nothing of the worker"
        );
        let counters = shaper.counters();
        assert_eq!(counters.reshapes(), 0);
        assert_eq!(counters.rasters(), 0);
        assert_eq!(counters.atlas_upload_bytes(), 0);
        assert_eq!(counters.evictions(), 0);
        assert_eq!(counters.admission_failures(), 0);
        let residency = shaper.residency();
        assert_eq!(
            residency.pool_resident_glyphs(GlyphImageKind::MaskA8),
            resident,
            "a warm frame admits nothing",
        );
        assert_eq!(residency.pool_page_count(GlyphImageKind::MaskA8), pages);
        assert_eq!(
            residency.pool_upload_bytes(GlyphImageKind::MaskA8),
            uploaded,
            "a warm frame uploads nothing",
        );
    }

    #[test]
    fn shapes_and_reuses_residency() {
        let mut gpu = HeadlessRaster::new();
        let _ = gpu.create_surface(RawWindowHandle::Headless, 128, 128);
        let mut shaper = shaper();
        let request = TextRequest {
            text: "Viso".into(),
            font_size: 22.0,
            color: WHITE,
            soft_wrap: false,
            locale: None,
        };
        let first = settle(&mut shaper, &mut gpu, slot(0), &request, None);
        shaper.end_frame();
        let second = shaper.shape(&mut gpu, slot(0), &request, 1.0, None);
        let Content::Text {
            glyphs,
            natural,
            atlas,
            ..
        } = &first
        else {
            panic!("text content");
        };
        assert!(!glyphs.is_empty());
        assert!(natural.x > 0.0 && natural.y > 0.0);
        let Content::Text {
            atlas: second_atlas,
            ..
        } = second
        else {
            panic!("text content");
        };
        assert_eq!(atlas, &second_atlas);
        assert_eq!(shaper.counters().reshapes(), 0);
        assert_eq!(shaper.counters().relinebreaks(), 0);
        assert_eq!(
            shaper.counters().shaped_runs(),
            0,
            "a static frame resolves and shapes nothing"
        );
        assert_eq!(shaper.counters().rasters(), 0);
        assert_eq!(shaper.counters().atlas_upload_bytes(), 0);
    }

    /// The multilingual sample must resolve CJK, Devanagari, and emoji through
    /// the CoreText system-font path and actually rasterize their glyphs — the
    /// live-`CTFont` registry regression guard. Apple's PingFang/`.SFNS`-fallback
    /// CJK and Devanagari faces carry proprietary `hvgl` outlines a generic parser
    /// cannot render, so they route through the CoreText grayscale-coverage raster;
    /// if the raster could not bind their live handle (the old `CTFontCreateWithName`
    /// path silently substituted a Latin fallback), those glyphs would vanish and
    /// the atlas would receive no coverage upload for them.
    ///
    /// macOS-only: it depends on the CoreText system-font provider resolving real
    /// system faces, which the non-macOS stub does not.
    #[cfg(target_os = "macos")]
    #[test]
    fn multilingual_sample_rasterizes_cjk_devanagari_emoji() {
        let mut gpu = HeadlessRaster::new();
        let _ = gpu.create_surface(RawWindowHandle::Headless, 1024, 256);
        // No app font loaded: every script falls through to the system provider,
        // exactly as the hello-world example does.
        let mut shaper = TextShaper::new();
        let request = TextRequest {
            text: "世界 नमस्ते 🥟".into(),
            font_size: 48.0,
            color: WHITE,
            soft_wrap: false,
            locale: None,
        };
        let content = settle(&mut shaper, &mut gpu, slot(0), &request, None);
        let Content::Text {
            glyphs,
            color_glyphs,
            baseline,
            ..
        } = &content
        else {
            panic!("text content");
        };
        let baseline = *baseline;

        // Every visible cluster placed a glyph: 2 Han + the नमस्ते cluster(s) +
        // the emoji. A dropped face would leave gaps; assert we got well more than
        // the two ASCII spaces could explain.
        assert!(
            glyphs.len() >= 4,
            "CJK/Devanagari/emoji glyphs must all place (got {})",
            glyphs.len()
        );

        // Baseline sanity: the emoji color quad must sit on the *same* baseline as
        // the CJK/Devanagari coverage glyphs, not float above or drop below it. An
        // emoji strike is roughly em-tall sitting on the baseline, so its top edge
        // is above the baseline and its bottom edge at/just below it. A sign error
        // in the color placement math (mistaking the ink-bbox bottom for its top)
        // would push the whole quad a full glyph height off — this guards it.
        let emoji = color_glyphs.first().expect("emoji placed a color glyph");
        let top = emoji.rect.y;
        let bottom = emoji.rect.y + emoji.rect.h;
        assert!(
            top < baseline,
            "emoji top ({top}) must be above the baseline ({baseline})",
        );
        assert!(
            bottom > baseline - emoji.rect.h,
            "emoji must rest on the baseline, not float a glyph-height above it",
        );
        // Its bottom must not sink far below the baseline (a small descent is fine).
        assert!(
            bottom <= baseline + emoji.rect.h * 0.5,
            "emoji bottom ({bottom}) sits too far below the baseline ({baseline})",
        );
        // The atlas received real coverage/color uploads — glyphs actually
        // rasterized rather than resolving to empty bitmaps.
        assert!(
            shaper.counters().atlas_upload_bytes() > 0,
            "system-font glyphs must upload atlas coverage",
        );

        // Each script in isolation must rasterize, so a passing aggregate above
        // cannot be one script (e.g. emoji) covering for a dropped face. CJK and
        // Devanagari place monochrome coverage glyphs; emoji places a *color*
        // glyph (its own atlas), so assert against the right vector per script.
        for (label, text, color) in [
            ("Han", "世界", false),
            ("Devanagari", "नमस्ते", false),
            ("emoji", "🥟", true),
        ] {
            let mut solo = TextShaper::new();
            let req = TextRequest {
                text: text.into(),
                font_size: 48.0,
                color: WHITE,
                soft_wrap: false,
                locale: None,
            };
            let c = settle(&mut solo, &mut gpu, slot(0), &req, None);
            let Content::Text {
                glyphs,
                color_glyphs,
                ..
            } = &c
            else {
                panic!("text content");
            };
            let placed = if color {
                color_glyphs.len()
            } else {
                glyphs.len()
            };
            assert!(
                placed > 0,
                "{label} must place at least one glyph on its own",
            );
            assert!(
                solo.counters().atlas_upload_bytes() > 0,
                "{label} must upload atlas coverage on its own",
            );
        }
    }

    /// The static `ttf-parser` coverage fast path must refuse a CFF2 (variable)
    /// outline so the pipeline falls through to the authoritative CoreText
    /// raster. This is the direct guard for the Devanagari-tofu regression: the
    /// macOS system fallback `.SFDevanagari-Regular` is a CFF2 face whose sfnt
    /// (reassembled from CoreText tables at a fixed instance) carries only the
    /// default master. Drawing that bare master with the generic parser produced
    /// the *wrong* shape — non-empty ink that the shape() loop happily uploaded,
    /// so the older "glyphs placed + atlas uploaded" assertions passed while the
    /// runtime showed tofu. Here we shape real Devanagari, and for every placed
    /// glyph whose face is CFF2 we require the static path to return `None`
    /// (refuse) while the live CoreText coverage path returns real ink. If the
    /// refusal regressed, the static path would return `Some` and this fails.
    ///
    /// macOS-only: depends on the CoreText provider resolving `.SFDevanagari`.
    #[cfg(target_os = "macos")]
    #[test]
    fn cff2_devanagari_routes_through_live_raster_not_static_parser() {
        let mut gpu = HeadlessRaster::new();
        let _ = gpu.create_surface(RawWindowHandle::Headless, 512, 128);
        let mut shaper = TextShaper::new();
        // Shape once so the CJK/Devanagari fallback face is resolved and its live
        // handle registered with the color raster.
        let request = TextRequest {
            text: "नमस्ते".into(),
            font_size: 48.0,
            color: WHITE,
            soft_wrap: false,
            locale: None,
        };
        settle(&mut shaper, &mut gpu, slot(0), &request, None);
        // The drawn layout's per-glyph face/glyph placements (GlyphInstanceData
        // in the shaped Content carries only rect/uv, not face/glyph).
        let drawn = shaper.slots[&slot(0)].drawn.as_ref().expect("drawn");
        let glyphs = &drawn.placed.glyphs;
        assert!(!glyphs.is_empty(), "Devanagari must place glyphs");

        // At least one placed glyph must come from a CFF2 face and be served by
        // the live raster, not the static parser — otherwise the guard is vacuous
        // (the platform resolved a non-CFF2 face and this regression can't recur).
        let mut saw_cff2 = false;
        for g in glyphs {
            let Some((bytes, index)) = shaper.face_bytes(g.face) else {
                continue;
            };
            let is_cff2 = ttf_parser::Face::parse(bytes, index)
                .map(|f| f.tables().cff2.is_some())
                .unwrap_or(false);
            if !is_cff2 {
                continue;
            }
            saw_cff2 = true;

            // The static fast path must refuse a CFF2 outline...
            assert!(
                rasterize_coverage(bytes, index, g.glyph, 48.0).is_none(),
                "static parser must refuse CFF2 glyph {} (variable outline)",
                g.glyph,
            );
            // ...and the authoritative live path must render real ink for it.
            let live = shaper
                .color_raster
                .rasterize_coverage_glyph(g.face, g.glyph, 48)
                .expect("live CoreText coverage for CFF2 glyph");
            assert!(
                live.coverage.iter().any(|&v| v > 0),
                "live raster must produce ink for CFF2 glyph {}",
                g.glyph,
            );
        }
        assert!(
            saw_cff2,
            "expected the macOS Devanagari fallback to be a CFF2 face",
        );
    }

    #[test]
    fn wrapping_reduces_width_and_increases_height() {
        let mut gpu = HeadlessRaster::new();
        let _ = gpu.create_surface(RawWindowHandle::Headless, 256, 256);
        let mut shaper = shaper();
        let request = TextRequest {
            text: "wrap this paragraph onto several lines".into(),
            font_size: 20.0,
            color: WHITE,
            soft_wrap: true,
            locale: None,
        };
        let wide = settle(&mut shaper, &mut gpu, slot(0), &request, None);
        let Content::Text { natural: wide, .. } = wide else {
            panic!("text content");
        };
        let narrow = settle(&mut shaper, &mut gpu, slot(0), &request, Some(wide.x * 0.4));
        let Content::Text {
            natural: narrow, ..
        } = narrow
        else {
            panic!("text content");
        };
        assert!(narrow.x < wide.x);
        assert!(narrow.y > wide.y);
    }

    fn wrapped(text: &str) -> TextRequest {
        TextRequest {
            soft_wrap: true,
            ..request(text, 16.0)
        }
    }

    /// Line structure as plain data: each line's source range and width, and
    /// each run's source range, inline extent and glyph ids.
    type LineShape = (
        (usize, usize),
        u32,
        Vec<((usize, usize), (u32, u32), Vec<u16>)>,
    );

    fn structure(lines: &[LineLayout]) -> Vec<LineShape> {
        lines
            .iter()
            .map(|line| {
                let runs = line
                    .runs
                    .iter()
                    .map(|run| {
                        (
                            (run.logical_range.0.0, run.logical_range.1.0),
                            (
                                run.visual_inline_range.0.to_bits(),
                                run.visual_inline_range.1.to_bits(),
                            ),
                            run.glyphs.iter().map(|g| g.glyph_id).collect(),
                        )
                    })
                    .collect();
                (
                    (line.logical_range.0.0, line.logical_range.1.0),
                    line.width.to_bits(),
                    runs,
                )
            })
            .collect()
    }

    fn drawn_text(shaper: &TextShaper, at: ParagraphSlot) -> String {
        shaper.slots[&at]
            .drawn
            .as_ref()
            .expect("drawn")
            .target
            .text
            .to_string()
    }

    fn drawn_tailoring(shaper: &TextShaper, at: ParagraphSlot) -> LineBreakTailoring {
        shaper.slots[&at]
            .drawn
            .as_ref()
            .expect("drawn")
            .target
            .tailoring
    }

    fn lines_of(shaper: &TextShaper, at: ParagraphSlot) -> Vec<LineShape> {
        structure(&shaper.slots[&at].drawn.as_ref().expect("drawn").lines)
    }

    /// The lines a fresh paragraph lays out for `text`, through the same
    /// shaper, with nothing retained.
    fn recomputed(text: &str, width_px: f32) -> Vec<LineShape> {
        let mut gpu = headless();
        let mut fresh = shaper();
        settle(
            &mut fresh,
            &mut gpu,
            slot(0),
            &wrapped(text),
            Some(width_px),
        );
        lines_of(&fresh, slot(0))
    }

    #[test]
    fn runtime_edits_equal_a_full_recompute() {
        let mut gpu = headless();
        let mut shaper = shaper();
        let width = 120.0;
        let mut text = String::from("the quick brown fox jumps over the lazy dog again and again");
        settle(&mut shaper, &mut gpu, slot(0), &wrapped(&text), Some(width));
        assert_eq!(lines_of(&shaper, slot(0)), recomputed(&text, width));
        assert!(lines_of(&shaper, slot(0)).len() > 2);

        let edits: [(usize, usize, &str); 7] = [
            (4, 9, "slow"),
            (0, 0, "and "),
            (20, 20, "\n"),
            (30, 31, ""),
            (10, 10, "extraordinarily "),
            (0, 4, ""),
            (25, 40, " "),
        ];
        for (start, end, replacement) in edits {
            text.replace_range(start..end, replacement);
            settle(&mut shaper, &mut gpu, slot(0), &wrapped(&text), Some(width));
            assert_eq!(drawn_text(&shaper, slot(0)), text);
            assert_eq!(
                lines_of(&shaper, slot(0)),
                recomputed(&text, width),
                "after replacing {start}..{end} with {replacement:?}",
            );
            shaper.end_frame();
        }
    }

    #[test]
    fn typing_into_a_covered_paragraph_loads_queries_and_parses_nothing() {
        let mut gpu = headless();
        let mut shaper = shaper();
        let mut text = String::from("note: ");
        settle(&mut shaper, &mut gpu, slot(0), &wrapped(&text), Some(120.0));
        shaper.end_frame();
        let io = |shaper: &TextShaper| {
            (
                shaper.face_cache().misses(),
                shaper.primary_queries(),
                shaper.fallback().system_fallback_query_count(),
                shaper.fallback().coverage_face_parses(),
                shaper.layout_coverage_parses(),
            )
        };
        let warm = io(&shaper);

        for (at, ch) in "the quick brown fox, again".char_indices() {
            text.push(ch);
            settle(&mut shaper, &mut gpu, slot(0), &wrapped(&text), Some(120.0));
            assert_eq!(io(&shaper), warm, "keystroke {at} ({ch:?})");
            shaper.end_frame();
        }
        text.replace_range(6..9, "");
        settle(&mut shaper, &mut gpu, slot(0), &wrapped(&text), Some(120.0));
        assert_eq!(io(&shaper), warm, "a deletion");
        assert_eq!(drawn_text(&shaper, slot(0)), text);
    }

    #[test]
    fn a_character_edit_in_a_long_paragraph_reshapes_a_bounded_neighbourhood() {
        const WORDS: [&str; 7] = ["lorem", "ipsum", "dolor", "sit", "amet", "a", "quod"];
        // A varied 10k-word paragraph, so line contents do not repeat.
        let mut seed = 0x2545_f491_u32;
        let mut text = String::new();
        for _ in 0..10_000 {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            text.push_str(WORDS[seed as usize % WORDS.len()]);
            text.push(' ');
        }
        let mut gpu = headless();
        let mut shaper = shaper();
        let width = 240.0;
        settle(&mut shaper, &mut gpu, slot(0), &wrapped(&text), Some(width));
        let full = shaper.slots[&slot(0)].shape_calls;
        let lines = lines_of(&shaper, slot(0)).len();
        assert!(lines > 500, "the paragraph wraps into many lines ({lines})");
        shaper.end_frame();

        let middle = text.len() / 2;
        let at = (middle..)
            .find(|&i| text.as_bytes()[i] != b' ')
            .unwrap_or(middle);
        text.replace_range(at..at + 1, "Q");
        settle(&mut shaper, &mut gpu, slot(0), &wrapped(&text), Some(width));
        let edit = shaper.slots[&slot(0)].shape_calls - full;
        assert_eq!(shaper.counters().reshapes(), 1);
        assert!(
            edit <= 64,
            "a one-character edit shaped {edit} runs (the full layout shaped {full})",
        );
        assert!(shaper.counters().shaped_runs() <= edit);
        assert_eq!(lines_of(&shaper, slot(0)), recomputed(&text, width));
    }

    #[test]
    fn an_edit_keeps_drawing_at_the_last_good_wrap_width() {
        let mut gpu = headless();
        let mut shaper = shaper();
        let text = "a paragraph long enough to wrap onto a few lines";
        let width = 100.0;
        let Content::Text { natural, .. } =
            settle(&mut shaper, &mut gpu, slot(0), &wrapped(text), Some(width))
        else {
            panic!("text content");
        };
        assert_eq!(shaper.wrap_width(slot(0)), Some(width));

        // The runtime reshapes an edited run at its retained width, so it
        // keeps its line structure rather than drawing unwrapped until layout
        // reflows it.
        let edited = format!("{text}!");
        let retained = shaper.wrap_width(slot(0));
        // Until the worker lays the edit out, the last good lines draw.
        let Content::Text {
            natural: pending,
            shaped_at_width,
            ..
        } = shaper.shape(&mut gpu, slot(0), &wrapped(&edited), 1.0, retained)
        else {
            panic!("text content");
        };
        assert_eq!(shaped_at_width, Some(width));
        assert_eq!(pending, natural);
        drain(&mut shaper, &mut gpu);
        let Content::Text {
            natural: after,
            shaped_at_width,
            ..
        } = shaper.shape(&mut gpu, slot(0), &wrapped(&edited), 1.0, retained)
        else {
            panic!("text content");
        };
        assert_eq!(shaped_at_width, Some(width));
        assert_eq!(after.y, natural.y);
        assert!(after.x <= width);
        assert_eq!(drawn_text(&shaper, slot(0)), edited);

        // A memory trim keeps the lines: the reshape it forces only rasters.
        shaper.end_frame();
        shaper.trim(|_| {});
        settle(&mut shaper, &mut gpu, slot(0), &wrapped(&edited), retained);
        assert_eq!(shaper.counters().reshapes(), 0);
        assert_eq!(shaper.counters().shaped_runs(), 0);
        assert!(shaper.counters().rasters() > 0);
    }

    #[test]
    fn the_content_locale_selects_the_paragraph_line_breaking() {
        let mut gpu = headless();
        let mut shaper = shaper();
        for (n, locale) in (0..).zip(["ja", "zh-Hans", "zh-Hant", "ko"]) {
            let request = TextRequest {
                locale: Some(locale.to_owned()),
                ..wrapped("line breaking")
            };
            settle(&mut shaper, &mut gpu, slot(n), &request, Some(80.0));
            assert_eq!(
                drawn_tailoring(&shaper, slot(n)),
                LineBreakTailoring::for_locale(locale),
                "{locale}",
            );
        }
        let tailoring = |n| drawn_tailoring(&shaper, slot(n));
        assert_eq!(tailoring(0), tailoring(1), "ja and zh share the CJK tables");
        assert_eq!(tailoring(1), tailoring(2));
        assert_ne!(tailoring(0), tailoring(3), "ko breaks as a spaced script");

        let unset = wrapped("line breaking");
        settle(&mut shaper, &mut gpu, slot(9), &unset, Some(80.0));
        assert_eq!(
            drawn_tailoring(&shaper, slot(9)),
            LineBreakTailoring::for_locale(process_locale()),
        );
    }

    #[test]
    fn posix_locales_map_to_bcp47_tags() {
        assert_eq!(posix_to_bcp47("zh_CN.UTF-8"), "zh-CN");
        assert_eq!(posix_to_bcp47("ja_JP"), "ja-JP");
        assert_eq!(posix_to_bcp47("ko_KR.eucKR@euro"), "ko-KR");
        assert_eq!(posix_to_bcp47("C"), "");
        assert_eq!(posix_to_bcp47("POSIX"), "");
    }

    #[test]
    fn affinity_places_a_wrap_boundary_caret_on_either_line() {
        let mut shaper = shaper();
        let mut gpu = headless();
        let request = wrapped("first second third fourth");
        settle(&mut shaper, &mut gpu, slot(0), &request, Some(70.0));
        let second = shaper.slots[&slot(0)].drawn.as_ref().expect("drawn").lines[1]
            .logical_range
            .0;
        let upstream = shaper
            .caret(slot(0), &request, TextPosition::upstream(second))
            .expect("drawn");
        let downstream = shaper
            .caret(slot(0), &request, TextPosition::downstream(second))
            .expect("drawn");
        assert_eq!(upstream.y, 0.0, "upstream ends the first line");
        assert!(upstream.x > 0.0);
        assert_eq!(downstream.y, upstream.h, "downstream starts the second");
        assert_eq!(downstream.x, 0.0);
        assert_eq!(
            shaper.counters().reshapes(),
            1,
            "carets read the drawn lines"
        );
    }
}
