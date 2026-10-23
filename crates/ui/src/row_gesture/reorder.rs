//! Reorder: a list whose rows a drag on a row's handle lifts and carries to
//! another place among them.
//!
//! A reorderable list ([`NodeStore::set_reorder_list`]) takes its children
//! as its rows — a list that scrolls, its content's children, the rows of a
//! virtual list among them. A handle ([`NodeStore::set_reorder_handle`]) is
//! any node within a row: a drag on it lifts its row above the others,
//! which follows the pointer along the list's axis, and the rows between
//! where it came from and where it would land slide aside to make room for
//! it, by the midpoints they lie at. A pointer held near either end of the
//! list's viewport scrolls it, faster the nearer, and a wheel scrolls it too,
//! the row staying under the pointer. Let go, the row springs into the gap,
//! and once all came to rest the list is told where it came from and went
//! ([`RowGesture::Reordered`]) on the next frame: the rows stand slid until
//! then, and as it is told they let go, the owner moving its items in that
//! frame. A row's place is its semantic position, as a virtual list
//! announces it, else its place among the rows.
//!
//! The keyboard lifts the row of a focused handle with Space or Enter; the
//! arrows along the list move it a place at a time, the viewport keeping it
//! in sight, and Space or Enter drops it. Escape puts a lifted row back
//! where it came from, as does the focus leaving its handle or a press. A
//! virtual list keeps a lifted row mounted wherever it scrolls. Under
//! reduced motion the rows move at once.

use core::time::Duration;

use crate::component::NodeStore;
use crate::drag::{DragPhase, DragSample};
use crate::input::{Key, KeyEvent, PointerId};
use crate::layout::{Axis, Vec2};
use crate::node::NodeId;
use crate::row_gesture::RowGesture;
use crate::row_gesture::swipe::{SETTLE_NEAR, SETTLE_STILL};
use crate::scroll_motion::{MAX_STEP, SPRING};

/// How near either end of its viewport the pointer scrolls the list.
const EDGE_BAND: f32 = 40.0;
/// How fast (pt/s) it scrolls the list at the very end, or past it.
const EDGE_SPEED: f32 = 800.0;

/// A row lifted from a reorderable list ([`NodeStore::reorder_lift`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReorderLift {
    /// The list.
    pub list: NodeId,
    /// The row lifted.
    pub row: NodeId,
    /// Its place among the rows.
    pub from: usize,
    /// The place it would land at, among the rows without it.
    pub to: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Following the pointer.
    Follow,
    /// Moved by the keyboard.
    Keys,
    /// Let go, springing into its gap.
    Drop,
    /// Put back, springing to where it came from.
    Cancel,
    /// Come to rest in its gap, the list told; the rows stand slid until it
    /// hears it.
    Told,
}

#[derive(Debug, Clone, Copy)]
struct Lift {
    list: NodeId,
    /// The node whose children are the rows.
    rows: NodeId,
    row: NodeId,
    /// The handle it lifted from.
    handle: NodeId,
    axis: Axis,
    /// `1.0` if the rows lie along the axis as their places grow, `-1.0`
    /// if against it (a row read right to left).
    flow: f32,
    from: usize,
    to: usize,
    count: usize,
    pointer: Option<PointerId>,
    phase: Phase,
    /// Where the pointer pressed and is, along the axis, in window space.
    grip: f32,
    at: f32,
    /// The viewport the list scrolls in, along the axis, and its offset as
    /// the row lifted and as last seen.
    viewport: Option<NodeId>,
    scroll_from: f32,
    scroll_seen: f32,
    /// How fast (pt/s) the pointer near an end scrolls the viewport.
    edge: f32,
    /// How far the others slide to make room: the row's extent and the gap
    /// after it.
    pitch: f32,
}

/// A row's slide along the list's axis.
#[derive(Debug, Clone, Copy)]
struct Shift {
    node: NodeId,
    /// Its place, as it last slid: a host a virtual list rebinds takes its
    /// new slide at once.
    place: usize,
    offset: f32,
    to: f32,
    v: f64,
    /// The slide written into its translate.
    shown: f32,
}

/// A window's reorderable lists and the row lifted. Cold: a store with no
/// such list spends nothing on them.
#[derive(Debug, Default)]
pub(crate) struct Reorders {
    lists: Vec<NodeId>,
    handles: Vec<NodeId>,
    lift: Option<Lift>,
    shifts: Vec<Shift>,
}

