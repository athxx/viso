//! Off-main text work: paragraph shaping and line breaking, and exact-coverage
//! glyph rasterization, run on a worker thread against face bytes the main
//! thread hands over (§12.20, §14.4).
//!
//! The unit of work is a whole paragraph. The worker keeps each paragraph's
//! retained lines, applies the new text as a range edit, and lays it out with
//! the paragraph's own incremental reflow, so an edit reshapes exactly what a
//! main-thread layout would and a worker boundary never cuts a shaped run
//! (§12.7, §12.9). What comes back is the paragraph's complete line set, which
//! the main thread commits atomically in place of the last good one (§12.21).
//!
//! Whatever needs a platform handle stays on the main thread: system fallback
//! resolution, and color and platform-outline rasterization. When a cluster
//! has no face yet, the worker reports it as a need; the main thread resolves
//! it, hands the face over, and lays the paragraph out again.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread::{self, JoinHandle, ThreadId};

use viso_text::fallback::{FallbackPlanKey, FallbackStyle, FontFallback};
use viso_text::inspect::{self, BudgetState, CacheKey, MissLedger};
use viso_text::paragraph::{LineLayout, Paragraph, ShapedSegment, ShapedSpan};
use viso_text::text_work::{JobKind, Priority, TextJob, TextWork};
use viso_text::{
    BaseDirection, Coverage, CoverageBitmap, Direction, FontFaceId, GlyphImageKind, GlyphKey,
    LineBreakTailoring, Segmenter, ShapedGlyph, ShapedRun, Shaper, TextOffset, rasterize_coverage,
};

/// Longest source substring the cross-paragraph span cache keeps. Labels,
/// button titles and short wrapped lines repeat across nodes and rebuilds;
/// long runs are unique to their paragraph, whose retained lines already
/// keep them.
const SPAN_CACHE_TEXT: usize = 64;

/// A face's bytes and collection index, shared with the main thread's owner.
pub(crate) type FaceData = (Arc<[u8]>, u32);

/// One grapheme under one base face and locale: the unit a fallback choice is
/// remembered for.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct ClusterKey {
    pub(crate) base: FontFaceId,
    pub(crate) locale: String,
    pub(crate) cluster: String,
}

/// Lay a paragraph out to a width.
#[derive(Debug)]
pub(crate) struct LayoutJob {
    /// The paragraph's slot, packed; results are routed back by it.
    pub(crate) slot: u64,
    /// Orders this paragraph's layouts so the main thread can tell a result
    /// from one it already superseded.
    pub(crate) seq: u64,
    pub(crate) text: Arc<str>,
    /// The wrap width in em, `0.0` for unwrapped text.
    pub(crate) width_em: f32,
    pub(crate) tailoring: LineBreakTailoring,
    pub(crate) base: FontFaceId,
    pub(crate) locale: String,
    /// Lay out from the top, reshaping every run: a cluster the last layout
    /// could not cover has a face now.
    pub(crate) full: bool,
    /// The raster bucket the lines will draw at. The coverage of every glyph
    /// not yet rasterized at it comes back with the lines.
    pub(crate) ppem: u16,
    pub(crate) priority: Priority,
}

/// Messages from the main thread, sent in one batch per dispatch.
#[derive(Debug)]
pub(crate) enum ToWorker {
    /// A face's bytes, sent once before any job that uses it.
    Face {
        face: FontFaceId,
        data: FaceData,
    },
    /// The face was dropped: forget everything keyed by it.
    Forget(FontFaceId),
    /// A face the main thread resolved for a fallback plan, so other clusters
    /// of the same script try it before asking again.
    Route {
        key: FallbackPlanKey,
        face: FontFaceId,
    },
    /// The face a grapheme draws with; the base face when nothing covers it.
    Assign {
        cluster: ClusterKey,
        face: FontFaceId,
    },
    /// The paragraph's node is gone: drop it and cancel its pending jobs.
    DropSlot(u64),
    /// A memory trim: drop the span cache and the record of what was
    /// rasterized.
    Clear,
    Layout(Box<LayoutJob>),
    /// Rasterize the coverage of these glyphs.
    Raster {
        id: u64,
        keys: Vec<GlyphKey>,
    },
    /// Reply [`FromWorker::Flushed`] once every job sent before it ran.
    #[cfg(test)]
    Flush,
}

