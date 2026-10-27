//! Animation: the look transitions and animations a node store runs.
//!
//! A look transition moves a node's fill, opacity, corner radius, text color,
//! translate, fixed size or check mark to a new value over a [`Timing`]: the store keeps
//! the moving ones in a flat list beside its columns and the driver ticks it
//! each frame, writing each value with the invalidation its property takes
//! ([`NodeStore::transition`](crate::NodeStore::transition)): PAINT for a look, TRANSFORM for a translate,
//! MEASURE and LAYOUT for a size.
//!
//! An animation a node runs by request ([`NodeStore::animate`](crate::NodeStore::animate)) moves the
//! value a node presents of its translate, scale, rotation or opacity toward
//! a target and then shows the property's model value again: what the node
//! is told to show meanwhile lands in the model and shows once it ends. How
//! it ended reaches its handler and the node's watcher after the frame's
//! layout ([`deliver_notices`](crate::notice::deliver_notices)).

use core::time::Duration;

use crate::layout::Vec2;
use crate::node::NodeId;
use viso_render::Rgba;

pub use viso_math::{CubicBezier, Curve, Easing, Spring};

/// How a move covers its way.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Pace {
    /// Over `duration`, its progress following `easing`.
    Eased { duration: Duration, easing: Easing },
    /// On a spring, released from the value shown at the velocity it had:
    /// the spring, not a duration, says how long it takes, and a move
    /// retargeted mid-flight carries its velocity on.
    Spring(Spring),
}

/// What becomes of a move while motion is reduced
/// ([`NodeStore::set_reduced_motion`](crate::NodeStore::set_reduced_motion)).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReducedMotion {
    /// It ends the moment it starts.
    #[default]
    Instant,
    /// It runs all the same: a move that tells something, a progress shown.
    Keep,
}

/// How a look property moves to a new value: it holds for `delay`, then
/// moves at its `pace`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Timing {
    /// The time before the move begins.
    pub delay: Duration,
    /// How the move covers its way.
    pub pace: Pace,
    /// What becomes of it while motion is reduced.
    pub reduced: ReducedMotion,
}

impl Default for Timing {
    fn default() -> Self {
        Timing::INSTANT
    }
}

impl Timing {
    /// A move that ends the moment it starts.
    pub const INSTANT: Timing = Timing::eased(Duration::ZERO, Easing::Linear);

    /// A move over `duration` following `easing`.
    pub const fn eased(duration: Duration, easing: Easing) -> Self {
        Timing {
            delay: Duration::ZERO,
            pace: Pace::Eased { duration, easing },
            reduced: ReducedMotion::Instant,
        }
    }

    /// A move on `spring`.
    pub const fn spring(spring: Spring) -> Self {
        Timing {
            delay: Duration::ZERO,
            pace: Pace::Spring(spring),
            reduced: ReducedMotion::Instant,
        }
    }

    /// The same move begun `delay` later.
    pub const fn after(self, delay: Duration) -> Self {
        Timing { delay, ..self }
    }

    /// The same move, `reduced` while motion is reduced.
    pub const fn reduced(self, reduced: ReducedMotion) -> Self {
        Timing { reduced, ..self }
    }

    /// Whether a move with this timing ends the moment it starts.
    #[inline]
    pub fn is_instant(&self) -> bool {
        self.delay.is_zero()
            && matches!(self.pace, Pace::Eased { duration, .. } if duration.is_zero())
    }
}

/// The numbers a look value moves by: up to four, the rest zero.
pub(crate) type Parts = [f32; 4];

/// A value of a node's look a transition moves.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LookValue {
    /// The color its box fills with.
    Fill(Rgba),
    /// The opacity it and its subtree paint at.
    Opacity(f32),
    /// Its corner radius: `dp`, and `pct` of the shorter side of its box.
    Radius { dp: f32, pct: f32 },
    /// The color its text draws with.
    TextColor(Rgba),
    /// How far it and its subtree are moved, in dp.
    Translate(Vec2),
    /// Its fixed width, in dp.
    Width(f32),
    /// Its fixed height, in dp.
    Height(f32),
    /// How much it and its subtree scale about its origin.
    Scale(f32),
    /// How far it and its subtree turn clockwise about its origin, in
    /// radians.
    Rotation(f32),
    /// How far a check's mark has moved toward checked: `0` unchecked, `1`
    /// checked.
    Check(f32),
    /// How far a segmented control's thumb has slid to the selected
    /// segment: `0` where it slid from, `1` on the segment.
    Thumb(f32),
}

impl LookValue {
    /// Whether `self` and `other` are values of the same property.
    #[inline]
    fn same_property(&self, other: &LookValue) -> bool {
        std::mem::discriminant(self) == std::mem::discriminant(other)
    }

    /// The numbers it moves by: a color's premultiplied linear channels, so
    /// a fade from transparent keeps its hue instead of passing through
    /// black, an offset's two axes, a radius's `dp` and `pct`, else the one
    /// number.
    fn parts(self) -> Parts {
        match self {
            LookValue::Fill(c) | LookValue::TextColor(c) => {
                let p = c.premultiply();
                [p.r, p.g, p.b, p.a]
            }
            LookValue::Radius { dp, pct } => [dp, pct, 0.0, 0.0],
            LookValue::Translate(v) => [v.x, v.y, 0.0, 0.0],
            LookValue::Opacity(v)
            | LookValue::Width(v)
            | LookValue::Height(v)
            | LookValue::Scale(v)
            | LookValue::Rotation(v)
            | LookValue::Check(v)
            | LookValue::Thumb(v) => [v, 0.0, 0.0, 0.0],
        }
    }