impl Reorders {
    pub(crate) fn clear(&mut self) {
        self.lists.clear();
        self.handles.clear();
        self.lift = None;
        self.shifts.clear();
    }

    /// Whether a row slides on its own, or the viewport scrolls under one.
    pub(crate) fn moving(&self) -> bool {
        self.lift.is_some_and(|l| l.edge != 0.0)
            || self.shifts.iter().any(|s| s.offset != s.to || s.v != 0.0)
    }
}

/// A slide of `offset` along `axis`.
fn along(axis: Axis, offset: f32) -> Vec2 {
    match axis {
        Axis::Row => Vec2::new(offset, 0.0),
        Axis::Column => Vec2::new(0.0, offset),
    }
}

fn main_of(axis: Axis, v: Vec2) -> f32 {
    match axis {
        Axis::Row => v.x,
        Axis::Column => v.y,
    }
}

impl NodeStore {
    /// Make node `id` a reorderable list, or no longer one: its rows lift
    /// from their handles.
    pub fn set_reorder_list(&mut self, id: NodeId, takes: bool) {
        let mut lists = std::mem::take(&mut self.row_gestures.reorders.lists);
        lists.retain(|&n| n != id && self.arena().is_live(n));
        if takes && self.arena().is_live(id) {
            lists.push(id);
        }
        self.row_gestures.reorders.lists = lists;
        if !takes
            && self
                .row_gestures
                .reorders
                .lift
                .is_some_and(|l| l.list == id)
        {
            self.end_reorder();
        }
    }

    /// Whether node `id` is a reorderable list.
    pub fn is_reorder_list(&self, id: NodeId) -> bool {
        self.row_gestures.reorders.lists.contains(&id)
    }

    /// Make node `id` a handle of the row of a reorderable list it lies in,
    /// or no longer one: a drag on it lifts the row.
    pub fn set_reorder_handle(&mut self, id: NodeId, takes: bool) {
        let mut handles = std::mem::take(&mut self.row_gestures.reorders.handles);
        let was = handles.contains(&id);
        handles.retain(|&n| n != id && self.arena().is_live(n));
        if takes && self.arena().is_live(id) {
            handles.push(id);
        }
        self.row_gestures.reorders.handles = handles;
        if takes != was {
            self.set_draggable(id, takes);
        }
    }

    /// Whether node `id` is a handle of a reorderable row.
    pub fn is_reorder_handle(&self, id: NodeId) -> bool {
        self.row_gestures.reorders.handles.contains(&id)
    }

    /// The row lifted from a list, while one is: after it is let go, until
    /// the list is told.
    pub fn reorder_lift(&self) -> Option<ReorderLift> {
        self.row_gestures
            .reorders
            .lift
            .filter(|l| l.phase != Phase::Cancel)
            .map(|l| ReorderLift {
                list: l.list,
                row: l.row,
                from: l.from,
                to: l.to,
            })
    }

    /// Whether node `id` is a lifted row, which rides above its siblings.
    #[inline]
    pub(crate) fn is_lifted(&self, id: NodeId) -> bool {
        self.row_gestures.reorders.lift.is_some_and(|l| l.row == id)
    }

    /// Whether a child of node `id` is a lifted row.
    #[inline]
    pub(crate) fn lifts_child(&self, id: NodeId) -> bool {
        self.row_gestures
            .reorders
            .lift
            .is_some_and(|l| l.rows == id)
    }

    /// The slide a reorder gives node `id`, past its own translate.
    #[inline]
    pub(crate) fn reorder_shift_of(&self, id: NodeId) -> Vec2 {
        let r = &self.row_gestures.reorders;
        let Some(lift) = r.lift.filter(|_| !r.shifts.is_empty()) else {
            return Vec2::ZERO;
        };
        r.shifts
            .iter()
            .find(|s| s.node == id)
            .map_or(Vec2::ZERO, |s| along(lift.axis, s.shown))
    }

    /// The node whose children are list `list`'s rows: its content's, for
    /// one that scrolls.
    fn reorder_rows(&self, list: NodeId) -> NodeId {
        if self.scroll_axis(list).is_some()
            && let Some(content) = self.arena().children(list).find(|&c| !self.is_chrome(c))
        {
            return content;
        }
        list
    }