/// A grapheme no known face covers, for the main thread to resolve.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Need {
    pub(crate) key: FallbackPlanKey,
    pub(crate) cluster: String,
}

/// A rasterized glyph; `None` when the face has no outline the parser reads.
pub(crate) type Rastered = (GlyphKey, Option<CoverageBitmap>);

#[derive(Debug)]
pub(crate) struct LayoutDone {
    pub(crate) slot: u64,
    pub(crate) seq: u64,
    /// The paragraph's complete lines, top to bottom.
    pub(crate) lines: Vec<LineLayout>,
    /// Whether a layout ran; `false` when the retained lines already matched.
    pub(crate) relaid: bool,
    /// Graphemes that drew with the base face for want of a face covering
    /// them.
    pub(crate) needs: Vec<Need>,
    /// Spans this layout handed to the shaper.
    pub(crate) shaped_runs: u64,
    /// The paragraph's cumulative shape invocations.
    pub(crate) shape_calls: u64,
    /// The thread the layout ran on.
    pub(crate) thread: ThreadId,
    pub(crate) glyphs: Vec<Rastered>,
    pub(crate) spans: SpanStats,
    /// The shaping cache's misses and evictions, when they changed since the
    /// last layout reported them; always `None` without the inspector.
    pub(crate) span_ledger: Option<MissLedger>,
}

#[derive(Debug)]
pub(crate) enum FromWorker {
    Laid(Box<LayoutDone>),
    Rastered(Vec<Rastered>),
    /// Jobs a [`ToWorker::DropSlot`] cancelled before they ran; each would
    /// otherwise have replied.
    Cancelled(u32),
    #[cfg(test)]
    Flushed,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct SpanKey {
    face: FontFaceId,
    direction: Direction,
    /// Fallback faces are chosen per locale, so the same text can shape
    /// differently under two locales.
    locale: String,
    text: String,
}

impl SpanKey {
    fn cache_key(&self) -> CacheKey {
        CacheKey::Shaping {
            face: self.face,
            direction: self.direction,
            locale: self.locale.clone(),
            text: self.text.clone(),
        }
    }
}

/// The shaping cache's counters and resident bytes, as last reported by the
/// worker that owns it.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct SpanStats {
    bytes: u64,
    budget_bytes: u64,
    hits: u64,
    misses: u64,
    evictions: u64,
    len: usize,
}

impl SpanStats {
    /// The counters of an empty cache of `budget_bytes`.
    pub(crate) fn empty(budget_bytes: u64) -> Self {
        Self {
            budget_bytes,
            ..Self::default()
        }
    }

    /// Resident bytes across both generations.
    pub(crate) fn bytes(&self) -> u64 {
        self.bytes
    }

    pub(crate) fn budget_bytes(&self) -> u64 {
        self.budget_bytes
    }

    pub(crate) fn hits(&self) -> u64 {
        self.hits
    }

    pub(crate) fn misses(&self) -> u64 {
        self.misses
    }

    /// Spans dropped by generation turnover (a face drop is not an eviction).
    pub(crate) fn evictions(&self) -> u64 {
        self.evictions
    }

    pub(crate) fn len(&self) -> usize {
        self.len
    }
}

/// The global shaping cache: shaped spans shared across paragraphs, so
/// identical short text in different nodes — or in the same node after a
/// rebuild gave it a new identity — shapes once.
///
/// It is budgeted in bytes on its own, apart from the face cache, and kept in
/// two generations: when the live generation reaches half the budget it
/// becomes the previous one and the one before is dropped, so the cache holds
/// at most the budget and a span still in use survives a turnover by moving
/// back into the live generation on its next hit.
#[derive(Debug)]
struct SpanCache {
    live: HashMap<SpanKey, ShapedSpan>,
    previous: HashMap<SpanKey, ShapedSpan>,
    live_bytes: u64,
    previous_bytes: u64,
    budget_bytes: u64,
    hits: u64,
    misses: u64,
    evictions: u64,
    ledger: MissLedger,
    /// The ledger recorded something since [`Self::changed_ledger`] last
    /// reported it.
    ledger_changed: bool,
}

impl SpanCache {
    fn with_budget(budget_bytes: u64) -> Self {
        Self {
            live: HashMap::new(),
            previous: HashMap::new(),
            live_bytes: 0,
            previous_bytes: 0,
            budget_bytes,
            hits: 0,
            misses: 0,
            evictions: 0,
            ledger: MissLedger::default(),
            ledger_changed: false,
        }
    }

