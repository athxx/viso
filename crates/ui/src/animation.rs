//! Animation: a small retained registry that advances per-node world-space
//! translates over time, and the look transitions a node store runs.
//!
//! An animation here is a [`TranslateAnim`]: a node sliding from one
//! world-space offset to another over a fixed duration, shaped by an
//! [`Easing`] curve. Each frame the driver calls [`AnimationRegistry::tick`]
//! with the elapsed delta; the registry advances every live animation by
//! writing the interpolated offset through [`NodeStore::set_translate`] — a
//! `TRANSFORM | HIT_TEST | PAINT` write that never re-measures or re-lays out
//! the node (the section 8.7 contract). This is the machinery a sheet drawer
//! rides in on.
//!
//! Steady-state cost: the registry is a single `Vec` (no per-node map — section
//! 45), `tick` is a flat pass with no heap allocation (section 28), and it
//! empties itself as animations finish so the frame loop can halt (the
//! zero-CPU-when-idle contract — a driver's `wants_animation` reads
//! [`is_empty`](AnimationRegistry::is_empty)).
//!
//! A look transition moves a node's fill or opacity to a new value over a
//! [`Timing`]: the store keeps the moving ones in a flat list beside its
//! columns and the driver ticks it each frame, a PAINT-only write
//! ([`NodeStore::transition`]).

use std::time::Duration;

use crate::component::NodeStore;
use crate::layout::Vec2;
use crate::node::NodeId;
use viso_render::Rgba;

/// A standard easing curve mapping normalized time `t ∈ [0, 1]` onto eased
/// progress, also in `[0, 1]`. These are the WAI/Material cubic curves: linear
/// for constant-rate motion, and the three cubic variants for natural
/// acceleration/deceleration. All satisfy `apply(0) == 0` and `apply(1) == 1`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Easing {
    /// Constant rate — no acceleration.
    Linear,
    /// Accelerate from rest (cubic): slow start, fast finish.
    EaseIn,
    /// Decelerate to rest (cubic): fast start, slow finish. The natural choice
    /// for content sliding *in* — it arrives and settles.
    #[default]
    EaseOut,
    /// Accelerate then decelerate (cubic): slow at both ends.
    EaseInOut,
}

impl Easing {
    /// Map normalized time `t` (clamped to `[0, 1]`) onto eased progress.
    #[inline]
    pub fn apply(&self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        match self {
            Easing::Linear => t,
            // t^3.
            Easing::EaseIn => t * t * t,
            // 1 - (1 - t)^3.
            Easing::EaseOut => {
                let inv = 1.0 - t;
                1.0 - inv * inv * inv
            }
            // Piecewise cubic: accelerate over the first half, decelerate over
            // the second — the standard "smooth" curve.
            Easing::EaseInOut => {
                if t < 0.5 {
                    4.0 * t * t * t
                } else {
                    let f = 2.0 * t - 2.0;
                    1.0 + f * f * f / 2.0
                }
            }
        }
    }
}

/// A completion callback, run once with the live store when an animation
/// finishes. Boxed because each callback captures a distinct environment (a
/// sheet's content node, its dismiss handler); it is invoked at most once per
/// animation, off the per-frame hot path.
type OnDone = Box<dyn FnOnce(&mut NodeStore)>;

/// A single node's translate animation: slide `node`'s world-space offset from
/// `from` to `to` over `duration`, shaped by `easing`.
///
/// `on_done` fires once, from within the [`tick`](AnimationRegistry::tick) that
/// completes the animation, with a mutable borrow of the [`NodeStore`] — so a
/// sheet's slide-out can hide its content and restore focus exactly when the
/// motion finishes, not before.
pub struct TranslateAnim {
    /// The node whose translate this animation drives.
    pub node: NodeId,
    /// World-space offset at `t == 0`.
    pub from: Vec2,
    /// World-space offset at `t == 1`.
    pub to: Vec2,
    /// Time elapsed so far; advanced by the per-frame delta.
    pub elapsed: Duration,
    /// Total run time. A zero duration completes on the first tick.
    pub duration: Duration,
    /// The curve shaping progress.
    pub easing: Easing,
    /// Fired once when the animation completes, with the live store.
    pub on_done: Option<OnDone>,
}