    /// The list, the node whose children are its rows, and the row node `id`
    /// lies in.
    fn reorder_row_of(&self, id: NodeId) -> Option<(NodeId, NodeId, NodeId)> {
        let lists = &self.row_gestures.reorders.lists;
        if lists.is_empty() {
            return None;
        }
        let mut node = id;
        while let Some(parent) = self.arena().parent(node) {
            if let Some(&list) = lists
                .iter()
                .find(|&&l| self.arena().is_live(l) && self.reorder_rows(l) == parent)
            {
                return Some((list, parent, node));
            }
            node = parent;
        }
        None
    }

    /// Row `row`'s place among the children of `rows`, and how many rows
    /// there are.
    fn reorder_place(&self, rows: NodeId, row: NodeId) -> (usize, usize) {
        if let Some((index, count)) = self.semantic_state(row).and_then(|s| s.position) {
            return ((index as usize).saturating_sub(1), count as usize);
        }
        let mut place = 0;
        let mut count = 0;
        for child in self.arena().children(rows) {
            if self.is_chrome(child) {
                continue;
            }
            if child == row {
                place = count;
            }
            count += 1;
        }
        (place, count)
    }

    /// Where node `id` lays out along `axis`, as `(start, end)`.
    fn reorder_span(&self, id: NodeId, axis: Axis) -> (f32, f32) {
        let b = self.bounds(id);
        match axis {
            Axis::Row => (b.x, b.x + b.w),
            Axis::Column => (b.y, b.y + b.h),
        }
    }

    /// Where row `id` lays out as the list flows `flow`: `(lo, hi)`, the
    /// lower toward the first row.
    fn reorder_flow_span(&self, id: NodeId, axis: Axis, flow: f32) -> (f32, f32) {
        let (start, end) = self.reorder_span(id, axis);
        if flow > 0.0 {
            (start, end)
        } else {
            (-end, -start)
        }
    }

    /// Lifts the row handle `handle` lies in, by pointer `pointer` pressed
    /// at `at` (window space) or by the keyboard with `None`. Returns
    /// whether it lifted one.
    pub(crate) fn lift_reorder(
        &mut self,
        handle: NodeId,
        pointer: Option<PointerId>,
        at: Vec2,
    ) -> bool {
        let Some((list, rows, row)) = self.reorder_row_of(handle) else {
            return false;
        };
        if self.row_gestures.reorders.lift.is_some() {
            self.end_reorder();
        }
        let axis = self
            .flex_arrangement(rows)
            .map(|(axis, ..)| axis)
            .or_else(|| self.scroll_axis(list))
            .unwrap_or(Axis::Column);
        let (from, count) = self.reorder_place(rows, row);
        let (start, end) = self.reorder_span(row, axis);
        let mid = (start + end) / 2.0;
        // Which way the places grow, and the gap to a neighbour.
        let mut flow = 1.0;
        let mut neighbour = None;
        for child in self.arena().children(rows) {
            if child == row || self.is_chrome(child) {
                continue;
            }
            let (place, _) = self.reorder_place(rows, child);
            if place.abs_diff(from) == 1 {
                let (s, e) = self.reorder_span(child, axis);
                let ahead = place > from;
                flow = if ((s + e) / 2.0 > mid) == ahead {
                    1.0
                } else {
                    -1.0
                };
                if neighbour.is_none() || ahead {
                    neighbour = Some((child, ahead));
                }
            }
        }
        let (lo, hi) = self.reorder_flow_span(row, axis, flow);
        let gap = neighbour.map_or(0.0, |(n, ahead)| {
            let (nlo, nhi) = self.reorder_flow_span(n, axis, flow);
            if ahead { nlo - hi } else { lo - nhi }.max(0.0)
        });
        let mut viewport = None;
        let mut node = Some(list);
        while let Some(n) = node {
            if self.scroll_axis(n) == Some(axis) {
                viewport = Some(n);
                break;
            }
            node = self.arena().parent(n);
        }
        let scroll = viewport.map_or(0.0, |v| main_of(axis, self.scroll(v)));
        let grip = main_of(axis, at);
        self.row_gestures.reorders.lift = Some(Lift {
            list,
            rows,
            row,
            handle,
            axis,
            flow,
            from,
            to: from,
            count,
            pointer,
            phase: if pointer.is_some() {
                Phase::Follow
            } else {
                Phase::Keys
            },
            grip,
            at: grip,
            viewport,
            scroll_from: scroll,
            scroll_seen: scroll,
            edge: 0.0,
            pitch: hi - lo + gap,
        });
        self.restack_children(rows);
        self.lay_reorder();
        true
    }

