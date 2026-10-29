//! Pinches: the gesture a node wins by taking `pinch_*`. A pinch scales,
//! turns and moves what it holds at once.
//!
//! Two fingers that land at or under the nearest node that takes pinches arm
//! one; once they travel past the slop together, spreading, turning or moving
//! their middle, it starts. The node captures both, their presses are
//! released so the lifts click nothing, their holds let go, and a drag either
//! began ends there; every move of either finger is then a sample of the
//! pinch, until one of them lifts or is cancelled. A first finger that a
//! viewport already pans arms none.
//!
//! A trackpad pinches by its own gesture ([`route_trackpad_pinch`]), and a
//! wheel turned with Control held zooms ([`route_wheel_zoom`]), as a browser
//! reports a trackpad pinch; a wheel's pinch ends once it has rested
//! [`WHEEL_PINCH_REST`].
//!
//! The router records the sample under dispatch on the pinch, so a handler
//! reads it through [`NodeStore::pinch_sample`] beside the pointer sample it
//! rides on, if any.

use core::f32::consts::{PI, TAU};
use core::time::Duration;

use crate::binding::BindingTable;
use crate::component::NodeStore;
use crate::hit_test::HitTestTree;
use crate::input::{PointerId, pointer_dispatch_chain};
use crate::layout::Vec2;
use crate::node::NodeId;
use crate::state::StateStore;
use crate::timer::TimerRequest;

/// How long a wheel's pinch rests before it ends.
pub const WHEEL_PINCH_REST: Duration = Duration::from_millis(150);

/// How far (pt) a wheel turns to zoom by a factor of `e`.
const WHEEL_ZOOM: f32 = 100.0;

/// How many slops a second finger's first move spans to count as its having
/// landed somewhere else.
const LEAP_SLOPS: f32 = 4.0;

/// Which sample of a pinch is under dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinchPhase {
    /// The fingers passed the slop, or the trackpad's gesture began.
    Start,
    /// They moved on.
    Move,
    /// One lifted or was cancelled, or the gesture ended.
    End,
}

/// A pinch's sample under dispatch, in window coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PinchSample {
    pub phase: PinchPhase,
    /// The middle of the fingers; the pointer, on a trackpad.
    pub focal: Vec2,
    /// How far the pinch has scaled since the fingers landed: their spread
    /// over what it was.
    pub scale: f32,
    /// How far it has turned since they landed, in radians, clockwise.
    pub rotation: f32,
    /// The factor it scaled by since its last sample.
    pub scale_delta: f32,
    /// How far it turned since its last sample.
    pub rotation_delta: f32,
    /// How far the focal point moved since its last sample.
    pub pan: Vec2,
}

/// What drives a pinch.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Source {
    /// Two fingers: each one's pointer and where it is, the second `None`
    /// until it lands.
    Touch([Option<(PointerId, [f32; 2])>; 2]),
    /// A trackpad's gesture.
    Trackpad,
    /// A wheel turned with Control held, ending once it rests; `token` tells
    /// its latest rest from an earlier one.
    Wheel { token: u64, due: bool },
}

/// One pinch, armed or started.
#[derive(Debug, Clone, Copy)]
struct Pinch {
    node: NodeId,
    source: Source,
    started: bool,
    /// The second finger landed and has not moved since.
    landed: bool,
    /// The fingers' spread, angle and middle as the second landed.
    spread: f32,
    angle: f32,
    /// The pinch at its last sample.
    scale: f32,
    rotation: f32,
    focal: [f32; 2],
    sample: Option<PinchSample>,
}

impl Pinch {
    fn new(node: NodeId, source: Source, focal: [f32; 2]) -> Self {
        Pinch {
            node,
            source,
            started: false,
            landed: false,
            spread: 0.0,
            angle: 0.0,
            scale: 1.0,
            rotation: 0.0,
            focal,
            sample: None,
        }
    }

    /// Whether pointer `pointer` drives it.
    fn has(&self, pointer: PointerId) -> bool {
        match self.source {
            Source::Touch(fingers) => fingers.iter().flatten().any(|f| f.0 == pointer),
            _ => pointer == PointerId::MOUSE,
        }
    }

    /// The fingers of a touch pinch both down.
    fn fingers(&self) -> Option<[(PointerId, [f32; 2]); 2]> {
        match self.source {
            Source::Touch([Some(a), Some(b)]) => Some([a, b]),
            _ => None,
        }
    }