    fn get(&mut self, key: &SpanKey) -> Option<ShapedSpan> {
        if let Some(span) = self.live.get(key) {
            self.hits += 1;
            return Some(span.clone());
        }
        let Some(span) = self.previous.remove(key) else {
            self.misses += 1;
            if inspect::ENABLED {
                let budget = BudgetState {
                    resident_bytes: self.live_bytes + self.previous_bytes,
                    budget_bytes: Some(self.budget_bytes),
                    entries: self.len() as u64,
                };
                self.ledger.missed(key.cache_key(), budget);
                self.ledger_changed = true;
            }
            return None;
        };
        self.hits += 1;
        self.previous_bytes -= span_bytes(key, &span);
        self.insert(key.clone(), span.clone());
        Some(span)
    }

    fn insert(&mut self, key: SpanKey, span: ShapedSpan) {
        let bytes = span_bytes(&key, &span);
        if self.live_bytes + bytes > self.budget_bytes / 2 {
            self.evictions += self.previous.len() as u64;
            if inspect::ENABLED {
                let by = key.cache_key();
                for evicted in self.previous.keys() {
                    self.ledger.evicted(evicted.cache_key(), Some(by.clone()));
                }
                self.ledger_changed = true;
            }
            self.previous = std::mem::take(&mut self.live);
            self.previous_bytes = std::mem::replace(&mut self.live_bytes, 0);
        }
        self.live_bytes += bytes;
        if let Some(old) = self.live.insert(key, span) {
            self.live_bytes -= segment_bytes(&old);
        }
    }

    /// Drop every span shaped from `face` or split onto it — the face itself
    /// was dropped. Spans of every other face stay.
    fn forget_face(&mut self, face: FontFaceId) {
        let ledger = &mut self.ledger;
        for (spans, bytes) in [
            (&mut self.live, &mut self.live_bytes),
            (&mut self.previous, &mut self.previous_bytes),
        ] {
            spans.retain(|key, span| {
                let keep = key.face != face && span.segments.iter().all(|s| s.run.face != face);
                if !keep {
                    *bytes -= span_bytes(key, span);
                    if inspect::ENABLED {
                        ledger.evicted(key.cache_key(), Some(CacheKey::Face(face)));
                    }
                }
                keep
            });
        }
        self.ledger_changed |= inspect::ENABLED;
    }

    fn len(&self) -> usize {
        self.live.len() + self.previous.len()
    }

    /// Drop every span, counting each as an eviction.
    fn clear(&mut self) {
        self.evictions += self.len() as u64;
        if inspect::ENABLED {
            for key in self.live.keys().chain(self.previous.keys()) {
                self.ledger.evicted(key.cache_key(), None);
            }
            self.ledger_changed = true;
        }
        self.live.clear();
        self.previous.clear();
        self.live_bytes = 0;
        self.previous_bytes = 0;
    }

    fn stats(&self) -> SpanStats {
        SpanStats {
            bytes: self.live_bytes + self.previous_bytes,
            budget_bytes: self.budget_bytes,
            hits: self.hits,
            misses: self.misses,
            evictions: self.evictions,
            len: self.len(),
        }
    }

    /// A copy of the ledger when it changed since this last returned one.
    fn changed_ledger(&mut self) -> Option<MissLedger> {
        if !std::mem::take(&mut self.ledger_changed) {
            return None;
        }
        Some(self.ledger.clone())
    }
}

/// The resident cost charged for one cached span: its key's owned strings, its
/// glyphs and ligature carets, and the fixed size of the entry itself.
fn span_bytes(key: &SpanKey, span: &ShapedSpan) -> u64 {
    let strings = key.text.len() + key.locale.len();
    let entry = std::mem::size_of::<SpanKey>() + std::mem::size_of::<ShapedSpan>();
    (strings + entry) as u64 + segment_bytes(span)
}

