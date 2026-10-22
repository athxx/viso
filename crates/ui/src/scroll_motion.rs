//! Scroll motion: a viewport's offset coasting on after the finger lifts.
//!
//! A touch pan tracks the finger's velocity over the last tenth of a second
//! of its samples, each timed by the loop's clock as it arrived
//! ([`NodeStore::set_input_time`]). Lifted fast enough, the viewports it
//! moved coast on along the platform's curve ([`FlingCurve`]): Android's
//! fling spline, Apple's per-millisecond decay, elsewhere a decay that lets a
//! hard flick glide. Each frame advances the coasts by its delta
//! ([`NodeStore::tick_scroll_motion`]), marking only `TRANSFORM`; a coast
//! ends slowed to a stop or at the edge of its range.
//!
//! A press on a coasting viewport catches it: the coast stops and, while it
//! still moved briskly, the press reaches no control (the finger meant to
//! stop the list, not to tap a row). A flick soon after in the same direction
//! adds the caught speed, so repeated flicks build up speed. A wheel over the
//! viewport, or a programmatic scroll of it, stops its coast too.

use std::sync::OnceLock;
use std::time::Duration;

use viso_runtime::Instant;

use crate::component::NodeStore;
use crate::input::PointerId;
use crate::layout::{Axis, OnAxis, Vec2};
use crate::node::NodeId;

/// How a released flick slows down.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FlingCurve {
    /// The speed decays by the factor `rate` every millisecond. A flick
    /// faster than 2000 pt/s blends toward `hard_rate` (fully at 8000 pt/s)
    /// and opens with a quarter second of near-constant glide, so a hard
    /// flick carries the way a native scroller's does; with `hard_rate` no
    /// greater than `rate`, every flick decays at `rate`.
    Decay { rate: f64, hard_rate: f64 },
    /// Android's `OverScroller` spline: a flick at `v` travels a distance
    /// growing as `v^1.7` over a duration growing as `v^0.7`, front-loaded
    /// and easing out long.
    Spline,
}

/// How a window's viewports coast: the curve, and the speeds a flick needs
/// to start and may reach. Logical points per second.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScrollPhysics {
    pub curve: FlingCurve,
    /// The release speed under which a lift is a stop, not a flick.
    pub min_velocity: f32,
    /// The fastest a flick (boosted ones too) coasts.
    pub max_velocity: f32,
}

impl ScrollPhysics {
    /// `UIScrollView`'s normal deceleration.
    pub const APPLE: ScrollPhysics = ScrollPhysics {
        curve: FlingCurve::Decay {
            rate: 0.998,
            hard_rate: 0.998,
        },
        min_velocity: 50.0,
        max_velocity: 8000.0,
    };
    /// Android's fling, at its `ViewConfiguration` speeds.
    pub const ANDROID: ScrollPhysics = ScrollPhysics {
        curve: FlingCurve::Spline,
        min_velocity: 50.0,
        max_velocity: 8000.0,
    };
    /// A firm decay whose hard flicks glide: touch screens elsewhere.
    pub const DEFAULT: ScrollPhysics = ScrollPhysics {
        curve: FlingCurve::Decay {
            rate: 0.997,
            hard_rate: 0.9985,
        },
        min_velocity: 50.0,
        max_velocity: 8000.0,
    };

    /// This system's coasting.
    pub fn native() -> ScrollPhysics {
        if cfg!(any(target_os = "macos", target_os = "ios")) {
            Self::APPLE
        } else if cfg!(target_os = "android") {
            Self::ANDROID
        } else {
            Self::DEFAULT
        }
    }
}

impl Default for ScrollPhysics {
    fn default() -> Self {
        Self::native()
    }
}

