//! Explanations for the text Inspector (§25): why a fallback walk chose the
//! face it did, why a cache missed and which eviction caused it, and what
//! representation a glyph draws in.
//!
//! Recording is compiled in only with the `inspector` feature (and in this
//! crate's own tests). Every record site sits behind [`ENABLED`], a constant, so
//! a build without the feature builds no key, keeps no log, and pays nothing on
//! any path. The logs keep the most recent [`TRACE_CAPACITY`] entries and are
//! read through `&self` accessors — data for a tool, never a print path.

use std::collections::VecDeque;
use std::time::Duration;

use unicode_script::Script;

use crate::shaping::Direction;
use crate::{FontFaceId, FontRole, GlyphKey, Representation};

/// Whether explanations are recorded in this build.
pub const ENABLED: bool = cfg!(any(test, feature = "inspector"));

/// How many entries each log keeps before it drops its oldest.
pub const TRACE_CAPACITY: usize = 1024;

/// A bounded log of the most recent entries, oldest first.
#[derive(Debug, Clone)]
pub struct TraceLog<T> {
    entries: VecDeque<T>,
}

impl<T> Default for TraceLog<T> {
    fn default() -> Self {
        Self {
            entries: VecDeque::new(),
        }
    }
}

impl<T> TraceLog<T> {
    /// Append `entry`, dropping the oldest when full.
    pub fn push(&mut self, entry: T) {
        if self.entries.len() == TRACE_CAPACITY {
            self.entries.pop_front();
        }
        self.entries.push_back(entry);
    }

    /// The entries, oldest first.
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = &T> + '_ {
        self.entries.iter()
    }

    /// The newest entry.
    pub fn last(&self) -> Option<&T> {
        self.entries.back()
    }

    pub(crate) fn last_mut(&mut self) -> Option<&mut T> {
        self.entries.back_mut()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Where a face in a fallback walk came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FallbackSource {
    /// The face the run was requested in. It missed coverage, which is why
    /// the walk ran.
    Requested,
    /// The candidate remembered for the run's shape.
    Remembered,
    /// The platform's answer to a query for the run.
    Platform,
}

/// Why a face in a fallback walk did not take the run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaceDecline {
    /// The face has no glyph for the run's leading cluster.
    NoCoverage,
    /// The face covers the run but could not be made resident to shape with.
    NotResident,
    /// The platform was asked and offered no face.
    DeclinedByProvider,
}

/// What one face in a fallback walk did with the run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaceOutcome {
    /// The face took the leading `mapped_len` bytes of the run.
    Mapped {
        mapped_len: usize,
    },
    Declined(FaceDecline),
}

/// One face a fallback walk tried.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FallbackStep {
    pub source: FallbackSource,
    /// `None` when the platform offered no face.
    pub face: Option<FontFaceId>,
    pub outcome: FaceOutcome,
}

/// One fallback walk: what was asked for, every face tried in order with why
/// it declined, and what was chosen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FallbackTrace {
    /// The requested face, which stands for the requested family.
    pub base: FontFaceId,
    pub role: FontRole,
    /// BCP-47 locale hint, empty when none.
    pub locale: String,
    pub script: Script,
    /// The run the requested face could not cover.
    pub run: String,
    pub steps: Vec<FallbackStep>,
    /// The face the run shapes with; `None` draws the requested face's notdef.
    pub chosen: Option<FontFaceId>,
}

impl FallbackTrace {
    /// A walk for `run` that begins with the requested face declining it.
    pub(crate) fn begin(
        base: FontFaceId,
        role: FontRole,
        locale: &str,
        script: Script,
        run: &str,
    ) -> Self {
        Self {
            base,
            role,
            locale: locale.to_owned(),
            script,
            run: run.to_owned(),
            steps: vec![FallbackStep {
                source: FallbackSource::Requested,
                face: Some(base),
                outcome: FaceOutcome::Declined(FaceDecline::NoCoverage),
            }],
            chosen: None,
        }
    }

    pub(crate) fn step(
        &mut self,
        source: FallbackSource,
        face: Option<FontFaceId>,
        outcome: FaceOutcome,
    ) {
        self.steps.push(FallbackStep {
            source,
            face,
            outcome,
        });
    }
}