/// The heap a span's segments hold, apart from its key.
fn segment_bytes(span: &ShapedSpan) -> u64 {
    let segments: usize = span
        .segments
        .iter()
        .map(|segment| {
            let run = &segment.run;
            let carets: usize = run
                .ligature_carets
                .iter()
                .map(|l| std::mem::size_of_val(l) + std::mem::size_of_val(l.carets.as_slice()))
                .sum();
            std::mem::size_of_val(segment)
                + run.glyphs.len() * std::mem::size_of::<ShapedGlyph>()
                + carets
        })
        .sum();
    segments as u64
}

/// Everything the worker owns: face bytes and coverage, the shaper, the span
/// cache, every paragraph's retained lines, and the queue of pending jobs.
#[derive(Debug)]
pub(crate) struct WorkerState {
    faces: HashMap<FontFaceId, FaceData>,
    coverage: Coverage,
    shaper: Shaper,
    spans: SpanCache,
    paragraphs: HashMap<u64, Paragraph>,
    routes: HashMap<FallbackPlanKey, Vec<FontFaceId>>,
    assigned: HashMap<ClusterKey, FontFaceId>,
    /// Glyphs whose coverage went back with a layout, so the next layout
    /// sends only new ones. A glyph the main thread later loses is asked for
    /// again explicitly.
    sent: HashSet<GlyphKey>,
    work: TextWork,
    layouts: HashMap<u64, Box<LayoutJob>>,
    rasters: HashMap<u64, Vec<GlyphKey>>,
    /// Graphemes the current layout could not cover, deduplicated.
    needs: Vec<Need>,
    /// Spans the current layout handed to the shaper.
    shaped: u64,
    #[cfg(test)]
    flushes: u32,
}

impl WorkerState {
    pub(crate) fn new(span_budget_bytes: u64) -> Self {
        Self {
            faces: HashMap::new(),
            coverage: Coverage::new(),
            shaper: Shaper::new(),
            spans: SpanCache::with_budget(span_budget_bytes),
            paragraphs: HashMap::new(),
            routes: HashMap::new(),
            assigned: HashMap::new(),
            sent: HashSet::new(),
            work: TextWork::default(),
            layouts: HashMap::new(),
            rasters: HashMap::new(),
            needs: Vec::new(),
            shaped: 0,
            #[cfg(test)]
            flushes: 0,
        }
    }

    /// Apply control messages at once and queue jobs by priority.
    fn accept(&mut self, batch: Vec<ToWorker>, out: &Sender<FromWorker>) {
        for message in batch {
            match message {
                ToWorker::Face { face, data } => {
                    self.faces.insert(face, data);
                }
                ToWorker::Forget(face) => self.forget_face(face),
                ToWorker::Route { key, face } => {
                    let faces = self.routes.entry(key).or_default();
                    if !faces.contains(&face) {
                        faces.push(face);
                    }
                }
                ToWorker::Assign { cluster, face } => {
                    self.assigned.insert(cluster, face);
                }
                ToWorker::DropSlot(slot) => {
                    self.paragraphs.remove(&slot);
                    if self.layouts.remove(&slot).is_some() {
                        let _ = out.send(FromWorker::Cancelled(1));
                    }
                }
                ToWorker::Clear => {
                    self.spans.clear();
                    self.sent.clear();
                }
                ToWorker::Layout(job) => {
                    self.work.submit(TextJob {
                        priority: job.priority,
                        kind: JobKind::Paragraph {
                            paragraph: job.slot,
                        },
                    });
                    // A newer layout of a paragraph still queued replaces it:
                    // the first of the two queued jobs runs the newer one and
                    // the second finds nothing, so the older never replies.
                    if self.layouts.insert(job.slot, job).is_some() {
                        let _ = out.send(FromWorker::Cancelled(1));
                    }
                }
                ToWorker::Raster { id, keys } => {
                    self.work.submit(TextJob {
                        priority: Priority::CriticalVisible,
                        kind: JobKind::Rasterize { glyph: id },
                    });
                    self.rasters.insert(id, keys);
                }
                #[cfg(test)]
                ToWorker::Flush => self.flushes += 1,
            }
        }
    }