    /// Records the sample of `phase` that takes the pinch to `scale`,
    /// `rotation` and `focal`.
    fn record(&mut self, phase: PinchPhase, scale: f32, rotation: f32, focal: [f32; 2]) {
        self.sample = Some(PinchSample {
            phase,
            focal: Vec2 {
                x: focal[0],
                y: focal[1],
            },
            scale,
            rotation,
            scale_delta: scale / self.scale,
            rotation_delta: rotation - self.rotation,
            pan: Vec2 {
                x: focal[0] - self.focal[0],
                y: focal[1] - self.focal[1],
            },
        });
        self.scale = scale;
        self.rotation = rotation;
        self.focal = focal;
    }
}

/// Two fingers' spread, angle and middle.
fn span(a: [f32; 2], b: [f32; 2]) -> (f32, f32, [f32; 2]) {
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    (
        dx.hypot(dy).max(1.0),
        dy.atan2(dx),
        [(a[0] + b[0]) * 0.5, (a[1] + b[1]) * 0.5],
    )
}

/// `angle` brought within a half turn either way.
fn wrapped(angle: f32) -> f32 {
    (angle + PI).rem_euclid(TAU) - PI
}

/// The nodes that take pinches and the pinches under way. Cold: a store none
/// of whose nodes takes pinches arms none.
#[derive(Debug, Default)]
pub(crate) struct Pinches {
    takers: Vec<NodeId>,
    pinches: Vec<Pinch>,
    next: u64,
}

impl Pinches {
    pub(crate) fn clear(&mut self) {
        self.takers.clear();
        self.pinches.clear();
    }

    fn of(&self, pointer: PointerId) -> Option<usize> {
        self.pinches.iter().position(|p| p.has(pointer))
    }
}

/// What a finger's move did to its pinch.
pub(crate) enum PinchStep {
    /// The pinch started: the fingers' nodes lose them, and the drags they
    /// began end.
    Started([PointerId; 2]),
    /// It moved, or holds the fingers before it starts.
    Moved,
}

impl NodeStore {
    /// Makes node `id` take pinches, or no longer.
    pub fn set_pinchable(&mut self, id: NodeId, takes: bool) {
        let mut takers = std::mem::take(&mut self.pinches.takers);
        takers.retain(|&n| n != id && self.arena().is_live(n));
        if takes && self.arena().is_live(id) {
            takers.push(id);
        }
        self.pinches.takers = takers;
    }

    /// Whether node `id` takes pinches.
    pub fn pinchable(&self, id: NodeId) -> bool {
        self.pinches.takers.contains(&id)
    }

    /// Whether node `id` won a pinch that runs.
    pub fn pinching(&self, id: NodeId) -> bool {
        self.pinches
            .pinches
            .iter()
            .any(|p| p.started && p.node == id)
    }

    /// The pinch sample pointer `pointer`'s dispatch carries: a finger's, or
    /// [`PointerId::MOUSE`]'s for a trackpad or a wheel.
    pub fn pinch_sample(&self, pointer: PointerId) -> Option<PinchSample> {
        self.pinches.pinches[self.pinches.of(pointer)?].sample
    }

    /// The node finger `pointer`'s pinch runs on.
    pub(crate) fn pinch_node(&self, pointer: PointerId) -> Option<NodeId> {
        Some(self.pinches.pinches[self.pinches.of(pointer)?].node)
    }

    /// The nearest node at or above `target` that takes pinches.
    fn pinch_taker(&self, target: NodeId) -> Option<NodeId> {
        let mut node = Some(target);
        while let Some(n) = node {
            if self.pinchable(n) {
                return Some(n);
            }
            node = self.parent(n);
        }
        None
    }

    /// Finger `pointer` landed on `target` at `at`: the first arms a pinch
    /// for the nearest node at or above it that takes pinches, the second
    /// joins it.
    pub(crate) fn arm_pinch(&mut self, pointer: PointerId, target: NodeId, at: [f32; 2]) {
        if let Some(i) = self.pinches.of(pointer) {
            self.pinches.pinches.swap_remove(i);
        }
        if self.pinches.takers.is_empty() {
            return;
        }
        let Some(node) = self.pinch_taker(target) else {
            return;
        };
        let pinches = &mut self.pinches.pinches;
        match pinches.iter_mut().find(|p| p.node == node) {
            Some(pinch) => {
                if let Source::Touch([Some(first), second @ None]) = &mut pinch.source
                    && !pinch.started
                {
                    *second = Some((pointer, at));
                    pinch.landed = true;
                    let (spread, angle, focal) = span(first.1, at);
                    pinch.spread = spread;
                    pinch.angle = angle;
                    pinch.focal = focal;
                }
            }
            None => pinches.push(Pinch::new(
                node,
                Source::Touch([Some((pointer, at)), None]),
                at,
            )),
        }
    }

