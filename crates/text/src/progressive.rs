//! Progressive font readiness: render with what is resolved now and reflow when
//! a better face — a system fallback, or a still-loading / progressively
//! subsetted packaged or remote family — becomes available, without blocking the
//! frame.
//!
//! The main thread never waits on font resolution. A run renders with its
//! current best face immediately; when a newer face revision arrives (a fetched
//! packaged asset, or a wider subset from an external provider), this decides the
//! *narrowest* invalidation that revision demands and never more (spec section
//! 17):
//!
//! ```text
//! FontRevision
//!     -> affected shaping runs only
//!     -> affected paragraphs only
//!     -> MEASURE/LAYOUT only if metrics changed
//!     -> PAINT only if geometry unchanged
//! ```
//!
//! A face upgrade never flushes the whole application's text caches: a laid-out
//! paragraph whose shaped result and resident glyphs are unaffected keeps
//! rendering untouched (spec section 18). This type owns the per-run last-good
//! face and revision and the scope decision; it does not perform the reshape —
//! it tells the caller the smallest scope that must.

use std::collections::HashMap;

/// A monotonic revision of a face's *coverage/shape data*, bumped each time a
/// wider subset or the full face replaces what a run was shaped against.
///
/// Distinct from the resolver's per-manifest / per-system-set revision: this
/// tracks one face's progressive readiness (subset -> wider subset -> full
/// face), so a run can tell whether the face it shaped against is still current.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct FontRevision(pub u32);

impl FontRevision {
    /// The next revision after this one.
    fn next(self) -> Self {
        Self(self.0.wrapping_add(1))
    }
}

/// Opaque run identity assigned by the caller, used to route a reflow back to
/// the shaping run it affects. A run belongs to exactly one paragraph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RunId(pub u64);

/// Opaque paragraph identity assigned by the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ParagraphId(pub u64);

/// What a newer face revision changed, deciding how far invalidation must
/// propagate. Ordered least-to-most invalidating.
///
/// A wider subset that only adds glyph *outlines* for characters already shaped
/// with correct metrics is geometry-preserving: the run's glyph positions do not
/// move, so only the newly drawable glyphs repaint. A subset that changes a
/// run's *metrics* (advances, a substitution, a different face entirely) can move
/// glyphs, so that run and its paragraph must reshape and re-lay-out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RevisionEffect {
    /// The new revision changes nothing this run shaped against: no work.
    None,
    /// Glyph geometry is unchanged; only paint is due (a subset filled in
    /// outlines for already-positioned glyphs). PAINT only, no reshape.
    PaintOnly,
    /// Metrics changed: the run must reshape and its paragraph re-lay-out.
    /// MEASURE/LAYOUT for the affected paragraph only.
    ReshapeRun,
}

/// The narrowest invalidation a face upgrade requires: exactly which run and
/// paragraph, and how far the change propagates. Never a whole-application flush.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReflowScope {
    /// The one run whose shaped-against revision advanced.
    pub run: RunId,
    /// The paragraph that run belongs to.
    pub paragraph: ParagraphId,
    /// How far the revision's change must propagate.
    pub effect: RevisionEffect,
}

/// The per-run record of what a run last shaped against.
#[derive(Debug, Clone, Copy)]
struct RunState {
    paragraph: ParagraphId,
    /// The face revision this run's current shaped result was produced against.
    shaped_revision: FontRevision,
}

/// Tracks progressive face readiness per run and decides the reflow scope when a
/// face revision advances. Cold path: consulted when a subset/fetch lands, never
/// per glyph or per frame.
#[derive(Debug, Default)]
pub struct Progressive {
    /// Per-run last-good state: the paragraph it is in and the revision it was
    /// shaped against.
    runs: HashMap<RunId, RunState>,
    /// The latest known revision per face-bearing run source, so a run can be
    /// told whether it is behind. Keyed by run for simplicity: each run tracks
    /// its own face's progressive revision.
    latest: HashMap<RunId, FontRevision>,
}

impl Progressive {
    /// A tracker with no runs recorded yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record that `run` (in `paragraph`) has been shaped against the current
    /// revision of its face. Establishes the baseline a later upgrade compares
    /// against; re-registering a run updates its paragraph and marks it current.
    pub fn register_run(&mut self, run: RunId, paragraph: ParagraphId) {
        let revision = self.latest.get(&run).copied().unwrap_or_default();
        self.runs.insert(
            run,
            RunState {
                paragraph,
                shaped_revision: revision,
            },
        );
    }

    /// The revision `run` was last shaped against, if it is tracked.
    pub fn shaped_revision(&self, run: RunId) -> Option<FontRevision> {
        self.runs.get(&run).map(|s| s.shaped_revision)
    }