    /// Run the most urgent pending job; `false` when nothing is pending.
    fn run_next(&mut self, out: &Sender<FromWorker>) -> bool {
        let Some(job) = self.work.take_next() else {
            #[cfg(test)]
            for _ in 0..std::mem::take(&mut self.flushes) {
                let _ = out.send(FromWorker::Flushed);
            }
            return false;
        };
        let reply = match job.kind {
            JobKind::Paragraph { paragraph } => match self.layouts.remove(&paragraph) {
                Some(job) => FromWorker::Laid(Box::new(self.layout(*job))),
                // Cancelled by its slot's drop.
                None => return true,
            },
            JobKind::Rasterize { glyph: id } => {
                let keys = self.rasters.remove(&id).unwrap_or_default();
                FromWorker::Rastered(keys.into_iter().map(|key| self.raster(key)).collect())
            }
            JobKind::PrewarmChar { .. } => return true,
        };
        let _ = out.send(reply);
        true
    }

    /// Lay `job`'s paragraph out: its retained lines take the new text as one
    /// range edit, so only the neighbourhood of the edit reflows.
    pub(crate) fn layout(&mut self, job: LayoutJob) -> LayoutDone {
        let epoch = u64::from(job.base.0);
        let mut paragraph = match self.paragraphs.remove(&job.slot) {
            Some(mut paragraph) => {
                if job.full {
                    paragraph.set_text(&*job.text);
                } else {
                    apply_text(&mut paragraph, &job.text);
                }
                paragraph.set_style_epoch(epoch);
                paragraph
            }
            None => Paragraph::new(&*job.text, BaseDirection::Auto, epoch),
        };
        paragraph.set_tailoring(job.tailoring);
        self.needs.clear();
        self.shaped = 0;
        let (base, locale) = (job.base, job.locale.as_str());
        let relaid = paragraph.layout(job.width_em, &mut |text: &str, direction: Direction| {
            self.shape_span(base, locale, text, direction)
        });
        let lines = paragraph.lines().to_vec();
        let mut glyphs = Vec::new();
        // A layout that still needs a face is laid out again once it has one;
        // its placeholder glyphs never draw.
        let runs = if self.needs.is_empty() {
            &lines[..]
        } else {
            &[]
        };
        for run in runs.iter().flat_map(|line| &line.runs) {
            for glyph in &run.glyphs {
                // Color glyphs rasterize on the main thread as they draw.
                let cluster = run.logical_range.0.0 + glyph.cluster as usize;
                if starts_emoji(&job.text, cluster) {
                    continue;
                }
                let key = GlyphKey {
                    face: run.face,
                    glyph: glyph.glyph_id,
                    bucket: job.ppem,
                    kind: GlyphImageKind::MaskA8,
                };
                if self.sent.insert(key) {
                    glyphs.push(self.raster(key));
                }
            }
        }
        let done = LayoutDone {
            slot: job.slot,
            seq: job.seq,
            lines,
            relaid,
            needs: std::mem::take(&mut self.needs),
            shaped_runs: self.shaped,
            shape_calls: paragraph.shape_call_count(),
            thread: thread::current().id(),
            glyphs,
            spans: self.spans.stats(),
            span_ledger: self.spans.changed_ledger(),
        };
        self.paragraphs.insert(job.slot, paragraph);
        done
    }

    fn raster(&self, key: GlyphKey) -> Rastered {
        let bitmap = self.faces.get(&key.face).and_then(|(bytes, index)| {
            rasterize_coverage(bytes, *index, key.glyph, f32::from(key.bucket))
        });
        (key, bitmap)
    }

    /// Drop exactly what is keyed by `face`; every other face keeps its state.
    fn forget_face(&mut self, face: FontFaceId) {
        self.faces.remove(&face);
        self.coverage.forget(face);
        self.spans.forget_face(face);
        self.sent.retain(|key| key.face != face);
        self.assigned.retain(|_, &mut f| f != face);
        self.routes.retain(|key, faces| {
            faces.retain(|&f| f != face);
            key.base != face && !faces.is_empty()
        });
    }

    /// Shape one direction run of `text` from `base`, splitting it into
    /// fallback faces per grapheme where `base` does not cover it. A span with
    /// a grapheme no known face covers is not cached: it reshapes once the
    /// main thread resolved a face for it.
    fn shape_span(
        &mut self,
        base: FontFaceId,
        locale: &str,
        text: &str,
        direction: Direction,
    ) -> ShapedSpan {
        let key = (text.len() <= SPAN_CACHE_TEXT).then(|| SpanKey {
            face: base,
            direction,
            locale: locale.to_owned(),
            text: text.to_owned(),
        });
        if let Some(span) = key.as_ref().and_then(|key| self.spans.get(key)) {
            return span;
        }
        self.shaped += 1;
        let (span, covered) = match self.shape_face(base, text, direction) {
            None => (ShapedSpan::default(), true),
            Some(run) if !run.has_coverage_miss() => (ShapedSpan::from(run), true),
            Some(_) => self.shape_clusters(base, locale, text, direction),
        };
        if covered && let Some(key) = key {
            self.spans.insert(key, span.clone());
        }
        span
    }