/// How many of a contact's latest samples its tracker keeps.
const TRACKED: usize = 20;
/// The history (seconds) a release velocity reads, as a native tracker's.
const SAMPLE_AGE: f32 = 0.1;
/// The latest slice (seconds) of the history the velocity is measured over,
/// so it is the speed at the lift, not the gesture's average.
const VELOCITY_SPAN: f32 = 0.04;
/// The least span (seconds) the samples must cover to mean a speed.
const MIN_SPAN: f32 = 0.01;
/// The least travel (points) along an axis across the history for a flick.
const MIN_TRAVEL: f32 = 4.0;
/// Under this speed (pt/s) a decaying coast has stopped.
const STOP_VELOCITY: f64 = 5.0;
/// Over this speed (pt/s) a press on a coast catches it whole: the press
/// reaches no control. Slower, the coast stops and the press goes through.
const CATCH_VELOCITY: f64 = 40.0;
/// How soon (seconds) after a catch a flick the same way adds its speed.
const BOOST_WINDOW: f32 = 0.4;
/// The longest step (seconds) one frame advances a coast: a hitch catches
/// up a little rather than jumping.
const MAX_STEP: f64 = 0.1;
/// The weight of a frame's interval in the running average a step's length
/// is held near, and the band around that average it is held to, so one
/// late or early frame does not jerk the motion.
const STEP_EMA: f64 = 0.15;
const STEP_BAND: (f64, f64) = (0.5, 1.5);

/// A contact's latest positions and when each was sampled, from which its
/// release velocity is read. Fixed-size: tracking allocates nothing.
#[derive(Debug, Clone, Copy)]
pub(crate) struct VelocityTracker {
    /// When the first sample was taken; samples time from it.
    start: Option<Instant>,
    /// Oldest first: seconds since `start`, and the position.
    samples: [(f32, [f32; 2]); TRACKED],
    len: usize,
}

impl Default for VelocityTracker {
    fn default() -> Self {
        VelocityTracker {
            start: None,
            samples: [(0.0, [0.0; 2]); TRACKED],
            len: 0,
        }
    }
}

impl VelocityTracker {
    /// Note the contact at `at` at time `now`, dropping what is too old.
    pub(crate) fn push(&mut self, now: Instant, at: [f32; 2]) {
        let start = *self.start.get_or_insert(now);
        let t = now.saturating_duration_since(start).as_secs_f32();
        let old = self.samples[..self.len]
            .iter()
            .take_while(|s| t - s.0 > SAMPLE_AGE)
            .count();
        let drop = if self.len - old == TRACKED {
            old + 1
        } else {
            old
        };
        if drop > 0 {
            self.samples.copy_within(drop..self.len, 0);
            self.len -= drop;
        }
        self.samples[self.len] = (t, at);
        self.len += 1;
    }

    /// The contact's velocity (pt/s) at its latest sample, per axis; zero on
    /// an axis it barely travelled along.
    pub(crate) fn velocity(&self) -> Vec2 {
        let samples = &self.samples[..self.len];
        let (Some(&(t1, p1)), Some(&(t0, p0))) = (samples.last(), samples.first()) else {
            return Vec2::ZERO;
        };
        let recent = samples
            .iter()
            .find(|s| t1 - s.0 <= VELOCITY_SPAN)
            .filter(|s| t1 - s.0 > MIN_SPAN)
            .copied()
            .unwrap_or((t0, p0));
        let dt = t1 - recent.0;
        if dt <= MIN_SPAN {
            return Vec2::ZERO;
        }
        let along = |i: usize| {
            if (p1[i] - p0[i]).abs() < MIN_TRAVEL {
                0.0
            } else {
                (p1[i] - recent.1[i]) / dt
            }
        };
        Vec2 {
            x: along(0),
            y: along(1),
        }
    }
}

/// One viewport coasting along one axis.
#[derive(Debug, Clone, Copy)]
struct Fling {
    viewport: NodeId,
    axis: Axis,
    motion: Motion,
}

/// A coast's motion along its curve.
#[derive(Debug, Clone, Copy)]
struct Motion {
    /// The offset's speed (pt/s), signed as the offset moves.
    velocity: f64,
    /// Seconds since release.
    age: f64,
    /// The running average of the frame interval (seconds).
    step: f64,
    curve: FlingCurve,
    /// A spline fling's whole travel and duration, and the travel handed out.
    spline: (f64, f64, f64),
}

impl Motion {
    fn new(velocity: f64, curve: FlingCurve) -> Motion {
        let spline = match curve {
            FlingCurve::Spline => {
                let (distance, duration) = spline_target(velocity);
                (distance, duration, 0.0)
            }
            FlingCurve::Decay { .. } => (0.0, 0.0, 0.0),
        };
        Motion {
            velocity,
            age: 0.0,
            step: 0.0,
            curve,
            spline,
        }
    }