impl TranslateAnim {
    /// A slide from `from` to `to` over `duration` with `easing`, no completion
    /// callback. Add one with [`on_done`](Self::on_done).
    pub fn new(node: NodeId, from: Vec2, to: Vec2, duration: Duration, easing: Easing) -> Self {
        Self {
            node,
            from,
            to,
            elapsed: Duration::ZERO,
            duration,
            easing,
            on_done: None,
        }
    }

    /// Attach a completion callback, fired once when the slide finishes.
    pub fn on_done(mut self, f: impl FnOnce(&mut NodeStore) + 'static) -> Self {
        self.on_done = Some(Box::new(f));
        self
    }

    /// Eased progress `[0, 1]` at the current elapsed time. `1.0` once the run
    /// time is met or exceeded (or the duration is zero).
    #[inline]
    fn progress(&self) -> f32 {
        if self.duration.is_zero() {
            return 1.0;
        }
        let raw = self.elapsed.as_secs_f32() / self.duration.as_secs_f32();
        self.easing.apply(raw)
    }

    /// Whether the run time has been met (linear time, pre-easing).
    #[inline]
    fn is_finished(&self) -> bool {
        self.elapsed >= self.duration
    }

    /// The current interpolated world-space offset.
    #[inline]
    fn current(&self) -> Vec2 {
        self.from.lerp(self.to, self.progress())
    }
}

/// The set of live transform animations. A flat `Vec` (section 45: no per-node
/// map on a path touched every animating frame); animations swap-remove
/// themselves as they finish, so a settled UI leaves it empty and the frame
/// loop halts.
#[derive(Default)]
pub struct AnimationRegistry {
    anims: Vec<TranslateAnim>,
}

impl AnimationRegistry {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether any animation is live. A driver's `wants_animation` reads this:
    /// empty means the frame loop can stop requesting beats.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.anims.is_empty()
    }

    /// The number of live animations (for tests and counters).
    #[inline]
    pub fn len(&self) -> usize {
        self.anims.len()
    }

    /// Start `anim`. If the same node already has an animation, it is
    /// *replaced* — a slide reversed mid-flight takes over immediately from the
    /// current position rather than stacking a second animation on the node.
    /// The replaced animation's `on_done` does **not** fire (it never finished).
    pub fn start(&mut self, anim: TranslateAnim) {
        if let Some(slot) = self.anims.iter_mut().find(|a| a.node == anim.node) {
            *slot = anim;
        } else {
            self.anims.push(anim);
        }
    }

    /// Advance every live animation by `delta`, writing each node's interpolated
    /// offset through [`NodeStore::set_translate`]. An animation that reaches or
    /// passes its duration is written to its final offset, removed, and its
    /// `on_done` fired. An animation whose node is no longer live is dropped
    /// without firing `on_done` (the node it targeted is gone).
    ///
    /// A flat pass with no heap allocation: it walks the `Vec` in reverse so a
    /// `swap_remove` of a finished animation does not skip its neighbour.
    pub fn tick(&mut self, store: &mut NodeStore, delta: Duration) {
        let mut i = self.anims.len();
        while i > 0 {
            i -= 1;
            // A node destroyed while animating: drop the animation. The
            // set_translate write below would no-op on the stale handle anyway,
            // but this also stops us from spinning on a dead node forever.
            if !store.arena().is_live(self.anims[i].node) {
                self.anims.swap_remove(i);
                continue;
            }
            self.anims[i].elapsed = self.anims[i].elapsed.saturating_add(delta);
            let node = self.anims[i].node;
            let offset = self.anims[i].current();
            store.set_translate(node, offset);
            if self.anims[i].is_finished() {
                let done = self.anims.swap_remove(i);
                if let Some(cb) = done.on_done {
                    cb(store);
                }
            }
        }
    }
}

/// How a look property moves to a new value: it holds for `delay`, then moves
/// over `duration` shaped by `easing`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Timing {
    /// The time the move takes.
    pub duration: Duration,
    /// The time before the move begins.
    pub delay: Duration,
    /// The curve shaping the move.
    pub easing: Easing,
}

impl Timing {
    /// Whether a move with this timing ends the moment it starts.
    #[inline]
    pub fn is_instant(&self) -> bool {
        self.duration.is_zero() && self.delay.is_zero()
    }
}

/// A value of a node's look a transition moves.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LookValue {
    /// The color its box fills with.
    Fill(Rgba),
    /// The opacity it and its subtree paint at.
    Opacity(f32),
}

