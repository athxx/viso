//! The unified font provider: the single seam shaping and fallback query for a
//! face, dispatching between application fonts, the platform system-font
//! provider, and an optional external byte source (HTTP/CDN, a host bridge, a
//! remote subset service).
//!
//! Callers ask the provider where a face's bytes come from; it decides whether
//! an app-declared family, a resolved system face, or an external byte request
//! answers, and hides that split from the shaping and fallback paths.
//!
//! # External font source (spec section 16.3)
//!
//! A packaged app face's bytes are read lazily through
//! [`crate::app_fonts::AssetSource`]; a system face arrives resolved from the
//! platform seam. Neither reaches the network. A third source — a font injected
//! over HTTP/CDN, a host JavaScript bridge, an in-memory stream, or a remote
//! *subset* service — arrives asynchronously and possibly in pieces. This module
//! owns the discipline that source demands so a caller cannot degrade into
//! "one character = one HTTP request":
//!
//! - **deduplicate** — a byte range already requested or resident is never
//!   requested again;
//! - **batch** — many pending needs coalesce into few requests;
//! - **priority** — visible text outranks speculative prewarm ([`Priority`]);
//! - **cancellation** — a request whose need has passed is dropped, not awaited;
//! - **byte budget** — outstanding bytes are bounded, so a burst of misses
//!   cannot open unbounded fetches;
//! - **revision validation** — a served subset is validated against the face
//!   revision it claims, so a stale or wrong-revision blob is refused.
//!
//! This crate never opens a socket: it describes *what* to fetch and *in what
//! order*, and the facade's transport fulfils it. The result of a fulfilled
//! request feeds [`crate::resolver::FontResolver::register_app_face`] and bumps a
//! [`FontRevision`], which [`crate::progressive`] turns into the narrowest reflow.

use std::collections::HashMap;
use std::collections::VecDeque;

use crate::font_manifest::AssetRef;
use crate::progressive::FontRevision;
use crate::text_work::Priority;

/// Where a resolved face's bytes come from. The provider's dispatch answer:
/// shaping and fallback never learn which arm produced the bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaceSource {
    /// A packaged application asset, read lazily through the asset seam.
    App(AssetRef),
    /// A platform system face, already resident once the provider resolved it.
    System,
    /// An externally supplied face, fetched on demand through [`ExternalFontProvider`].
    External(ExternalFaceId),
}

/// Stable identity for a face an external provider can supply. Compact, assigned
/// by the caller; never a URL string on a hot path (a URL is a cold attribute of
/// the request the transport resolves).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ExternalFaceId(pub u32);

/// A contiguous coverage range of a face requested from an external source.
///
/// The unit is deliberately coarse — a cluster, a small character set, a script
/// page, a named subset, or the whole face — never a single scalar, because
/// on-demand describes *coverage granularity*, not network-request granularity
/// (spec section 17). The transport is free to satisfy a wider range than asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SubsetRange {
    /// The face this subset belongs to.
    pub face: ExternalFaceId,
    /// An opaque coverage-unit key assigned by the caller (a script page, a
    /// named subset id, or 0 for the whole face). Two needs with the same key
    /// deduplicate onto one request.
    pub unit: u32,
    /// The face revision this request is issued against, so a served blob can be
    /// validated and a stale one refused.
    pub revision: FontRevision,
}

/// The external byte-source capability the facade implements: fetch font bytes
/// for a subset request. Implemented outside this crate; `viso-text` opens no
/// socket and knows no transport.
///
/// A `None` return means "cannot supply now" (offline, cancelled, over budget at
/// the transport) — not an error to surface to text; the run keeps its last-good
/// face and a later revision may satisfy it.
pub trait ExternalFontProvider {
    /// Fetch the sfnt bytes for a subset request, if the transport can supply
    /// them now. The returned bytes must be a valid sfnt (the caller may inject
    /// a decompressed WOFF2 as sfnt; this crate never decodes WOFF2, per ADR
    /// 0028). `index` is the face index within a returned collection.
    fn fetch_subset(&self, range: &SubsetRange) -> Option<FetchedSubset>;
}