    /// The value of its property moved to `parts`. Parts carried past where
    /// a move ends (an easing or a spring that overshoots) move a translate,
    /// scale or rotation past its target, but hold a color, an opacity and a
    /// check's mark in range and an extent and a radius at zero or more.
    fn with_parts(self, p: Parts) -> LookValue {
        let unit = |x: f32| x.clamp(0.0, 1.0);
        let size = |x: f32| x.max(0.0);
        let color = || {
            let a = unit(p[3]);
            let c = |x: f32| unit(x).min(a);
            viso_math::LinearPremul::new(c(p[0]), c(p[1]), c(p[2]), a).unpremultiply()
        };
        match self {
            LookValue::Fill(_) => LookValue::Fill(color()),
            LookValue::TextColor(_) => LookValue::TextColor(color()),
            LookValue::Opacity(_) => LookValue::Opacity(unit(p[0])),
            LookValue::Radius { .. } => LookValue::Radius {
                dp: size(p[0]),
                pct: size(p[1]),
            },
            LookValue::Translate(_) => LookValue::Translate(Vec2 { x: p[0], y: p[1] }),
            LookValue::Width(_) => LookValue::Width(size(p[0])),
            LookValue::Height(_) => LookValue::Height(size(p[0])),
            LookValue::Scale(_) => LookValue::Scale(size(p[0])),
            LookValue::Rotation(_) => LookValue::Rotation(p[0]),
            LookValue::Check(_) => LookValue::Check(unit(p[0])),
            LookValue::Thumb(_) => LookValue::Thumb(unit(p[0])),
        }
    }

    /// How near its target each part of a spring's move comes before it
    /// rests there: a hundredth of a dp, else a thousandth.
    fn rest(&self) -> f32 {
        match self {
            LookValue::Translate(_) | LookValue::Width(_) | LookValue::Height(_) => 0.01,
            _ => 1e-3,
        }
    }

    /// The value `t` of the way from `self` to `to` (of the same property),
    /// held as [`with_parts`](Self::with_parts) holds.
    fn lerp(self, to: LookValue, t: f32) -> LookValue {
        if !self.same_property(&to) {
            return to;
        }
        let (a, b) = (self.parts(), to.parts());
        to.with_parts(core::array::from_fn(|i| a[i] + (b[i] - a[i]) * t))
    }
}

/// Names an animation a node runs by request ([`NodeStore::animate`](crate::NodeStore::animate)).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AnimationId(pub(crate) u64);

/// How an animation ended.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AnimationEnd {
    pub animation: AnimationId,
    /// The value it moved toward.
    pub target: LookValue,
    /// Whether it ran its course or was finished, rather than cancelled,
    /// replaced by a newer one or dropped with its node.
    pub finished: bool,
}

/// Called once an animation ends, with the state cells it writes.
pub type AnimationEndHandler = Box<dyn FnOnce(&AnimationEnd, &mut crate::state::StateStore)>;

/// A stop of a [`Keyframes`] track: the value it reaches `at` this share of
/// a cycle, eased in from the stop before.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Keyframe {
    /// Its share of a cycle, in `[0, 1]`.
    pub at: f32,
    /// The value it reaches.
    pub value: LookValue,
    /// The curve the move into it follows.
    pub easing: Easing,
}

impl Keyframe {
    /// The stop reaching `value` at `at` of a cycle, linearly.
    pub fn new(at: f32, value: LookValue) -> Self {
        Keyframe {
            at,
            value,
            easing: Easing::Linear,
        }
    }

    /// The same stop, reached along `easing`.
    pub fn eased(self, easing: Easing) -> Self {
        Keyframe { easing, ..self }
    }
}

/// How many cycles a [`Keyframes`] track runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Repeat {
    /// This many, then it ends.
    Times(u32),
    /// Until it is cancelled, finished or replaced.
    Forever,
}

impl Default for Repeat {
    fn default() -> Self {
        Repeat::Times(1)
    }
}

/// Which way the cycles of a [`Keyframes`] track run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PlayDirection {
    /// Each from its first stop to its last.
    #[default]
    Normal,
    /// Each from its last stop to its first, its curves run backward.
    Reverse,
    /// Forward, then backward, and so on.
    Alternate,
    /// Backward, then forward, and so on.
    AlternateReverse,
}

/// A run through stops: each cycle moves the value through them in order of
/// their `at`, from the value shown where the first stop is past the start
/// and holding the last one where it falls short of the end.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Keyframes {
    /// Its stops, of one property.
    pub stops: Vec<Keyframe>,
    /// How long a cycle takes.
    pub duration: Duration,
    /// The time before the first cycle begins.
    pub delay: Duration,
    /// How many cycles it runs.
    pub repeat: Repeat,
    /// Which way each cycle runs.
    pub direction: PlayDirection,
    /// What becomes of it while motion is reduced.
    pub reduced: ReducedMotion,
}

impl Keyframes {
    /// One forward cycle of `duration` through `stops`.
    pub fn new(duration: Duration, stops: impl IntoIterator<Item = Keyframe>) -> Self {
        Keyframes {
            stops: stops.into_iter().collect(),
            duration,
            ..Default::default()
        }
    }

    /// The same track, run `repeat` cycles.
    pub fn repeat(self, repeat: Repeat) -> Self {
        Keyframes { repeat, ..self }
    }

    /// The same track, its cycles run `direction`.
    pub fn direction(self, direction: PlayDirection) -> Self {
        Keyframes { direction, ..self }
    }

    /// The same track, begun `delay` later.
    pub fn after(self, delay: Duration) -> Self {
        Keyframes { delay, ..self }
    }

    /// The same track, `reduced` while motion is reduced.
    pub fn reduced(self, reduced: ReducedMotion) -> Self {
        Keyframes { reduced, ..self }
    }

    /// Whether it ends the moment it starts.
    pub fn is_instant(&self) -> bool {
        self.delay.is_zero()
            && (self.duration.is_zero() || self.repeat == Repeat::Times(0) || self.stops.is_empty())
    }
}