impl LookValue {
    /// Whether `self` and `other` are values of the same property.
    #[inline]
    fn same_property(&self, other: &LookValue) -> bool {
        std::mem::discriminant(self) == std::mem::discriminant(other)
    }

    /// The value `t` of the way from `self` to `to` (of the same property). A
    /// fill moves in premultiplied linear light, so a fade from transparent
    /// keeps its hue instead of passing through black.
    fn lerp(self, to: LookValue, t: f32) -> LookValue {
        match (self, to) {
            (LookValue::Opacity(a), LookValue::Opacity(b)) => LookValue::Opacity(a + (b - a) * t),
            (LookValue::Fill(a), LookValue::Fill(b)) => {
                let (mut mixed, b) = (a.premultiply(), b.premultiply());
                let mix = |x: f32, y: f32| x + (y - x) * t;
                mixed.r = mix(mixed.r, b.r);
                mixed.g = mix(mixed.g, b.g);
                mixed.b = mix(mixed.b, b.b);
                mixed.a = mix(mixed.a, b.a);
                LookValue::Fill(mixed.unpremultiply())
            }
            _ => to,
        }
    }
}

/// A node's look property moving from the value it showed toward its target.
/// A rebuild lifts one off the node it leaves
/// ([`NodeStore::lift_transitions`]) and resumes it on the node that replaces
/// it ([`NodeStore::resume_transition`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LookTransition {
    pub(crate) node: NodeId,
    from: LookValue,
    pub(crate) to: LookValue,
    elapsed: Duration,
    pub(crate) timing: Timing,
}