    /// Puts the lifted row back where it came from.
    fn cancel_reorder(&mut self) {
        let Some(lift) = self.row_gestures.reorders.lift.as_mut() else {
            return;
        };
        lift.phase = Phase::Cancel;
        lift.to = lift.from;
        lift.edge = 0.0;
        self.lay_reorder();
        self.settle_reorder();
    }

    /// Lets the lifted row go into its gap.
    fn drop_reorder(&mut self) {
        let Some(lift) = self.row_gestures.reorders.lift.as_mut() else {
            return;
        };
        lift.phase = Phase::Drop;
        lift.edge = 0.0;
        self.lay_reorder();
        self.settle_reorder();
    }

    /// Ends the reorder: the rows let go of their slides.
    fn end_reorder(&mut self) {
        let Some(lift) = self.row_gestures.reorders.lift.take() else {
            return;
        };
        let shifts = std::mem::take(&mut self.row_gestures.reorders.shifts);
        for s in &shifts {
            if self.arena().is_live(s.node) {
                let own = self.translate(s.node) - along(lift.axis, s.shown);
                self.put_translate(s.node, own);
            }
        }
        self.row_gestures.reorders.shifts = shifts;
        self.row_gestures.reorders.shifts.clear();
        if self.arena().is_live(lift.rows) {
            self.restack_children(lift.rows);
        }
    }

    /// The slide that lands the lifted row at place `to`.
    fn reorder_landing(&self, lift: &Lift) -> f32 {
        if lift.to == lift.from {
            return 0.0;
        }
        let (lo, hi) = self.reorder_flow_span(lift.row, lift.axis, lift.flow);
        let there = self.arena().children(lift.rows).find(|&c| {
            c != lift.row && !self.is_chrome(c) && self.reorder_place(lift.rows, c).0 == lift.to
        });
        let slide = match there {
            Some(c) => {
                let (clo, chi) = self.reorder_flow_span(c, lift.axis, lift.flow);
                if lift.to > lift.from {
                    chi - hi
                } else {
                    clo - lo
                }
            }
            None => (lift.to as f32 - lift.from as f32) * lift.pitch,
        };
        slide * lift.flow
    }

    /// The place among the rows without it the lifted row slid `offset`
    /// would land at: before the first other row whose midpoint lies past
    /// its own.
    fn reorder_slot(&self, lift: &Lift, offset: f32) -> usize {
        let (lo, hi) = self.reorder_flow_span(lift.row, lift.axis, lift.flow);
        let mid = (lo + hi) / 2.0 + offset * lift.flow;
        let mut before: Option<usize> = None;
        let mut last: Option<usize> = None;
        for c in self.arena().children(lift.rows) {
            if c == lift.row || self.is_chrome(c) {
                continue;
            }
            let (place, _) = self.reorder_place(lift.rows, c);
            let (clo, chi) = self.reorder_flow_span(c, lift.axis, lift.flow);
            let landed = place - usize::from(place > lift.from);
            if (clo + chi) / 2.0 >= mid {
                before = Some(before.map_or(landed, |b| b.min(landed)));
            } else {
                last = Some(last.map_or(landed, |l| l.max(landed)));
            }
        }
        let slot = before.or(last.map(|l| l + 1)).unwrap_or(lift.from);
        slot.min(lift.count.saturating_sub(1))
    }

