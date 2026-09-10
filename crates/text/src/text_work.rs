//! The worker scheduling seam: describe shaping / layout / rasterization work
//! as prioritized jobs so all heavy text work runs off the main thread with
//! zero main-thread work in the steady state.
//!
//! The main thread submits typed requests and reads results through queues; it
//! never shapes, breaks, or rasterizes inline. Jobs carry a [`Priority`] so a
//! worker drains critical, visible work before speculative prewarm. Prediction
//! prewarm lets the worker begin likely-next work (the next input glyphs)
//! before it is demanded, under a byte/count budget that converges when its
//! hit rate is low and yields to critical work under pressure. This crate owns
//! the job description and the scheduling policy; the facade owns the thread
//! pool and drives the queue.
//!
//! # Priority classes
//!
//! Cold text work is ordered by [`Priority`], highest first: on-screen glyphs
//! before near-viewport, interactive edits before background prewarm, catalog
//! previews last. A worker always drains higher-priority jobs first, so a burst
//! of speculative prewarm can never starve a visible glyph or an edit.
//!
//! # Prewarm is budgeted and self-limiting
//!
//! Prewarm bets on the next input: the hot characters at the edit point, common
//! successors, the current IME composition candidates — never the whole alphabet
//! or the whole face. It is bounded by a byte and a count budget per drain, and
//! it converges (shrinks its budget) when its observed hit rate is low, so a bad
//! bet wastes bounded worker CPU rather than unbounded. Under memory or worker
//! pressure prewarm yields entirely to [`Priority::CriticalVisible`] and
//! [`Priority::NearViewport`].
//!
//! # Miss fallback keeps the main thread at zero shaping
//!
//! When a predicted glyph is not warm at edit time, the main thread never
//! synchronously reshapes to cover the miss. Instead the caret/selection
//! geometry advances immediately (no reshape), the dirty run's glyphs are filled
//! by the worker and land a frame or two later, and a brief visual lag is
//! accepted. [`MainThreadEditStep`] records exactly this: the main thread's edit
//! step performs zero shaping on both the warm-hit and the cold-miss path.

use std::collections::VecDeque;

/// The priority class a text job is scheduled at. Ordered so the derived
/// ordering ranks more-urgent work higher: a worker drains the greatest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Priority {
    /// Lowest: speculative catalog/preview shaping for off-screen browsing UI.
    CatalogPreview,
    /// Speculative prewarm of likely-next input; yields to everything above it.
    BackgroundPrewarm,
    /// A local edit's dirty run: must land within a display frame or two.
    InteractiveEdit,
    /// Content just outside the viewport, likely to scroll in next.
    NearViewport,
    /// Highest: glyphs on screen this frame.
    CriticalVisible,
}

impl Priority {
    /// Whether this is the speculative prewarm class. Prewarm is the work that
    /// yields under pressure, so this is the class [`TextWork::take_next`] skips
    /// while pressured (it stays queued for when pressure clears).
    fn is_prewarm(self) -> bool {
        matches!(self, Priority::BackgroundPrewarm)
    }
}

/// What a job asks a worker to produce. This crate describes the work; the
/// facade's worker pool executes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobKind {
    /// Shape and lay out a paragraph (or a dirty run within one).
    Paragraph {
        /// Opaque paragraph identity assigned by the caller, for result routing.
        paragraph: u64,
    },
    /// Rasterize a glyph into its resolved representation.
    Rasterize {
        /// Opaque glyph identity for result routing.
        glyph: u64,
    },
    /// Speculatively shape a predicted-next character so an imminent edit hits a
    /// warm result instead of forcing a main-thread reshape.
    PrewarmChar {
        /// The predicted scalar's UTF-8 byte length: the prewarm budget unit.
        bytes: u32,
        /// The predicted scalar, for hit accounting and result routing.
        scalar: char,
    },
}

