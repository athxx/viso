//! Drags: the gesture a node wins by taking `drag_*`.
//!
//! A primary press arms a drag for the nearest node at or above the pressed
//! one that takes drags. Once the pointer travels past the slop the drag
//! starts: the node captures the pointer, the press is released so the lift
//! clicks nothing, and every sample after is one of the drag's until the lift
//! or a cancel ends it. A touch that lands in a scroll viewport below that
//! node is the viewport's first: the drag starts only if the viewport has no
//! room to pan, and a pan forgets it. The node is `dragging` while its drag
//! runs.
//!
//! The router records the sample under dispatch on the drag, so a handler
//! reads it through [`NodeStore::drag_sample`] beside the pointer sample it
//! rides on. Each sample carries the pointer's velocity, read off its recent
//! samples by the input clock; a drag that ends moving fast enough along one
//! axis is a swipe ([`DragSample::swipe`]).

use crate::component::NodeStore;
use crate::input::PointerId;
use crate::layout::{Axis, Vec2};
use crate::node::NodeId;
use crate::scroll_motion::VelocityTracker;

/// How far a mouse travels from where it pressed before the press becomes a
/// drag.
pub const MOUSE_SLOP: f32 = 4.0;

/// How fast (pt/s) a drag ends moving along its axis to be a swipe.
pub const SWIPE_SPEED: f32 = 400.0;

/// Which way a swipe went, in window axes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwipeDirection {
    Left,
    Right,
    Up,
    Down,
}

/// Which sample of a drag is under dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DragPhase {
    /// The pointer passed the slop.
    Start,
    /// It moved on.
    Move,
    /// It lifted or was cancelled.
    End,
}

/// A drag's sample under dispatch, in window coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DragSample {
    pub phase: DragPhase,
    /// Where the pointer is.
    pub at: Vec2,
    /// How far it moved since the drag's last sample; since the press for a
    /// start.
    pub delta: Vec2,
    /// How far it moved since the press.
    pub total: Vec2,
    /// How fast it moves (pt/s), along each axis; at the end, how fast it
    /// moved as it lifted.
    pub velocity: Vec2,
}

impl DragSample {
    /// The swipe this sample ends the drag with: it lifted moving at least
    /// [`SWIPE_SPEED`] along the axis it moved fastest along, the way it
    /// travelled from the press. A cancel ends it with none.
    pub fn swipe(&self) -> Option<SwipeDirection> {
        if self.phase != DragPhase::End {
            return None;
        }
        let Vec2 { x, y } = self.velocity;
        let (speed, along, travel, ahead, back) = if x.abs() >= y.abs() {
            (
                x,
                x.abs(),
                self.total.x,
                SwipeDirection::Right,
                SwipeDirection::Left,
            )
        } else {
            (
                y,
                y.abs(),
                self.total.y,
                SwipeDirection::Down,
                SwipeDirection::Up,
            )
        };
        if along < SWIPE_SPEED || speed * travel <= 0.0 {
            return None;
        }
        Some(if speed > 0.0 { ahead } else { back })
    }
}

/// One pointer's drag, armed or started.
#[derive(Debug, Clone, Copy)]
struct Drag {
    pointer: PointerId,
    node: NodeId,
    origin: [f32; 2],
    last: [f32; 2],
    started: bool,
    /// A touch reaches a scroll viewport below the node first.
    behind_scroll: bool,
    /// The axis the node drags along alone, if it keeps to one.
    axis: Option<Axis>,
    sample: Option<DragSample>,
    track: VelocityTracker,
}

/// The nodes that take drags and the drags under way. Cold: a store none of
/// whose nodes takes drags arms none.
#[derive(Debug, Default)]
pub(crate) struct Drags {
    takers: Vec<NodeId>,
    /// The takers that drag along one axis alone, and the axis.
    axes: Vec<(NodeId, Axis)>,
    drags: Vec<Drag>,
}