    /// Sets where each row slides to: the lifted row where the pointer or
    /// the keyboard puts it, the others aside of its gap.
    fn lay_reorder(&mut self) {
        let Some(mut lift) = self.row_gestures.reorders.lift else {
            return;
        };
        if !self.arena().is_live(lift.list)
            || !self.arena().is_live(lift.rows)
            || !self.arena().is_live(lift.row)
            || self.arena().parent(lift.row) != Some(lift.rows)
        {
            self.end_reorder();
            return;
        }
        let instant = self.reduced_motion();
        let scroll = lift
            .viewport
            .filter(|&v| self.arena().is_live(v))
            .map_or(lift.scroll_from, |v| main_of(lift.axis, self.scroll(v)));
        lift.scroll_seen = scroll;
        let mut lifted = None;
        if lift.phase == Phase::Follow {
            let (start, end) = self.reorder_span(lift.row, lift.axis);
            let (lo, hi) = self.reorder_span(lift.rows, lift.axis);
            let offset = (lift.at - lift.grip + scroll - lift.scroll_from)
                .clamp(lo - start, (hi - end).max(lo - start));
            lift.to = self.reorder_slot(&lift, offset);
            lifted = Some(offset);
            lift.edge = self.reorder_edge(&lift, scroll);
        }
        let landing = self.reorder_landing(&lift);
        let mut shifts = std::mem::take(&mut self.row_gestures.reorders.shifts);
        shifts.retain(|s| {
            let keep =
                self.arena().is_live(s.node) && self.arena().parent(s.node) == Some(lift.rows);
            if !keep && self.arena().is_live(s.node) {
                let own = self.translate(s.node) - along(lift.axis, s.shown);
                self.put_translate(s.node, own);
            }
            keep
        });
        for c in self.arena().children(lift.rows) {
            if self.is_chrome(c) {
                continue;
            }
            let (place, _) = self.reorder_place(lift.rows, c);
            let to = if c == lift.row {
                match (lift.phase, lifted) {
                    (Phase::Cancel, _) => 0.0,
                    (_, Some(offset)) => offset,
                    _ => landing,
                }
            } else if lift.phase == Phase::Cancel {
                0.0
            } else if lift.from < place && place <= lift.to {
                -lift.flow * lift.pitch
            } else if lift.to <= place && place < lift.from {
                lift.flow * lift.pitch
            } else {
                0.0
            };
            match shifts.iter_mut().find(|s| s.node == c) {
                Some(s) => {
                    s.to = to;
                    // A rebound host, the followed row and every row under
                    // reduced motion take theirs at once.
                    if s.place != place || (lifted.is_some() && c == lift.row) || instant {
                        s.offset = to;
                        s.v = 0.0;
                    }
                    s.place = place;
                }
                // A row come into the list takes its slide at once.
                None => shifts.push(Shift {
                    node: c,
                    place,
                    offset: to,
                    to,
                    v: 0.0,
                    shown: 0.0,
                }),
            }
        }
        self.row_gestures.reorders.shifts = shifts;
        self.row_gestures.reorders.lift = Some(lift);
        self.show_reorder();
    }

    /// Writes each row's slide past its own translate.
    fn show_reorder(&mut self) {
        let Some(lift) = self.row_gestures.reorders.lift else {
            return;
        };
        for k in 0..self.row_gestures.reorders.shifts.len() {
            let s = self.row_gestures.reorders.shifts[k];
            if s.shown == s.offset || !self.arena().is_live(s.node) {
                continue;
            }
            let own = self.translate(s.node) - along(lift.axis, s.shown);
            self.put_translate(s.node, own + along(lift.axis, s.offset));
            self.row_gestures.reorders.shifts[k].shown = s.offset;
        }
    }

    /// How fast the pointer of lift `lift` near an end of its viewport
    /// scrolls it, the viewport at `scroll`: along the axis's way, nothing
    /// once it can scroll no further.
    fn reorder_edge(&self, lift: &Lift, scroll: f32) -> f32 {
        let Some(v) = lift.viewport.filter(|&v| self.arena().is_live(v)) else {
            return 0.0;
        };
        let w = self.world(v);
        let (v0, v1) = match lift.axis {
            Axis::Row => (w.x, w.x + w.w),
            Axis::Column => (w.y, w.y + w.h),
        };
        let band = EDGE_BAND.min((v1 - v0) / 3.0);
        if band <= 0.0 {
            return 0.0;
        }
        let near = ((v0 + band - lift.at) / band).min(1.0);
        let far = ((lift.at - (v1 - band)) / band).min(1.0);
        let (min, max) = self.scroll_span(v, lift.axis);
        if near > 0.0 && scroll > min {
            -EDGE_SPEED * near
        } else if far > 0.0 && scroll < max {
            EDGE_SPEED * far
        } else {
            0.0
        }
    }

    /// Whether the lifted row came to rest let go: in its gap, the list is
    /// told; put back, the reorder ends.
    fn settle_reorder(&mut self) {
        let r = &self.row_gestures.reorders;
        let Some(lift) = r.lift else {
            return;
        };
        if !matches!(lift.phase, Phase::Drop | Phase::Cancel)
            || r.shifts.iter().any(|s| s.offset != s.to || s.v != 0.0)
        {
            return;
        }
        if lift.phase == Phase::Drop && lift.to != lift.from {
            self.tell_row_gesture(
                lift.list,
                RowGesture::Reordered {
                    from: lift.from,
                    to: lift.to,
                },
            );
            if let Some(l) = self.row_gestures.reorders.lift.as_mut() {
                l.phase = Phase::Told;
            }
        } else {
            self.end_reorder();
        }
    }

