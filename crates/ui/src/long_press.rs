//! Long presses: the gesture a node wins by taking `long_press`.
//!
//! A primary press arms a hold for the nearest node at or above the pressed
//! one that takes long presses from it — a node may take them from fingers
//! and pens alone, as a text field does — and asks for a timer [`LONG_PRESS`] out. The
//! pointer travelling past the slop — a drag or a pan in the making — or
//! lifting first lets the hold go. A hold whose timer fires before either is
//! due, and the frame's [`deliver_long_presses`] runs it: the press is
//! released so the lift clicks nothing, an armed drag is dropped, a touch is
//! captured to the node so no pan takes it after, and the node's chain sees
//! the long press.

use core::time::Duration;

use crate::binding::BindingTable;
use crate::component::NodeStore;
use crate::input::{PointerId, pointer_dispatch_chain};
use crate::layout::Vec2;
use crate::node::NodeId;
use crate::state::StateStore;
use crate::timer::TimerRequest;

/// How long a press holds still before it is a long press.
pub const LONG_PRESS: Duration = Duration::from_millis(500);

/// A long press under dispatch, in window coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LongPressSample {
    /// Where the pointer pressed.
    pub at: Vec2,
}

/// One pointer's hold.
#[derive(Debug, Clone, Copy)]
struct Hold {
    pointer: PointerId,
    node: NodeId,
    origin: [f32; 2],
    direct: bool,
    /// Tells this hold's timer from an earlier one of the same pointer.
    token: u64,
    due: bool,
}

/// The nodes that take long presses and the holds under way. Cold: a store
/// none of whose nodes takes long presses arms none.
#[derive(Debug, Default)]
pub(crate) struct Holds {
    /// Each node that takes long presses, and whether from direct contacts
    /// alone.
    takers: Vec<(NodeId, bool)>,
    holds: Vec<Hold>,
    next: u64,
    /// The pointer and sample under dispatch.
    sample: Option<(PointerId, LongPressSample)>,
}

impl Holds {
    pub(crate) fn clear(&mut self) {
        self.takers.clear();
        self.holds.clear();
        self.sample = None;
    }

    fn forget(&mut self, pointer: PointerId) -> Option<Hold> {
        let at = self.holds.iter().position(|h| h.pointer == pointer)?;
        Some(self.holds.swap_remove(at))
    }
}

impl NodeStore {
    /// Makes node `id` take long presses, or no longer.
    pub fn set_long_pressable(&mut self, id: NodeId, takes: bool) {
        self.take_holds(id, takes, false);
    }

    /// Makes node `id` take long presses from fingers and pens alone, or no
    /// longer.
    pub fn set_touch_long_pressable(&mut self, id: NodeId, takes: bool) {
        self.take_holds(id, takes, true);
    }

    fn take_holds(&mut self, id: NodeId, takes: bool, direct_only: bool) {
        let mut takers = std::mem::take(&mut self.holds.takers);
        takers.retain(|&(n, _)| n != id && self.arena().is_live(n));
        if takes && self.arena().is_live(id) {
            takers.push((id, direct_only));
        }
        self.holds.takers = takers;
    }

    /// Whether node `id` takes long presses.
    pub fn long_pressable(&self, id: NodeId) -> bool {
        self.holds.takers.iter().any(|&(n, _)| n == id)
    }

    /// Whether node `id` takes long presses from a pointer, `direct` or not.
    fn holds_for(&self, id: NodeId, direct: bool) -> bool {
        self.holds
            .takers
            .iter()
            .any(|&(n, direct_only)| n == id && (direct || !direct_only))
    }

    /// The long press pointer `pointer`'s dispatch carries.
    pub fn long_press_sample(&self, pointer: PointerId) -> Option<LongPressSample> {
        self.holds
            .sample
            .filter(|(p, _)| *p == pointer)
            .map(|(_, sample)| sample)
    }

    /// Pointer `pointer` pressed `target` at `at`: arms a hold for the nearest
    /// node at or above it that takes long presses, in place of any it had.
    pub(crate) fn arm_hold(
        &mut self,
        pointer: PointerId,
        target: NodeId,
        at: [f32; 2],
        direct: bool,
    ) {
        self.holds.forget(pointer);
        if self.holds.takers.is_empty() {
            return;
        }
        let mut node = Some(target);
        while let Some(n) = node {
            if self.holds_for(n, direct) {
                self.holds.next += 1;
                let token = self.holds.next;
                self.holds.holds.push(Hold {
                    pointer,
                    node: n,
                    origin: at,
                    direct,
                    token,
                    due: false,
                });
                self.queue_timer(TimerRequest::new(n, LONG_PRESS, move |store| {
                    if let Some(hold) = store
                        .holds
                        .holds
                        .iter_mut()
                        .find(|h| h.pointer == pointer && h.token == token)
                    {
                        hold.due = true;
                    }
                }));
                return;
            }
            node = self.parent(n);
        }
    }

    /// Pointer `pointer` moved to `at`: a hold it travelled past `slop` from
    /// lets go.
    pub(crate) fn follow_hold(&mut self, pointer: PointerId, at: [f32; 2], slop: f32) {
        let Some(hold) = self.holds.holds.iter().find(|h| h.pointer == pointer) else {
            return;
        };
        let (dx, dy) = (at[0] - hold.origin[0], at[1] - hold.origin[1]);
        if dx.hypot(dy) >= slop {
            self.holds.forget(pointer);
        }
    }

    /// Pointer `pointer` lifted, was cancelled or lost its contact: its hold
    /// lets go.
    pub(crate) fn release_hold(&mut self, pointer: PointerId) {
        self.holds.forget(pointer);
    }

    /// Whether a hold is due.
    pub fn long_press_due(&self) -> bool {
        self.holds.holds.iter().any(|h| h.due)
    }
}

/// After the frame's timers fired: runs each hold that came due on its node's
/// chain. Returns whether any handler ran.
pub fn deliver_long_presses(
    store: &mut NodeStore,
    states: &mut StateStore,
    bindings: &BindingTable,
) -> bool {
    let mut ran = false;
    let mut chain = Vec::new();
    while let Some(at) = store.holds.holds.iter().position(|h| h.due) {
        let hold = store.holds.holds.swap_remove(at);
        if !store.arena().is_live(hold.node) || store.inert(hold.node) {
            continue;
        }
        store.release_press(hold.pointer);
        store.drop_drag(hold.pointer);
        if hold.direct {
            store.set_capture(hold.pointer, Some(hold.node));
        }
        store.holds.sample = Some((
            hold.pointer,
            LongPressSample {
                at: Vec2 {
                    x: hold.origin[0],
                    y: hold.origin[1],
                },
            },
        ));
        ran |= pointer_dispatch_chain(store, states, bindings, hold.node, hold.pointer, &mut chain);
        store.holds.sample = None;
    }
    ran
}