impl Drags {
    pub(crate) fn clear(&mut self) {
        self.takers.clear();
        self.axes.clear();
        self.drags.clear();
    }

    fn of(&self, pointer: PointerId) -> Option<&Drag> {
        self.drags.iter().find(|d| d.pointer == pointer)
    }

    fn of_mut(&mut self, pointer: PointerId) -> Option<&mut Drag> {
        self.drags.iter_mut().find(|d| d.pointer == pointer)
    }

    fn forget(&mut self, pointer: PointerId) -> Option<Drag> {
        let at = self.drags.iter().position(|d| d.pointer == pointer)?;
        Some(self.drags.swap_remove(at))
    }
}

/// Whether `travel` past `slop` runs across `axis` rather than along it.
fn crosses(travel: Vec2, axis: Axis, slop: f32) -> bool {
    if travel.x.hypot(travel.y) < slop {
        return false;
    }
    let (along, across) = match axis {
        Axis::Row => (travel.x, travel.y),
        Axis::Column => (travel.y, travel.x),
    };
    across.abs() > along.abs()
}

fn vec(to: [f32; 2], from: [f32; 2]) -> Vec2 {
    Vec2 {
        x: to[0] - from[0],
        y: to[1] - from[1],
    }
}

impl NodeStore {
    /// Makes node `id` take drags, or no longer.
    pub fn set_draggable(&mut self, id: NodeId, takes: bool) {
        let mut takers = std::mem::take(&mut self.drags.takers);
        takers.retain(|&n| n != id && self.arena().is_live(n));
        if takes && self.arena().is_live(id) {
            takers.push(id);
        }
        self.drags.takers = takers;
    }

    /// Whether node `id` takes drags.
    pub fn draggable(&self, id: NodeId) -> bool {
        self.drags.takers.contains(&id)
    }

    /// Makes node `id` drag along `axis` alone, or along either with
    /// `None`: a press that travels across the axis is no drag of its, and a
    /// viewport above may pan it.
    pub fn set_drag_axis(&mut self, id: NodeId, axis: Option<Axis>) {
        let mut axes = std::mem::take(&mut self.drags.axes);
        axes.retain(|&(n, _)| n != id && self.arena().is_live(n));
        if let Some(axis) = axis
            && self.arena().is_live(id)
        {
            axes.push((id, axis));
        }
        self.drags.axes = axes;
    }

    /// The axis node `id` drags along alone, if it keeps to one.
    pub fn drag_axis(&self, id: NodeId) -> Option<Axis> {
        self.drags
            .axes
            .iter()
            .find(|&&(n, _)| n == id)
            .map(|&(_, axis)| axis)
    }

    /// Whether node `id` won a drag that runs.
    pub fn dragging(&self, id: NodeId) -> bool {
        self.drags.drags.iter().any(|d| d.started && d.node == id)
    }

    /// The node pointer `pointer`'s started drag runs on.
    pub(crate) fn drag_node(&self, pointer: PointerId) -> Option<NodeId> {
        self.drags.of(pointer).filter(|d| d.started).map(|d| d.node)
    }

    /// Where pointer `pointer`'s drag saw it last.
    pub(crate) fn drag_last(&self, pointer: PointerId) -> Option<[f32; 2]> {
        self.drags.of(pointer).map(|d| d.last)
    }

    /// The drag sample pointer `pointer`'s sample under dispatch carries.
    pub fn drag_sample(&self, pointer: PointerId) -> Option<DragSample> {
        self.drags.of(pointer)?.sample
    }