    /// Advance by a frame of `delta`; the offset's travel over it.
    fn advance(&mut self, delta: Duration) -> f64 {
        let raw = delta.as_secs_f64().min(MAX_STEP);
        self.step = if self.step <= 0.0 {
            raw
        } else {
            self.step * (1.0 - STEP_EMA) + raw * STEP_EMA
        };
        let dt = raw.clamp(self.step * STEP_BAND.0, self.step * STEP_BAND.1);
        match self.curve {
            FlingCurve::Decay { rate, hard_rate } => self.decay(dt, rate, hard_rate),
            FlingCurve::Spline => self.along_spline(dt),
        }
    }

    fn decay(&mut self, dt: f64, rate: f64, hard_rate: f64) -> f64 {
        const HARD_START: f64 = 2000.0;
        const HARD_FULL: f64 = 8000.0;
        const GLIDE_RATE: f64 = 0.9997;
        const GLIDE_TIME: f64 = 0.25;
        let base = rate;
        let hard = if hard_rate > base {
            ((self.velocity.abs() - HARD_START) / (HARD_FULL - HARD_START)).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let mut rate = base + (hard_rate - base) * hard;
        let glide = (1.0 - self.age / GLIDE_TIME).clamp(0.0, 1.0) * hard;
        rate += (GLIDE_RATE.max(rate) - rate) * glide;
        self.age += dt;
        // v(t) = v0·e^(−λt), so the travel over dt is v0·(1 − e^(−λ·dt))/λ.
        let factor = rate.powf(dt * 1000.0);
        let lambda = -rate.ln() * 1000.0;
        let travel = self.velocity * (1.0 - factor) / lambda;
        self.velocity *= factor;
        travel
    }

    fn along_spline(&mut self, dt: f64) -> f64 {
        self.age += dt;
        let (distance, duration, emitted) = self.spline;
        if duration <= 0.0 || self.age >= duration {
            self.velocity = 0.0;
            self.spline.2 = distance;
            return distance - emitted;
        }
        let table = spline_table();
        let t = self.age / duration;
        let at = ((SPLINE_SAMPLES as f64 * t) as usize).min(SPLINE_SAMPLES - 1);
        let t0 = at as f64 / SPLINE_SAMPLES as f64;
        let slope = (table[at + 1] - table[at]) * SPLINE_SAMPLES as f64;
        let target = (table[at] + (t - t0) * slope) * distance;
        self.spline.2 = target;
        self.velocity = slope * distance / duration;
        target - emitted
    }

    fn done(&self) -> bool {
        match self.curve {
            FlingCurve::Decay { .. } => self.velocity.abs() < STOP_VELOCITY,
            FlingCurve::Spline => self.age >= self.spline.1,
        }
    }
}

// Android's `OverScroller.SplineOverScroller` constants. Logical points are
// the dp its coefficient is figured at 160 dpi for.
const SCROLL_FRICTION: f64 = 0.015;
/// ln(0.78) / ln(0.9): the speed drops 22% for every 10% of the time left.
const DECELERATION_RATE: f64 = 2.358_201_815_425_944_8;
const INFLEXION: f64 = 0.35;
/// Gravity × inches per metre × 160 dpi × the look-and-feel factor.
const PHYSICAL_COEFF: f64 = 9.80665 * 39.37 * 160.0 * 0.84;
const SPLINE_SAMPLES: usize = 100;

/// The share of a spline fling's travel covered at each hundredth of its
/// duration (`OverScroller`'s `SPLINE_POSITION`).
fn spline_table() -> &'static [f64; SPLINE_SAMPLES + 1] {
    static TABLE: OnceLock<[f64; SPLINE_SAMPLES + 1]> = OnceLock::new();
    TABLE.get_or_init(|| {
        const START_TENSION: f64 = 0.5;
        const END_TENSION: f64 = 1.0;
        const P1: f64 = START_TENSION * INFLEXION;
        const P2: f64 = 1.0 - END_TENSION * (1.0 - INFLEXION);
        let mut table = [1.0; SPLINE_SAMPLES + 1];
        let mut x_min = 0.0;
        for (i, slot) in table.iter_mut().enumerate().take(SPLINE_SAMPLES) {
            let alpha = i as f64 / SPLINE_SAMPLES as f64;
            let mut x_max = 1.0;
            loop {
                let x = x_min + (x_max - x_min) / 2.0;
                let coef = 3.0 * x * (1.0 - x);
                let tx = coef * ((1.0 - x) * P1 + x * P2) + x * x * x;
                if (tx - alpha).abs() < 1e-5 {
                    *slot = coef * ((1.0 - x) * START_TENSION + x * END_TENSION) + x * x * x;
                    break;
                }
                if tx > alpha {
                    x_max = x;
                } else {
                    x_min = x;
                }
            }
        }
        table
    })
}

