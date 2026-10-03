//! The fixed-step clock: scaled wall time accumulates into whole ticks.

/// What a frame does with the ticks it owes beyond
/// [`Clock::max_catch_up_steps`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum TickOverrun {
    /// Discards their time: the game keeps wall-clock pace and skips ahead.
    #[default]
    DropTime,
    /// Keeps their time for later frames, up to one frame's catch-up: the
    /// game slows down instead of skipping.
    SlowMotion,
}

/// The simulation clock of a game: `tick` advances only in whole fixed steps,
/// and `time = tick × fixed_dt` never reads the wall clock.
///
/// Each frame adds its wall time, scaled by `time_scale`, to an accumulator
/// and owes one tick per whole `fixed_dt` in it, at most
/// `max_catch_up_steps`; a paused clock accumulates nothing. Ticks owed
/// beyond the cap count as `overrun_ticks`, and accumulated time discarded by
/// the [`TickOverrun`] policy as `dropped_time`.
#[derive(Debug, Clone, PartialEq)]
pub struct Clock {
    fixed_dt: f64,
    time_scale: f64,
    paused: bool,
    max_catch_up_steps: u32,
    overrun: TickOverrun,
    tick: u64,
    accumulator: f64,
    overrun_ticks: u64,
    dropped_time: f64,
}

impl Default for Clock {
    /// A 60 Hz clock.
    fn default() -> Clock {
        Clock::new(1.0 / 60.0)
    }
}

impl Clock {
    /// A clock at tick 0 stepping `fixed_dt` seconds, at scale 1, running,
    /// catching up at most 8 ticks a frame and dropping the rest.
    ///
    /// # Panics
    ///
    /// If `fixed_dt` is not a positive finite number of seconds.
    pub fn new(fixed_dt: f64) -> Clock {
        assert!(
            fixed_dt.is_finite() && fixed_dt > 0.0,
            "fixed_dt must be positive and finite, not {fixed_dt}"
        );
        Clock {
            fixed_dt,
            time_scale: 1.0,
            paused: false,
            max_catch_up_steps: 8,
            overrun: TickOverrun::DropTime,
            tick: 0,
            accumulator: 0.0,
            overrun_ticks: 0,
            dropped_time: 0.0,
        }
    }

    /// A clock stepping `1 / tick_rate` seconds, as [`Clock::new`] does.
    ///
    /// # Panics
    ///
    /// If `tick_rate` is 0.
    pub fn at_rate(tick_rate: u32) -> Clock {
        assert!(tick_rate > 0, "a tick rate is at least 1 Hz");
        Clock::new(1.0 / f64::from(tick_rate))
    }

    /// The seconds one tick stands for.
    pub fn fixed_dt(&self) -> f64 {
        self.fixed_dt
    }

    /// How far into the next tick the accumulated time is, in `[0, 1)`: the
    /// weight a frame interpolates the last two ticks with.
    pub fn alpha(&self) -> f32 {
        // The largest `f32` below 1.
        const BELOW_ONE: f32 = 1.0 - f32::EPSILON / 2.0;
        ((self.accumulator / self.fixed_dt) as f32).clamp(0.0, BELOW_ONE)
    }

    /// The ticks run so far.
    pub fn tick(&self) -> u64 {
        self.tick
    }

    /// The simulation time, `tick × fixed_dt` seconds.
    pub fn time(&self) -> f64 {
        self.tick as f64 * self.fixed_dt
    }

    /// The rate wall time feeds the accumulator at.
    pub fn time_scale(&self) -> f64 {
        self.time_scale
    }

    /// Sets the rate wall time feeds the accumulator at; a negative or
    /// non-finite scale stops it, as 0 does. `fixed_dt` is unchanged.
    pub fn set_time_scale(&mut self, scale: f64) {
        self.time_scale = if scale.is_finite() && scale > 0.0 {
            scale
        } else {
            0.0
        };
    }

    /// Whether the clock is paused.
    pub fn paused(&self) -> bool {
        self.paused
    }

    /// Pauses or resumes accumulation. A paused game still runs its
    /// `FrameUpdate` hooks every frame and can be stepped.
    pub fn set_paused(&mut self, paused: bool) {
        self.paused = paused;
    }

    /// The most ticks one frame runs.
    pub fn max_catch_up_steps(&self) -> u32 {
        self.max_catch_up_steps
    }

    /// Sets the most ticks one frame runs, at least 1.
    pub fn set_max_catch_up_steps(&mut self, steps: u32) {
        self.max_catch_up_steps = steps.max(1);
    }

    /// The overrun policy.
    pub fn overrun(&self) -> TickOverrun {
        self.overrun
    }

    /// Sets the overrun policy.
    pub fn set_overrun(&mut self, overrun: TickOverrun) {
        self.overrun = overrun;
    }

    /// `game.overrun_ticks`: the ticks frames owed beyond
    /// `max_catch_up_steps`, counted in each frame that owed them.
    pub fn overrun_ticks(&self) -> u64 {
        self.overrun_ticks
    }

    /// `game.dropped_time`: the scaled seconds the overrun policy discarded.
    pub fn dropped_time(&self) -> f64 {
        self.dropped_time
    }

    /// Adds a frame's `wall_dt` seconds and returns the ticks it owes. A
    /// negative or non-finite `wall_dt` adds nothing.
    pub fn advance(&mut self, wall_dt: f64) -> u32 {
        if self.paused {
            return 0;
        }
        let wall = if wall_dt.is_finite() && wall_dt > 0.0 {
            wall_dt
        } else {
            0.0
        };
        self.accumulator += wall * self.time_scale;
        let owed = (self.accumulator / self.fixed_dt).floor();
        let cap = f64::from(self.max_catch_up_steps);
        if owed <= cap {
            self.accumulator -= owed * self.fixed_dt;
            return owed as u32;
        }
        let excess = owed - cap;
        self.overrun_ticks = self.overrun_ticks.saturating_add(excess as u64);
        match self.overrun {
            TickOverrun::DropTime => {
                self.dropped_time += excess * self.fixed_dt;
                self.accumulator -= owed * self.fixed_dt;
            }
            TickOverrun::SlowMotion => {
                self.accumulator -= cap * self.fixed_dt;
                let backlog = cap * self.fixed_dt;
                if self.accumulator > backlog {
                    self.dropped_time += self.accumulator - backlog;
                    self.accumulator = backlog;
                }
            }
        }
        self.max_catch_up_steps
    }

    /// Ends the running tick.
    pub(crate) fn finish_tick(&mut self) {
        self.tick += 1;
    }

    /// Moves the clock to `tick`, keeping the accumulated time.
    pub(crate) fn rewind(&mut self, tick: u64) {
        self.tick = tick;
    }
}