    /// Pointer `pointer` pressed `target` at `at`: arms a drag for the nearest
    /// node at or above it that takes drags, in place of any it had.
    pub(crate) fn arm_drag(
        &mut self,
        pointer: PointerId,
        target: NodeId,
        at: [f32; 2],
        direct: bool,
    ) {
        self.drags.forget(pointer);
        if self.drags.takers.is_empty() {
            return;
        }
        let mut behind_scroll = false;
        let mut node = Some(target);
        while let Some(n) = node {
            if self.draggable(n) {
                let mut track = VelocityTracker::default();
                if let Some(now) = self.input_time() {
                    track.push(now, at);
                }
                let axis = self.drag_axis(n);
                self.drags.drags.push(Drag {
                    pointer,
                    node: n,
                    origin: at,
                    last: at,
                    started: false,
                    behind_scroll,
                    axis,
                    sample: None,
                    track,
                });
                return;
            }
            behind_scroll |= direct && self.scroll_axis(n).is_some();
            node = self.parent(n);
        }
    }

    /// Whether pointer `pointer`'s drag decides its move to `at` before a
    /// pan may: one that started, or one armed above no scroll viewport. A
    /// drag along one axis that the pointer leaves past `slop` across it is
    /// dropped first, and leads nothing.
    pub(crate) fn drag_leads_at(&mut self, pointer: PointerId, at: [f32; 2], slop: f32) -> bool {
        let Some(d) = self.drags.of(pointer) else {
            return false;
        };
        if !d.started
            && let Some(axis) = d.axis
            && crosses(vec(at, d.origin), axis, slop)
        {
            self.drags.forget(pointer);
            return false;
        }
        let total = vec(at, d.origin);
        if !d.started && total.x.hypot(total.y) >= slop && self.viewer_yields(d.node, total) {
            self.drags.forget(pointer);
            return false;
        }
        d.started || !d.behind_scroll
    }

    /// Forgets pointer `pointer`'s armed drag: a pan took the contact.
    pub(crate) fn drop_drag(&mut self, pointer: PointerId) {
        if self.drags.forget(pointer).is_some_and(|d| d.started) {
            self.interaction_states.moved();
        }
    }

    /// Pointer `pointer` moved to `at`: starts its armed drag once past
    /// `slop`, and records the sample a started drag sees. Returns the node
    /// the start took the pointer from, which loses the contact.
    pub(crate) fn step_drag(
        &mut self,
        pointer: PointerId,
        at: [f32; 2],
        slop: f32,
    ) -> Option<NodeId> {
        let now = self.input_time();
        let d = self.drags.of_mut(pointer)?;
        if let Some(now) = now {
            d.track.push(now, at);
        }
        let (node, started, origin) = (d.node, d.started, d.origin);
        if started {
            if self.capture_of(pointer) != Some(node) {
                // A handler took the pointer from the drag.
                self.drop_drag(pointer);
                return None;
            }
            let d = self.drags.of_mut(pointer)?;
            d.sample = Some(DragSample {
                phase: DragPhase::Move,
                at: Vec2 { x: at[0], y: at[1] },
                delta: vec(at, d.last),
                total: vec(at, d.origin),
                velocity: d.track.velocity(),
            });
            d.last = at;
            return None;
        }
        let total = vec(at, origin);
        if total.x.hypot(total.y) < slop {
            return None;
        }
        if let Some(axis) = self.drags.of(pointer).and_then(|d| d.axis)
            && crosses(total, axis, slop)
        {
            self.drags.forget(pointer);
            return None;
        }
        if !self.arena().is_live(node) {
            self.drags.forget(pointer);
            return None;
        }
        let lost = self.set_capture(pointer, Some(node));
        self.set_pressed(None);
        self.interaction_states.moved();
        let d = self.drags.of_mut(pointer)?;
        d.started = true;
        d.sample = Some(DragSample {
            phase: DragPhase::Start,
            at: Vec2 { x: at[0], y: at[1] },
            delta: total,
            total,
            velocity: d.track.velocity(),
        });
        d.last = at;
        lost.filter(|&n| n != node)
    }

