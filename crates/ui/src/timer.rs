//! One-shot UI timers: a small retained registry that fires a callback once a
//! wall-clock deadline is crossed.
//!
//! A timer here is a [`Timer`]: a node-scoped deadline that, when reached, runs
//! its `on_fire` callback once with the live store. This is the machinery a
//! toast rides in on — an overlay that shows, then auto-dismisses after a fixed
//! delay.
//!
//! Unlike [`AnimationRegistry`](crate::animation::AnimationRegistry), a timer
//! does **not** cost a frame while it waits. The registry surfaces the
//! [`earliest`](TimerRegistry::earliest) deadline; the scheduler turns that into
//! a `ControlFlow::WaitUntil(deadline)` so the OS blocks the loop until the
//! deadline instead of spinning through beats (the zero-CPU-when-idle contract,
//! section 7.1). The driver calls [`fire_due`](TimerRegistry::fire_due) once at
//! each frame head with the current instant; every timer whose deadline has
//! passed fires and is removed.
//!
//! Steady-state cost: the registry is a single `Vec` (no per-node map — section
//! 45), `earliest` and `fire_due` are flat passes with no heap allocation
//! (section 28), and it empties itself as timers fire so the frame loop can halt
//! (a driver's `next_timer_deadline` reads
//! [`earliest`](TimerRegistry::earliest); an empty registry returns `None`).

use std::time::{Duration, Instant};

use crate::component::NodeStore;
use crate::node::NodeId;

/// A fire callback, run once with the live store when a timer's deadline is
/// crossed. Boxed because each callback captures a distinct environment (a
/// toast's content node, its dismiss handler); it is invoked at most once per
/// timer, off the per-frame hot path.
///
/// Like an animation's `on_done`, the callback receives only `&mut NodeStore`:
/// it may hide a node or restore state, but it cannot itself arm another timer
/// or animation (that would need a scheduling context). A toast that closes
/// with a slide-out queues that motion through the deferred request seam, not
/// from inside its own fire.
type OnFire = Box<dyn FnOnce(&mut NodeStore)>;

/// A deferred request to arm a timer, produced by a handler through
/// [`EventCx::request_timer`](crate::context::EventCx::request_timer) and carried
/// — like a [`TranslateAnim`](crate::animation::TranslateAnim) request — through
/// the store's handoff queue to the driver, which arms it on its live
/// [`TimerRegistry`] the next frame (against that frame's `now`).
///
/// It holds the `delay` rather than a resolved `deadline`: the handler that
/// records it has no notion of the frame clock's "now", so the deadline is
/// computed where the timer is armed, from the driver's frame instant. This
/// keeps a request produced under a headless
/// [`ManualClock`](../../viso_runtime/clock/struct.ManualClock.html) deterministic
/// — its deadline is `arm-frame now + delay`, not a stray `Instant::now()`.
pub struct TimerRequest {
    /// The node the timer is scoped to (dropped unfired if the node dies first).
    pub node: NodeId,
    /// How long after the arming frame's `now` the timer fires.
    pub delay: Duration,
    /// Run once when the deadline is crossed, with the live store.
    pub on_fire: OnFire,
}

impl TimerRequest {
    /// A one-shot timer request on `node`, firing `on_fire` once `delay` after
    /// the frame that arms it.
    pub fn new(
        node: NodeId,
        delay: Duration,
        on_fire: impl FnOnce(&mut NodeStore) + 'static,
    ) -> Self {
        Self {
            node,
            delay,
            on_fire: Box::new(on_fire),
        }
    }
}

/// A transient, monotonic timer identity. Unlike [`NodeId`], a timer is
/// short-lived and never reused, so a plain incrementing counter suffices for
/// cancellation — there is no ABA hazard to guard against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TimerId(u64);

impl TimerId {
    /// The raw counter value (for tests and counters).
    #[inline]
    pub fn raw(self) -> u64 {
        self.0
    }
}

/// A single one-shot timer: fire `on_fire` once the wall clock reaches
/// `deadline`, scoped to `node`.
///
/// `on_fire` fires from within the [`fire_due`](TimerRegistry::fire_due) at the
/// head of the frame that first observes the deadline passed, with a mutable
/// borrow of the [`NodeStore`] — so a toast's auto-dismiss hides its content
/// exactly when the delay elapses. A timer whose node is no longer live is
/// dropped without firing (the node it targeted is gone).
struct Timer {
    /// This timer's identity, for cancellation.
    id: TimerId,
    /// The node this timer is scoped to. If the node dies, the timer is dropped
    /// unfired.
    node: NodeId,
    /// The wall-clock instant at which this timer fires.
    deadline: Instant,
    /// Run once when the deadline is crossed, with the live store.
    on_fire: Option<OnFire>,
}