    /// The list was told where its row went: the rows let go of their
    /// slides as its owner moves its items.
    pub(crate) fn told_reorder(&mut self) {
        if self
            .row_gestures
            .reorders
            .lift
            .is_some_and(|l| l.phase == Phase::Told)
        {
            self.end_reorder();
        }
    }

    /// After a sample of pointer `pointer`'s drag on node `node` was
    /// dispatched: a handle's drag lifts its row, carries it, and lets it go.
    pub(crate) fn steer_reorder_drag(&mut self, node: NodeId, pointer: PointerId, s: DragSample) {
        if self.row_gestures.reorders.handles.is_empty() || !self.is_reorder_handle(node) {
            return;
        }
        if s.phase == DragPhase::Start && !self.lift_reorder(node, Some(pointer), s.at - s.total) {
            return;
        }
        let Some(lift) = self.row_gestures.reorders.lift.as_mut() else {
            return;
        };
        if lift.pointer != Some(pointer) || lift.phase != Phase::Follow {
            return;
        }
        lift.at = main_of(lift.axis, s.at);
        if s.phase == DragPhase::End {
            self.drop_reorder();
        } else {
            self.lay_reorder();
        }
    }

    /// Key `ev`: Space or Enter on a handle lifts its row and drops it, the
    /// arrows along the list move it, Escape puts it back. Returns whether
    /// it took the key.
    pub(crate) fn reorder_key(&mut self, ev: &KeyEvent) -> bool {
        let r = &self.row_gestures.reorders;
        if !ev.pressed || r.handles.is_empty() {
            return false;
        }
        let m = ev.modifiers;
        let bare = !m.shift && !m.control && !m.alt && !m.logo;
        let toggle = bare && !ev.repeat && matches!(ev.key, Key::Space | Key::Enter);
        let Some(lift) = r.lift else {
            let Some(f) = self
                .focused()
                .filter(|&f| toggle && self.is_reorder_handle(f))
            else {
                return false;
            };
            return self.lift_reorder(f, None, Vec2::ZERO);
        };
        match lift.phase {
            Phase::Follow if ev.key == Key::Escape => {
                self.cancel_reorder();
                true
            }
            Phase::Keys if ev.key == Key::Escape => {
                self.cancel_reorder();
                true
            }
            Phase::Keys if toggle => {
                self.drop_reorder();
                true
            }
            Phase::Keys if bare => {
                let dir = match (lift.axis, ev.key) {
                    (Axis::Column, Key::Up) | (Axis::Row, Key::Left) => -1.0,
                    (Axis::Column, Key::Down) | (Axis::Row, Key::Right) => 1.0,
                    _ => return false,
                };
                let to = if dir * lift.flow > 0.0 {
                    (lift.to + 1).min(lift.count.saturating_sub(1))
                } else {
                    lift.to.saturating_sub(1)
                };
                if let Some(l) = self.row_gestures.reorders.lift.as_mut() {
                    l.to = to;
                }
                self.lay_reorder();
                self.reveal_reorder();
                true
            }
            _ => false,
        }
    }

    /// Scrolls the lifted row's landing into its viewport's sight.
    fn reveal_reorder(&mut self) {
        let Some(lift) = self.row_gestures.reorders.lift else {
            return;
        };
        let Some(v) = lift.viewport.filter(|&v| self.arena().is_live(v)) else {
            return;
        };
        let landing = self.reorder_landing(&lift);
        let (start, end) = self.reorder_span(lift.row, lift.axis);
        let (v0, v1) = self.reorder_span(v, lift.axis);
        let scroll = main_of(lift.axis, self.scroll(v));
        let (lo, hi) = (start + landing - v0 - scroll, end + landing - v0 - scroll);
        let by = if lo < 0.0 {
            lo
        } else if hi > v1 - v0 {
            (hi - (v1 - v0)).min(lo)
        } else {
            return;
        };
        self.scroll_by(v, along(lift.axis, by));
    }

    /// The focus moved to `to`: off the handle a keyboard lifted from, the
    /// row goes back.
    pub(crate) fn reorder_focus(&mut self, to: Option<NodeId>) {
        if self
            .row_gestures
            .reorders
            .lift
            .is_some_and(|l| l.phase == Phase::Keys && to != Some(l.handle))
        {
            self.cancel_reorder();
        }
    }