    /// Pointer `pointer` lifted at `at`, or was cancelled there: records the
    /// end a started drag sees, moving as it lifted and still once cancelled,
    /// and forgets one that never started.
    pub(crate) fn end_drag(&mut self, pointer: PointerId, at: [f32; 2], lifted: bool) {
        let now = self.input_time();
        let Some(d) = self.drags.of_mut(pointer) else {
            return;
        };
        if !d.started {
            self.drags.forget(pointer);
            return;
        }
        if let Some(now) = now {
            d.track.push(now, at);
        }
        d.sample = Some(DragSample {
            phase: DragPhase::End,
            at: Vec2 { x: at[0], y: at[1] },
            delta: vec(at, d.last),
            total: vec(at, d.origin),
            velocity: if lifted {
                d.track.velocity()
            } else {
                Vec2::ZERO
            },
        });
        d.last = at;
    }

    /// After pointer `pointer`'s sample was dispatched: a viewer's drag pans
    /// it by the sample, the drag sample it carried clears, and a drag it
    /// ended is forgotten along with the capture the drag took.
    pub(crate) fn settle_drag(&mut self, pointer: PointerId) {
        let Some(d) = self.drags.of_mut(pointer) else {
            return;
        };
        let node = d.node;
        let sample = d.sample.take();
        if let Some(sample) = sample {
            self.steer_viewer_drag(node, pointer, sample);
            self.steer_swipe_drag(node, sample);
        }
        if !sample.is_some_and(|s| s.phase == DragPhase::End) {
            return;
        }
        self.drop_drag(pointer);
        if self.capture_of(pointer) == Some(node) {
            self.set_capture(pointer, None);
        }
    }
}

#[cfg(test)]
mod tests {
    use core::time::Duration;

    use viso_runtime::Instant;

    use super::*;
    use crate::component::{BuildCx, LeafStyle};
    use crate::layout::Size;
    use crate::style::BoxStyle;

    /// A drag of pointer 1 on a fresh node from `(10, 10)` through `steps`,
    /// one every 10 ms, lifted (or cancelled) at the last; its end sample.
    fn drag(steps: &[[f32; 2]], lifted: bool) -> DragSample {
        let mut store = NodeStore::new();
        let node = {
            let mut cx = BuildCx::new(&mut store);
            cx.leaf(LeafStyle {
                size: Size::fixed(100.0, 100.0),
                style: BoxStyle::NONE,
            })
            .id()
        };
        store.set_draggable(node, true);
        let t0 = Instant::now();
        let pointer = PointerId(1);
        store.set_input_time(t0);
        store.arm_drag(pointer, node, [10.0, 10.0], false);
        for (k, &at) in steps.iter().enumerate() {
            store.set_input_time(t0 + Duration::from_millis(10 * (k as u64 + 1)));
            store.step_drag(pointer, at, MOUSE_SLOP);
            store.settle_drag(pointer);
        }
        let last = *steps.last().unwrap();
        store.end_drag(pointer, last, lifted);
        store.drag_sample(pointer).expect("an end")
    }

    #[test]
    fn a_drag_that_lifts_moving_fast_along_an_axis_is_a_swipe_that_way() {
        let left = drag(&[[0.0, 11.0], [-20.0, 12.0], [-40.0, 12.0]], true);
        assert!(left.velocity.x < -SWIPE_SPEED, "{left:?}");
        assert_eq!(left.swipe(), Some(SwipeDirection::Left));
        let down = drag(&[[11.0, 30.0], [12.0, 60.0], [12.0, 90.0]], true);
        assert_eq!(down.swipe(), Some(SwipeDirection::Down));
        let slow = drag(&[[16.0, 10.0], [17.0, 10.0], [18.0, 10.0]], true);
        assert_eq!(slow.swipe(), None, "too slow");
        let cancelled = drag(&[[0.0, 11.0], [-20.0, 12.0], [-40.0, 12.0]], false);
        assert_eq!(cancelled.velocity, Vec2::ZERO);
        assert_eq!(cancelled.swipe(), None, "a cancel is no swipe");
    }
}