/// A [`Keyframes`] track a node runs, from the value it presented.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TrackRun {
    pub(crate) node: NodeId,
    start: LookValue,
    /// Its stops of one property, in order of `at`, each in `[0, 1]`: never
    /// empty.
    stops: Box<[Keyframe]>,
    elapsed: Duration,
    duration: Duration,
    delay: Duration,
    repeat: Repeat,
    direction: PlayDirection,
}

impl TrackRun {
    /// `track` run on `node` from `start`, keeping the stops of `start`'s
    /// property with a finite `at`, held in `[0, 1]`; `None` without any.
    pub(crate) fn new(node: NodeId, start: LookValue, track: Keyframes) -> Option<Self> {
        let mut stops: Vec<Keyframe> = track
            .stops
            .into_iter()
            .filter(|stop| stop.at.is_finite() && stop.value.same_property(&start))
            .map(|stop| Keyframe {
                at: stop.at.clamp(0.0, 1.0),
                ..stop
            })
            .collect();
        if stops.is_empty() {
            return None;
        }
        stops.sort_by(|a, b| a.at.total_cmp(&b.at));
        Some(TrackRun {
            node,
            start,
            stops: stops.into_boxed_slice(),
            elapsed: Duration::ZERO,
            duration: track.duration,
            delay: track.delay,
            repeat: track.repeat,
            direction: track.direction,
        })
    }

    /// The value it shows `p` of the way through a forward cycle.
    fn sample(&self, p: f32) -> LookValue {
        let (mut at, mut value) = (0.0, self.start);
        for stop in &self.stops {
            if p <= stop.at {
                let span = stop.at - at;
                let t = if span > 0.0 { (p - at) / span } else { 1.0 };
                return value.lerp(stop.value, stop.easing.apply(t));
            }
            (at, value) = (stop.at, stop.value);
        }
        value
    }

    /// The value it shows `time` into its cycles, and whether they are done.
    fn at_time(&self, time: Duration) -> (LookValue, bool) {
        let Some(moving) = time.checked_sub(self.delay) else {
            return (self.start, false);
        };
        let cycles = match self.repeat {
            Repeat::Times(n) => f64::from(n),
            Repeat::Forever => f64::INFINITY,
        };
        let span = self.duration.as_secs_f64();
        let run = if span > 0.0 {
            moving.as_secs_f64() / span
        } else {
            f64::INFINITY
        };
        if run >= cycles {
            return (self.target(), true);
        }
        let cycle = run.floor();
        let mut p = (run - cycle) as f32;
        let odd = cycle % 2.0 == 1.0;
        let backward = match self.direction {
            PlayDirection::Normal => false,
            PlayDirection::Reverse => true,
            PlayDirection::Alternate => odd,
            PlayDirection::AlternateReverse => !odd,
        };
        if backward {
            p = 1.0 - p;
        }
        (self.sample(p), false)
    }

    /// Its last stop's value.
    fn target(&self) -> LookValue {
        self.stops[self.stops.len() - 1].value
    }
}

/// The presented value's move an animation makes: toward a target, or
/// through a track's stops.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Run {
    Move(LookTransition),
    Track(TrackRun),
}

impl Run {
    pub(crate) fn node(&self) -> NodeId {
        match self {
            Run::Move(m) => m.node,
            Run::Track(t) => t.node,
        }
    }

    /// The value it moves toward: a track's last stop.
    pub(crate) fn target(&self) -> LookValue {
        match self {
            Run::Move(m) => m.to,
            Run::Track(t) => t.target(),
        }
    }

    /// Whether it moves the same property of the same node as `value` on
    /// `node`.
    pub(crate) fn moves(&self, node: NodeId, value: &LookValue) -> bool {
        self.node() == node && self.target().same_property(value)
    }

    /// Advances it by `delta`: the value it shows now, and whether it is done.
    pub(crate) fn advance(&mut self, delta: Duration) -> (LookValue, bool) {
        match self {
            Run::Move(m) => m.advance(delta),
            Run::Track(t) => {
                t.elapsed = t.elapsed.saturating_add(delta);
                t.at_time(t.elapsed)
            }
        }
    }

    /// How fast each part moves now, per second: a track's over the last
    /// millisecond.
    pub(crate) fn velocity(&self) -> Parts {
        match self {
            Run::Move(m) => m.velocity(),
            Run::Track(t) => {
                const H: Duration = Duration::from_millis(1);
                let (now, done) = t.at_time(t.elapsed);
                if done || t.elapsed < t.delay + H {
                    return [0.0; 4];
                }
                let (a, b) = (t.at_time(t.elapsed - H).0.parts(), now.parts());
                core::array::from_fn(|i| (b[i] - a[i]) / H.as_secs_f32())
            }
        }
    }
}

/// An animation a node runs by request.
pub(crate) struct NodeAnimation {
    pub(crate) id: AnimationId,
    /// The presented value's move.
    pub(crate) motion: Run,
    /// The property's model value, which shows once it ends.
    pub(crate) model: LookValue,
    pub(crate) on_end: Option<AnimationEndHandler>,
    /// What becomes of it while motion is reduced.
    pub(crate) reduced: ReducedMotion,
    /// Whether its end goes unheard: a page's move through a navigation.
    pub(crate) quiet: bool,
    /// The page that leaves its stack, folding away, once it runs its course.
    pub(crate) hides: Option<NodeId>,
}

/// An ended animation waiting for delivery: its node, how it ended and its
/// handler.
pub(crate) type EndedAnimation = (NodeId, AnimationEnd, Option<AnimationEndHandler>);