/// A spline fling's signed travel and its duration (seconds) from a release
/// at `velocity`.
fn spline_target(velocity: f64) -> (f64, f64) {
    if velocity == 0.0 {
        return (0.0, 0.0);
    }
    let friction = SCROLL_FRICTION * PHYSICAL_COEFF;
    let l = (INFLEXION * velocity.abs() / friction).ln();
    let duration = (l / (DECELERATION_RATE - 1.0)).exp();
    let distance = friction * (DECELERATION_RATE / (DECELERATION_RATE - 1.0) * l).exp();
    (distance.copysign(velocity), duration)
}

/// A coast a press caught: a flick soon after the same way adds its speed.
#[derive(Debug, Clone, Copy)]
struct Caught {
    viewport: NodeId,
    axis: Axis,
    velocity: f64,
    at: Instant,
}

/// A window's coasting viewports, and what the catches and flicks read.
#[derive(Debug, Default)]
pub(crate) struct ScrollMotions {
    physics: ScrollPhysics,
    flings: Vec<Fling>,
    caught: Option<Caught>,
    /// The pointer whose press caught a coast: its samples reach no control.
    swallowed: Option<PointerId>,
    /// When the sample under routing arrived.
    input_time: Option<Instant>,
}

impl ScrollMotions {
    /// Forget every coast; the physics and the clock stay.
    pub(crate) fn clear(&mut self) {
        self.flings.clear();
        self.caught = None;
        self.swallowed = None;
    }
}

impl NodeStore {
    /// How this window's viewports coast; [`ScrollPhysics::native`] unless
    /// set.
    pub fn scroll_physics(&self) -> ScrollPhysics {
        self.motions.physics
    }

    /// Make this window's viewports coast by `physics`.
    pub fn set_scroll_physics(&mut self, physics: ScrollPhysics) {
        self.motions.physics = physics;
    }

    /// Note when the input sample about to be routed arrived, by the loop's
    /// clock: a flick's speed is read from its samples' times.
    pub fn set_input_time(&mut self, at: Instant) {
        self.motions.input_time = Some(at);
    }

    /// When the latest input sample arrived; `None` before any.
    pub fn input_time(&self) -> Option<Instant> {
        self.motions.input_time
    }

    /// Whether a viewport coasts: the frame loop keeps beating while one does.
    #[inline]
    pub fn is_scroll_moving(&self) -> bool {
        !self.motions.flings.is_empty()
    }

    /// Whether viewport `id` coasts.
    pub fn is_coasting(&self, id: NodeId) -> bool {
        self.motions.flings.iter().any(|f| f.viewport == id)
    }

    /// Coast viewport `id` along its axis from an offset speed of
    /// `velocity` (pt/s, signed as the offset moves), clamped to the
    /// physics' fastest, as a flick released at that speed would; a speed
    /// under the physics' least stops its coast instead. A stale handle or a
    /// node that does not scroll is a no-op.
    pub fn fling(&mut self, id: NodeId, velocity: f32) {
        let Some(axis) = self.scroll_axis(id).filter(|_| self.arena().is_live(id)) else {
            return;
        };
        self.stop_coast(id);
        let physics = self.motions.physics;
        if velocity.abs() < physics.min_velocity {
            return;
        }
        let velocity = velocity.clamp(-physics.max_velocity, physics.max_velocity);
        self.motions.flings.push(Fling {
            viewport: id,
            axis,
            motion: Motion::new(f64::from(velocity), physics.curve),
        });
    }

