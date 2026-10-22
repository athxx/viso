//! Click counting: a press soon after and near the one before it counts on
//! from it, so a handler tells a double or triple click from a single one.
//!
//! A primary press of a pointer within [`CLICK_INTERVAL`] of that pointer's
//! last press, no further from it than the slop, is the next click of the
//! same run, up to three; anything else starts a run at one. The interval is
//! a timer armed at each press, so the count needs no clock of its own and
//! replays as the input does. Only a press at or under a node that counts
//! clicks (a text field) is counted, so a store with none arms nothing.
//! Fingers and pens are one pointer here: each tap is a contact of its own,
//! and a double tap is two of them.

use core::time::Duration;

use crate::component::NodeStore;
use crate::input::PointerId;
use crate::node::NodeId;
use crate::timer::TimerRequest;

/// How soon after a press the next one continues its run.
pub const CLICK_INTERVAL: Duration = Duration::from_millis(500);

/// The most clicks a run counts: presses on after a triple click are triple
/// clicks too.
const MOST: u8 = 3;

/// The run every direct contact counts on.
const TOUCH: PointerId = PointerId(u64::MAX);

/// One pointer's last press.
#[derive(Debug, Clone, Copy)]
struct Press {
    pointer: PointerId,
    at: [f32; 2],
    count: u8,
    /// Tells this press's timer from an earlier one's.
    token: u64,
    /// Whether the interval since it is still running.
    open: bool,
}

/// The nodes that count clicks and the last press of each pointer, as many
/// as press together. Cold.
#[derive(Debug, Default)]
pub(crate) struct Clicks {
    counters: Vec<NodeId>,
    presses: Vec<Press>,
    next: u64,
}

impl Clicks {
    pub(crate) fn clear(&mut self) {
        self.counters.clear();
        self.presses.clear();
    }

    /// Forgets the slot `index`, freed or reused.
    pub(crate) fn forget(&mut self, index: u32) {
        if !self.counters.is_empty() {
            self.counters.retain(|n| n.index() != index);
        }
    }
}

impl NodeStore {
    /// Makes node `id` count no more clicks of its own.
    pub(crate) fn uncount_clicks(&mut self, id: NodeId) {
        self.clicks.counters.retain(|&n| n != id);
    }

    /// Makes node `id` count the clicks pressed at or under it.
    pub fn count_clicks(&mut self, id: NodeId) {
        if self.arena().is_live(id) && !self.clicks.counters.contains(&id) {
            self.clicks.counters.push(id);
        }
    }

    /// Counts the primary press of `pointer` on `target` at `at` on its run
    /// of clicks when a node at or above `target` counts them, arming the
    /// interval on `root`; `slop` is how far a press may land from the last
    /// one and still count on.
    pub(crate) fn count_click(
        &mut self,
        pointer: PointerId,
        target: NodeId,
        root: NodeId,
        at: [f32; 2],
        slop: f32,
    ) {
        if self.clicks.counters.is_empty() {
            return;
        }
        let pointer = self.click_run(pointer);
        let mut node = Some(target);
        while let Some(n) = node {
            if self.clicks.counters.contains(&n) {
                break;
            }
            node = self.parent(n);
        }
        if node.is_none() {
            self.clicks.presses.retain(|p| p.pointer != pointer);
            return;
        }
        let clicks = &mut self.clicks;
        clicks.next += 1;
        let token = clicks.next;
        let count = match clicks.presses.iter().find(|p| p.pointer == pointer) {
            Some(last) if last.open && (at[0] - last.at[0]).hypot(at[1] - last.at[1]) <= slop => {
                (last.count + 1).min(MOST)
            }
            _ => 1,
        };
        clicks.presses.retain(|p| p.pointer != pointer);
        clicks.presses.push(Press {
            pointer,
            at,
            count,
            token,
            open: true,
        });
        self.queue_timer(TimerRequest::new(root, CLICK_INTERVAL, move |store| {
            if let Some(press) = store
                .clicks
                .presses
                .iter_mut()
                .find(|p| p.pointer == pointer && p.token == token)
            {
                press.open = false;
            }
        }));
    }

    /// How many clicks the press of `pointer` under dispatch is in its run:
    /// `1` for a single click, `2` for a double, `3` for a triple.
    pub fn click_count(&self, pointer: PointerId) -> u8 {
        let pointer = self.click_run(pointer);
        self.clicks
            .presses
            .iter()
            .find(|p| p.pointer == pointer)
            .map_or(1, |p| p.count)
    }

    /// The run `pointer`'s presses count on: its own, or for a direct
    /// contact the one all of them share.
    fn click_run(&self, pointer: PointerId) -> PointerId {
        if self.contacts().is_direct(pointer) {
            TOUCH
        } else {
            pointer
        }
    }
}