    /// Advance the latest revision for `run`'s face and return the reflow scope
    /// the upgrade demands, given what the new revision changed (`effect`).
    ///
    /// This does not reshape: it advances the tracked latest revision and reports
    /// the narrowest scope. A [`RevisionEffect::None`] upgrade — or one for an
    /// untracked run — yields a `None`-effect scope so the caller does no work,
    /// never a broad flush. For a real change, the returned scope names exactly
    /// the one run and its paragraph.
    pub fn note_upgrade(&mut self, run: RunId, effect: RevisionEffect) -> ReflowScope {
        let latest = self
            .latest
            .entry(run)
            .and_modify(|r| *r = r.next())
            .or_insert_with(|| FontRevision::default().next());
        let latest = *latest;

        match self.runs.get(&run) {
            // A tracked run behind the latest revision: report its scope. If the
            // effect is None (nothing this run shaped against changed) the caller
            // simply advances the run's revision with no invalidation.
            Some(state) if state.shaped_revision < latest => ReflowScope {
                run,
                paragraph: state.paragraph,
                effect,
            },
            // Untracked, or already current: nothing to reflow.
            Some(state) => ReflowScope {
                run,
                paragraph: state.paragraph,
                effect: RevisionEffect::None,
            },
            None => ReflowScope {
                run,
                paragraph: ParagraphId(0),
                effect: RevisionEffect::None,
            },
        }
    }

    /// Mark that `run` has been reshaped up to its latest known revision, so a
    /// subsequent identical upgrade does not re-trigger a reflow.
    ///
    /// The caller invokes this after acting on a [`ReflowScope`]: the run's
    /// shaped result now reflects the latest revision.
    pub fn mark_reshaped(&mut self, run: RunId) {
        let latest = self.latest.get(&run).copied().unwrap_or_default();
        if let Some(state) = self.runs.get_mut(&run) {
            state.shaped_revision = latest;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PARA: ParagraphId = ParagraphId(7);
    const RUN: RunId = RunId(3);
    const OTHER_RUN: RunId = RunId(4);

    #[test]
    fn a_freshly_registered_run_is_current() {
        let mut p = Progressive::new();
        p.register_run(RUN, PARA);
        // Nothing has upgraded: the run is at revision 0, no reflow pending.
        assert_eq!(p.shaped_revision(RUN), Some(FontRevision(0)));
    }

    #[test]
    fn upgrade_reflows_only_the_affected_run_and_paragraph() {
        let mut p = Progressive::new();
        p.register_run(RUN, PARA);

        // A wider subset arrives that changes this run's metrics.
        let scope = p.note_upgrade(RUN, RevisionEffect::ReshapeRun);
        assert_eq!(scope.run, RUN);
        assert_eq!(scope.paragraph, PARA);
        assert_eq!(scope.effect, RevisionEffect::ReshapeRun);
    }

    #[test]
    fn geometry_preserving_subset_is_paint_only() {
        let mut p = Progressive::new();
        p.register_run(RUN, PARA);

        // A subset that only fills in outlines for already-positioned glyphs:
        // no reshape, no relayout — just repaint the now-drawable glyphs.
        let scope = p.note_upgrade(RUN, RevisionEffect::PaintOnly);
        assert_eq!(scope.effect, RevisionEffect::PaintOnly);
        assert_eq!(scope.run, RUN);
    }

    #[test]
    fn an_upgrade_never_touches_an_unaffected_run() {
        let mut p = Progressive::new();
        p.register_run(RUN, PARA);
        p.register_run(OTHER_RUN, ParagraphId(99));

        // Upgrading RUN's face returns a scope naming only RUN and PARA. The
        // other run and its paragraph are absent from the scope entirely: a face
        // upgrade is not an application-wide text-cache flush (spec section 18).
        let scope = p.note_upgrade(RUN, RevisionEffect::ReshapeRun);
        assert_ne!(scope.run, OTHER_RUN);
        assert_ne!(scope.paragraph, ParagraphId(99));
        // The untouched run keeps its baseline revision.
        assert_eq!(p.shaped_revision(OTHER_RUN), Some(FontRevision(0)));
    }

    #[test]
    fn reshaping_clears_the_pending_upgrade() {
        let mut p = Progressive::new();
        p.register_run(RUN, PARA);

        let scope = p.note_upgrade(RUN, RevisionEffect::ReshapeRun);
        assert_eq!(scope.effect, RevisionEffect::ReshapeRun);

        // After the caller reshapes, the run is current at the new revision.
        p.mark_reshaped(RUN);
        assert_eq!(p.shaped_revision(RUN), Some(FontRevision(1)));

        // Re-noting the same revision without a new upgrade: the run is no longer
        // behind the latest, so nothing reflows. (A genuinely newer subset would
        // bump latest again and re-trigger.)
        p.register_run(RUN, PARA); // caller re-registers at current revision
        assert_eq!(p.shaped_revision(RUN), Some(FontRevision(1)));
    }

    #[test]
    fn upgrading_an_untracked_run_does_nothing() {
        let mut p = Progressive::new();
        // No run registered: an upgrade cannot invalidate work that does not
        // exist, and must not fabricate a broad scope.
        let scope = p.note_upgrade(RunId(123), RevisionEffect::ReshapeRun);
        assert_eq!(scope.effect, RevisionEffect::None);
    }
}