    /// Viewport `id`'s coasting speed.
    #[cfg(test)]
    pub(crate) fn coast_velocity(&self, id: NodeId) -> Option<f64> {
        self.motions
            .flings
            .iter()
            .find(|f| f.viewport == id)
            .map(|f| f.motion.velocity)
    }

    /// Stop viewport `id`'s coast where it is.
    pub fn stop_coast(&mut self, id: NodeId) {
        self.motions.flings.retain(|f| f.viewport != id);
    }

    /// Advance every coast by a frame of `delta`, moving its viewport; one
    /// that slowed to a stop, reached its edge or lost its node ends. A flat
    /// pass with no allocation.
    pub fn tick_scroll_motion(&mut self, delta: Duration) {
        let mut i = self.motions.flings.len();
        while i > 0 {
            i -= 1;
            let mut fling = self.motions.flings[i];
            let id = fling.viewport;
            if !self.arena().is_live(id) {
                self.motions.flings.swap_remove(i);
                continue;
            }
            let travel = fling.motion.advance(delta) as f32;
            let before = self.scroll(id).on(fling.axis);
            let (least, most) = self.scroll_span(id, fling.axis);
            let wanted = before + travel;
            self.scroll_by(id, axis_vec(fling.axis, travel));
            let edge = wanted <= least || wanted >= most;
            if fling.motion.done() || edge {
                self.motions.flings.swap_remove(i);
            } else {
                self.motions.flings[i] = fling;
            }
        }
    }

    /// A press at or under `target` by `pointer`: stop the coasts of the
    /// viewports enclosing it. Returns whether one moved briskly, so the
    /// press is the catch and reaches no control.
    pub(crate) fn catch_coast(&mut self, target: NodeId, pointer: PointerId) -> bool {
        if self.motions.flings.is_empty() {
            return false;
        }
        let mut brisk = false;
        let mut node = Some(target);
        while let Some(n) = node {
            if let Some(at) = self.motions.flings.iter().position(|f| f.viewport == n) {
                let fling = self.motions.flings.swap_remove(at);
                if fling.motion.velocity.abs() > CATCH_VELOCITY {
                    brisk = true;
                    self.motions.caught = self.motions.input_time.map(|at| Caught {
                        viewport: n,
                        axis: fling.axis,
                        velocity: fling.motion.velocity,
                        at,
                    });
                }
                continue;
            }
            node = self.arena().parent(n);
        }
        if brisk {
            self.motions.swallowed = Some(pointer);
        }
        brisk
    }

    /// Whether `pointer`'s press caught a coast, so its samples reach no
    /// control.
    pub(crate) fn swallows(&self, pointer: PointerId) -> bool {
        self.motions.swallowed == Some(pointer)
    }

    /// `pointer`'s catch ended with its lift.
    pub(crate) fn end_swallow(&mut self, pointer: PointerId) {
        if self.swallows(pointer) {
            self.motions.swallowed = None;
        }
    }

    /// Stop the coasts of the viewports at or above `target`: a wheel there
    /// moves them now.
    pub(crate) fn stop_coasts_over(&mut self, target: NodeId) {
        if self.motions.flings.is_empty() {
            return;
        }
        let mut node = Some(target);
        while let Some(n) = node {
            self.stop_coast(n);
            node = self.arena().parent(n);
        }
    }

    /// A pan from `from` lifted with the finger at `velocity` (pt/s): the
    /// viewport carrying each axis of it coasts on, the way the content
    /// moved, with the speed of a coast caught moments before the same way
    /// added.
    pub(crate) fn release_pan(&mut self, from: NodeId, velocity: Vec2) {
        let caught = self.motions.caught.take();
        let now = self.motions.input_time;
        for axis in [Axis::Row, Axis::Column] {
            // Content follows the finger: the offset moves against it.
            let speed = -velocity.on(axis);
            if speed == 0.0 {
                continue;
            }
            let Some(viewport) = self.carrier(from, axis, speed) else {
                continue;
            };
            let boost = caught
                .filter(|c| {
                    c.viewport == viewport
                        && c.axis == axis
                        && c.velocity.signum() == f64::from(speed).signum()
                        && now.is_some_and(|now| {
                            now.saturating_duration_since(c.at).as_secs_f32() < BOOST_WINDOW
                        })
                })
                .map_or(0.0, |c| c.velocity as f32);
            if speed.abs() >= self.motions.physics.min_velocity {
                self.fling(viewport, speed + boost);
            }
        }
    }