    /// Whether finger `pointer` is one of a pinch's two: a pan may not take
    /// it.
    pub(crate) fn pinch_leads(&self, pointer: PointerId) -> bool {
        self.pinches
            .of(pointer)
            .is_some_and(|i| self.pinches.pinches[i].fingers().is_some())
    }

    /// Finger `pointer` moved to `at`: starts its pinch once the fingers
    /// travel past `slop`, and records the sample a started one sees.
    pub(crate) fn step_pinch(
        &mut self,
        pointer: PointerId,
        at: [f32; 2],
        slop: f32,
    ) -> Option<PinchStep> {
        let i = self.pinches.of(pointer)?;
        let live = self.arena().is_live(self.pinches.pinches[i].node);
        let pinch = &mut self.pinches.pinches[i];
        let mut leapt = 0.0f32;
        if let Source::Touch(fingers) = &mut pinch.source {
            for finger in fingers.iter_mut().flatten() {
                if finger.0 == pointer {
                    leapt = (at[0] - finger.1[0]).hypot(at[1] - finger.1[1]);
                    finger.1 = at;
                }
            }
        }
        let [a, b] = pinch.fingers()?;
        let (spread, angle, focal) = span(a.1, b.1);
        // A second finger whose first move leaps landed where it was not:
        // some systems (an emulator's) report the landing at a stale point.
        // The pinch is measured from where it really is.
        let first_move = pinch.landed && b.0 == pointer;
        if first_move {
            pinch.landed = false;
        }
        if first_move && !pinch.started && leapt > LEAP_SLOPS * slop {
            pinch.spread = spread;
            pinch.angle = angle;
            pinch.focal = focal;
            return Some(PinchStep::Moved);
        }
        let scale = spread / pinch.spread;
        let rotation = pinch.rotation + wrapped(angle - pinch.angle - pinch.rotation);
        if pinch.started {
            pinch.record(PinchPhase::Move, scale, rotation, focal);
            return Some(PinchStep::Moved);
        }
        let moved = (focal[0] - pinch.focal[0]).hypot(focal[1] - pinch.focal[1]);
        let spread_by = (spread - pinch.spread).abs();
        let turned_by = rotation.abs() * pinch.spread * 0.5;
        if moved.max(spread_by).max(turned_by) < slop {
            return Some(PinchStep::Moved);
        }
        if !live {
            self.pinches.pinches.swap_remove(i);
            return None;
        }
        pinch.started = true;
        pinch.record(PinchPhase::Start, scale, rotation, focal);
        Some(PinchStep::Started([a.0, b.0]))
    }

    /// Finger `pointer` lifted at `at`, or was cancelled there: ends its
    /// started pinch, and leaves one that never started.
    pub(crate) fn end_pinch(&mut self, pointer: PointerId, at: [f32; 2]) {
        let Some(i) = self.pinches.of(pointer) else {
            return;
        };
        let pinch = &mut self.pinches.pinches[i];
        if !pinch.started {
            match &mut pinch.source {
                Source::Touch([first, second]) => {
                    if first.is_some_and(|f| f.0 == pointer) {
                        *first = second.take();
                    } else {
                        *second = None;
                    }
                    if first.is_none() {
                        self.pinches.pinches.swap_remove(i);
                    }
                }
                _ => {
                    self.pinches.pinches.swap_remove(i);
                }
            }
            return;
        }
        let focal = match pinch.fingers() {
            Some([a, b]) => {
                let other = if a.0 == pointer { b.1 } else { a.1 };
                span(other, at).2
            }
            None => pinch.focal,
        };
        let (scale, rotation) = (pinch.scale, pinch.rotation);
        pinch.record(PinchPhase::End, scale, rotation, focal);
    }