/// A cache key, named by the cache it belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum CacheKey {
    /// A loaded face in the face cache.
    Face(FontFaceId),
    /// A shaped span in the shaping cache.
    Shaping {
        face: FontFaceId,
        direction: Direction,
        locale: String,
        text: String,
    },
    /// A face's coverage set.
    Coverage(FontFaceId),
    /// A glyph in a residency pool.
    Glyph(GlyphKey),
}

/// A cache's occupancy when it missed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BudgetState {
    pub resident_bytes: u64,
    /// `None` for a cache charged against another's budget.
    pub budget_bytes: Option<u64>,
    pub entries: u64,
}

/// Why a key was not resident.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MissCause {
    /// Never resident, or evicted before the ledger's memory reaches.
    Cold,
    /// Evicted earlier. `by` is the admission that needed the room, or the
    /// dropped face it went with; `None` when memory pressure shed it.
    Evicted { by: Option<CacheKey> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheMiss {
    pub key: CacheKey,
    pub budget: BudgetState,
    pub cause: MissCause,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Eviction {
    pub evicted: CacheKey,
    /// The admission that needed the room, or the dropped face the entry went
    /// with; `None` when memory pressure shed it.
    pub by: Option<CacheKey>,
}

/// One cache's misses and evictions. A miss names the eviction that caused it
/// when the key is still in the eviction log.
#[derive(Debug, Clone, Default)]
pub struct MissLedger {
    misses: TraceLog<CacheMiss>,
    evictions: TraceLog<Eviction>,
}

impl MissLedger {
    /// Record that `evicted` left the cache, for `by`. Callers building a key
    /// only to record it check [`ENABLED`] first.
    pub fn evicted(&mut self, evicted: CacheKey, by: Option<CacheKey>) {
        if ENABLED {
            self.evictions.push(Eviction { evicted, by });
        }
    }

    /// Record that `key` missed with the cache at `budget`.
    pub fn missed(&mut self, key: CacheKey, budget: BudgetState) {
        if !ENABLED {
            return;
        }
        let cause = self
            .evictions
            .iter()
            .rev()
            .find(|eviction| eviction.evicted == key)
            .map_or(MissCause::Cold, |eviction| MissCause::Evicted {
                by: eviction.by.clone(),
            });
        self.misses.push(CacheMiss { key, budget, cause });
    }

    pub fn misses(&self) -> &TraceLog<CacheMiss> {
        &self.misses
    }

    pub fn evictions(&self) -> &TraceLog<Eviction> {
        &self.evictions
    }
}

/// A run's promotion state: the representation it draws, the one it waits
/// for, and the observed transform the hysteresis reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RepresentationExplanation {
    pub drawn: Representation,
    pub pending: Option<Representation>,
    /// `pending` was produced and is drawn from the next frame.
    pub pending_ready: bool,
    /// Raster bucket of the last evaluated transform.
    pub bucket: u16,
    pub rotated: bool,
    pub world_space: bool,
    /// Raster-bucket changes in the current streak and when it began; `None`
    /// while the transform holds still.
    pub streak: Option<(u32, Duration)>,
    /// When the current rotation began.
    pub rotated_since: Option<Duration>,
    /// Promotion is suppressed until then after a promoted representation was
    /// lost.
    pub cooldown_until: Option<Duration>,
}

/// One resident glyph: its representation, where it lives, and its promotion
/// state when a run tracks one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GlyphExplanation {
    /// Carries the representation kind and resolution bucket.
    pub key: GlyphKey,
    pub page: usize,
    pub generation: u32,
    /// `None` when no promotion state drives the glyph: it draws the kind its
    /// face produces at its raster size.
    pub promotion: Option<RepresentationExplanation>,
}

/// Everything the text runtime can explain, as of now.
#[derive(Debug, Clone, Default)]
pub struct TextInspection {
    pub fallbacks: Vec<FallbackTrace>,
    pub faces: MissLedger,
    pub shaping: MissLedger,
    pub coverage: MissLedger,
    /// One ledger per residency pool, keys naming their kind.
    pub glyphs: Vec<MissLedger>,
    pub resident: Vec<GlyphExplanation>,
}