    /// The viewport at or above `from` scrolling along `axis` with room
    /// left for a move of `delta`'s sign.
    fn carrier(&self, from: NodeId, axis: Axis, delta: f32) -> Option<NodeId> {
        let mut node = Some(from);
        while let Some(n) = node {
            if self.scroll_axis(n) == Some(axis) {
                let offset = self.scroll(n).on(axis);
                let (least, most) = self.scroll_span(n, axis);
                if (delta > 0.0 && offset < most) || (delta < 0.0 && offset > least) {
                    return Some(n);
                }
            }
            node = self.arena().parent(n);
        }
        None
    }
}

fn axis_vec(axis: Axis, value: f32) -> Vec2 {
    match axis {
        Axis::Row => Vec2 { x: value, y: 0.0 },
        Axis::Column => Vec2 { x: 0.0, y: value },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn coast(physics: ScrollPhysics, velocity: f64) -> (f64, f64) {
        let mut fling = Motion::new(velocity, physics.curve);
        let (mut travel, mut time) = (0.0, 0.0);
        while !fling.done() && time < 20.0 {
            travel += fling.advance(Duration::from_millis(16));
            time += 0.016;
        }
        (travel, time)
    }

    #[test]
    fn apple_decay_travels_about_half_the_release_speed() {
        let (travel, time) = coast(ScrollPhysics::APPLE, 2000.0);
        // v/λ with λ = −ln(0.998)·1000 ≈ 2.002/s: about 999 pt.
        assert!((travel - 997.0).abs() < 5.0, "{travel}");
        assert!((2.5..3.5).contains(&time), "{time}");
        let (back, _) = coast(ScrollPhysics::APPLE, -2000.0);
        assert!((back + travel).abs() < 1e-6, "symmetric");
    }

    #[test]
    fn a_hard_flick_glides_farther_than_its_speed_alone_carries() {
        let (gentle, _) = coast(ScrollPhysics::DEFAULT, 1000.0);
        let (hard, _) = coast(ScrollPhysics::DEFAULT, 8000.0);
        assert!(hard > gentle * 8.0 * 1.5, "{gentle} {hard}");
    }

    #[test]
    fn the_android_spline_covers_its_whole_travel_in_its_duration() {
        let (distance, duration) = spline_target(4000.0);
        let (travel, time) = coast(ScrollPhysics::ANDROID, 4000.0);
        assert!((travel - distance).abs() < 1e-6);
        assert!((time - duration).abs() < 0.02, "{time} {duration}");
        // A maximum fling covers about 7,200 dp in about 2.6 s.
        let (far, long) = spline_target(8000.0);
        assert!((6500.0..8000.0).contains(&far), "{far}");
        assert!((2.3..2.9).contains(&long), "{long}");
    }

    #[test]
    fn the_tracker_reads_the_speed_at_the_lift() {
        let t0 = Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        let mut track = VelocityTracker::default();
        // Slow, then 1 pt/ms upward over the last 50 ms.
        for (n, y) in [(0, 300.0), (16, 298.0), (32, 296.0)] {
            track.push(ms(n), [10.0, y]);
        }
        for k in 1..=5 {
            track.push(ms(32 + k * 10), [10.0, 296.0 - k as f32 * 10.0]);
        }
        let v = track.velocity();
        assert!((v.y + 1000.0).abs() < 1.0, "{v:?}");
        assert_eq!(v.x, 0.0);
        // Held still before the lift: no speed.
        track.push(ms(200), [10.0, 246.0]);
        assert_eq!(track.velocity(), Vec2::ZERO);
    }

    #[test]
    fn the_tracker_keeps_a_bounded_history() {
        let t0 = Instant::now();
        let mut track = VelocityTracker::default();
        for n in 0..100u64 {
            track.push(t0 + Duration::from_millis(n), [0.0, n as f32]);
        }
        assert_eq!(track.len, TRACKED);
        assert!((track.velocity().y - 1000.0).abs() < 1.0);
    }
}
