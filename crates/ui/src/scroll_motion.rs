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
//!
//! A smooth programmatic scroll glides: the offset eases toward its target
//! along a critically damped spring ([`NodeStore::glide_to`]), and a new
//! target takes over at the speed the offset already moves. The finger, the
//! wheel and the scroll bar stop a glide as they stop a coast.
//!
//! A tailing viewport ([`NodeStore::set_scroll_tail`]) opens at the end of
//! its content and, while it stands there, follows the end as the content
//! grows, gliding; scrolled away it stays put, and back at the end it
//! follows again.

use std::sync::OnceLock;
use std::time::Duration;

use viso_runtime::Instant;

use crate::component::NodeStore;
use crate::dirty::DirtyClass;
use crate::input::PointerId;
use crate::layout::{Axis, OnAxis, Vec2};
use crate::node::NodeId;
use crate::refresh::PullLook;

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

/// How a viewport shows a pull past the end of its range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverscrollStyle {
    /// The range ends hard.
    None,
    /// The content follows the finger past the end, ever more stiffly, and
    /// springs back; a coast reaching the end bounces (iOS).
    Bounce,
    /// The content stretches along the axis from the end it is pulled at,
    /// and relaxes back; a coast reaching the end is absorbed into a
    /// stretch (Android).
    Stretch,
}

/// How a window's viewports coast and overscroll: the curve, the speeds a
/// flick needs to start and may reach (logical points per second), and how
/// a pull past the end shows.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScrollPhysics {
    pub curve: FlingCurve,
    pub overscroll: OverscrollStyle,
    /// Whether the system coasts a trackpad scroll on after the fingers
    /// lift (macOS); otherwise the window coasts it as it does a flick.
    pub system_coast: bool,
    /// The release speed under which a lift is a stop, not a flick.
    pub min_velocity: f32,
    /// The fastest a flick (boosted ones too) coasts.
    pub max_velocity: f32,
}

/// macOS sends its own coast after a trackpad scroll.
const SYSTEM_COAST: bool = cfg!(target_os = "macos");