    /// Shape `text` grapheme by grapheme onto the first face that covers each,
    /// and whether every grapheme found one.
    fn shape_clusters(
        &mut self,
        base: FontFaceId,
        locale: &str,
        text: &str,
        direction: Direction,
    ) -> (ShapedSpan, bool) {
        let boundaries: Vec<usize> = Segmenter::new(text)
            .grapheme_boundaries()
            .map(|offset| offset.0)
            .collect();
        let mut covered = true;
        let mut groups: Vec<(FontFaceId, usize, usize)> = Vec::new();
        for pair in boundaries.windows(2) {
            let cluster = &text[pair[0]..pair[1]];
            let face = match self.cluster_face(base, locale, cluster) {
                Some(face) => face,
                None => {
                    covered = false;
                    base
                }
            };
            if let Some(last) = groups.last_mut()
                && last.0 == face
            {
                last.2 = pair[1];
            } else {
                groups.push((face, pair[0], pair[1]));
            }
        }
        let mut span = ShapedSpan::default();
        for (face, start, end) in groups {
            if let Some(run) = self.shape_face(face, &text[start..end], direction) {
                span.segments.push(ShapedSegment {
                    start: start as u32,
                    run,
                });
            }
        }
        (span, covered)
    }

    /// The face `cluster` draws with: the base face when it covers it, the
    /// face the main thread assigned it, or a routed fallback face of its
    /// script that covers it. `None` records a need.
    fn cluster_face(
        &mut self,
        base: FontFaceId,
        locale: &str,
        cluster: &str,
    ) -> Option<FontFaceId> {
        if self.face_covers(base, cluster) {
            return Some(base);
        }
        let assigned = ClusterKey {
            base,
            locale: locale.to_owned(),
            cluster: cluster.to_owned(),
        };
        if let Some(&face) = self.assigned.get(&assigned) {
            return Some(face);
        }
        let key = FallbackPlanKey {
            base,
            script: FontFallback::run_script(cluster),
            locale: assigned.locale,
            style: FallbackStyle::default(),
            source_revision: 0,
        };
        let routed = self.routes.get(&key).cloned().unwrap_or_default();
        if let Some(face) = routed.into_iter().find(|&f| self.face_covers(f, cluster)) {
            return Some(face);
        }
        let need = Need {
            key,
            cluster: assigned.cluster,
        };
        if !self.needs.contains(&need) {
            self.needs.push(need);
        }
        None
    }

    fn face_covers(&mut self, face: FontFaceId, text: &str) -> bool {
        let Some((bytes, index)) = self.faces.get(&face) else {
            return false;
        };
        self.coverage.face_covers(face, bytes, *index, text)
    }

    fn shape_face(
        &mut self,
        face: FontFaceId,
        text: &str,
        direction: Direction,
    ) -> Option<ShapedRun> {
        let (bytes, index) = self.faces.get(&face)?;
        self.shaper.shape_run(face, bytes, *index, text, direction)
    }

    #[cfg(test)]
    pub(crate) fn span_stats(&self) -> SpanStats {
        self.spans.stats()
    }

    #[cfg(all(test, feature = "inspector"))]
    pub(crate) fn span_ledger(&self) -> &MissLedger {
        &self.spans.ledger
    }

    #[cfg(test)]
    pub(crate) fn add_face(&mut self, face: FontFaceId, data: FaceData) {
        self.faces.insert(face, data);
    }

    #[cfg(test)]
    pub(crate) fn drop_face(&mut self, face: FontFaceId) {
        self.forget_face(face);
    }

    #[cfg(test)]
    pub(crate) fn shape_text(&mut self, base: FontFaceId, text: &str) -> ShapedSpan {
        self.shape_span(base, "", text, Direction::LeftToRight)
    }
}

fn is_emoji(ch: char) -> bool {
    matches!(
        ch as u32,
        0x1F000..=0x1FAFF | 0x2600..=0x27BF | 0xFE0F | 0x200D
    )
}

