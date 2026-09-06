//! The frame clock: the injectable time source the scheduler samples once per
//! frame to compute a delta.
//!
//! The runtime does not read the wall clock directly. It reads it through a
//! [`FrameClock`] so headless tests can inject a [`ManualClock`] and step time
//! by a fixed `dt` each frame — the section 66 deterministic-time contract.
//! Production uses [`WallClock`], a zero-state wrapper over [`Instant::now`]
//! that costs a single syscall per frame (the delta a driver needs to advance
//! animations by real elapsed time). This is taking the semantics (an
//! injectable, deterministic time source) over the coarse mechanism (a
//! hard-coded wall clock) — animation tests are non-deterministic otherwise.

use std::time::Instant;

/// A source of monotonic time, sampled once at the head of each frame.
///
/// `now` takes `&mut self` so a manual clock can advance its cursor as a side
/// effect of being read; a wall clock ignores the mutability and returns the
/// live instant.
pub trait FrameClock {
    /// The current instant. The scheduler diffs consecutive samples to derive
    /// the per-frame delta.
    fn now(&mut self) -> Instant;
}

/// The production clock: reads the real monotonic clock. Zero state, one
/// `Instant::now()` per frame.
#[derive(Debug, Default, Clone, Copy)]
pub struct WallClock;

impl FrameClock for WallClock {
    #[inline]
    fn now(&mut self) -> Instant {
        Instant::now()
    }
}

/// A deterministic clock for headless tests: holds a cursor the test advances
/// explicitly, so each frame observes exactly the delta the test scripted.
///
/// Construct at some base instant, then [`advance`](Self::advance) before each
/// scripted beat; the next `now` returns the advanced cursor. Because the
/// runtime samples `now` once per frame and diffs consecutive samples, a test
/// that advances by a fixed `dt` before every beat drives frames that each see
/// exactly that `dt`.
#[derive(Debug, Clone, Copy)]
pub struct ManualClock {
    cursor: Instant,
}

impl ManualClock {
    /// Start the clock at `base`. Tests typically pass `Instant::now()`; only
    /// the *differences* between samples matter, so the absolute base is free.
    pub fn new(base: Instant) -> Self {
        Self { cursor: base }
    }

    /// Move the cursor forward by `delta`. The next `now` reflects it.
    pub fn advance(&mut self, delta: std::time::Duration) {
        self.cursor += delta;
    }
}

impl FrameClock for ManualClock {
    #[inline]
    fn now(&mut self) -> Instant {
        self.cursor
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn wall_clock_is_monotonic_and_advances() {
        let mut clock = WallClock;
        let a = clock.now();
        let b = clock.now();
        assert!(b >= a, "the wall clock never runs backwards");
    }

    #[test]
    fn manual_clock_holds_until_advanced() {
        let base = Instant::now();
        let mut clock = ManualClock::new(base);
        assert_eq!(clock.now(), base, "an un-advanced manual clock is frozen");
        assert_eq!(clock.now(), base, "repeated reads return the same cursor");
    }

    #[test]
    fn manual_clock_advances_by_exactly_the_scripted_delta() {
        let base = Instant::now();
        let mut clock = ManualClock::new(base);
        let dt = Duration::from_millis(16);
        clock.advance(dt);
        assert_eq!(clock.now() - base, dt, "one advance moves the cursor by dt");
        clock.advance(dt);
        assert_eq!(
            clock.now() - base,
            dt * 2,
            "advances accumulate on the cursor"
        );
    }
}