impl JobKind {
    /// The prewarm byte cost of this job: nonzero only for prewarm jobs, which
    /// are the only work the byte budget bounds.
    fn prewarm_bytes(&self) -> u32 {
        match self {
            JobKind::PrewarmChar { bytes, .. } => *bytes,
            _ => 0,
        }
    }
}

/// A prioritized unit of text work handed to a worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextJob {
    pub priority: Priority,
    pub kind: JobKind,
}

/// The prewarm budget: hard byte and count caps per drain, plus a hit-rate
/// governor that shrinks the effective budget when speculation is not paying
/// off. Cold path only — consulted when scheduling prewarm, never per glyph.
#[derive(Debug, Clone, Copy)]
pub struct PrewarmBudget {
    /// Hard ceiling on prewarm bytes admitted per drain window.
    max_bytes: u32,
    /// Hard ceiling on prewarm jobs admitted per drain window.
    max_count: u32,
    /// Prewarm jobs whose predicted glyph was later demanded (a good bet).
    hits: u32,
    /// Prewarm jobs completed but never demanded (a wasted bet).
    misses: u32,
}

impl Default for PrewarmBudget {
    fn default() -> Self {
        // A conservative default: a handful of successor characters, a few dozen
        // bytes. The facade may tune these for a device.
        Self {
            max_bytes: 64,
            max_count: 8,
            hits: 0,
            misses: 0,
        }
    }
}

impl PrewarmBudget {
    /// A budget with explicit caps.
    pub fn new(max_bytes: u32, max_count: u32) -> Self {
        Self {
            max_bytes,
            max_count,
            hits: 0,
            misses: 0,
        }
    }

    /// The observed prewarm hit rate in `[0, 1]`, or `1.0` before any outcome is
    /// known (speculation is given the benefit of the doubt at first).
    pub fn hit_rate(&self) -> f32 {
        let total = self.hits + self.misses;
        if total == 0 {
            1.0
        } else {
            self.hits as f32 / total as f32
        }
    }

    /// The effective count cap after hit-rate convergence: when the hit rate is
    /// low, the budget shrinks toward one so a bad predictor wastes little. The
    /// byte cap scales the same way.
    fn effective_count(&self) -> u32 {
        // Below a 50% hit rate, converge linearly toward a single job.
        let rate = self.hit_rate();
        if rate >= 0.5 {
            self.max_count
        } else {
            // Scale 0..0.5 -> 1..max_count.
            let scaled = (self.max_count as f32 * (rate / 0.5)).floor() as u32;
            scaled.max(1)
        }
    }

    fn effective_bytes(&self) -> u32 {
        let rate = self.hit_rate();
        if rate >= 0.5 {
            self.max_bytes
        } else {
            let scaled = (self.max_bytes as f32 * (rate / 0.5)).floor() as u32;
            scaled.max(1)
        }
    }

    /// Record that a prewarmed glyph was demanded (a good bet).
    pub fn record_hit(&mut self) {
        self.hits += 1;
    }

    /// Record that a prewarmed glyph was never demanded (a wasted bet).
    pub fn record_miss(&mut self) {
        self.misses += 1;
    }
}

/// What the main thread actually did for one edit step. The invariant the whole
/// scheduler exists to preserve: `shaped_on_main` is always zero — the main
/// thread advances caret geometry and dispatches to the worker, but never
/// shapes, on both the warm-hit and the cold-miss path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MainThreadEditStep {
    /// Glyphs the main thread shaped inline. Must always be zero.
    pub shaped_on_main: u32,
    /// Whether the edit hit a warm prewarmed result (no worker dispatch needed
    /// for the glyph) or missed (worker fills it, lands a frame or two later).
    pub warm_hit: bool,
    /// Whether caret/selection geometry advanced this step (always true for a
    /// real edit — geometry never waits on shaping).
    pub caret_advanced: bool,
}