/// The set of live one-shot timers. A flat `Vec` (section 45: no per-node map on
/// a path consulted every frame to compute the next deadline); timers
/// swap-remove themselves as they fire, so a settled UI leaves it empty and the
/// frame loop blocks with no pending deadline.
#[derive(Default)]
pub struct TimerRegistry {
    timers: Vec<Timer>,
    next_id: u64,
}

impl TimerRegistry {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether any timer is live. A driver's `next_timer_deadline` reads
    /// [`earliest`](Self::earliest); this is the cheap emptiness back for
    /// counters and tests.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.timers.is_empty()
    }

    /// The number of live timers (for tests and counters).
    #[inline]
    pub fn len(&self) -> usize {
        self.timers.len()
    }

    /// Arm a one-shot timer on `node` that fires `on_fire` once, `delay` after
    /// `now`. Returns a [`TimerId`] for [`cancel`](Self::cancel). A zero delay
    /// fires at the next [`fire_due`](Self::fire_due).
    ///
    /// Unlike [`AnimationRegistry::start`](crate::animation::AnimationRegistry::start),
    /// arming does not replace an existing timer on the same node: a node may
    /// hold several independent timers, and re-showing a toast resets its timer
    /// by cancelling the old id explicitly, not implicitly here.
    pub fn arm(
        &mut self,
        node: NodeId,
        delay: Duration,
        now: Instant,
        on_fire: impl FnOnce(&mut NodeStore) + 'static,
    ) -> TimerId {
        let id = TimerId(self.next_id);
        self.next_id = self.next_id.wrapping_add(1);
        // `Instant + Duration` panics only on literal overflow, which a UI delay
        // never reaches; `checked_add` keeps a pathological delay from aborting
        // the frame — it clamps to a far-future deadline (`now` + an hour) that
        // simply never fires within the session.
        let deadline = now
            .checked_add(delay)
            .unwrap_or_else(|| now + Duration::from_secs(3600));
        self.timers.push(Timer {
            id,
            node,
            deadline,
            on_fire: Some(Box::new(on_fire)),
        });
        id
    }

    /// Arm a request the driver drained from the store's handoff queue, resolving
    /// its `delay` against this frame's `now`. The counterpart to
    /// [`arm`](Self::arm) that takes the pre-boxed callback out of a
    /// [`TimerRequest`] rather than a fresh closure.
    pub fn arm_request(&mut self, req: TimerRequest, now: Instant) -> TimerId {
        let id = TimerId(self.next_id);
        self.next_id = self.next_id.wrapping_add(1);
        let deadline = now
            .checked_add(req.delay)
            .unwrap_or_else(|| now + Duration::from_secs(3600));
        self.timers.push(Timer {
            id,
            node: req.node,
            deadline,
            on_fire: Some(req.on_fire),
        });
        id
    }

    /// Cancel the timer with `id`, if it is still live, without firing it.
    /// A no-op if the timer already fired or was never armed.
    pub fn cancel(&mut self, id: TimerId) {
        if let Some(i) = self.timers.iter().position(|t| t.id == id) {
            self.timers.swap_remove(i);
        }
    }

    /// The earliest deadline among live timers, or `None` if there are none.
    /// The scheduler turns this into a `ControlFlow::WaitUntil(deadline)`, so the
    /// loop blocks until the nearest timer is due rather than polling.
    ///
    /// A flat pass with no allocation; the registry is small (a handful of live
    /// overlays at most), so a linear min beats the bookkeeping of a heap.
    pub fn earliest(&self) -> Option<Instant> {
        self.timers.iter().map(|t| t.deadline).min()
    }

    /// Fire every timer whose deadline is at or before `now`, in the order the
    /// `Vec` is walked, removing each as it fires. A timer whose node is no
    /// longer live is dropped without firing (the node it targeted is gone).
    ///
    /// A flat pass with no heap allocation: it walks the `Vec` in reverse so a
    /// `swap_remove` of a fired timer does not skip its neighbour. Callbacks run
    /// with the live store; because a callback cannot arm a new timer, the set
    /// cannot grow mid-pass.
    pub fn fire_due(&mut self, store: &mut NodeStore, now: Instant) {
        let mut i = self.timers.len();
        while i > 0 {
            i -= 1;
            // A node destroyed while its timer waited: drop the timer unfired.
            // The callback would target a stale handle, and dropping it stops us
            // from holding a deadline for a node that no longer exists.
            if !store.arena().is_live(self.timers[i].node) {
                self.timers.swap_remove(i);
                continue;
            }
            if self.timers[i].deadline <= now {
                let fired = self.timers.swap_remove(i);
                if let Some(cb) = fired.on_fire {
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
    use crate::node::NodeId;
    use viso_render::Rect;

    /// A single 100x100 leaf laid out at the surface origin — a live node for a
    /// timer to scope to. Returns the store and the node.
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
    fn arm_reports_the_earliest_deadline() {
        let (_store, node) = one_leaf();
        let now = Instant::now();
        let mut reg = TimerRegistry::new();
        assert!(reg.is_empty());
        assert_eq!(reg.earliest(), None, "no timers, no deadline");

        reg.arm(node, Duration::from_millis(200), now, |_| {});
        let early = reg.arm(node, Duration::from_millis(50), now, |_| {});
        reg.arm(node, Duration::from_millis(120), now, |_| {});

        assert_eq!(reg.len(), 3);
        assert_eq!(
            reg.earliest(),
            Some(now + Duration::from_millis(50)),
            "earliest is the nearest of the three deadlines"
        );
        let _ = early;
    }

    #[test]
    fn fire_due_fires_only_reached_deadlines() {
        let (mut store, node) = one_leaf();
        let now = Instant::now();
        let count = Rc::new(Cell::new(0u32));

        let mut reg = TimerRegistry::new();
        for delay in [40u64, 80, 200] {
            let tally = Rc::clone(&count);
            reg.arm(node, Duration::from_millis(delay), now, move |_| {
                tally.set(tally.get() + 1)
            });
        }

        // Cross only the first two deadlines.
        reg.fire_due(&mut store, now + Duration::from_millis(100));
        assert_eq!(count.get(), 2, "the 40ms and 80ms timers fired");
        assert_eq!(reg.len(), 1, "the 200ms timer is still armed");
        assert_eq!(
            reg.earliest(),
            Some(now + Duration::from_millis(200)),
            "earliest updates after the near timers fire"
        );

        // Cross the last one.
        reg.fire_due(&mut store, now + Duration::from_millis(250));
        assert_eq!(count.get(), 3, "the 200ms timer fired");
        assert!(reg.is_empty(), "all timers fired and removed");
        assert_eq!(reg.earliest(), None);
    }

    #[test]
    fn a_zero_delay_timer_fires_at_the_next_pass() {
        let (mut store, node) = one_leaf();
        let now = Instant::now();
        let fired = Rc::new(Cell::new(false));
        let flag = Rc::clone(&fired);

        let mut reg = TimerRegistry::new();
        reg.arm(node, Duration::ZERO, now, move |_| flag.set(true));
        // The same instant it was armed at counts as due (deadline <= now).
        reg.fire_due(&mut store, now);
        assert!(fired.get(), "a zero-delay timer fires immediately");
        assert!(reg.is_empty());
    }

    #[test]
    fn cancel_removes_a_timer_without_firing() {
        let (mut store, node) = one_leaf();
        let now = Instant::now();
        let fired = Rc::new(Cell::new(false));
        let flag = Rc::clone(&fired);

        let mut reg = TimerRegistry::new();
        let id = reg.arm(node, Duration::from_millis(50), now, move |_| {
            flag.set(true)
        });
        reg.cancel(id);
        assert!(reg.is_empty(), "cancel removes the timer");

        // Even well past the deadline, a cancelled timer never fires.
        reg.fire_due(&mut store, now + Duration::from_secs(10));
        assert!(!fired.get(), "a cancelled timer does not fire");
    }

    #[test]
    fn cancelling_an_unknown_id_is_a_noop() {
        let (_store, node) = one_leaf();
        let now = Instant::now();
        let mut reg = TimerRegistry::new();
        let id = reg.arm(node, Duration::from_millis(50), now, |_| {});
        reg.cancel(id);
        // Cancelling the same (now-gone) id again does nothing and does not panic.
        reg.cancel(id);
        assert!(reg.is_empty());
    }

    #[test]
    fn a_dead_node_timer_is_dropped_without_firing() {
        let (mut store, node) = one_leaf();
        let now = Instant::now();
        let fired = Rc::new(Cell::new(false));
        let flag = Rc::clone(&fired);

        let mut reg = TimerRegistry::new();
        reg.arm(node, Duration::from_millis(50), now, move |_| {
            flag.set(true)
        });

        // Free the node, invalidating the handle. fire_due drops the timer even
        // though its deadline has passed.
        store.clear();
        reg.fire_due(&mut store, now + Duration::from_secs(10));
        assert!(reg.is_empty(), "a timer on a dead node is dropped");
        assert!(!fired.get(), "dropping a dead-node timer never fires it");
    }

    #[test]
    fn ids_are_unique_across_arms() {
        let (_store, node) = one_leaf();
        let now = Instant::now();
        let mut reg = TimerRegistry::new();
        let a = reg.arm(node, Duration::from_millis(10), now, |_| {});
        let b = reg.arm(node, Duration::from_millis(10), now, |_| {});
        assert_ne!(a, b, "each arm yields a distinct id");
        assert_ne!(a.raw(), b.raw());
    }
}