/// A node's look property moving from the value it showed toward its target.
/// A rebuild lifts one off the node it leaves
/// ([`NodeStore::lift_transitions`](crate::NodeStore::lift_transitions)) and resumes it on the node that replaces
/// it ([`NodeStore::resume_transition`](crate::NodeStore::resume_transition)).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LookTransition {
    pub(crate) node: NodeId,
    from: LookValue,
    pub(crate) to: LookValue,
    elapsed: Duration,
    pub(crate) timing: Timing,
    /// How fast each part of `from` moved as the move began, per second: what
    /// a spring carries on with.
    velocity: Parts,
}

impl LookTransition {
    pub(crate) fn new(node: NodeId, from: LookValue, to: LookValue, timing: Timing) -> Self {
        LookTransition {
            node,
            from,
            to,
            elapsed: Duration::ZERO,
            timing,
            velocity: [0.0; 4],
        }
    }

    /// The same move begun at `velocity`, when it moves on a spring at once:
    /// a move that waits first, or runs over a duration, begins still.
    pub(crate) fn carrying(mut self, velocity: Parts) -> Self {
        if self.timing.delay.is_zero() && matches!(self.timing.pace, Pace::Spring(_)) {
            self.velocity = velocity;
        }
        self
    }

    /// Whether it moves the same property of the same node as `value` on
    /// `node`.
    #[inline]
    pub(crate) fn moves(&self, node: NodeId, value: &LookValue) -> bool {
        self.node == node && self.to.same_property(value)
    }

    /// Advances it by `delta`: the value it shows now, and whether it is done.
    pub(crate) fn advance(&mut self, delta: Duration) -> (LookValue, bool) {
        self.elapsed = self.elapsed.saturating_add(delta);
        self.at()
    }

    /// The value it shows at its elapsed time, and whether it is done: a
    /// spring once every part has settled within its rest of the target.
    pub(crate) fn at(&self) -> (LookValue, bool) {
        let Some(moving) = self.elapsed.checked_sub(self.timing.delay) else {
            return (self.from, false);
        };
        match self.timing.pace {
            Pace::Eased { duration, easing } => {
                if moving >= duration {
                    return (self.to, true);
                }
                let t = moving.as_secs_f32() / duration.as_secs_f32();
                (self.from.lerp(self.to, easing.apply(t)), false)
            }
            Pace::Spring(spring) => {
                if !self.from.same_property(&self.to) {
                    return (self.to, true);
                }
                let (from, to, rest) = (self.from.parts(), self.to.parts(), self.to.rest());
                let t = moving.as_secs_f32();
                let mut parts = to;
                let mut settled = true;
                for i in 0..4 {
                    let (x, v) = spring.at(from[i] - to[i], self.velocity[i], t);
                    parts[i] += x;
                    settled &= spring.settled(x, v, rest);
                }
                if settled {
                    (self.to, true)
                } else {
                    (self.to.with_parts(parts), false)
                }
            }
        }
    }