/// The job description and scheduling seam. The facade drives the actual worker
/// pool; this owns the pending queue, the prewarm budget, and the drain policy.
#[derive(Debug, Default)]
pub struct TextWork {
    /// Pending jobs. Kept unordered here; [`Self::next`] selects by priority so
    /// submission order never lets low-priority work jump the queue.
    pending: VecDeque<TextJob>,
    /// The prewarm speculation budget and hit-rate governor.
    budget: PrewarmBudget,
    /// Whether the system is under memory/worker pressure. While set, prewarm
    /// yields entirely to CriticalVisible / NearViewport.
    under_pressure: bool,
}

impl TextWork {
    /// A scheduler with an explicit prewarm budget.
    pub fn with_budget(budget: PrewarmBudget) -> Self {
        Self {
            pending: VecDeque::new(),
            budget,
            under_pressure: false,
        }
    }

    /// Submit a job for a worker to run off the main thread. Non-prewarm jobs are
    /// always accepted; the main thread does zero shaping regardless.
    pub fn submit(&mut self, job: TextJob) {
        self.pending.push_back(job);
    }

    /// Submit prewarm jobs under budget: admit predicted-next work up to the
    /// effective byte and count caps (which shrink when the hit rate is low),
    /// and admit nothing while under pressure — prewarm yields to visible work.
    /// Returns how many were admitted. Excess predictions are dropped, not
    /// queued, so a bad predictor cannot flood the worker.
    pub fn submit_prewarm(&mut self, predicted: impl IntoIterator<Item = JobKind>) -> u32 {
        if self.under_pressure {
            return 0;
        }
        let count_cap = self.budget.effective_count();
        let byte_cap = self.budget.effective_bytes();
        let mut admitted = 0u32;
        let mut bytes = 0u32;
        for kind in predicted {
            debug_assert!(
                matches!(kind, JobKind::PrewarmChar { .. }),
                "submit_prewarm takes only PrewarmChar jobs"
            );
            if admitted >= count_cap {
                break;
            }
            let cost = kind.prewarm_bytes();
            if bytes + cost > byte_cap {
                break;
            }
            bytes += cost;
            admitted += 1;
            self.pending.push_back(TextJob {
                priority: Priority::BackgroundPrewarm,
                kind,
            });
        }
        admitted
    }

    /// Enter or leave memory/worker pressure. Under pressure, prewarm admission
    /// is refused and already-queued prewarm jobs are deprioritized behind all
    /// visible and interactive work (they already sit at the lowest-but-one
    /// class, so [`Self::next`] naturally drains them last).
    pub fn set_pressure(&mut self, under_pressure: bool) {
        self.under_pressure = under_pressure;
    }

    /// Take the highest-priority pending job for a worker to run. Under pressure,
    /// prewarm jobs are skipped so critical/visible work drains first; they
    /// remain queued for when pressure clears. Returns `None` when nothing is
    /// runnable.
    pub fn take_next(&mut self) -> Option<TextJob> {
        // Select the max-priority runnable job. Under pressure, a prewarm job is
        // not runnable (it yields to visible work) but stays queued.
        let mut best: Option<usize> = None;
        for (i, job) in self.pending.iter().enumerate() {
            if self.under_pressure && job.priority.is_prewarm() {
                continue;
            }
            match best {
                Some(b) if self.pending[b].priority >= job.priority => {}
                _ => best = Some(i),
            }
        }
        best.map(|i| self.pending.remove(i).expect("index in range"))
    }

    /// The number of pending jobs, for tests and counters.
    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    /// The prewarm budget/governor, for inspection and outcome recording.
    pub fn budget_mut(&mut self) -> &mut PrewarmBudget {
        &mut self.budget
    }