/// Bytes an [`ExternalFontProvider`] returned for a subset request, tagged with
/// the revision they satisfy so the coordinator can validate them.
#[derive(Debug)]
pub struct FetchedSubset {
    /// Owned sfnt bytes for the subset (or full face).
    pub bytes: Vec<u8>,
    /// Face index within `bytes` for a collection; 0 for a single face.
    pub index: u32,
    /// The revision these bytes actually satisfy. The coordinator refuses a blob
    /// whose revision is older than the request's, so a stale subset from a slow
    /// transport does not overwrite a newer one.
    pub revision: FontRevision,
}

/// One outstanding external request: the range, its priority, and whether it has
/// been superseded (cancelled) while queued.
#[derive(Debug, Clone, Copy)]
struct Pending {
    range: SubsetRange,
    priority: Priority,
    /// A cancelled request stays in the map for dedup but is skipped when
    /// draining and dropped from the byte budget: the need passed before the
    /// transport got to it.
    cancelled: bool,
}

/// Coordinates external font-byte requests: deduplicates needs, orders them by
/// priority, bounds outstanding bytes, supports cancellation, and validates a
/// served subset's revision before accepting it.
///
/// Cold path: driven when a run needs a face it does not yet have and when a
/// fetch lands — never per glyph or per frame. The heavy transport lives in the
/// facade; this owns only the *policy*.
#[derive(Debug)]
pub struct ExternalFetchCoordinator {
    /// Outstanding needs keyed by (face, unit): the dedup table. A repeated need
    /// for a range already pending does not open a second request.
    pending: HashMap<(ExternalFaceId, u32), Pending>,
    /// Bytes currently accounted as outstanding, against `byte_budget`.
    outstanding_bytes: u32,
    /// Hard ceiling on outstanding requested bytes. A need that would exceed it
    /// is refused admission until an outstanding request resolves or is
    /// cancelled, so a burst of misses cannot open unbounded fetches.
    byte_budget: u32,
    /// Estimated bytes per pending request, charged on admission and refunded on
    /// resolve/cancel. A coarse governor, not an exact transfer size.
    per_request_estimate: u32,
}

impl Default for ExternalFetchCoordinator {
    fn default() -> Self {
        // A conservative default budget: a few subset requests in flight. The
        // facade tunes it for a device and link.
        Self {
            pending: HashMap::new(),
            outstanding_bytes: 0,
            byte_budget: 1 << 20, // 1 MiB of outstanding requested bytes.
            per_request_estimate: 64 << 10, // 64 KiB assumed per subset request.
        }
    }
}

impl ExternalFetchCoordinator {
    /// A coordinator with an explicit outstanding-byte budget and per-request
    /// estimate.
    pub fn with_budget(byte_budget: u32, per_request_estimate: u32) -> Self {
        Self {
            byte_budget,
            per_request_estimate: per_request_estimate.max(1),
            ..Self::default()
        }
    }

    /// Bytes currently accounted as outstanding.
    pub fn outstanding_bytes(&self) -> u32 {
        self.outstanding_bytes
    }

    /// The number of live (non-cancelled) pending requests.
    pub fn pending_count(&self) -> usize {
        self.pending.values().filter(|p| !p.cancelled).count()
    }