impl ScrollPhysics {
    /// `UIScrollView`'s normal deceleration.
    pub const APPLE: ScrollPhysics = ScrollPhysics {
        curve: FlingCurve::Decay {
            rate: 0.998,
            hard_rate: 0.998,
        },
        overscroll: OverscrollStyle::Bounce,
        system_coast: SYSTEM_COAST,
        min_velocity: 50.0,
        max_velocity: 8000.0,
    };
    /// Android's fling, at its `ViewConfiguration` speeds.
    pub const ANDROID: ScrollPhysics = ScrollPhysics {
        curve: FlingCurve::Spline,
        overscroll: OverscrollStyle::Stretch,
        system_coast: false,
        min_velocity: 50.0,
        max_velocity: 8000.0,
    };
    /// A firm decay whose hard flicks glide: touch screens elsewhere.
    pub const DEFAULT: ScrollPhysics = ScrollPhysics {
        curve: FlingCurve::Decay {
            rate: 0.997,
            hard_rate: 0.9985,
        },
        overscroll: OverscrollStyle::Bounce,
        system_coast: false,
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
pub(crate) const CATCH_VELOCITY: f64 = 40.0;
/// How soon (seconds) after a catch a flick the same way adds its speed.
const BOOST_WINDOW: f32 = 0.4;
/// A pull `raw` points past the end shows `raw·c·d / (raw·c + d)` of it,
/// `d` being this share of the viewport's extent: just over half the travel
/// at first, flattening so it never shows more than `d`.
pub(crate) const PULL_COEFF: f32 = 0.55;
pub(crate) const PULL_RANGE: f32 = 0.35;
/// The spring a pull relaxes along, `x(t) = (x0 + (v0 + λ·x0)·t)·e^(−λ·t)`:
/// critically damped, its `λ` per second. A coast reaching the end at `v`
/// overshoots by `v/(λ·e)` and settles in about `7.5/λ` seconds.
pub(crate) const SPRING: f64 = 20.0;
/// The spring a glide eases its offset along, `λ` per second: nine tenths
/// of the way in about `4/λ` seconds, whatever the distance.
const GLIDE: f64 = 14.0;
/// Within this distance (points) and under this speed (pt/s) a glide has
/// arrived.
const GLIDE_NEAR: f64 = 0.5;
const GLIDE_STILL: f64 = 10.0;
/// How far a full pull stretches the content along its axis: a share of
/// the shown pull over the extent.
const STRETCH: f32 = 0.25;
/// The longest step (seconds) one frame advances a coast: a hitch catches
/// up a little rather than jumping.
pub(crate) const MAX_STEP: f64 = 0.1;
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
pub(crate) struct Motion {
    /// The offset's speed (pt/s), signed as the offset moves.
    pub(crate) velocity: f64,
    /// Seconds since release.
    age: f64,
    /// The running average of the frame interval (seconds).
    step: f64,
    curve: FlingCurve,
    /// A spline fling's whole travel and duration, and the travel handed out.
    spline: (f64, f64, f64),
}

impl Motion {
    pub(crate) fn new(velocity: f64, curve: FlingCurve) -> Motion {
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
    pub(crate) fn advance(&mut self, delta: Duration) -> f64 {
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

    pub(crate) fn done(&self) -> bool {
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

/// A viewport's offset easing toward `target`, moving at `velocity` (pt/s),
/// along a critically damped spring of `rate` (1/s).
#[derive(Debug, Clone, Copy)]
pub(crate) struct Glide {
    pub(crate) viewport: NodeId,
    pub(crate) target: Vec2,
    pub(crate) velocity: [f64; 2],
    pub(crate) rate: f64,
}

/// A viewport that follows the end of its content while it stands there:
/// `end` is where the end was at the last layout, `NaN` before the first.
#[derive(Debug, Clone, Copy)]
struct Tail {
    viewport: NodeId,
    end: f32,
}

/// A viewport pulled past the end of its range along its axis.
#[derive(Debug, Clone, Copy)]
struct Pull {
    viewport: NodeId,
    axis: Axis,
    /// How far past the end it shows (points): positive past the greatest
    /// offset, negative past the least.
    shown: f32,
    /// How it relaxes once let go; `None` while a finger holds it.
    spring: Option<Spring>,
    /// Whether a finger pulled it, not a coast's bounce.
    finger: bool,
}

/// A pull relaxing from `x0` at speed `v0` (displayed points, signed as the
/// pull), `t` seconds since.
#[derive(Debug, Clone, Copy)]
struct Spring {
    x0: f64,
    v0: f64,
    t: f64,
}

impl Spring {
    fn at(&self) -> f64 {
        (self.x0 + (self.v0 + SPRING * self.x0) * self.t) * (-SPRING * self.t).exp()
    }
}

/// The shown pull for `raw` points of travel past the end of a viewport
/// that shows a pull up to `limit`, and back.
fn shown_pull(raw: f32, limit: f32) -> f32 {
    let d = limit.max(1.0);
    (raw.abs() * PULL_COEFF * d / (raw.abs() * PULL_COEFF + d)).copysign(raw)
}

fn raw_pull(shown: f32, limit: f32) -> f32 {
    let d = limit.max(1.0);
    let s = shown.abs().min(d * 0.999);
    (s * d / (PULL_COEFF * (d - s))).copysign(shown)
}

/// A window's coasting and pulled viewports, and what the catches and
/// flicks read.
#[derive(Debug, Default)]
pub(crate) struct ScrollMotions {
    physics: ScrollPhysics,
    flings: Vec<Fling>,
    pub(crate) glides: Vec<Glide>,
    tails: Vec<Tail>,
    pulls: Vec<Pull>,
    caught: Option<Caught>,
    /// The pointer whose press caught a coast: its samples reach no control.
    swallowed: Option<PointerId>,
    /// When the sample under routing arrived.
    input_time: Option<Instant>,
    /// When the last trackpad scroll sample arrived, for the speed of the
    /// system's coast.
    last_scroll: Option<Instant>,
    /// The viewport the system's coast bounced at: the rest of that coast
    /// moves it no more.
    pinned: Option<NodeId>,
    /// The trackpad fingers' scroll so far, and its samples, for the speed
    /// it lifts at.
    pad: (Vec2, VelocityTracker),
    /// The viewports along rows that a wheel turning up and down scrolls,
    /// ascending by index.
    across: Vec<NodeId>,
}

impl ScrollMotions {
    /// Forget every coast; the physics and the clock stay.
    pub(crate) fn clear(&mut self) {
        self.flings.clear();
        self.glides.clear();
        self.tails.clear();
        self.pulls.clear();
        self.caught = None;
        self.swallowed = None;
        self.pinned = None;
        self.across.clear();
    }
}

impl NodeStore {
    /// Let a wheel turning up and down scroll viewport `id`, which scrolls
    /// along rows, as one turning across does — a strip of tabs a mouse
    /// with one wheel scrolls — or no longer.
    pub fn set_wheel_across(&mut self, id: NodeId, takes: bool) {
        let live = self.arena().is_live(id);
        let across = &mut self.motions.across;
        across.retain(|&n| n != id);
        if takes && live {
            let at = across.partition_point(|n| n.index() < id.index());
            across.insert(at, id);
        }
    }

    /// Whether a wheel turning up and down scrolls viewport `id` across.
    pub fn wheel_across(&self, id: NodeId) -> bool {
        self.motions.across.contains(&id)
    }

    /// A wheel's `delta` over `target` as the viewport that takes it reads
    /// it: one turning only up and down is across under the first viewport
    /// from `target` up, if that viewport [takes it so](Self::set_wheel_across).
    pub(crate) fn wheel_delta(&self, target: NodeId, delta: Vec2) -> Vec2 {
        if self.motions.across.is_empty() || delta.x != 0.0 || delta.y == 0.0 {
            return delta;
        }
        let mut node = Some(target);
        while let Some(n) = node {
            if self.scroll_axis(n).is_some() {
                return if self.wheel_across(n) {
                    Vec2::new(delta.y, 0.0)
                } else {
                    delta
                };
            }
            node = self.arena().parent(n);
        }
        delta
    }
}

impl NodeStore {
    /// How this window's viewports coast and overscroll;
    /// [`ScrollPhysics::native`] unless set.
    pub fn scroll_physics(&self) -> ScrollPhysics {
        self.motions.physics
    }

    /// Make this window's viewports coast and overscroll by `physics`.
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

    /// Whether a viewport coasts or glides or a pull relaxes: the frame loop
    /// keeps beating while one does.
    #[inline]
    pub fn is_scroll_moving(&self) -> bool {
        !self.motions.flings.is_empty()
            || !self.motions.glides.is_empty()
            || self.motions.pulls.iter().any(|p| p.spring.is_some())
            || self.refreshers.easing()
            || self.snap_aims()
            || self.viewers.moving()
            || self.row_gestures.moving()
            || self.reorder_scrolled()
            || self.sheets.moving()
            || self.drag_drops.moving()
    }

    /// Whether viewport `id` glides toward a target.
    pub fn is_gliding(&self, id: NodeId) -> bool {
        self.motions.glides.iter().any(|g| g.viewport == id)
    }

    /// Ease viewport `id`'s offset to `offset`, clamped to its range as it
    /// goes: a glide under way turns to the new target at its speed, and a
    /// coast stops. While motion is reduced it moves there at once. A stale
    /// handle or a node that does not scroll is a no-op.
    pub fn glide_to(&mut self, id: NodeId, offset: Vec2) {
        if self.scroll_axis(id).is_none() || !self.arena().is_live(id) {
            return;
        }
        self.motions.flings.retain(|f| f.viewport != id);
        if self.reduced_motion() {
            self.motions.glides.retain(|g| g.viewport != id);
            self.set_scroll(id, offset);
            return;
        }
        match self.motions.glides.iter_mut().find(|g| g.viewport == id) {
            Some(glide) => glide.target = offset,
            None => self.motions.glides.push(Glide {
                viewport: id,
                target: offset,
                velocity: [0.0; 2],
                rate: GLIDE,
            }),
        }
    }

    /// Make viewport `id` follow the end of its content while it stands
    /// there, or stop it. Turned on, it opens at the end.
    pub fn set_scroll_tail(&mut self, id: NodeId, on: bool) {
        self.motions.tails.retain(|t| t.viewport != id);
        if on && self.arena().is_live(id) {
            self.motions.tails.push(Tail {
                viewport: id,
                end: f32::NAN,
            });
        }
    }

    /// After a layout, move each tailing viewport that stood at the end of
    /// its content (or glides toward it) to where the end is now.
    pub(crate) fn follow_tails(&mut self) {
        let mut i = self.motions.tails.len();
        while i > 0 {
            i -= 1;
            let tail = self.motions.tails[i];
            let id = tail.viewport;
            let Some(axis) = self.scroll_axis(id).filter(|_| self.arena().is_live(id)) else {
                self.motions.tails.swap_remove(i);
                continue;
            };
            let (least, most) = self.scroll_span(id, axis);
            // A row read right to left ends at its least offset.
            let rtl = axis == Axis::Row
                && self.direction_at(id.index()) == crate::adaptive::LayoutDirection::Rtl;
            let end = if rtl { least } else { most };
            self.motions.tails[i].end = end;
            let offset = self.scroll(id);
            if tail.end.is_nan() {
                self.set_scroll(id, with_axis(offset, axis, end));
                continue;
            }
            let near = |a: f32, b: f32| (a - b).abs() <= 0.5;
            let stood = near(offset.on(axis), tail.end)
                || self.is_gliding(id) && near(self.scroll_target(id).on(axis), tail.end);
            if stood && !near(offset.on(axis), end) {
                self.glide_to(id, with_axis(self.scroll_target(id), axis, end));
            }
        }
    }

    /// Glide viewport `id` to `distance` from its start along `axis`
    /// (leftward on a row read right to left).
    pub(crate) fn glide_along(&mut self, id: NodeId, axis: Axis, distance: f32) {
        let along = if axis == Axis::Row
            && self.direction_at(id.index()) == crate::adaptive::LayoutDirection::Rtl
        {
            -distance
        } else {
            distance
        };
        self.glide_to(id, with_axis(self.scroll_target(id), axis, along));
    }

    /// Where viewport `id` is headed: its glide's target, else its offset.
    pub fn scroll_target(&self, id: NodeId) -> Vec2 {
        self.motions
            .glides
            .iter()
            .find(|g| g.viewport == id)
            .map_or_else(|| self.scroll(id), |g| g.target)
    }

    /// Whether viewport `id` coasts.
    pub fn is_coasting(&self, id: NodeId) -> bool {
        self.motions.flings.iter().any(|f| f.viewport == id)
    }

    /// How far past the end of its range viewport `id` shows pulled
    /// (points): positive past its greatest offset, negative past its
    /// least, zero while it is within.
    pub fn overscroll(&self, id: NodeId) -> f32 {
        self.motions
            .pulls
            .iter()
            .find(|p| p.viewport == id)
            .map_or(0.0, |p| p.shown)
    }

    /// The shift a bounced pull adds to the content of the viewport in slot
    /// `index`: nothing for a stretch or a pull a puck shows, or with no
    /// pull.
    #[inline]
    pub(crate) fn pull_shift_at(&self, index: u32) -> Vec2 {
        if self.motions.pulls.is_empty() {
            return Vec2::ZERO;
        }
        self.motions
            .pulls
            .iter()
            .find(|p| p.viewport.index() == index && self.arena().is_live(p.viewport))
            .filter(|p| self.pull_look(p) == PullLook::Shift)
            .map_or(Vec2::ZERO, |p| axis_vec(p.axis, p.shown))
    }

    /// How pull `p` shows.
    fn pull_look(&self, p: &Pull) -> PullLook {
        let physics = match self.motions.physics.overscroll {
            OverscrollStyle::Stretch => PullLook::Stretch,
            OverscrollStyle::Bounce | OverscrollStyle::None => PullLook::Shift,
        };
        if self.refreshers.is_empty() {
            return physics;
        }
        self.refresh_pull_look(p.viewport, p.axis, p.shown, p.finger, physics)
    }

    /// The stretch a pull gives viewport `id`'s content: the axis, the
    /// factor, and the end it stretches from (layout space).
    pub(crate) fn pull_stretch(&self, id: NodeId) -> Option<(Axis, f32, f32)> {
        if self.motions.pulls.is_empty() {
            return None;
        }
        let p = self
            .motions
            .pulls
            .iter()
            .find(|p| p.viewport == id)
            .filter(|p| self.pull_look(p) == PullLook::Stretch)?;
        let b = self.bounds(id);
        let (start, extent) = match p.axis {
            Axis::Row => (b.x, b.w),
            Axis::Column => (b.y, b.h),
        };
        if extent <= 0.0 || p.shown == 0.0 {
            return None;
        }
        let pivot = if p.shown < 0.0 { start } else { start + extent };
        Some((p.axis, 1.0 + STRETCH * p.shown.abs() / extent, pivot))
    }

    /// Coast viewport `id` along its axis from an offset speed of
    /// `velocity` (pt/s, signed as the offset moves), clamped to the
    /// physics' fastest, as a flick released at that speed would; a speed
    /// under the physics' least stops its coast instead. A stale handle or a
    /// node that does not scroll is a no-op.
    ///
    /// A snapping viewport glides to the item the coast would near instead,
    /// however slow (see [`snap`](crate::snap)).
    pub fn fling(&mut self, id: NodeId, velocity: f32) {
        let physics = self.motions.physics;
        if self.snaps(id)
            && self.release_snap(
                id,
                velocity.clamp(-physics.max_velocity, physics.max_velocity),
            )
        {
            return;
        }
        self.coast(id, velocity);
    }

    /// Coast viewport `id` from `velocity` (pt/s), snapping or not.
    pub(crate) fn coast(&mut self, id: NodeId, velocity: f32) {
        let Some(axis) = self.scroll_axis(id).filter(|_| self.arena().is_live(id)) else {
            return;
        };
        self.motions.flings.retain(|f| f.viewport != id);
        self.motions.glides.retain(|g| g.viewport != id);
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

    /// Stop viewport `id`'s coast or glide where it is, and drop a pull of
    /// it back within its range at once.
    pub fn stop_coast(&mut self, id: NodeId) {
        self.motions.flings.retain(|f| f.viewport != id);
        self.motions.glides.retain(|g| g.viewport != id);
        if let Some(at) = self.motions.pulls.iter().position(|p| p.viewport == id) {
            self.motions.pulls.swap_remove(at);
            self.mark_dirty(id, DirtyClass::TRANSFORM);
        }
    }

    /// Advance every coast, glide and relaxing pull by a frame of `delta`,
    /// moving its viewport; a coast that slowed to a stop or lost its node
    /// ends, one reaching its end bounces or is absorbed into a stretch, a
    /// glide that arrived ends on its target, and a pull that settled is
    /// gone. A flat pass with no allocation.
    pub fn tick_scroll_motion(&mut self, delta: Duration) {
        self.tick_glides(delta.as_secs_f64().min(MAX_STEP));
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
                if edge && !fling.motion.done() {
                    self.bounce(id, fling.axis, fling.motion.velocity);
                }
            } else {
                self.motions.flings[i] = fling;
            }
        }
        let dt = delta.as_secs_f64().min(MAX_STEP);
        let mut i = self.motions.pulls.len();
        while i > 0 {
            i -= 1;
            let mut pull = self.motions.pulls[i];
            let id = pull.viewport;
            let Some(mut spring) = pull.spring else {
                continue;
            };
            if !self.arena().is_live(id) {
                self.motions.pulls.swap_remove(i);
                continue;
            }
            spring.t += dt;
            let limit = f64::from(self.pull_limit(id, pull.axis));
            let x = spring.at().clamp(-limit, limit) as f32;
            self.mark_dirty(id, DirtyClass::TRANSFORM);
            if spring.t * SPRING > 1.0 && x.abs() < 0.25 || spring.t * SPRING > 7.5 {
                self.motions.pulls.swap_remove(i);
                continue;
            }
            pull.shown = x;
            pull.spring = Some(spring);
            self.motions.pulls[i] = pull;
        }
        self.tick_refreshers(dt as f32);
        self.tick_viewers(delta);
        self.tick_swipes(delta);
        self.tick_reorders(delta);
        self.tick_sheets(delta);
        self.tick_drag_drops(delta);
    }

    /// Ease every glide `dt` seconds along its spring.
    fn tick_glides(&mut self, dt: f64) {
        let mut i = self.motions.glides.len();
        while i > 0 {
            i -= 1;
            let mut glide = self.motions.glides[i];
            let id = glide.viewport;
            if !self.arena().is_live(id) {
                self.motions.glides.swap_remove(i);
                continue;
            }
            let now = self.scroll(id);
            let mut next = now;
            let mut arrived = true;
            for (k, axis) in [Axis::Row, Axis::Column].into_iter().enumerate() {
                let (least, most) = self.scroll_span(id, axis);
                let target = f64::from(glide.target.on(axis).clamp(least, most));
                let e = f64::from(now.on(axis)) - target;
                let v = glide.velocity[k];
                // The spring's closed form over the step:
                // e(t) = (e + (v + λe)t)·e^(−λt).
                let decay = (-glide.rate * dt).exp();
                let c = v + glide.rate * e;
                let e = (e + c * dt) * decay;
                let v = (v - glide.rate * c * dt) * decay;
                glide.velocity[k] = v;
                let at = if e.abs() < GLIDE_NEAR && v.abs() < GLIDE_STILL {
                    target
                } else {
                    arrived = false;
                    target + e
                };
                match axis {
                    Axis::Row => next.x = at as f32,
                    Axis::Column => next.y = at as f32,
                }
            }
            self.set_scroll(id, next);
            if arrived {
                self.motions.glides.swap_remove(i);
                self.snap_arrived(id);
            } else {
                self.motions.glides[i] = glide;
            }
        }
    }

    /// The farthest viewport `id` shows pulled along `axis`: a refreshable
    /// one, far enough past its mark.
    fn pull_limit(&self, id: NodeId, axis: Axis) -> f32 {
        let b = self.bounds(id);
        let extent = match axis {
            Axis::Row => b.w,
            Axis::Column => b.h,
        };
        let limit = (extent * PULL_RANGE).max(1.0);
        match self.refresh_reach(id) {
            Some(reach) if axis == Axis::Column => limit.max(reach),
            _ => limit,
        }
    }

    /// Add `by` to how far viewport `id` shows pulled along `axis`: a pull a
    /// finger holds stays held, one relaxing relaxes from there, and with
    /// none a new one springs back from `by`.
    pub(crate) fn shift_pull(&mut self, id: NodeId, axis: Axis, by: f32) {
        if by == 0.0 {
            return;
        }
        self.mark_dirty(id, DirtyClass::TRANSFORM);
        let Some(at) = self.motions.pulls.iter().position(|p| p.viewport == id) else {
            self.motions.pulls.push(Pull {
                viewport: id,
                axis,
                shown: by,
                spring: Some(Spring {
                    x0: f64::from(by),
                    v0: 0.0,
                    t: 0.0,
                }),
                finger: false,
            });
            return;
        };
        let pull = &mut self.motions.pulls[at];
        pull.shown += by;
        if let Some(spring) = &mut pull.spring {
            *spring = Spring {
                x0: f64::from(pull.shown),
                v0: 0.0,
                t: 0.0,
            };
        }
        if pull.shown.abs() < 1e-3 {
            self.motions.pulls.swap_remove(at);
        }
    }

    /// Whether a finger holds viewport `id` pulled down past its start.
    pub(crate) fn finger_pull(&self, id: NodeId) -> bool {
        self.motions
            .pulls
            .iter()
            .any(|p| p.viewport == id && p.finger && p.spring.is_none() && p.shown < 0.0)
    }

    /// Drop the pull a finger holds of viewport `id` past its start; whether
    /// there was one.
    pub(crate) fn take_finger_pull(&mut self, id: NodeId) -> bool {
        let at = self
            .motions
            .pulls
            .iter()
            .position(|p| p.viewport == id && p.finger && p.spring.is_none() && p.shown < 0.0);
        if let Some(at) = at {
            self.motions.pulls.swap_remove(at);
            self.mark_dirty(id, DirtyClass::TRANSFORM);
        }
        at.is_some()
    }

    /// A coast reached viewport `id`'s end at `velocity`: it carries on past
    /// the end and springs back, its overshoot easing under the limit.
    fn bounce(&mut self, id: NodeId, axis: Axis, velocity: f64) {
        if self.motions.physics.overscroll == OverscrollStyle::None
            || self.scroll_range(id, axis) <= 0.0
        {
            return;
        }
        let x0 = f64::from(self.overscroll(id));
        let limit = f64::from(self.pull_limit(id, axis));
        let headroom = (limit - x0.abs()).max(0.0);
        let peak = velocity.abs() / (SPRING * std::f64::consts::E);
        let v0 = if peak + headroom > 0.0 {
            velocity * headroom / (peak + headroom)
        } else {
            0.0
        };
        self.motions.pulls.retain(|p| p.viewport != id);
        self.motions.pulls.push(Pull {
            viewport: id,
            axis,
            shown: x0 as f32,
            spring: Some(Spring { x0, v0, t: 0.0 }),
            finger: false,
        });
    }

    /// A press at or under `target` by `pointer`: stop the coasts of the
    /// viewports enclosing it and hold their relaxing pulls, and shut the
    /// open swipe rows it is not on the actions of. Returns whether one
    /// moved briskly or showed pulled, or a row it shut stands in the
    /// press's list, so the press is the catch and reaches no control.
    pub(crate) fn catch_coast(&mut self, target: NodeId, pointer: PointerId) -> bool {
        let mut brisk = self.press_swipes(target, pointer);
        if !self.is_scroll_moving() && !brisk {
            return false;
        }
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
            }
            if let Some(at) = self.motions.glides.iter().position(|g| g.viewport == n) {
                let glide = self.motions.glides.swap_remove(at);
                brisk |= glide.velocity.iter().any(|v| v.abs() > CATCH_VELOCITY);
                if self.snaps(n) {
                    self.note_snap_catch(target);
                }
            }
            if let Some(pull) = self.motions.pulls.iter_mut().find(|p| p.viewport == n) {
                pull.spring = None;
                brisk = true;
            }
            node = self.arena().parent(n);
        }
        brisk |= self.catch_viewers(target);
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

    /// `pointer`'s catch ended with its lift: the pulls it held relax.
    pub(crate) fn end_swallow(&mut self, pointer: PointerId) {
        if self.swallows(pointer) {
            self.motions.swallowed = None;
            self.let_go_pulls();
        }
        self.settle_caught_snap();
    }

    /// Every pull a finger holds relaxes from rest; a refresher's puck
    /// takes the one it showed.
    pub(crate) fn let_go_pulls(&mut self) {
        self.release_refreshers();
        for pull in &mut self.motions.pulls {
            if pull.spring.is_none() {
                pull.spring = Some(Spring {
                    x0: f64::from(pull.shown),
                    v0: 0.0,
                    t: 0.0,
                });
            }
        }
    }

    /// Stop the coasts and glides of the viewports at or above `target`: a
    /// wheel there moves them now.
    pub(crate) fn stop_coasts_over(&mut self, target: NodeId) {
        self.stop_viewer_coasts(target);
        if self.motions.flings.is_empty() && self.motions.glides.is_empty() {
            return;
        }
        let mut node = Some(target);
        while let Some(n) = node {
            self.motions.flings.retain(|f| f.viewport != n);
            self.motions.glides.retain(|g| g.viewport != n);
            node = self.arena().parent(n);
        }
    }

    /// Fingers came down on a trackpad over `target`: the relaxing pulls of
    /// the viewports enclosing it are held, and a bounced coast's pin is
    /// lifted.
    pub(crate) fn hold_pulls_over(&mut self, target: NodeId) {
        self.motions.pinned = None;
        self.stop_viewer_coasts(target);
        if self.motions.pulls.is_empty() {
            return;
        }
        let mut node = Some(target);
        while let Some(n) = node {
            if let Some(pull) = self.motions.pulls.iter_mut().find(|p| p.viewport == n) {
                pull.spring = None;
            }
            node = self.arena().parent(n);
        }
    }

    /// The seconds since the last trackpad sample, noting this one's time.
    pub(crate) fn scroll_interval(&mut self) -> f32 {
        let now = self.motions.input_time;
        let last = std::mem::replace(&mut self.motions.last_scroll, now);
        match (last, now) {
            (Some(last), Some(now)) => now
                .saturating_duration_since(last)
                .as_secs_f32()
                .clamp(1.0 / 240.0, 0.1),
            _ => 1.0 / 60.0,
        }
    }

    /// Note a trackpad fingers' sample moving the offsets by `delta`; a new
    /// gesture starts afresh. A sample that moves nothing (the lift's) tells
    /// no speed.
    pub(crate) fn track_pad(&mut self, delta: Vec2, began: bool) {
        let Some(now) = self.motions.input_time else {
            return;
        };
        if delta == Vec2::ZERO && !began {
            return;
        }
        let (at, track) = &mut self.motions.pad;
        if began {
            *at = Vec2::ZERO;
            *track = VelocityTracker::default();
        }
        *at += delta;
        track.push(now, [at.x, at.y]);
    }

    /// The trackpad fingers lifted from `target`: unless the system coasts
    /// on, the viewports they moved coast at the speed they lifted at, and a
    /// pull they held springs back.
    pub(crate) fn lift_pad(&mut self, target: NodeId) {
        // A viewer they moved lets go first; the rest is the viewports'.
        let v = self.motions.pad.1.velocity();
        let v = self.lift_viewer(target, v);
        if self.motions.physics.system_coast {
            // A snapping viewport snaps instead, and the system's coast
            // leaves it be.
            if let Some(viewport) = self.snap_lift(target, Vec2 { x: -v.x, y: -v.y }) {
                self.motions.pinned = Some(viewport);
            }
            self.let_go_pulls();
            return;
        }
        self.motions.pad.1 = VelocityTracker::default();
        // The offset moved by the samples' deltas: the finger's way is back.
        self.release_pan(target, Vec2 { x: -v.x, y: -v.y });
    }

    /// Whether the system's coast moves nothing at or above `target` any
    /// more: it bounced at a viewport there.
    pub(crate) fn coast_pinned(&self, target: NodeId) -> bool {
        self.motions
            .pinned
            .is_some_and(|pin| self.arena().is_live(pin) && self.encloses(pin, target))
    }

    /// The system's coast ended.
    pub(crate) fn unpin_coast(&mut self) {
        self.motions.pinned = None;
    }

    /// What of the system's coast from `target` no viewport could scroll,
    /// at `velocity` (pt/s): the viewport it reached the end of bounces, and
    /// the rest of the coast leaves it be.
    pub(crate) fn bounce_coast(&mut self, target: NodeId, velocity: Vec2) {
        if self.motions.physics.overscroll == OverscrollStyle::None {
            return;
        }
        for axis in [Axis::Row, Axis::Column] {
            let v = velocity.on(axis);
            if v == 0.0 {
                continue;
            }
            if let Some(viewport) = self.puller(target, axis) {
                self.bounce(viewport, axis, f64::from(v));
                self.motions.pinned = Some(viewport);
            }
        }
    }

    /// Whether a pan from `from` along `axis`, moving the offset by
    /// `delta`, may pull a viewport past its end: one at or above it scrolls
    /// that way with range to show, or refreshes when pulled so.
    pub(crate) fn can_pull(&self, from: NodeId, axis: Axis, delta: f32) -> bool {
        self.puller(from, axis).is_some_and(|n| {
            self.motions.physics.overscroll != OverscrollStyle::None
                || self.refresh_takes(n, axis, delta)
        })
    }

    /// The viewport at or above `from` a pull along `axis` shows on: the
    /// innermost scrolling that way with range, or refreshable.
    fn puller(&self, from: NodeId, axis: Axis) -> Option<NodeId> {
        let mut node = Some(from);
        while let Some(n) = node {
            if self.scroll_axis(n) == Some(axis)
                && (self.scroll_range(n, axis) > 0.0 || self.is_refreshable(n))
            {
                return Some(n);
            }
            node = self.arena().parent(n);
        }
        None
    }

    /// A pan from `from` moves the offsets by `delta`, a pulled viewport's
    /// pull first: travel back toward the range eases the pull and, past
    /// it, scrolls; travel further out pulls on. Returns what is left to
    /// scroll.
    pub(crate) fn ease_pulls(&mut self, from: NodeId, delta: Vec2) -> Vec2 {
        let mut rest = delta;
        if self.motions.pulls.is_empty() {
            return rest;
        }
        for axis in [Axis::Row, Axis::Column] {
            let d = rest.on(axis);
            if d == 0.0 {
                continue;
            }
            let Some(at) = self.motions.pulls.iter().position(|p| {
                p.axis == axis && p.spring.is_none() && self.encloses(p.viewport, from)
            }) else {
                continue;
            };
            let pull = self.motions.pulls[at];
            let limit = self.pull_limit(pull.viewport, axis);
            let raw = raw_pull(pull.shown, limit) + d;
            self.mark_dirty(pull.viewport, DirtyClass::TRANSFORM);
            // Back within the range (to within a rounding of it): the rest
            // scrolls.
            let left = if raw.abs() < 1e-3 || raw.signum() != pull.shown.signum() {
                self.motions.pulls.swap_remove(at);
                if raw.abs() < 1e-3 { 0.0 } else { raw }
            } else {
                self.motions.pulls[at].shown = shown_pull(raw, limit);
                0.0
            };
            match axis {
                Axis::Row => rest.x = left,
                Axis::Column => rest.y = left,
            }
        }
        rest
    }

    /// What of a pan's `delta` from `from` no viewport could scroll pulls
    /// the innermost one along its axis past its end.
    pub(crate) fn pull_on(&mut self, from: NodeId, delta: Vec2) {
        for axis in [Axis::Row, Axis::Column] {
            let d = delta.on(axis);
            if d == 0.0 {
                continue;
            }
            let Some(viewport) = self.puller(from, axis) else {
                continue;
            };
            if self.motions.physics.overscroll == OverscrollStyle::None
                && !self.refresh_takes(viewport, axis, d)
            {
                continue;
            }
            let shown = shown_pull(d, self.pull_limit(viewport, axis));
            self.motions.pulls.retain(|p| p.viewport != viewport);
            self.motions.pulls.push(Pull {
                viewport,
                axis,
                shown,
                spring: None,
                finger: true,
            });
            self.mark_dirty(viewport, DirtyClass::TRANSFORM);
        }
    }

    /// A pan from `from` lifted with the finger at `velocity` (pt/s): a
    /// pull it holds springs back carrying that speed; else the viewport
    /// carrying each axis of it coasts on, the way the content moved, with
    /// the speed of a coast caught moments before the same way added.
    pub(crate) fn release_pan(&mut self, from: NodeId, velocity: Vec2) {
        self.release_refreshers();
        let caught = self.motions.caught.take();
        let now = self.motions.input_time;
        for axis in [Axis::Row, Axis::Column] {
            // Content follows the finger: the offset moves against it.
            let speed = -velocity.on(axis);
            if let Some(at) = self
                .motions
                .pulls
                .iter()
                .position(|p| p.axis == axis && p.spring.is_none())
            {
                let pull = self.motions.pulls[at];
                // The finger's speed as the pull shows it: the curve's slope
                // where it stands.
                let d = self.pull_limit(pull.viewport, axis);
                let raw = raw_pull(pull.shown, d).abs();
                let slope = PULL_COEFF * d * d / (raw * PULL_COEFF + d).powi(2);
                self.motions.pulls[at].spring = Some(Spring {
                    x0: f64::from(pull.shown),
                    v0: f64::from(speed * slope),
                    t: 0.0,
                });
                continue;
            }
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
        // A snapping viewport released too slowly to coast snaps where it is.
        self.settle_snaps_over(from);
        self.let_go_pulls();
    }

    /// How far a coast from `velocity` (pt/s) travels before it stops, on
    /// the physics' curve, signed as the velocity.
    pub(crate) fn coast_travel(&self, velocity: f32) -> f32 {
        let physics = self.motions.physics;
        if velocity.abs() < physics.min_velocity {
            return 0.0;
        }
        let v = velocity.clamp(-physics.max_velocity, physics.max_velocity);
        let mut motion = Motion::new(f64::from(v), physics.curve);
        let step = Duration::from_micros(8333);
        let mut travel = 0.0;
        for _ in 0..2000 {
            travel += motion.advance(step);
            if motion.done() {
                break;
            }
        }
        travel as f32
    }

    /// Whether `node` is `ancestor` or under it.
    fn encloses(&self, ancestor: NodeId, node: NodeId) -> bool {
        let mut at = Some(node);
        while let Some(n) = at {
            if n == ancestor {
                return true;
            }
            at = self.arena().parent(n);
        }
        false
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

fn with_axis(v: Vec2, axis: Axis, value: f32) -> Vec2 {
    match axis {
        Axis::Row => Vec2 { x: value, ..v },
        Axis::Column => Vec2 { y: value, ..v },
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

    use crate::component::{BuildCx, FlexStyle, ScrollBehavior, ScrollRequest, ScrollStyle};
    use crate::layout::{Length, Size};
    use crate::scroll_bar::ScrollBars;
    use viso_render::Rect;

    const VIEW: Rect = Rect {
        x: 0.0,
        y: 0.0,
        w: 100.0,
        h: 200.0,
    };

    /// A 100 by 200 viewport over a column of `rows` rows 50 points tall,
    /// its rows' column inside, tailing or not.
    fn list(rows: usize, tail: bool) -> (NodeStore, NodeId, NodeId) {
        let mut store = NodeStore::new();
        let mut column = None;
        let viewport = {
            let mut cx = BuildCx::new(&mut store);
            cx.scroll(
                ScrollStyle {
                    size: Size::fixed(VIEW.w, VIEW.h),
                    bars: ScrollBars::Hidden,
                    tail,
                    ..ScrollStyle::default()
                },
                |cx| {
                    column = Some(
                        cx.flex(
                            FlexStyle {
                                axis: Axis::Column,
                                size: Size {
                                    width: Length::Fill { weight: 1.0 },
                                    height: Length::Fit,
                                },
                                ..FlexStyle::default()
                            },
                            |cx| {
                                for _ in 0..rows {
                                    row(cx);
                                }
                            },
                        )
                        .id(),
                    );
                },
            )
            .id()
        };
        store.layout(viewport, VIEW, &mut Vec::new());
        (store, viewport, column.unwrap())
    }

    fn row(cx: &mut BuildCx<'_>) -> NodeId {
        cx.leaf(crate::component::LeafStyle {
            size: Size::fixed(100.0, 50.0),
            ..Default::default()
        })
        .id()
    }

    fn frame(store: &mut NodeStore, viewport: NodeId) {
        store.relayout_dirty(viewport, VIEW, &mut Vec::new(), &mut Vec::new());
    }

    /// Run `n` frames of 16 ms; the offsets they showed.
    fn frames(store: &mut NodeStore, viewport: NodeId, n: usize) -> Vec<f32> {
        (0..n)
            .map(|_| {
                store.tick_scroll_motion(Duration::from_millis(16));
                frame(store, viewport);
                store.scroll(viewport).y
            })
            .collect()
    }

    fn smooth(store: &mut NodeStore, id: NodeId, request: ScrollRequest) {
        store.request_scroll_with(id, request, ScrollBehavior::Smooth);
    }

    #[test]
    fn a_smooth_scroll_glides_to_its_target_moving_only_the_transform() {
        let (mut store, viewport, _) = list(20, false);
        smooth(
            &mut store,
            viewport,
            ScrollRequest::To(Vec2 { x: 0.0, y: 600.0 }),
        );
        frame(&mut store, viewport);
        assert!(store.is_gliding(viewport) && store.is_scroll_moving());
        assert_eq!(store.scroll(viewport).y, 0.0, "it starts where it is");
        assert_eq!(store.scroll_target(viewport).y, 600.0);
        store.clear_dirty();
        let seen = frames(&mut store, viewport, 60);
        let dirty = store.dirty(viewport);
        assert!(
            !dirty.intersects(DirtyClass::LAYOUT | DirtyClass::PAINT),
            "{dirty:?}"
        );
        assert!(
            seen.windows(2).all(|w| w[1] >= w[0]),
            "it never overshoots back"
        );
        assert!(
            seen[17] > 540.0,
            "most of the way in a quarter second: {}",
            seen[17]
        );
        assert_eq!(*seen.last().unwrap(), 600.0, "it ends on its target");
        assert!(!store.is_gliding(viewport));

        // Past the range, it glides to the end.
        smooth(
            &mut store,
            viewport,
            ScrollRequest::To(Vec2 { x: 0.0, y: 5000.0 }),
        );
        frame(&mut store, viewport);
        frames(&mut store, viewport, 60);
        assert_eq!(store.scroll(viewport).y, 800.0);
    }

    #[test]
    fn under_reduced_motion_a_smooth_scroll_jumps_but_a_flick_still_coasts() {
        let (mut store, viewport, _) = list(20, false);
        smooth(
            &mut store,
            viewport,
            ScrollRequest::To(Vec2 { x: 0.0, y: 300.0 }),
        );
        frame(&mut store, viewport);
        frames(&mut store, viewport, 2);
        assert!(store.is_gliding(viewport));
        // A glide under way lands as motion is reduced.
        store.set_reduced_motion(true);
        assert_eq!(store.scroll(viewport).y, 300.0);
        assert!(!store.is_gliding(viewport));
        smooth(
            &mut store,
            viewport,
            ScrollRequest::To(Vec2 { x: 0.0, y: 5000.0 }),
        );
        frame(&mut store, viewport);
        assert_eq!(store.scroll(viewport).y, 800.0, "at once, clamped");
        assert!(!store.is_scroll_moving());
        store.fling(viewport, -2000.0);
        assert!(store.is_coasting(viewport), "a flick is the finger's");
    }

    #[test]
    fn a_new_target_takes_over_at_the_speed_it_moves() {
        let (mut store, viewport, _) = list(20, false);
        smooth(
            &mut store,
            viewport,
            ScrollRequest::To(Vec2 { x: 0.0, y: 400.0 }),
        );
        frame(&mut store, viewport);
        let before = frames(&mut store, viewport, 5);
        let step = before[4] - before[3];
        // Smooth steps add to where it is headed.
        smooth(
            &mut store,
            viewport,
            ScrollRequest::By(Vec2 { x: 0.0, y: 200.0 }),
        );
        frame(&mut store, viewport);
        assert_eq!(store.scroll_target(viewport).y, 600.0);
        let after = frames(&mut store, viewport, 1);
        let next = after[0] - before[4];
        assert!(next > 0.0 && (next - step).abs() < step, "{step} {next}");
        frames(&mut store, viewport, 80);
        assert_eq!(store.scroll(viewport).y, 600.0);

        // An instant scroll, the finger or the wheel stops it where it is.
        smooth(&mut store, viewport, ScrollRequest::To(Vec2::ZERO));
        frame(&mut store, viewport);
        frames(&mut store, viewport, 3);
        store.stop_coasts_over(viewport);
        let at = store.scroll(viewport).y;
        assert!(at > 0.0 && at < 600.0);
        frames(&mut store, viewport, 10);
        assert_eq!(store.scroll(viewport).y, at);
        // A press on a brisk glide is a catch.
        smooth(&mut store, viewport, ScrollRequest::To(Vec2::ZERO));
        frame(&mut store, viewport);
        frames(&mut store, viewport, 2);
        assert!(store.catch_coast(viewport, PointerId(1)));
        assert!(!store.is_gliding(viewport));
    }

    #[test]
    fn a_smooth_reveal_glides_each_viewport_where_the_inner_ones_head() {
        let (mut store, viewport, column) = list(20, false);
        let target = store.arena().children(column).nth(15).unwrap();
        smooth(&mut store, target, ScrollRequest::Reveal);
        frame(&mut store, viewport);
        // Row 15 spans 750..800: its bottom meets the viewport's.
        assert_eq!(store.scroll_target(viewport).y, 600.0);
        frames(&mut store, viewport, 80);
        assert_eq!(store.world(target).y, 150.0);
        // Asked again while it glides back, the reveal reads its target.
        let top = store.arena().children(column).next().unwrap();
        smooth(&mut store, top, ScrollRequest::Reveal);
        frame(&mut store, viewport);
        frames(&mut store, viewport, 2);
        smooth(&mut store, top, ScrollRequest::Reveal);
        frame(&mut store, viewport);
        assert_eq!(store.scroll_target(viewport).y, 0.0);
    }

    #[test]
    fn a_tailing_viewport_follows_the_end_while_it_stands_there() {
        let (mut store, viewport, column) = list(20, true);
        assert_eq!(store.scroll(viewport).y, 800.0, "it opens at the end");
        let grow = |store: &mut NodeStore, n: usize| {
            for _ in 0..n {
                let leaf = store.alloc(
                    crate::layout::LayoutInput::Leaf {
                        size: Size::fixed(100.0, 50.0),
                    },
                    crate::style::BoxStyle::NONE,
                );
                store.arena_append_child(column, leaf);
                store.mark_dirty(column, DirtyClass::MEASURE | DirtyClass::LAYOUT);
            }
        };
        grow(&mut store, 2);
        frame(&mut store, viewport);
        assert!(store.is_gliding(viewport));
        frames(&mut store, viewport, 60);
        assert_eq!(store.scroll(viewport).y, 900.0, "it followed");
        // More arrives while it glides: it heads for the new end.
        grow(&mut store, 2);
        frame(&mut store, viewport);
        frames(&mut store, viewport, 3);
        grow(&mut store, 2);
        frame(&mut store, viewport);
        frames(&mut store, viewport, 80);
        assert_eq!(store.scroll(viewport).y, 1100.0);

        // Scrolled away, it stays put.
        store.set_scroll(viewport, Vec2 { x: 0.0, y: 300.0 });
        frame(&mut store, viewport);
        grow(&mut store, 2);
        frame(&mut store, viewport);
        frames(&mut store, viewport, 30);
        assert_eq!(store.scroll(viewport).y, 300.0);
        // Back at the end, it follows again.
        store.set_scroll(viewport, Vec2 { x: 0.0, y: 5000.0 });
        frame(&mut store, viewport);
        grow(&mut store, 1);
        frame(&mut store, viewport);
        frames(&mut store, viewport, 60);
        assert_eq!(store.scroll(viewport).y, 1250.0);
    }
}
