//! The loop's measured frame interval, and the main thread's share of it for
//! the work a build without worker threads runs inline.
//!
//! One account serves every window: a frame earns a quarter of the time since
//! the last one, and each inline consumer of each window spends from what is
//! left. However many windows draw, and however many frames their beats run
//! per display interval, inline work takes a quarter of the wall clock.

use std::time::Duration;

use viso_platform::Instant;

use crate::text_content::DEFAULT_FRAME_INTERVAL;

/// Inline work may take one part in `SHARE` of each frame interval.
const SHARE: u32 = 4;

/// A gap between frames longer than this is idle time, not an interval.
const IDLE_GAP: Duration = Duration::from_millis(100);

/// Frame-interval estimate and inline-work account.
pub(crate) struct FramePace {
    /// The measured interval between frames, smoothed.
    interval: Duration,
    /// What inline work may still spend, in nanoseconds; below zero after a
    /// job ran past the slice it started in, a debt the next frames repay.
    credit: i64,
}

impl Default for FramePace {
    fn default() -> Self {
        let mut pace = Self {
            interval: DEFAULT_FRAME_INTERVAL,
            credit: 0,
        };
        pace.credit = pace.cap();
        pace
    }
}

impl FramePace {
    /// Start a frame that came `delta` after the last one: fold `delta` into
    /// the interval and earn its share, up to one interval's worth.
    pub(crate) fn begin_frame(&mut self, delta: Duration) {
        if !delta.is_zero() && delta <= IDLE_GAP {
            self.interval = (self.interval * 7 + delta) / 8;
        }
        let earned = nanos(delta / SHARE);
        self.credit = self.credit.saturating_add(earned).min(self.cap());
    }

    /// The measured interval between frames.
    pub(crate) fn interval(&self) -> Duration {
        self.interval
    }

    /// What inline work may spend now; `None` when this frame's share is
    /// spent. A headless build spends without limit, so what a run shows
    /// never depends on the machine's speed.
    pub(crate) fn slice(&self) -> Option<Duration> {
        if cfg!(feature = "headless") {
            return Some(Duration::MAX);
        }
        (self.credit > 0).then(|| Duration::from_nanos(self.credit as u64))
    }

    /// Charge `spent` to the account.
    pub(crate) fn charge(&mut self, spent: Duration) {
        self.credit = self.credit.saturating_sub(nanos(spent));
    }

    /// Run `work` with what is left of the share, `None` when it is spent,
    /// and charge the time it took.
    pub(crate) fn run<R>(&mut self, work: impl FnOnce(Option<Duration>) -> R) -> R {
        let slice = self.slice();
        let start = Instant::now();
        let result = work(slice);
        if slice.is_some() {
            self.charge(start.elapsed());
        }
        result
    }

    /// The most one frame may hold: one interval's share.
    fn cap(&self) -> i64 {
        nanos(self.interval / SHARE)
    }
}

fn nanos(d: Duration) -> i64 {
    i64::try_from(d.as_nanos()).unwrap_or(i64::MAX)
}

#[cfg(all(test, not(feature = "headless")))]
mod tests {
    use super::*;

    const HZ_120: Duration = Duration::from_nanos(8_333_333);

    /// `windows` windows at 120 Hz, each beat running a frame: every frame,
    /// each window's three consumers take the slice they are given and run a
    /// `job` past it. The inline time spent over `seconds`.
    fn spend(windows: u32, job: Duration, seconds: u32) -> Duration {
        let mut pace = FramePace::default();
        let mut total = Duration::ZERO;
        for _ in 0..120 * seconds * windows {
            pace.begin_frame(HZ_120 / windows);
            for _ in 0..windows * 3 {
                if let Some(slice) = pace.slice() {
                    let spent = slice + job;
                    pace.charge(spent);
                    total += spent;
                }
            }
        }
        total
    }

    #[test]
    fn the_interval_follows_the_frames_and_ignores_idle_gaps() {
        let mut pace = FramePace::default();
        for _ in 0..64 {
            pace.begin_frame(HZ_120);
        }
        assert!(pace.interval().abs_diff(HZ_120) < Duration::from_micros(50));
        pace.begin_frame(Duration::from_secs(3));
        assert!(pace.interval().abs_diff(HZ_120) < Duration::from_micros(50));
        let slice = pace.slice().unwrap();
        assert!(
            slice.abs_diff(HZ_120 / 4) < Duration::from_micros(50),
            "after idle a frame holds one interval's share, {slice:?}"
        );
    }

    #[test]
    fn at_120_hz_the_slice_holds_a_quarter_of_the_clock_with_any_number_of_windows() {
        for windows in [1, 2, 4, 8] {
            for job in [
                Duration::ZERO,
                Duration::from_micros(300),
                Duration::from_millis(3),
            ] {
                let spent = spend(windows, job, 2);
                let quarter = Duration::from_millis(500);
                // The share of 2 s, plus the first frame's head start and the
                // job that may run past the last slice.
                let most = quarter + DEFAULT_FRAME_INTERVAL / 4 + job;
                assert!(
                    spent <= most,
                    "{windows} windows, {job:?} jobs: {spent:?} over {most:?}"
                );
                assert!(
                    spent >= quarter.mul_f32(0.95),
                    "{windows} windows, {job:?} jobs: only {spent:?}"
                );
            }
        }
    }

    #[test]
    fn a_frame_after_a_spent_share_runs_nothing_inline() {
        let mut pace = FramePace::default();
        pace.charge(Duration::from_millis(20));
        pace.begin_frame(HZ_120);
        assert_eq!(pace.slice(), None);
        assert_eq!(pace.run(|slice| slice), None);
    }
}
