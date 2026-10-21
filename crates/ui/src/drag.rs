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
//! rides on.

use crate::component::NodeStore;
use crate::input::PointerId;
use crate::layout::Vec2;
use crate::node::NodeId;

/// How far a mouse travels from where it pressed before the press becomes a
/// drag.
pub const MOUSE_SLOP: f32 = 4.0;

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
    sample: Option<DragSample>,
}

/// The nodes that take drags and the drags under way. Cold: a store none of
/// whose nodes takes drags arms none.
#[derive(Debug, Default)]
pub(crate) struct Drags {
    takers: Vec<NodeId>,
    drags: Vec<Drag>,
}

impl Drags {
    pub(crate) fn clear(&mut self) {
        self.takers.clear();
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

    /// Whether node `id` won a drag that runs.
    pub fn dragging(&self, id: NodeId) -> bool {
        self.drags.drags.iter().any(|d| d.started && d.node == id)
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
                self.drags.drags.push(Drag {
                    pointer,
                    node: n,
                    origin: at,
                    last: at,
                    started: false,
                    behind_scroll,
                    sample: None,
                });
                return;
            }
            behind_scroll |= direct && self.scroll_axis(n).is_some();
            node = self.parent(n);
        }
    }

    /// Whether pointer `pointer`'s drag decides its moves before a pan may:
    /// one that started, or one armed above no scroll viewport.
    pub(crate) fn drag_leads(&self, pointer: PointerId) -> bool {
        self.drags
            .of(pointer)
            .is_some_and(|d| d.started || !d.behind_scroll)
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
        let drag = *self.drags.of(pointer)?;
        if drag.started {
            if self.capture_of(pointer) != Some(drag.node) {
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
            });
            d.last = at;
            return None;
        }
        let total = vec(at, drag.origin);
        if total.x.hypot(total.y) < slop {
            return None;
        }
        if !self.arena().is_live(drag.node) {
            self.drags.forget(pointer);
            return None;
        }
        let lost = self.set_capture(pointer, Some(drag.node));
        self.set_pressed(None);
        self.interaction_states.moved();
        let d = self.drags.of_mut(pointer)?;
        d.started = true;
        d.sample = Some(DragSample {
            phase: DragPhase::Start,
            at: Vec2 { x: at[0], y: at[1] },
            delta: total,
            total,
        });
        d.last = at;
        lost.filter(|&n| n != drag.node)
    }

    /// Pointer `pointer` lifted or was cancelled at `at`: records the end a
    /// started drag sees, and forgets one that never started.
    pub(crate) fn end_drag(&mut self, pointer: PointerId, at: [f32; 2]) {
        let Some(d) = self.drags.of_mut(pointer) else {
            return;
        };
        if !d.started {
            self.drags.forget(pointer);
            return;
        }
        d.sample = Some(DragSample {
            phase: DragPhase::End,
            at: Vec2 { x: at[0], y: at[1] },
            delta: vec(at, d.last),
            total: vec(at, d.origin),
        });
        d.last = at;
    }

    /// After pointer `pointer`'s sample was dispatched: clears the drag
    /// sample it carried, and forgets a drag it ended along with the capture
    /// the drag took.
    pub(crate) fn settle_drag(&mut self, pointer: PointerId) {
        let Some(d) = self.drags.of_mut(pointer) else {
            return;
        };
        let ended = d.sample.take().is_some_and(|s| s.phase == DragPhase::End);
        if !ended {
            return;
        }
        let node = d.node;
        self.drop_drag(pointer);
        if self.capture_of(pointer) == Some(node) {
            self.set_capture(pointer, None);
        }
    }
}