impl LookTransition {
    pub(crate) fn new(node: NodeId, from: LookValue, to: LookValue, timing: Timing) -> Self {
        LookTransition {
            node,
            from,
            to,
            elapsed: Duration::ZERO,
            timing,
        }
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

    /// The value it shows at its elapsed time, and whether it is done.
    pub(crate) fn at(&self) -> (LookValue, bool) {
        let Some(moving) = self.elapsed.checked_sub(self.timing.delay) else {
            return (self.from, false);
        };
        if moving >= self.timing.duration {
            return (self.to, true);
        }
        let t = moving.as_secs_f32() / self.timing.duration.as_secs_f32();
        (self.from.lerp(self.to, self.timing.easing.apply(t)), false)
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use super::*;
    use crate::component::{BuildCx, LeafStyle, NodeStore};
    use crate::layout::Size;
    use viso_render::Rect;

    /// A single 100x100 leaf laid out at the surface origin — the same shape the
    /// component-level translate tests ride on. Returns the store and the node.
    fn one_leaf() -> (NodeStore, NodeId) {
        let mut store = NodeStore::new();
        let root = {
            let mut cx = BuildCx::new(&mut store);
            cx.leaf(LeafStyle {
                size: Size::fixed(100.0, 100.0),
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
            Easing::EaseIn,
            Easing::EaseOut,
            Easing::EaseInOut,
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
    fn easing_clamps_out_of_range_time() {
        // Time outside [0, 1] pins to the endpoints rather than extrapolating.
        assert_eq!(Easing::Linear.apply(-1.0), 0.0);
        assert_eq!(Easing::Linear.apply(2.0), 1.0);
        assert_eq!(Easing::EaseOut.apply(5.0), 1.0);
    }

    #[test]
    fn tick_advances_the_world_toward_the_target() {
        let (mut store, node) = one_leaf();
        let base = store.bounds(node);

        let mut reg = AnimationRegistry::new();
        reg.start(TranslateAnim::new(
            node,
            Vec2::ZERO,
            Vec2 { x: 0.0, y: -100.0 },
            Duration::from_millis(100),
            Easing::Linear,
        ));
        assert!(!reg.is_empty(), "a started animation is live");

        // Half the duration under a linear curve → exactly half the translate.
        // world = bounds − translate, so a translate of −50 shifts world by +50.
        reg.tick(&mut store, Duration::from_millis(50));
        store.resolve_transforms(node);
        assert_eq!(
            store.world(node).y,
            base.y + 50.0,
            "half the linear duration applies half the translate"
        );
        assert!(
            !reg.is_empty(),
            "still mid-flight before the duration is met"
        );
    }

    #[test]
    fn reaching_the_duration_lands_on_the_target_and_empties() {
        let (mut store, node) = one_leaf();
        let base = store.bounds(node);

        let mut reg = AnimationRegistry::new();
        reg.start(TranslateAnim::new(
            node,
            Vec2::ZERO,
            Vec2 { x: 0.0, y: -100.0 },
            Duration::from_millis(100),
            Easing::Linear,
        ));
        // A single tick that meets the duration finishes the run.
        reg.tick(&mut store, Duration::from_millis(100));
        store.resolve_transforms(node);
        // to.y = −100 → world shifts by −translate = +100.
        assert_eq!(store.world(node).y, base.y + 100.0, "lands on the target");
        assert!(reg.is_empty(), "a finished animation removes itself");
    }

    #[test]
    fn overshooting_the_duration_still_lands_exactly_on_the_target() {
        let (mut store, node) = one_leaf();
        let base = store.bounds(node);

        let mut reg = AnimationRegistry::new();
        reg.start(TranslateAnim::new(
            node,
            Vec2::ZERO,
            Vec2 { x: 40.0, y: 0.0 },
            Duration::from_millis(100),
            Easing::EaseOut,
        ));
        // A delta far larger than the duration clamps progress to 1.0.
        reg.tick(&mut store, Duration::from_secs(10));
        store.resolve_transforms(node);
        assert_eq!(
            store.world(node).x,
            base.x - 40.0,
            "a huge delta clamps to the final offset, never past it"
        );
        assert!(reg.is_empty());
    }

    #[test]
    fn starting_the_same_node_replaces_without_stacking() {
        let (_store, node) = one_leaf();

        let fired = Rc::new(Cell::new(false));
        let flag = Rc::clone(&fired);
        let mut reg = AnimationRegistry::new();
        reg.start(
            TranslateAnim::new(
                node,
                Vec2::ZERO,
                Vec2 { x: 100.0, y: 0.0 },
                Duration::from_millis(100),
                Easing::Linear,
            )
            .on_done(move |_| flag.set(true)),
        );
        assert_eq!(reg.len(), 1);

        // A second start for the same node takes over — one animation, and the
        // replaced one's on_done never fires (it did not finish).
        reg.start(TranslateAnim::new(
            node,
            Vec2::ZERO,
            Vec2 { x: -100.0, y: 0.0 },
            Duration::from_millis(100),
            Easing::Linear,
        ));
        assert_eq!(reg.len(), 1, "same node replaces rather than stacks");
        assert!(
            !fired.get(),
            "the replaced animation's on_done does not fire"
        );
    }

    #[test]
    fn on_done_fires_once_on_completion() {
        let (mut store, node) = one_leaf();

        let count = Rc::new(Cell::new(0u32));
        let tally = Rc::clone(&count);
        let mut reg = AnimationRegistry::new();
        reg.start(
            TranslateAnim::new(
                node,
                Vec2::ZERO,
                Vec2 { x: 0.0, y: 20.0 },
                Duration::from_millis(100),
                Easing::Linear,
            )
            .on_done(move |_| tally.set(tally.get() + 1)),
        );

        reg.tick(&mut store, Duration::from_millis(50));
        assert_eq!(count.get(), 0, "not yet done at the halfway point");
        reg.tick(&mut store, Duration::from_millis(50));
        assert_eq!(count.get(), 1, "fires exactly once on completion");
    }

    #[test]
    fn a_dead_node_animation_is_dropped_without_firing_on_done() {
        let (mut store, node) = one_leaf();

        let fired = Rc::new(Cell::new(false));
        let flag = Rc::clone(&fired);
        let mut reg = AnimationRegistry::new();
        reg.start(
            TranslateAnim::new(
                node,
                Vec2::ZERO,
                Vec2 { x: 0.0, y: 50.0 },
                Duration::from_millis(100),
                Easing::Linear,
            )
            .on_done(move |_| flag.set(true)),
        );

        // Free the node, invalidating the handle. The next tick drops the anim.
        store.clear();
        reg.tick(&mut store, Duration::from_millis(16));
        assert!(reg.is_empty(), "an animation on a dead node is dropped");
        assert!(!fired.get(), "dropping a dead-node anim never runs on_done");
    }

    fn linear(ms: u64) -> Timing {
        Timing {
            duration: Duration::from_millis(ms),
            delay: Duration::ZERO,
            easing: Easing::Linear,
        }
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
        let timing = Timing {
            delay: Duration::from_millis(40),
            ..linear(100)
        };
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
}