/// Whether the character that starts `cluster` in `text` is emoji.
pub(crate) fn starts_emoji(text: &str, cluster: usize) -> bool {
    text.get(cluster..)
        .and_then(|rest| rest.chars().next())
        .is_some_and(is_emoji)
}

/// Turn `paragraph`'s text into `text` as one range replacement: the span
/// between their common prefix and common suffix, both cut back to char
/// boundaries of either string.
fn apply_text(paragraph: &mut Paragraph, text: &str) {
    let old = paragraph.text();
    if old == text {
        return;
    }
    let mut prefix = old
        .bytes()
        .zip(text.bytes())
        .take_while(|(a, b)| a == b)
        .count();
    while !(old.is_char_boundary(prefix) && text.is_char_boundary(prefix)) {
        prefix -= 1;
    }
    let room = (old.len() - prefix).min(text.len() - prefix);
    let mut suffix = old
        .bytes()
        .rev()
        .zip(text.bytes().rev())
        .take(room)
        .take_while(|(a, b)| a == b)
        .count();
    while !(old.is_char_boundary(old.len() - suffix) && text.is_char_boundary(text.len() - suffix))
    {
        suffix -= 1;
    }
    let removed = (TextOffset(prefix), TextOffset(old.len() - suffix));
    paragraph.edit(removed, &text[prefix..text.len() - suffix]);
}

/// The worker as the main thread holds it: a thread fed batches of messages,
/// or — where no thread can be spawned — the same state run inline on send.
pub(crate) struct TextWorker {
    to: Option<Sender<Vec<ToWorker>>>,
    from: Receiver<FromWorker>,
    thread: Option<JoinHandle<()>>,
    inline: Option<(Box<WorkerState>, Sender<FromWorker>)>,
}

impl std::fmt::Debug for TextWorker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TextWorker")
            .field("threaded", &self.thread.is_some())
            .finish_non_exhaustive()
    }
}

impl TextWorker {
    /// Start the worker thread with a span cache of `span_budget_bytes`. A
    /// target that cannot spawn threads runs the same state inline instead, on
    /// the sending thread.
    pub(crate) fn spawn(span_budget_bytes: u64) -> Self {
        let (to, inbox) = mpsc::channel::<Vec<ToWorker>>();
        let (outbox, from) = mpsc::channel();
        let spawned = {
            let outbox = outbox.clone();
            thread::Builder::new()
                .name("viso-text".into())
                .spawn(move || run(WorkerState::new(span_budget_bytes), &inbox, &outbox))
                .ok()
        };
        match spawned {
            Some(thread) => Self {
                to: Some(to),
                from,
                thread: Some(thread),
                inline: None,
            },
            None => Self {
                to: None,
                from,
                thread: None,
                inline: Some((Box::new(WorkerState::new(span_budget_bytes)), outbox)),
            },
        }
    }

    /// Hand a batch to the worker. Inline, the batch runs to completion here.
    pub(crate) fn send(&mut self, batch: Vec<ToWorker>) {
        if let Some(to) = &self.to {
            let _ = to.send(batch);
        } else if let Some((state, out)) = &mut self.inline {
            state.accept(batch, out);
            while state.run_next(out) {}
        }
    }

    /// The next reply, if one is ready; never blocks. `Err(Disconnected)`
    /// means the worker is gone.
    pub(crate) fn try_recv(&self) -> Result<FromWorker, TryRecvError> {
        self.from.try_recv()
    }

    /// Block for the next reply.
    #[cfg(test)]
    pub(crate) fn recv(&self) -> Option<FromWorker> {
        self.from.recv().ok()
    }
}

impl Drop for TextWorker {
    fn drop(&mut self) {
        self.to = None;
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// The worker loop: take every batch already sent, run one job, and repeat;
/// sleep on the channel only when nothing is pending. Ends when the main
/// thread drops its sender.
fn run(mut state: WorkerState, inbox: &Receiver<Vec<ToWorker>>, out: &Sender<FromWorker>) {
    loop {
        loop {
            match inbox.try_recv() {
                Ok(batch) => state.accept(batch, out),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return,
            }
        }
        if state.run_next(out) {
            continue;
        }
        match inbox.recv() {
            Ok(batch) => state.accept(batch, out),
            Err(_) => return,
        }
    }
}