    /// A press: a row the keyboard lifted goes back.
    pub(crate) fn press_reorder(&mut self) {
        if self
            .row_gestures
            .reorders
            .lift
            .is_some_and(|l| l.phase == Phase::Keys)
        {
            self.cancel_reorder();
        }
    }

    /// Whether the viewport under a row the pointer carries scrolled since
    /// it was last laid: a wheel turned.
    pub(crate) fn reorder_scrolled(&self) -> bool {
        self.row_gestures.reorders.lift.is_some_and(|l| {
            l.phase == Phase::Follow
                && l.viewport.is_some_and(|v| {
                    self.arena().is_live(v) && main_of(l.axis, self.scroll(v)) != l.scroll_seen
                })
        })
    }

    /// Advance the reorder by `delta`: the pointer near an end scrolls the
    /// viewport, the carried row keeps under it, and the others spring.
    pub(crate) fn tick_reorders(&mut self, delta: Duration) {
        let Some(lift) = self.row_gestures.reorders.lift else {
            return;
        };
        let dt = delta.as_secs_f64().min(MAX_STEP);
        if lift.phase == Phase::Follow {
            if lift.edge != 0.0
                && let Some(v) = lift.viewport
            {
                self.scroll_by(v, along(lift.axis, lift.edge * dt as f32));
            }
            self.lay_reorder();
        }
        for s in &mut self.row_gestures.reorders.shifts {
            if s.offset == s.to && s.v == 0.0 {
                continue;
            }
            // A critically damped spring about its rest:
            // e(t) = (e + (v + λe)t)·e^(−λt).
            let e = f64::from(s.offset - s.to);
            let c = s.v + SPRING * e;
            let decay = (-SPRING * dt).exp();
            let e = (e + c * dt) * decay;
            let v = (s.v - SPRING * c * dt) * decay;
            if e.abs() < SETTLE_NEAR && v.abs() < SETTLE_STILL {
                s.offset = s.to;
                s.v = 0.0;
            } else {
                s.offset = s.to + e as f32;
                s.v = v;
            }
        }
        self.show_reorder();
        self.settle_reorder();
    }