    /// After pointer `pointer`'s sample was dispatched: a viewer's pinch
    /// zooms it by the sample, the pinch sample it carried clears, and a
    /// pinch it ended is forgotten.
    pub(crate) fn settle_pinch(&mut self, pointer: PointerId) {
        let Some(i) = self.pinches.of(pointer) else {
            return;
        };
        let pinch = &mut self.pinches.pinches[i];
        let (node, touch) = (pinch.node, !matches!(pinch.source, Source::Wheel { .. }));
        let Some(sample) = pinch.sample.take() else {
            return;
        };
        if sample.phase == PinchPhase::End {
            self.pinches.pinches.swap_remove(i);
        }
        self.steer_viewer_pinch(node, sample, touch);
    }

    /// Whether a wheel's pinch has rested long enough to end.
    pub fn pinch_end_due(&self) -> bool {
        self.pinches
            .pinches
            .iter()
            .any(|p| matches!(p.source, Source::Wheel { due: true, .. }))
    }
}

/// A trackpad's pinch, in window coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TrackpadPinch {
    /// Where the pointer is.
    pub x: f32,
    pub y: f32,
    /// The factor it scaled by since its last sample.
    pub scale: f32,
    /// How far it turned since its last sample, in radians, clockwise.
    pub rotation: f32,
    /// How far the fingers' middle moved since its last sample.
    pub dx: f32,
    pub dy: f32,
    pub phase: PinchPhase,
}

/// Runs a trackpad's pinch on the nearest node at or above the one under the
/// pointer that takes pinches, along its chain. Returns whether any handler
/// ran; a pinch over no such node reaches none.
pub fn route_trackpad_pinch(
    store: &mut NodeStore,
    states: &mut StateStore,
    bindings: &BindingTable,
    root: NodeId,
    ev: TrackpadPinch,
    chain: &mut Vec<NodeId>,
) -> bool {
    let pointer = PointerId::MOUSE;
    let mut ran = false;
    let at = [ev.x, ev.y];
    let running = store
        .pinches
        .of(pointer)
        .filter(|&i| store.pinches.pinches[i].source == Source::Trackpad);
    let i = match (running, ev.phase) {
        (Some(i), PinchPhase::Move | PinchPhase::End) => i,
        (running, _) => {
            // A pinch begun anew ends the one before.
            if running.is_some() {
                ran |= end_pointer_pinch(store, states, bindings, chain);
            }
            if ev.phase == PinchPhase::End {
                return ran;
            }
            match open(store, root, at, Source::Trackpad) {
                Some(i) => i,
                None => return ran,
            }
        }
    };
    let pinch = &mut store.pinches.pinches[i];
    let (phase, at) = if pinch.started {
        // The middle starts at the pointer and moves with the fingers.
        (ev.phase, [pinch.focal[0] + ev.dx, pinch.focal[1] + ev.dy])
    } else {
        pinch.started = true;
        (PinchPhase::Start, at)
    };
    let (scale, rotation) = (pinch.scale * ev.scale, pinch.rotation + ev.rotation);
    pinch.record(phase, scale, rotation, at);
    ran | dispatch(store, states, bindings, i, chain)
}

/// Runs a wheel turned by `delta_y` (pt, content motion) with Control held
/// at `(x, y)` as a pinch zooming out as the content moves down, on the
/// nearest node at or above the one under the pointer that takes pinches.
/// Returns `None` when no such node is there, and the wheel scrolls as
/// usual; whether any handler ran otherwise.
pub fn route_wheel_zoom(
    store: &mut NodeStore,
    states: &mut StateStore,
    bindings: &BindingTable,
    root: NodeId,
    (x, y, delta_y): (f32, f32, f32),
    chain: &mut Vec<NodeId>,
) -> Option<bool> {
    let pointer = PointerId::MOUSE;
    if store.pinches.takers.is_empty() {
        return None;
    }
    let at = [x, y];
    let mut ran = false;
    let running = store
        .pinches
        .of(pointer)
        .filter(|&i| matches!(store.pinches.pinches[i].source, Source::Wheel { .. }));
    let i = match running {
        Some(i) => i,
        None => {
            if store.pinches.of(pointer).is_some() {
                ran |= end_pointer_pinch(store, states, bindings, chain);
            }
            open(
                store,
                root,
                at,
                Source::Wheel {
                    token: 0,
                    due: false,
                },
            )?
        }
    };
    store.pinches.next += 1;
    let token = store.pinches.next;
    let pinch = &mut store.pinches.pinches[i];
    pinch.source = Source::Wheel { token, due: false };
    let phase = if pinch.started {
        PinchPhase::Move
    } else {
        pinch.started = true;
        PinchPhase::Start
    };
    let scale = pinch.scale * (-delta_y / WHEEL_ZOOM).exp();
    let rotation = pinch.rotation;
    pinch.record(phase, scale, rotation, at);
    let node = pinch.node;
    store.queue_timer(TimerRequest::new(node, WHEEL_PINCH_REST, move |store| {
        for pinch in &mut store.pinches.pinches {
            if let Source::Wheel { token: t, due } = &mut pinch.source
                && *t == token
            {
                *due = true;
            }
        }
    }));
    Some(ran | dispatch(store, states, bindings, i, chain))
}