    /// How fast each part moves at its elapsed time, per second: still while
    /// it waits and once it is done.
    pub(crate) fn velocity(&self) -> Parts {
        let Some(moving) = self.elapsed.checked_sub(self.timing.delay) else {
            return [0.0; 4];
        };
        if !self.from.same_property(&self.to) {
            return [0.0; 4];
        }
        let (from, to) = (self.from.parts(), self.to.parts());
        match self.timing.pace {
            Pace::Eased { duration, easing } => {
                if moving >= duration {
                    return [0.0; 4];
                }
                // The curve's slope over the millisecond about now.
                let span = duration.as_secs_f32();
                let h = (1e-3 / span).min(1.0);
                let t = (moving.as_secs_f32() / span).clamp(h, 1.0);
                let slope = (easing.apply(t) - easing.apply(t - h)) / (h * span);
                core::array::from_fn(|i| (to[i] - from[i]) * slope)
            }
            Pace::Spring(spring) => {
                let t = moving.as_secs_f32();
                core::array::from_fn(|i| spring.at(from[i] - to[i], self.velocity[i], t).1)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use super::*;
    use crate::component::{BuildCx, LeafStyle, NodeStore};
    use crate::layout::Sizing;
    use viso_render::Rect;

    /// A single 100x100 leaf laid out at the surface origin — the same shape the
    /// component-level translate tests ride on. Returns the store and the node.
    fn one_leaf() -> (NodeStore, NodeId) {
        let mut store = NodeStore::new();
        let root = {
            let mut cx = BuildCx::new(&mut store);
            cx.leaf(LeafStyle {
                size: Sizing::fixed(100.0, 100.0),
                ..Default::default()
            });
            cx.root().unwrap()
        };
        let mut scratch = Vec::new();
        store.layout(
            root,
            Rect {
                x: 0.0,
                y: 0.0,
                w: 200.0,
                h: 200.0,
            },
            &mut scratch,
        );
        (store, root)
    }

    #[test]
    fn each_easing_pins_its_endpoints_and_stays_monotonic() {
        for easing in [
            Easing::Linear,
            Easing::EASE_IN,
            Easing::EASE_OUT,
            Easing::EASE_IN_OUT,
        ] {
            assert_eq!(easing.apply(0.0), 0.0, "{easing:?} starts at 0");
            assert_eq!(easing.apply(1.0), 1.0, "{easing:?} ends at 1");
            // Non-decreasing across the unit interval: no overshoot for these
            // standard curves.
            let mut prev = easing.apply(0.0);
            for step in 1..=20 {
                let t = step as f32 / 20.0;
                let v = easing.apply(t);
                assert!(v >= prev, "{easing:?} is monotonic (t={t}): {v} >= {prev}");
                prev = v;
            }
        }
    }

    #[test]
    fn an_overshooting_easing_carries_a_move_past_its_target_but_holds_a_look_in_range() {
        let t = Easing::Out(Curve::Back).apply(0.6);
        assert!(t > 1.0);
        assert_eq!(
            LookValue::Opacity(0.0).lerp(LookValue::Opacity(1.0), t),
            LookValue::Opacity(1.0)
        );
        assert_eq!(
            LookValue::Width(100.0).lerp(LookValue::Width(0.0), t),
            LookValue::Width(0.0)
        );
        let LookValue::Translate(at) = LookValue::Translate(Vec2::ZERO)
            .lerp(LookValue::Translate(Vec2 { x: 10.0, y: 0.0 }), t)
        else {
            unreachable!()
        };
        assert!(at.x > 10.0, "passes its target");
        let LookValue::Fill(c) = LookValue::Fill(Rgba::new(0.0, 0.0, 0.0, 1.0))
            .lerp(LookValue::Fill(Rgba::new(1.0, 1.0, 1.0, 1.0)), t)
        else {
            unreachable!()
        };
        assert_eq!((c.r, c.a), (1.0, 1.0));
    }

    #[test]
    fn easing_clamps_out_of_range_time() {
        // Time outside [0, 1] pins to the endpoints rather than extrapolating.
        assert_eq!(Easing::Linear.apply(-1.0), 0.0);
        assert_eq!(Easing::Linear.apply(2.0), 1.0);
        assert_eq!(Easing::EASE_OUT.apply(5.0), 1.0);
    }

    fn linear(ms: u64) -> Timing {
        Timing::eased(Duration::from_millis(ms), Easing::Linear)
    }

    const RED: Rgba = Rgba {
        r: 1.0,
        g: 0.0,
        b: 0.0,
        a: 1.0,
    };

    #[test]
    fn a_look_transition_moves_from_the_shown_value_and_settles() {
        let (mut store, node) = one_leaf();
        store.transition(node, LookValue::Opacity(0.0), linear(100));
        assert_eq!(store.opacity(node), 1.0, "it starts from the shown value");
        assert!(store.is_transitioning());

        store.tick_transitions(Duration::from_millis(50));
        assert_eq!(store.opacity(node), 0.5);
        store.tick_transitions(Duration::from_millis(60));
        assert_eq!(store.opacity(node), 0.0, "it arrives at the target");
        assert!(
            !store.is_transitioning(),
            "an arrived transition is dropped"
        );
    }

    fn ink(store: &NodeStore, node: NodeId) -> Rgba {
        match store.content_payload(node) {
            Some(crate::content::Content::Text { color, .. }) => *color,
            _ => panic!("a text"),
        }
    }

    fn text(color: Rgba) -> crate::content::Content {
        crate::content::Content::Text {
            ink: None,
            glyphs: Vec::new(),
            atlas: viso_render::TextureId::new(1),
            color_glyphs: Vec::new(),
            color_atlas: None,
            color,
            natural: Vec2::ZERO,
            baseline: 0.0,
            shaped_at_width: None,
            width_use: crate::WidthUse::Ignored,
            min_content: 0.0,
            max_content: 0.0,
        }
    }

    #[test]
    fn a_radius_translate_size_and_text_color_move_and_follow_what_arrives() {
        use crate::length::{LengthEnv, LengthTerms};
        let ms = Duration::from_millis;
        let (mut store, node) = one_leaf();
        let surface = Rect {
            x: 0.0,
            y: 0.0,
            w: 200.0,
            h: 200.0,
        };
        let relayout = |store: &mut NodeStore| {
            let (mut scratch, mut redo) = (Vec::new(), Vec::new());
            store.relayout_dirty(node, surface, &mut scratch, &mut redo);
        };

        // A radius moves from the one shown; one set at once ends the move.
        store.transition_corner_radius(node, LengthTerms::dp(10.0), linear(100));
        store.tick_transitions(ms(50));
        assert_eq!(store.style(node).radius, 5.0);
        store.set_corner_radius(node, LengthTerms::dp(2.0));
        assert_eq!(store.style(node).radius, 2.0);
        assert!(!store.is_transitioning());

        // A translate in `sp` moves, then follows the text scale it reads.
        let none = LengthTerms::dp(0.0);
        store.set_translate_terms(node, LengthTerms::sp(20.0), none, Some(linear(100)));
        store.tick_transitions(ms(50));
        assert_eq!(store.translate(node).x, 10.0);
        store.set_length_env(LengthEnv {
            text_scale: 2.0,
            ..store.length_env()
        });
        relayout(&mut store);
        store.tick_transitions(ms(50));
        assert_eq!(store.translate(node).x, 25.0, "from 10 halfway to 40");
        store.tick_transitions(ms(50));
        assert_eq!(store.translate(node).x, 40.0);

        // A fixed width moves between fixed extents, laying out each step.
        store.transition_size(node, true, LengthTerms::dp(200.0), linear(100));
        relayout(&mut store);
        let width = |store: &NodeStore| store.size_request(node).unwrap().width;
        assert_eq!(
            width(&store),
            crate::layout::Length::Fixed(100.0),
            "the fold turns the move, not ends it"
        );
        store.tick_transitions(ms(50));
        relayout(&mut store);
        assert_eq!(width(&store), crate::layout::Length::Fixed(150.0));
        store.tick_transitions(ms(50));
        relayout(&mut store);
        assert_eq!(width(&store), crate::layout::Length::Fixed(200.0));

        // A text color moves; a reshaped run arriving keeps the shown color.
        store.set_content_payload(node, text(Rgba::TRANSPARENT));
        store.transition(node, LookValue::TextColor(RED), linear(100));
        store.tick_transitions(ms(50));
        assert_eq!(ink(&store, node).a, 0.5);
        store.set_reflowed_content(node, text(RED));
        assert_eq!(ink(&store, node).a, 0.5, "the move carries on");
        store.tick_transitions(ms(50));
        assert_eq!(ink(&store, node), RED);
        assert!(!store.is_transitioning());
    }

    #[test]
    fn a_retarget_starts_from_where_the_value_is() {
        let (mut store, node) = one_leaf();
        store.transition(node, LookValue::Opacity(0.0), linear(100));
        store.tick_transitions(Duration::from_millis(50));
        store.transition(node, LookValue::Opacity(1.0), linear(100));
        store.tick_transitions(Duration::from_millis(50));
        assert_eq!(store.opacity(node), 0.75, "from 0.5 halfway to 1.0");
        assert!(store.is_transitioning());

        store.transition(node, LookValue::Opacity(1.0), linear(100));
        store.tick_transitions(Duration::from_millis(50));
        assert_eq!(store.opacity(node), 1.0, "the same target keeps its clock");
    }

    #[test]
    fn a_delay_holds_the_shown_value() {
        let (mut store, node) = one_leaf();
        let timing = linear(100).after(Duration::from_millis(40));
        store.transition(node, LookValue::Opacity(0.0), timing);
        store.tick_transitions(Duration::from_millis(40));
        assert_eq!(store.opacity(node), 1.0);
        store.tick_transitions(Duration::from_millis(50));
        assert_eq!(store.opacity(node), 0.5);
    }

    #[test]
    fn a_fill_fades_in_without_losing_its_hue() {
        let (mut store, node) = one_leaf();
        store.transition(node, LookValue::Fill(RED), linear(100));
        store.tick_transitions(Duration::from_millis(50));
        let fill = store.style(node).fill;
        assert_eq!((fill.r, fill.g, fill.b, fill.a), (1.0, 0.0, 0.0, 0.5));
    }

    #[test]
    fn an_instant_timing_or_a_direct_write_settles_at_once() {
        let (mut store, node) = one_leaf();
        store.transition(node, LookValue::Fill(RED), Timing::default());
        assert_eq!(store.style(node).fill, RED);
        assert!(!store.is_transitioning());

        store.transition(node, LookValue::Opacity(0.0), linear(100));
        store.set_opacity(node, 0.25);
        assert!(!store.is_transitioning(), "a direct write ends the move");
        store.tick_transitions(Duration::from_millis(50));
        assert_eq!(store.opacity(node), 0.25);

        store.transition(node, LookValue::Opacity(0.25), linear(100));
        assert!(!store.is_transitioning(), "the shown target moves nothing");
    }

    #[test]
    fn a_transition_on_a_dead_node_is_dropped() {
        let (mut store, node) = one_leaf();
        store.transition(node, LookValue::Opacity(0.0), linear(100));
        store.clear();
        store.tick_transitions(Duration::from_millis(16));
        assert!(!store.is_transitioning());
    }

    #[test]
    fn presenting_keeps_a_move_heading_there_and_turns_one_heading_elsewhere() {
        let (mut store, node) = one_leaf();
        store.transition(node, LookValue::Opacity(0.0), linear(100));
        store.tick_transitions(Duration::from_millis(50));
        store.present(node, LookValue::Opacity(0.0));
        store.tick_transitions(Duration::from_millis(25));
        assert_eq!(store.opacity(node), 0.25, "the same target keeps its clock");

        store.present(node, LookValue::Opacity(1.0));
        store.tick_transitions(Duration::from_millis(50));
        assert_eq!(store.opacity(node), 0.625, "from 0.25 halfway to 1.0");

        let (mut idle, other) = one_leaf();
        idle.present(other, LookValue::Opacity(0.5));
        assert_eq!(idle.opacity(other), 0.5, "nothing moving shows at once");
        assert!(!idle.is_transitioning());
    }

    #[test]
    fn a_lifted_transition_resumes_on_the_node_that_replaces_it() {
        let (mut store, old) = one_leaf();
        store.transition(old, LookValue::Opacity(0.0), linear(100));
        store.tick_transitions(Duration::from_millis(50));
        let mut lifted = Vec::new();
        store.lift_transitions(old, &mut lifted);
        assert_eq!(lifted.len(), 1);
        assert!(!store.is_transitioning());

        let (mut next, node) = one_leaf();
        next.set_opacity(node, 0.0);
        next.resume_transition(node, lifted[0]);
        assert_eq!(next.opacity(node), 0.5, "it shows where the move was");
        next.tick_transitions(Duration::from_millis(25));
        assert_eq!(next.opacity(node), 0.25, "on the move's clock");

        let (mut turned, node) = one_leaf();
        turned.set_opacity(node, 1.0);
        turned.resume_transition(node, lifted[0]);
        assert_eq!(turned.opacity(node), 0.5);
        turned.tick_transitions(Duration::from_millis(50));
        assert_eq!(
            turned.opacity(node),
            0.75,
            "a new target turns it from there"
        );
    }

    #[test]
    fn an_animation_holds_what_the_node_is_told_and_shows_it_once_it_ends() {
        use crate::notice::deliver_notices;
        use crate::state::StateStore;
        use crate::virtual_list::VirtualLists;
        let (mut store, id) = one_leaf();
        store.set_opacity(id, 0.5);
        let ends = Rc::new(Cell::new(None));
        let heard = Rc::clone(&ends);
        let timing = linear(100);
        let on_end: AnimationEndHandler = Box::new(move |end, _| heard.set(Some(end.finished)));
        let animation = store
            .animate(id, LookValue::Opacity(1.0), timing, Some(on_end))
            .expect("opacity animates");
        assert!(store.is_animating(animation));
        store.tick_transitions(Duration::from_millis(50));
        assert_eq!(store.opacity(id), 0.75);
        store.set_opacity(id, 0.25);
        assert_eq!(store.opacity(id), 0.75, "a model write waits for the end");
        store.tick_transitions(Duration::from_millis(50));
        assert_eq!(store.opacity(id), 0.25, "the model shows once it ends");
        assert!(!store.is_animating(animation));
        let mut states = StateStore::new();
        deliver_notices(&mut store, &VirtualLists::new(), &mut states);
        assert_eq!(ends.get(), Some(true));
        assert!(!store.is_transitioning());
        assert_eq!(
            store.animate(id, LookValue::Width(10.0), timing, None),
            None,
            "a size would lay out"
        );
    }

    /// Ticks `store` a frame at a time until it rests, at most `frames`;
    /// the translates it showed along the way.
    fn run(store: &mut NodeStore, node: NodeId, frames: usize) -> Vec<f32> {
        let mut shown = Vec::new();
        for _ in 0..frames {
            if !store.is_transitioning() {
                break;
            }
            store.tick_transitions(Duration::from_millis(4));
            shown.push(store.translate(node).x);
        }
        shown
    }

    #[test]
    fn a_spring_rings_past_its_target_then_rests_on_it() {
        let (mut store, node) = one_leaf();
        let to = LookValue::Translate(Vec2 { x: 100.0, y: 0.0 });
        store.transition(node, to, Timing::spring(Spring::BOUNCY));
        let shown = run(&mut store, node, 2000);
        assert!(!store.is_transitioning(), "it settles");
        assert!(shown.iter().any(|&x| x > 101.0), "it rings past its target");
        assert_eq!(store.translate(node).x, 100.0, "and rests on it exactly");
        // A smooth one never passes it.
        store.transition(
            node,
            LookValue::Translate(Vec2::ZERO),
            Timing::spring(Spring::SMOOTH),
        );
        let shown = run(&mut store, node, 2000);
        assert!(shown.iter().all(|&x| x >= 0.0));
        assert_eq!(store.translate(node).x, 0.0);
        // An opacity rings within its range.
        store.transition(
            node,
            LookValue::Opacity(0.0),
            Timing::spring(Spring::BOUNCY),
        );
        store.tick_transitions(Duration::from_millis(500));
        assert!((0.0..=1.0).contains(&store.opacity(node)));
    }

    #[test]
    fn a_retargeted_spring_carries_its_velocity_on() {
        let (mut store, node) = one_leaf();
        let spring = Timing::spring(Spring::SMOOTH);
        store.transition(
            node,
            LookValue::Translate(Vec2 { x: 100.0, y: 0.0 }),
            spring,
        );
        let shown = run(&mut store, node, 20);
        let (last, before) = (shown[19], shown[19] - shown[18]);
        assert!(before > 0.5, "it moves fast toward 100");
        // Sent back home, it keeps going the way it went, then turns.
        store.transition(node, LookValue::Translate(Vec2::ZERO), spring);
        let shown = run(&mut store, node, 2000);
        let after = shown[0] - last;
        assert!(
            (after - before).abs() < before * 0.3,
            "{before} then {after}"
        );
        assert!(shown.iter().copied().fold(0.0, f32::max) > last + 1.0);
        assert_eq!(store.translate(node).x, 0.0);
    }

    #[test]
    fn a_spring_takes_over_an_eased_move_at_its_speed() {
        let (mut store, node) = one_leaf();
        let to = LookValue::Translate(Vec2 { x: 100.0, y: 0.0 });
        // 250 dp a second: a dp a frame.
        store.transition(node, to, linear(400));
        store.tick_transitions(Duration::from_millis(100));
        assert_eq!(store.translate(node).x, 25.0);
        store.transition(
            node,
            LookValue::Translate(Vec2::ZERO),
            Timing::spring(Spring::SMOOTH),
        );
        let shown = run(&mut store, node, 2000);
        assert!((shown[0] - 25.0 - 1.0).abs() < 0.3, "{}", shown[0]);
        // An eased move that follows a spring begins still: from where it is.
        store.transition(node, to, Timing::spring(Spring::BOUNCY));
        store.tick_transitions(Duration::from_millis(40));
        let at = store.translate(node).x;
        store.transition(node, LookValue::Translate(Vec2::ZERO), linear(100));
        store.tick_transitions(Duration::from_millis(50));
        assert_eq!(store.translate(node).x, at / 2.0);
    }

    #[test]
    fn an_animation_replacing_a_spring_carries_its_velocity_on() {
        let (mut store, id) = one_leaf();
        let spring = Timing::spring(Spring::SMOOTH);
        let to = |x: f32| LookValue::Translate(Vec2 { x, y: 0.0 });
        store.animate(id, to(100.0), spring, None).unwrap();
        let shown = run(&mut store, id, 20);
        let (last, before) = (shown[19], shown[19] - shown[18]);
        let back = store.animate(id, to(0.0), spring, None).unwrap();
        store.tick_transitions(Duration::from_millis(4));
        let after = store.translate(id).x - last;
        assert!(
            (after - before).abs() < before * 0.3,
            "{before} then {after}"
        );
        for _ in 0..2000 {
            store.tick_transitions(Duration::from_millis(4));
        }
        assert!(!store.is_animating(back), "it settles");
    }

    #[test]
    fn a_track_moves_through_its_stops_from_the_value_shown() {
        let (mut store, id) = one_leaf();
        let ms = Duration::from_millis;
        let ends = Rc::new(Cell::new(None));
        let heard = Rc::clone(&ends);
        let on_end: AnimationEndHandler = Box::new(move |end, _| heard.set(Some(*end)));
        // From the shown 1, down to 0 by the half and back by the end.
        let track = Keyframes::new(
            ms(100),
            [
                Keyframe::new(1.0, LookValue::Opacity(1.0)),
                Keyframe::new(0.5, LookValue::Opacity(0.0)),
            ],
        );
        let animation = store.play(id, track, Some(on_end)).unwrap();
        for (step, shown) in [(25, 0.5), (25, 0.0), (25, 0.5)] {
            store.tick_transitions(ms(step));
            assert_eq!(store.opacity(id), shown);
        }
        store.tick_transitions(ms(25));
        assert!(!store.is_animating(animation));
        let ended = store.take_animation_ends();
        assert_eq!(ended.len(), 1);
        let (_, end, handler) = ended.into_iter().next().unwrap();
        assert_eq!((end.target, end.finished), (LookValue::Opacity(1.0), true));
        handler.unwrap()(&end, &mut crate::state::StateStore::new());
        assert!(ends.get().is_some());
    }

    #[test]
    fn a_track_holds_its_last_stop_and_eases_into_each() {
        let (mut store, id) = one_leaf();
        let ms = Duration::from_millis;
        let track = Keyframes::new(
            ms(100),
            [Keyframe::new(0.5, LookValue::Scale(2.0)).eased(Easing::EASE_IN)],
        );
        store.play(id, track, None).unwrap();
        store.tick_transitions(ms(25));
        assert_eq!(store.turn(id).scale, 1.0 + Easing::EASE_IN.apply(0.5));
        store.tick_transitions(ms(50));
        assert_eq!(store.turn(id).scale, 2.0, "held past its last stop");
        store.tick_transitions(ms(25));
        assert_eq!(store.turn(id).scale, 1.0, "the model shows once it ends");
    }

    #[test]
    fn a_track_repeats_reverses_and_alternates() {
        let ms = Duration::from_millis;
        let turn = |direction, repeat, at: &[(u64, f32)]| {
            let (mut store, id) = one_leaf();
            let track = Keyframes::new(ms(100), [Keyframe::new(1.0, LookValue::Rotation(1.0))])
                .repeat(repeat)
                .direction(direction);
            let animation = store.play(id, track, None).unwrap();
            let mut clock = 0;
            for &(t, shown) in at {
                store.tick_transitions(ms(t - clock));
                clock = t;
                let r = store.turn(id).rotation;
                assert!((r - shown).abs() < 1e-5, "{direction:?} at {t}: {r}");
            }
            store.is_animating(animation)
        };
        let twice = Repeat::Times(2);
        assert!(!turn(
            PlayDirection::Normal,
            twice,
            &[(50, 0.5), (150, 0.5), (175, 0.75), (200, 0.0)]
        ));
        assert!(turn(
            PlayDirection::Reverse,
            twice,
            &[(25, 0.75), (125, 0.75)]
        ));
        assert!(!turn(
            PlayDirection::Alternate,
            twice,
            &[(25, 0.25), (125, 0.75), (200, 0.0)]
        ));
        assert!(turn(
            PlayDirection::AlternateReverse,
            Repeat::Forever,
            &[(25, 0.75), (125, 0.25), (10_025, 0.75)]
        ));
    }

    #[test]
    fn a_track_keeps_its_property_s_stops_and_needs_one() {
        let (mut store, id) = one_leaf();
        let ms = Duration::from_millis;
        let mixed = Keyframes::new(
            ms(100),
            [
                Keyframe::new(1.0, LookValue::Opacity(0.0)),
                Keyframe::new(0.5, LookValue::Scale(3.0)),
                Keyframe::new(f32::NAN, LookValue::Opacity(0.5)),
            ],
        );
        store.play(id, mixed, None).unwrap();
        store.tick_transitions(ms(50));
        assert_eq!(store.opacity(id), 0.5);
        assert_eq!(store.turn(id).scale, 1.0);
        assert_eq!(store.play(id, Keyframes::default(), None), None);
        let sized = Keyframes::new(ms(100), [Keyframe::new(1.0, LookValue::Width(4.0))]);
        assert_eq!(store.play(id, sized, None), None, "a size would lay out");
        // One that runs no cycle ends at once, finished.
        let none = Keyframes::new(ms(100), [Keyframe::new(1.0, LookValue::Scale(2.0))])
            .repeat(Repeat::Times(0));
        let animation = store.play(id, none, None).unwrap();
        assert!(!store.is_animating(animation));
    }

    #[test]
    fn reduced_motion_lands_what_reduces_and_keeps_what_does_not() {
        let ms = Duration::from_millis;
        let (mut store, id) = one_leaf();
        // A move under way lands as motion is reduced.
        store.transition(id, LookValue::Opacity(0.0), linear(100));
        store.tick_transitions(ms(50));
        store.set_reduced_motion(true);
        assert_eq!(store.opacity(id), 0.0);
        assert!(!store.is_transitioning());
        // A new one lands at once, a spring too; a kept one moves.
        store.transition(id, LookValue::Opacity(1.0), linear(100));
        assert_eq!(store.opacity(id), 1.0);
        let spring = Timing::spring(Spring::BOUNCY);
        store.transition(id, LookValue::Translate(Vec2 { x: 9.0, y: 0.0 }), spring);
        assert_eq!(store.translate(id).x, 9.0);
        store.transition(
            id,
            LookValue::Opacity(0.0),
            linear(100).reduced(ReducedMotion::Keep),
        );
        store.tick_transitions(ms(50));
        assert_eq!(store.opacity(id), 0.5);
        // An animation ends finished at once; a kept track runs on.
        let shake = store
            .animate(id, LookValue::Rotation(1.0), linear(100), None)
            .unwrap();
        assert!(!store.is_animating(shake));
        let spin = Keyframes::new(ms(100), [Keyframe::new(1.0, LookValue::Rotation(6.0))])
            .repeat(Repeat::Forever);
        let reduced = store.play(id, spin.clone(), None).unwrap();
        assert!(!store.is_animating(reduced));
        let kept = store
            .play(id, spin.reduced(ReducedMotion::Keep), None)
            .unwrap();
        store.tick_transitions(ms(50));
        assert!(store.is_animating(kept));
        assert_eq!(store.turn(id).rotation, 3.0);
    }
}
