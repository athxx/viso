//! Transform-only animation: a small retained registry that advances per-node
//! world-space translates over time.
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

use std::time::Duration;

use crate::component::NodeStore;
use crate::layout::Vec2;
use crate::node::NodeId;

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
}