/// After the frame's timers fired: ends each wheel's pinch that rested long
/// enough on its node's chain. Returns whether any handler ran.
pub fn deliver_pinch_ends(
    store: &mut NodeStore,
    states: &mut StateStore,
    bindings: &BindingTable,
) -> bool {
    if !store.pinch_end_due() {
        return false;
    }
    end_pointer_pinch(store, states, bindings, &mut Vec::new())
}

/// Opens a started-to-be pinch from `source` on the node under `at` that
/// takes pinches; its index.
fn open(store: &mut NodeStore, root: NodeId, at: [f32; 2], source: Source) -> Option<usize> {
    let hit = HitTestTree::hit(store, root, at[0], at[1])?;
    if store.inert(hit) {
        return None;
    }
    let node = store.pinch_taker(hit)?;
    store.pinches.pinches.push(Pinch::new(node, source, at));
    Some(store.pinches.pinches.len() - 1)
}

/// Ends the trackpad's or the wheel's pinch where it is.
fn end_pointer_pinch(
    store: &mut NodeStore,
    states: &mut StateStore,
    bindings: &BindingTable,
    chain: &mut Vec<NodeId>,
) -> bool {
    let Some(i) = store.pinches.of(PointerId::MOUSE) else {
        return false;
    };
    let pinch = &mut store.pinches.pinches[i];
    let (scale, rotation, focal) = (pinch.scale, pinch.rotation, pinch.focal);
    pinch.record(PinchPhase::End, scale, rotation, focal);
    dispatch(store, states, bindings, i, chain)
}