    /// Perform one main-thread edit step. This is the whole point of the seam:
    /// the main thread advances caret geometry and, on a miss, dispatches the
    /// dirty run to the worker at [`Priority::InteractiveEdit`] — but it never
    /// shapes. Returns a [`MainThreadEditStep`] whose `shaped_on_main` is always
    /// zero, on both the warm-hit and the cold-miss path.
    ///
    /// `warm_hit` says whether the edited glyph was already prewarmed. On a hit
    /// the budget records a hit and no dispatch is needed; on a miss the budget
    /// records a miss, the dirty run is dispatched to the worker (landing a frame
    /// or two later), and the caret still advances immediately by geometry.
    pub fn main_thread_edit_step(
        &mut self,
        dirty_paragraph: u64,
        warm_hit: bool,
    ) -> MainThreadEditStep {
        if warm_hit {
            self.budget.record_hit();
        } else {
            self.budget.record_miss();
            // Miss fallback: dispatch the dirty run to the worker. The main
            // thread does NOT reshape — the glyph is filled asynchronously.
            self.submit(TextJob {
                priority: Priority::InteractiveEdit,
                kind: JobKind::Paragraph {
                    paragraph: dirty_paragraph,
                },
            });
        }
        MainThreadEditStep {
            // The invariant: the main thread shaped nothing.
            shaped_on_main: 0,
            warm_hit,
            caret_advanced: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prewarm(scalar: char) -> JobKind {
        JobKind::PrewarmChar {
            bytes: scalar.len_utf8() as u32,
            scalar,
        }
    }

    #[test]
    fn worker_drains_higher_priority_first() {
        let mut work = TextWork::default();
        // Submit out of priority order.
        work.submit(TextJob {
            priority: Priority::CatalogPreview,
            kind: JobKind::Paragraph { paragraph: 1 },
        });
        work.submit(TextJob {
            priority: Priority::CriticalVisible,
            kind: JobKind::Rasterize { glyph: 2 },
        });
        work.submit(TextJob {
            priority: Priority::InteractiveEdit,
            kind: JobKind::Paragraph { paragraph: 3 },
        });

        // Drained highest priority first regardless of submission order.
        assert_eq!(
            work.take_next().unwrap().priority,
            Priority::CriticalVisible
        );
        assert_eq!(
            work.take_next().unwrap().priority,
            Priority::InteractiveEdit
        );
        assert_eq!(work.take_next().unwrap().priority, Priority::CatalogPreview);
        assert!(work.take_next().is_none());
    }

    #[test]
    fn prewarm_respects_count_cap() {
        // Count cap of 3, generous byte cap: only three predictions admitted.
        let mut work = TextWork::with_budget(PrewarmBudget::new(1000, 3));
        let admitted = work.submit_prewarm(['a', 'b', 'c', 'd', 'e'].map(prewarm));
        assert_eq!(admitted, 3, "count cap bounds prewarm admission");
        assert_eq!(work.pending_len(), 3);
    }

    #[test]
    fn prewarm_respects_byte_cap() {
        // Byte cap of 6: three 3-byte CJK predictions (9 bytes) admit only two.
        let mut work = TextWork::with_budget(PrewarmBudget::new(6, 100));
        let admitted = work.submit_prewarm(['\u{4F60}', '\u{597D}', '\u{554A}'].map(prewarm));
        assert_eq!(
            admitted, 2,
            "byte cap bounds prewarm admission (2 x 3 bytes = 6, third would exceed)"
        );
    }

    #[test]
    fn prewarm_converges_when_hit_rate_is_low() {
        // Drive the hit rate low, then a big prediction batch is throttled well
        // below the nominal count cap.
        let mut work = TextWork::with_budget(PrewarmBudget::new(1000, 10));
        // 1 hit, 9 misses -> 10% hit rate.
        work.budget_mut().record_hit();
        for _ in 0..9 {
            work.budget_mut().record_miss();
        }
        assert!(work.budget_mut().hit_rate() < 0.2);

        let admitted = work.submit_prewarm((0..20u32).map(|i| {
            let c = char::from_u32('a' as u32 + i).unwrap_or('z');
            prewarm(c)
        }));
        assert!(
            admitted < 10,
            "low hit rate converges the budget below the nominal cap (got {admitted})"
        );
        assert!(admitted >= 1, "convergence never drops below one");
    }

    #[test]
    fn prewarm_yields_to_visible_work_under_pressure() {
        let mut work = TextWork::with_budget(PrewarmBudget::new(1000, 10));
        work.set_pressure(true);
        // No prewarm is admitted while under pressure.
        let admitted = work.submit_prewarm(['a', 'b', 'c'].map(prewarm));
        assert_eq!(admitted, 0, "prewarm yields entirely under pressure");
        assert_eq!(work.pending_len(), 0);
    }

    #[test]
    fn queued_prewarm_is_skipped_under_pressure_but_not_dropped() {
        let mut work = TextWork::with_budget(PrewarmBudget::new(1000, 10));
        // Queue prewarm while healthy.
        assert_eq!(work.submit_prewarm(['a', 'b'].map(prewarm)), 2);
        // A critical job arrives; then pressure hits.
        work.submit(TextJob {
            priority: Priority::CriticalVisible,
            kind: JobKind::Rasterize { glyph: 1 },
        });
        work.set_pressure(true);

        // Under pressure the critical job drains but prewarm is skipped.
        assert_eq!(
            work.take_next().unwrap().priority,
            Priority::CriticalVisible
        );
        assert!(
            work.take_next().is_none(),
            "prewarm is not runnable under pressure"
        );
        // It was not dropped: clearing pressure makes it runnable again.
        work.set_pressure(false);
        assert_eq!(
            work.take_next().unwrap().priority,
            Priority::BackgroundPrewarm
        );
    }

    #[test]
    fn main_thread_shapes_nothing_on_warm_hit() {
        let mut work = TextWork::default();
        let step = work.main_thread_edit_step(42, true);
        assert_eq!(
            step.shaped_on_main, 0,
            "main thread shaped nothing on a hit"
        );
        assert!(step.warm_hit);
        assert!(step.caret_advanced, "caret advanced by geometry");
        // A hit needs no worker dispatch.
        assert_eq!(work.pending_len(), 0);
    }

    #[test]
    fn main_thread_shapes_nothing_on_cold_miss() {
        let mut work = TextWork::default();
        let step = work.main_thread_edit_step(42, false);
        // The whole invariant: even on a miss the main thread does zero shaping.
        assert_eq!(
            step.shaped_on_main, 0,
            "main thread never reshapes on a miss"
        );
        assert!(!step.warm_hit);
        assert!(
            step.caret_advanced,
            "caret advances by geometry despite the miss"
        );
        // The dirty run was dispatched to the worker at InteractiveEdit, to land
        // a frame or two later.
        assert_eq!(work.pending_len(), 1);
        let job = work.take_next().unwrap();
        assert_eq!(job.priority, Priority::InteractiveEdit);
        assert_eq!(job.kind, JobKind::Paragraph { paragraph: 42 });
    }

    #[test]
    fn edit_steps_never_shape_on_main_across_a_mixed_run() {
        // A realistic mix of hits and misses: the main thread's shaping count is
        // zero every step, and every step advances the caret.
        let mut work = TextWork::default();
        let outcomes = [true, false, true, true, false, false, true];
        for (i, &hit) in outcomes.iter().enumerate() {
            let step = work.main_thread_edit_step(i as u64, hit);
            assert_eq!(step.shaped_on_main, 0, "step {i} shaped on main");
            assert!(step.caret_advanced, "step {i} did not advance caret");
        }
        // Every miss produced exactly one InteractiveEdit worker job.
        let miss_count = outcomes.iter().filter(|&&h| !h).count();
        let mut interactive = 0;
        while let Some(job) = work.take_next() {
            if job.priority == Priority::InteractiveEdit {
                interactive += 1;
            }
        }
        assert_eq!(interactive, miss_count, "one worker dispatch per miss");
    }

    #[test]
    fn only_background_prewarm_is_the_prewarm_class() {
        assert!(Priority::BackgroundPrewarm.is_prewarm());
        assert!(!Priority::CriticalVisible.is_prewarm());
        assert!(!Priority::NearViewport.is_prewarm());
        assert!(!Priority::InteractiveEdit.is_prewarm());
        assert!(!Priority::CatalogPreview.is_prewarm());
    }
}