    /// Once layout placed the tree: the rows slide where they now lie.
    pub(crate) fn fit_reorders(&mut self) {
        if self.row_gestures.reorders.lift.is_some() {
            self.lay_reorder();
            self.settle_reorder();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::component::{BuildCx, FlexStyle, LeafStyle};
    use crate::input::Modifiers;
    use crate::layout::Size;
    use crate::style::BoxStyle;
    use viso_render::Rect;

    const FRAME: Duration = Duration::from_millis(16);
    const MOUSE: PointerId = PointerId(1);

    /// A column of four 300×40 rows, each a 40×40 handle and a filler.
    fn list() -> (NodeStore, NodeId, Vec<NodeId>, Vec<NodeId>) {
        let mut store = NodeStore::new();
        let mut rows = Vec::new();
        let mut handles = Vec::new();
        let root = {
            let mut cx = BuildCx::new(&mut store);
            cx.flex(
                FlexStyle {
                    axis: Axis::Column,
                    size: Size::fixed(300.0, 160.0),
                    ..FlexStyle::default()
                },
                |cx| {
                    for _ in 0..4 {
                        let row = cx.flex(
                            FlexStyle {
                                axis: Axis::Row,
                                size: Size::fixed(300.0, 40.0),
                                ..FlexStyle::default()
                            },
                            |cx| {
                                handles.push(
                                    cx.leaf(LeafStyle {
                                        size: Size::fixed(40.0, 40.0),
                                        style: BoxStyle::NONE,
                                    })
                                    .id(),
                                );
                            },
                        );
                        rows.push(row.id());
                    }
                },
            )
            .id()
        };
        store.set_reorder_list(root, true);
        for &h in &handles {
            store.set_reorder_handle(h, true);
        }
        store.layout(root, Rect::new(0.0, 0.0, 300.0, 160.0), &mut Vec::new());
        (store, root, rows, handles)
    }

    fn settle(store: &mut NodeStore) {
        for _ in 0..200 {
            if !store.row_gestures.moving() {
                return;
            }
            store.tick_reorders(FRAME);
        }
        panic!("the rows never came to rest");
    }

    fn sample(phase: DragPhase, at: Vec2, total: Vec2) -> DragSample {
        DragSample {
            phase,
            at,
            delta: Vec2::ZERO,
            total,
            velocity: Vec2::ZERO,
        }
    }

    fn key(store: &mut NodeStore, key: Key) -> bool {
        store.reorder_key(&KeyEvent {
            key,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::default(),
        })
    }

    fn told(store: &mut NodeStore) -> Vec<(NodeId, RowGesture)> {
        let mut out = Vec::new();
        store.take_row_gestures(&mut out);
        out
    }

    fn slid(store: &NodeStore, rows: &[NodeId]) -> Vec<f32> {
        rows.iter().map(|&r| store.translate(r).y).collect()
    }

    #[test]
    fn a_handle_drag_carries_its_row_and_the_others_make_room() {
        let (mut store, root, rows, handles) = list();
        let h = handles[0];
        let press = Vec2::new(20.0, 20.0);
        let start = press + Vec2::new(0.0, 10.0);
        store.steer_reorder_drag(h, MOUSE, sample(DragPhase::Start, start, start - press));
        assert!(store.is_lifted(rows[0]));
        assert_eq!(store.stack_layer(rows[0]), 2, "it rides above the others");
        let at = press + Vec2::new(0.0, 90.0);
        store.steer_reorder_drag(h, MOUSE, sample(DragPhase::Move, at, at - press));
        let lift = store.reorder_lift().unwrap();
        assert_eq!((lift.list, lift.from, lift.to), (root, 0, 2));
        assert_eq!(store.translate(rows[0]).y, 90.0, "under the pointer");
        settle(&mut store);
        assert_eq!(slid(&store, &rows[1..]), [-40.0, -40.0, 0.0]);
        store.steer_reorder_drag(h, MOUSE, sample(DragPhase::End, at, at - press));
        settle(&mut store);
        assert_eq!(store.translate(rows[0]).y, 80.0, "in the gap");
        assert_eq!(
            told(&mut store),
            [(root, RowGesture::Reordered { from: 0, to: 2 })]
        );
        assert_eq!(slid(&store, &rows), [0.0; 4], "let go as told");
        assert_eq!(store.reorder_lift(), None);
        assert_eq!(store.stack_layer(rows[0]), 0);
    }

    #[test]
    fn the_keyboard_lifts_moves_and_drops_a_row() {
        let (mut store, root, rows, handles) = list();
        store.set_focused(Some(handles[2]));
        assert!(key(&mut store, Key::Space));
        assert!(key(&mut store, Key::Up));
        assert!(key(&mut store, Key::Up));
        assert!(key(&mut store, Key::Up), "at the top it stays");
        settle(&mut store);
        assert_eq!(slid(&store, &rows), [40.0, 40.0, -80.0, 0.0]);
        assert!(key(&mut store, Key::Enter));
        settle(&mut store);
        assert_eq!(
            told(&mut store),
            [(root, RowGesture::Reordered { from: 2, to: 0 })]
        );
        assert!(!key(&mut store, Key::Up), "nothing lifted takes no arrow");
    }

    #[test]
    fn escape_or_the_focus_leaving_puts_the_row_back() {
        let (mut store, _, rows, handles) = list();
        let press = Vec2::new(20.0, 20.0);
        let at = press + Vec2::new(0.0, 70.0);
        store.steer_reorder_drag(handles[0], MOUSE, sample(DragPhase::Start, at, at - press));
        assert!(key(&mut store, Key::Escape));
        settle(&mut store);
        assert_eq!(slid(&store, &rows), [0.0; 4]);
        assert!(told(&mut store).is_empty());
        assert_eq!(store.reorder_lift(), None);

        store.set_focused(Some(handles[1]));
        assert!(key(&mut store, Key::Space));
        assert!(key(&mut store, Key::Down));
        store.set_focused(Some(handles[3]));
        settle(&mut store);
        assert_eq!(slid(&store, &rows), [0.0; 4]);
        assert!(told(&mut store).is_empty());
    }

    #[test]
    fn dropped_back_in_its_own_place_it_tells_nothing() {
        let (mut store, _, rows, handles) = list();
        let press = Vec2::new(20.0, 20.0);
        let at = press + Vec2::new(0.0, 15.0);
        store.steer_reorder_drag(handles[0], MOUSE, sample(DragPhase::Start, at, at - press));
        store.steer_reorder_drag(handles[0], MOUSE, sample(DragPhase::End, at, at - press));
        settle(&mut store);
        assert!(told(&mut store).is_empty());
        assert_eq!(slid(&store, &rows), [0.0; 4]);
        assert_eq!(store.reorder_lift(), None);
    }
}