/// Runs pinch `i`'s recorded sample along its node's chain, then settles it.
fn dispatch(
    store: &mut NodeStore,
    states: &mut StateStore,
    bindings: &BindingTable,
    i: usize,
    chain: &mut Vec<NodeId>,
) -> bool {
    let node = store.pinches.pinches[i].node;
    let ran = store.arena().is_live(node)
        && pointer_dispatch_chain(store, states, bindings, node, PointerId::MOUSE, chain);
    store.settle_pinch(PointerId::MOUSE);
    ran
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::component::{BuildCx, LeafStyle};
    use crate::layout::Sizing;
    use crate::style::BoxStyle;
    use viso_render::Rect;

    fn store() -> (NodeStore, NodeId) {
        let mut store = NodeStore::new();
        let node = {
            let mut cx = BuildCx::new(&mut store);
            cx.leaf(LeafStyle {
                size: Sizing::fixed(200.0, 200.0),
                style: BoxStyle::NONE,
            })
            .id()
        };
        store.layout(node, Rect::new(0.0, 0.0, 200.0, 200.0), &mut Vec::new());
        store.set_pinchable(node, true);
        (store, node)
    }

    #[test]
    fn two_fingers_that_spread_and_turn_start_a_pinch_that_scales_and_turns() {
        let (mut store, node) = store();
        let (a, b) = (PointerId(1), PointerId(2));
        store.arm_pinch(a, node, [50.0, 100.0]);
        assert!(!store.pinch_leads(a), "one finger is no pinch");
        store.arm_pinch(b, node, [150.0, 100.0]);
        assert!(store.pinch_leads(a) && store.pinch_leads(b));
        assert!(matches!(
            store.step_pinch(b, [152.0, 100.0], 8.0),
            Some(PinchStep::Moved)
        ));
        assert!(!store.pinching(node), "inside the slop");
        assert!(matches!(
            store.step_pinch(b, [250.0, 100.0], 8.0),
            Some(PinchStep::Started([p, q])) if p == a && q == b
        ));
        let start = store.pinch_sample(b).unwrap();
        assert_eq!(start.phase, PinchPhase::Start);
        assert_eq!(start.scale, 2.0);
        assert_eq!(start.focal, Vec2 { x: 150.0, y: 100.0 });
        store.settle_pinch(b);

        // A quarter turn clockwise about the first finger.
        store.step_pinch(b, [50.0, 300.0], 8.0);
        let turn = store.pinch_sample(b).unwrap();
        assert!((turn.rotation - PI / 2.0).abs() < 1e-5, "{turn:?}");
        assert_eq!(turn.scale, 2.0);
        assert_eq!(turn.scale_delta, 1.0);
        assert_eq!(
            turn.pan,
            Vec2 {
                x: -100.0,
                y: 100.0
            }
        );
        store.settle_pinch(b);

        store.end_pinch(a, [50.0, 100.0]);
        assert_eq!(store.pinch_sample(a).unwrap().phase, PinchPhase::End);
        store.settle_pinch(a);
        assert!(!store.pinching(node));
        assert!(store.pinch_sample(b).is_none(), "the pinch is gone");
    }

    #[test]
    fn a_second_finger_that_landed_at_a_stale_point_is_measured_from_where_it_moves() {
        let (mut store, node) = store();
        let (a, b) = (PointerId(1), PointerId(2));
        store.arm_pinch(a, node, [90.0, 100.0]);
        store.arm_pinch(b, node, [400.0, 900.0]);
        assert!(matches!(
            store.step_pinch(b, [110.0, 100.0], 8.0),
            Some(PinchStep::Moved)
        ));
        assert!(!store.pinching(node), "the leap is no pinch");
        store.step_pinch(b, [150.0, 100.0], 8.0);
        let start = store.pinch_sample(b).unwrap();
        assert_eq!(start.phase, PinchPhase::Start);
        assert_eq!(start.scale, 3.0, "spreading from 20 to 60 apart");
    }

    #[test]
    fn a_finger_that_lifts_before_the_pinch_starts_leaves_the_other_armed() {
        let (mut store, node) = store();
        let (a, b, c) = (PointerId(1), PointerId(2), PointerId(3));
        store.arm_pinch(a, node, [50.0, 100.0]);
        store.arm_pinch(b, node, [150.0, 100.0]);
        store.end_pinch(a, [50.0, 100.0]);
        assert!(!store.pinch_leads(b));
        store.arm_pinch(c, node, [100.0, 50.0]);
        assert!(store.pinch_leads(c), "a third finger pairs with the second");
    }

    #[test]
    fn a_wheel_with_control_zooms_until_it_rests() {
        let (mut store, node) = store();
        let mut states = StateStore::new();
        let bindings = BindingTable::new();
        let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let log = seen.clone();
        store.set_pointer_handler(
            node,
            Box::new(move |cx: &mut crate::context::EventCx<'_>| {
                if let Some(pinch) = cx.pinch() {
                    log.borrow_mut().push((pinch.phase, pinch.scale));
                }
            }),
        );
        let mut chain = Vec::new();
        let zoom = |store: &mut NodeStore, states: &mut StateStore, dy: f32, chain: &mut _| {
            route_wheel_zoom(store, states, &bindings, node, (100.0, 100.0, dy), chain)
        };
        assert_eq!(
            zoom(&mut store, &mut states, -100.0, &mut chain),
            Some(true)
        );
        assert_eq!(zoom(&mut store, &mut states, 100.0, &mut chain), Some(true));
        let mut timers = Vec::new();
        store.take_timer_requests(&mut timers);
        assert_eq!(timers.len(), 2);
        for timer in timers {
            (timer.on_fire)(&mut store);
        }
        assert!(deliver_pinch_ends(&mut store, &mut states, &bindings));
        let seen = seen.borrow();
        assert_eq!(seen[0], (PinchPhase::Start, core::f32::consts::E));
        assert_eq!(seen[1].0, PinchPhase::Move);
        assert!((seen[1].1 - 1.0).abs() < 1e-5);
        assert_eq!(seen[2].0, PinchPhase::End);
        assert!(!store.pinching(node));

        store.set_pinchable(node, false);
        assert_eq!(zoom(&mut store, &mut states, -100.0, &mut chain), None);
    }
}