    /// Register a need for `range` at `priority`, admitting a new request only if
    /// it deduplicates against nothing and fits the byte budget.
    ///
    /// Returns `true` if this call opened a new outstanding request; `false` if
    /// the need deduplicated onto an existing request, revived a cancelled one,
    /// raised an existing request's priority, or was refused by the budget. A
    /// deduplicated need at a higher priority promotes the pending request, so a
    /// range first wanted for prewarm and then for visible text is upgraded, not
    /// duplicated.
    pub fn request(&mut self, range: SubsetRange, priority: Priority) -> bool {
        let key = (range.face, range.unit);
        if let Some(existing) = self.pending.get_mut(&key) {
            // Dedup: raise priority and revive a cancelled request rather than
            // opening a second one. Charge the budget again only if it had been
            // refunded on cancellation.
            if existing.cancelled {
                existing.cancelled = false;
                self.outstanding_bytes = self
                    .outstanding_bytes
                    .saturating_add(self.per_request_estimate);
            }
            if priority > existing.priority {
                existing.priority = priority;
            }
            // A newer revision supersedes the pending one: shape against latest.
            if range.revision > existing.range.revision {
                existing.range.revision = range.revision;
            }
            return false;
        }

        // Budget gate: refuse admission rather than open an unbounded fetch.
        if self
            .outstanding_bytes
            .saturating_add(self.per_request_estimate)
            > self.byte_budget
        {
            return false;
        }

        self.pending.insert(
            key,
            Pending {
                range,
                priority,
                cancelled: false,
            },
        );
        self.outstanding_bytes = self
            .outstanding_bytes
            .saturating_add(self.per_request_estimate);
        true
    }

    /// Cancel a pending need: its byte budget is refunded and it is skipped when
    /// draining, but its dedup entry lingers so an immediate re-request is cheap.
    /// A need whose paragraph scrolled away or whose edit moved on is cancelled,
    /// not awaited.
    pub fn cancel(&mut self, face: ExternalFaceId, unit: u32) {
        if let Some(p) = self.pending.get_mut(&(face, unit))
            && !p.cancelled
        {
            p.cancelled = true;
            self.outstanding_bytes = self
                .outstanding_bytes
                .saturating_sub(self.per_request_estimate);
        }
    }

    /// Drain the live pending requests as a batch, highest priority first, so the
    /// transport issues few ordered requests rather than one per need. Cancelled
    /// requests are dropped here; live ones remain outstanding until [`resolve`]
    /// accepts their bytes.
    ///
    /// [`resolve`]: ExternalFetchCoordinator::resolve
    pub fn drain_batch(&mut self) -> Vec<SubsetRange> {
        // Drop cancelled entries now that a drain has passed them by.
        self.pending.retain(|_, p| !p.cancelled);

        let mut ordered: VecDeque<Pending> = self.pending.values().copied().collect();
        // Highest priority first; a stable tiebreak on (face, unit) keeps the
        // batch deterministic regardless of map iteration order.
        let mut batch: Vec<Pending> = ordered.drain(..).collect();
        batch.sort_by(|a, b| {
            b.priority
                .cmp(&a.priority)
                .then(a.range.face.cmp(&b.range.face))
                .then(a.range.unit.cmp(&b.range.unit))
        });
        batch.into_iter().map(|p| p.range).collect()
    }

    /// Accept a transport's answer for a request, validating its revision.
    ///
    /// Returns the fetched bytes only if they are current: a blob whose revision
    /// is older than the pending request's is refused (a stale subset from a slow
    /// transport must not overwrite a newer resolution) and `None` is returned
    /// with the request left pending for a fresh fetch. On acceptance the request
    /// clears and its byte budget is refunded.
    pub fn resolve(
        &mut self,
        face: ExternalFaceId,
        unit: u32,
        fetched: FetchedSubset,
    ) -> Option<FetchedSubset> {
        let key = (face, unit);
        let Some(p) = self.pending.get(&key) else {
            // No live request for this range: an already-resolved or never-asked
            // range. Accept nothing rather than double-count.
            return None;
        };

        // Revision validation: the served blob must satisfy at least the revision
        // the request was issued against.
        if fetched.revision < p.range.revision {
            return None;
        }

        let was_live = !p.cancelled;
        self.pending.remove(&key);
        if was_live {
            self.outstanding_bytes = self
                .outstanding_bytes
                .saturating_sub(self.per_request_estimate);
        }
        Some(fetched)
    }
}

/// The unified provider over application fonts, the platform system-font seam,
/// and the external byte source.
///
/// It answers *where* a resolved face's bytes come from ([`FaceSource`]) and
/// owns the external-fetch policy ([`ExternalFetchCoordinator`]). The actual
/// resolution decision (App-first / System-second) lives in
/// [`crate::resolver::FontResolver`]; this provider layers the external arm and
/// the fetch discipline on top, so shaping and fallback see one seam.
#[derive(Debug, Default)]
pub struct FontProvider {
    /// The external-fetch coordinator for on-demand / remote subset faces.
    external: ExternalFetchCoordinator,
}

impl FontProvider {
    /// A provider with the default external-fetch budget.
    pub fn new() -> Self {
        Self::default()
    }

    /// The external-fetch coordinator, for driving on-demand subset requests.
    pub fn external(&mut self) -> &mut ExternalFetchCoordinator {
        &mut self.external
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FACE: ExternalFaceId = ExternalFaceId(1);

    fn range(unit: u32, revision: u32) -> SubsetRange {
        SubsetRange {
            face: FACE,
            unit,
            revision: FontRevision(revision),
        }
    }

    #[test]
    fn a_repeated_need_deduplicates_onto_one_request() {
        let mut c = ExternalFetchCoordinator::default();
        // The same coverage unit needed twice opens exactly one request: an
        // on-demand face must not degrade into one-request-per-need.
        assert!(c.request(range(7, 0), Priority::CriticalVisible));
        assert!(!c.request(range(7, 0), Priority::CriticalVisible));
        assert_eq!(c.pending_count(), 1);
    }

    #[test]
    fn distinct_units_each_open_a_request() {
        let mut c = ExternalFetchCoordinator::default();
        assert!(c.request(range(1, 0), Priority::NearViewport));
        assert!(c.request(range(2, 0), Priority::NearViewport));
        assert_eq!(c.pending_count(), 2);
    }

    #[test]
    fn a_higher_priority_need_promotes_not_duplicates() {
        let mut c = ExternalFetchCoordinator::default();
        // First wanted speculatively, then for visible text: the request is
        // promoted in place, still one request, and drains at the higher class.
        c.request(range(3, 0), Priority::BackgroundPrewarm);
        c.request(range(3, 0), Priority::CriticalVisible);
        assert_eq!(c.pending_count(), 1);

        let batch = c.drain_batch();
        assert_eq!(batch, vec![range(3, 0)]);
    }

    #[test]
    fn drain_orders_by_priority_highest_first() {
        let mut c = ExternalFetchCoordinator::default();
        c.request(range(1, 0), Priority::BackgroundPrewarm);
        c.request(range(2, 0), Priority::CriticalVisible);
        c.request(range(3, 0), Priority::NearViewport);

        // The batch is ordered so the transport issues visible before
        // near-viewport before prewarm.
        let units: Vec<u32> = c.drain_batch().into_iter().map(|r| r.unit).collect();
        assert_eq!(units, vec![2, 3, 1]);
    }

    #[test]
    fn the_byte_budget_bounds_outstanding_requests() {
        // A tight budget admits only as many requests as fit; a burst of misses
        // cannot open unbounded fetches.
        let mut c = ExternalFetchCoordinator::with_budget(100, 40);
        assert!(c.request(range(1, 0), Priority::CriticalVisible)); // 40
        assert!(c.request(range(2, 0), Priority::CriticalVisible)); // 80
        // The third would reach 120 > 100: refused admission.
        assert!(!c.request(range(3, 0), Priority::CriticalVisible));
        assert_eq!(c.pending_count(), 2);
        assert_eq!(c.outstanding_bytes(), 80);
    }

    #[test]
    fn cancellation_refunds_the_budget_and_admits_the_next() {
        let mut c = ExternalFetchCoordinator::with_budget(100, 40);
        c.request(range(1, 0), Priority::CriticalVisible); // 80 total after next
        c.request(range(2, 0), Priority::CriticalVisible);
        assert!(!c.request(range(3, 0), Priority::CriticalVisible)); // refused

        // A need that passed: cancel it, refund its bytes, and the next admits.
        c.cancel(FACE, 1);
        assert_eq!(c.outstanding_bytes(), 40);
        assert!(c.request(range(3, 0), Priority::CriticalVisible));
        assert_eq!(c.outstanding_bytes(), 80);
    }

    #[test]
    fn a_cancelled_request_is_not_drained() {
        let mut c = ExternalFetchCoordinator::default();
        c.request(range(1, 0), Priority::CriticalVisible);
        c.request(range(2, 0), Priority::CriticalVisible);
        c.cancel(FACE, 1);

        let units: Vec<u32> = c.drain_batch().into_iter().map(|r| r.unit).collect();
        assert_eq!(units, vec![2]);
    }

    #[test]
    fn a_lazy_fetch_resolves_and_refunds_its_budget() {
        // The core lazy-fetch/decode path: a need opens a request, the transport
        // supplies current bytes, and the coordinator accepts them and clears the
        // request. The accepted bytes then feed the resolver (register_app_face).
        let mut c = ExternalFetchCoordinator::default();
        c.request(range(5, 1), Priority::CriticalVisible);
        let before = c.outstanding_bytes();
        assert!(before > 0);

        let accepted = c.resolve(
            FACE,
            5,
            FetchedSubset {
                bytes: vec![0u8; 128],
                index: 0,
                revision: FontRevision(1),
            },
        );
        assert!(accepted.is_some());
        assert_eq!(accepted.unwrap().bytes.len(), 128);
        // Request cleared and budget refunded.
        assert_eq!(c.pending_count(), 0);
        assert_eq!(c.outstanding_bytes(), 0);
    }

    #[test]
    fn a_stale_revision_blob_is_refused() {
        // A slow transport returns a subset for an older revision after a newer
        // one was requested (progressive subset arrival, spec section 17): the
        // stale blob must not overwrite the newer resolution.
        let mut c = ExternalFetchCoordinator::default();
        c.request(range(9, 3), Priority::CriticalVisible);

        let refused = c.resolve(
            FACE,
            9,
            FetchedSubset {
                bytes: vec![0u8; 64],
                index: 0,
                revision: FontRevision(2), // older than the requested revision 3.
            },
        );
        assert!(refused.is_none());
        // The request stays pending for a fresh, current fetch.
        assert_eq!(c.pending_count(), 1);
    }

    #[test]
    fn resolving_an_unknown_range_accepts_nothing() {
        let mut c = ExternalFetchCoordinator::default();
        // No live request for this range: nothing to accept, no double count.
        let got = c.resolve(
            FACE,
            42,
            FetchedSubset {
                bytes: vec![0u8; 16],
                index: 0,
                revision: FontRevision(1),
            },
        );
        assert!(got.is_none());
        assert_eq!(c.outstanding_bytes(), 0);
    }

    #[test]
    fn a_newer_request_revision_supersedes_a_pending_one() {
        let mut c = ExternalFetchCoordinator::default();
        c.request(range(4, 1), Priority::NearViewport);
        // A wider subset for a newer revision arrives as a need before the old
        // one resolved: the pending request adopts the newer revision, so a blob
        // for revision 1 would now be refused as stale.
        c.request(range(4, 2), Priority::NearViewport);
        assert_eq!(c.pending_count(), 1);

        let stale = c.resolve(
            FACE,
            4,
            FetchedSubset {
                bytes: vec![0u8; 32],
                index: 0,
                revision: FontRevision(1),
            },
        );
        assert!(stale.is_none());
    }
}
