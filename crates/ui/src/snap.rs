//! Scroll snapping: a viewport that comes to rest on its items.
//!
//! A snapping viewport's snap points are its content's children (a virtual
//! list's rows), each where the viewport shows it at its [`SnapAlign`]. A
//! released flick does not coast where its speed carries it: the viewport
//! glides to the point nearest where the coast would have stopped, at the
//! speed the flick left it, never past the point. A paging viewport moves
//! one item per flick at most, a pager's way; a proximity one snaps only
//! near a point and otherwise coasts on. A slow release, a press that
//! caught the glide, and the end of a wheel's turning snap the same way, and
//! a layout that moves the item a viewport rests on keeps it there.
//!
//! The viewport reports the item it rests on, heads for, or shows nearest
//! ([`NodeStore::snap_index`], [`Notice::Snap`](crate::notice::Notice)).

use std::time::Duration;

use crate::adaptive::LayoutDirection;
use crate::component::{BuildCx, Handle, NodeStore};
use crate::layout::{Axis, OnAxis, Vec2};
use crate::node::NodeId;
use crate::scroll_motion::Glide;
use crate::timer::TimerRequest;

/// Where an item sits in its viewport when the viewport rests on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SnapAlign {
    /// Its start at the viewport's start.
    #[default]
    Start,
    /// Its middle at the viewport's middle.
    Center,
    /// Its end at the viewport's end.
    End,
}

/// How a viewport snaps.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ScrollSnap {
    /// Where an item sits when the viewport rests on it.
    pub align: SnapAlign,
    /// Whether a flick moves one item at most, as a pager's does, instead of
    /// to the item nearest where it would coast.
    pub paging: bool,
    /// Whether it snaps only when it would come to rest near an item, and
    /// otherwise coasts on.
    pub proximity: bool,
    /// How far in from the viewport's edges its items rest: a carousel's
    /// item rests past the inset its content starts with.
    pub inset: f32,
}

impl ScrollSnap {
    /// Snaps each item to the viewport's start.
    pub const START: ScrollSnap = ScrollSnap {
        align: SnapAlign::Start,
        paging: false,
        proximity: false,
        inset: 0.0,
    };
    /// Snaps each item to the viewport's middle.
    pub const CENTER: ScrollSnap = ScrollSnap {
        align: SnapAlign::Center,
        paging: false,
        proximity: false,
        inset: 0.0,
    };
    /// A pager's: one item per flick, each at the viewport's start.
    pub const PAGES: ScrollSnap = ScrollSnap {
        align: SnapAlign::Start,
        paging: true,
        proximity: false,
        inset: 0.0,
    };
}

/// How far a proximity snap reaches, as a share of the viewport's extent.
const PROXIMITY: f32 = 0.3;
/// How long after a wheel's last notch its viewport snaps.
const WHEEL_QUIET: Duration = Duration::from_millis(140);
/// The rate of the spring a snap glides along (1/s).
const SNAP_RATE: f64 = 16.0;
/// Within this of a point a viewport rests on it.
const ON_POINT: f32 = 0.5;

/// A viewport's snapping.
#[derive(Debug, Clone, Copy)]
struct Snap {
    viewport: NodeId,
    snap: ScrollSnap,
    /// The item it rests on, heads for, or shows nearest.
    index: Option<usize>,
    /// The offset it came to rest on its item at; `NaN` once anything else
    /// moved it.
    rest: f32,
    /// A release a virtual list resolves against its rows.
    aim: Option<SnapAim>,
    /// Bumped by each wheel notch: only the latest quiet timer snaps.
    wheel: u32,
    /// Whether its key handler hears the item it settles on change.
    tells: bool,
    /// The page dots that show its item.
    dots: Option<NodeId>,
    /// How many items it snaps to.
    count: usize,
}

/// What a virtual list's viewport waits on its list to snap to.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum SnapAim {
    /// A release, in distances from its start: where it was, where its
    /// coast would rest, and its speed.
    Release { from: f32, rest: f32, velocity: f32 },
    /// Row `0`, asked for.
    Item(usize),
}

/// The snapping viewports of a store.
#[derive(Debug, Default)]
pub(crate) struct Snaps {
    list: Vec<Snap>,
    /// The node under a press that caught a snapping glide: its lift snaps.
    caught: Option<NodeId>,
    /// Reused: the snap points of the viewport being snapped.
    points: Vec<(f32, usize)>,
    /// The viewports whose item changed, with the item, for their key
    /// handlers.
    told: Vec<(NodeId, usize)>,
}

impl Snaps {
    pub(crate) fn clear(&mut self) {
        self.list.clear();
        self.caught = None;
        self.told.clear();
    }

    /// Forgets the viewport of the slot `index`, freed or reused.
    pub(crate) fn forget(&mut self, index: u32) {
        self.list.retain(|s| s.viewport.index() != index);
    }

    fn find(&self, id: NodeId) -> Option<usize> {
        self.list.iter().position(|s| s.viewport == id)
    }
}

/// The point of `points` (distance, item) a viewport at `from` released at
/// `velocity` comes to rest on, its coast resting at `rest`: under a flick
/// a paging viewport's next one the flick's way, else the nearest to
/// `rest`; `None` when a proximity snap reaches none.
pub(crate) fn choose(
    points: &[(f32, usize)],
    snap: ScrollSnap,
    (from, rest, velocity): (f32, f32, f32),
    flick: f32,
    extent: f32,
) -> Option<(f32, usize)> {
    if snap.paging && velocity.abs() >= flick {
        let next = if velocity > 0.0 {
            points
                .iter()
                .find(|p| p.0 > from + ON_POINT)
                .or(points.last())
        } else {
            points
                .iter()
                .rev()
                .find(|p| p.0 < from - ON_POINT)
                .or(points.first())
        };
        return next.copied();
    }
    let nearest = points
        .iter()
        .copied()
        .min_by(|a, b| (a.0 - rest).abs().total_cmp(&(b.0 - rest).abs()))?;
    if snap.proximity && !snap.paging && (nearest.0 - rest).abs() > extent * PROXIMITY {
        return None;
    }
    Some(nearest)
}

/// Where a viewport `extent` long rests to show an item `start` from its
/// content's start and `size` long as `snap` says, held within `range`.
pub(crate) fn snap_point(snap: ScrollSnap, start: f32, size: f32, extent: f32, range: f32) -> f32 {
    // The inset narrows the room the item aligns in.
    let (start, extent) = (start - snap.inset, extent - snap.inset * 2.0);
    let at = match snap.align {
        SnapAlign::Start => start,
        SnapAlign::Center => start + (size - extent) * 0.5,
        SnapAlign::End => start + size - extent,
    };
    at.clamp(0.0, range.max(0.0))
}

impl BuildCx<'_> {
    /// Tell snapping viewport `handle`'s key handler each item it settles on
    /// ([`NodeStore::tell_snaps`]), or stop.
    pub fn tell_snaps(&mut self, handle: Handle, on: bool) {
        self.store.tell_snaps(handle.id(), on);
    }
}

impl NodeStore {
    /// Make viewport `id` snap as `snap` says, or not at all.
    pub fn set_scroll_snap(&mut self, id: NodeId, snap: Option<ScrollSnap>) {
        if self.scroll_axis(id).is_none() || !self.arena().is_live(id) {
            return;
        }
        let at = self.snaps.find(id);
        match (at, snap) {
            (Some(k), Some(snap)) => self.snaps.list[k].snap = snap,
            (None, Some(snap)) => self.snaps.list.push(Snap {
                viewport: id,
                snap,
                index: None,
                rest: f32::NAN,
                aim: None,
                wheel: 0,
                tells: false,
                dots: None,
                count: 0,
            }),
            (Some(k), None) => {
                self.snaps.list.swap_remove(k);
            }
            (None, None) => {}
        }
    }

    /// Tell snapping viewport `id`'s key handler each item it comes to rest
    /// on, heads for, or shows nearest, as it changes
    /// ([`EventCx::snapped`](crate::EventCx::snapped)), or stop.
    pub fn tell_snaps(&mut self, id: NodeId, on: bool) {
        if let Some(k) = self.snaps.find(id) {
            self.snaps.list[k].tells = on;
        }
    }

    /// Whether a snapping viewport's item changed for its key handler.
    #[inline]
    pub fn snaps_due(&self) -> bool {
        !self.snaps.told.is_empty()
    }

    /// Move the viewports whose item changed, with the item, into `out`.
    pub fn take_snap_changes(&mut self, out: &mut Vec<(NodeId, usize)>) {
        out.append(&mut self.snaps.told);
    }

    /// Entry `k` rests on, heads for, or shows `index`: a change from the
    /// item it showed first tells its key handler.
    fn set_snap_index(&mut self, k: usize, index: usize) {
        let s = &mut self.snaps.list[k];
        if s.index == Some(index) {
            return;
        }
        let first = s.index.is_none();
        s.index = Some(index);
        let (id, tells) = (s.viewport, s.tells);
        if tells && !first {
            self.snaps.told.retain(|&(n, _)| n != id);
            self.snaps.told.push((id, index));
        }
        self.show_snap(k);
    }

    /// Entry `k` snaps to `count` items.
    fn set_snap_count(&mut self, k: usize, count: usize) {
        if self.snaps.list[k].count != count {
            self.snaps.list[k].count = count;
            self.show_snap(k);
        }
    }

    /// Entry `k`'s page dots show its item and count.
    fn show_snap(&mut self, k: usize) {
        let s = self.snaps.list[k];
        if let Some(dots) = s.dots {
            self.show_page(dots, s.index.unwrap_or(0), s.count);
        }
    }

    /// Let `dots` show the item snapping viewport `id` rests on.
    pub(crate) fn link_page_dots(&mut self, id: NodeId, dots: NodeId) {
        if let Some(k) = self.snaps.find(id) {
            self.snaps.list[k].dots = Some(dots);
            self.show_snap(k);
        }
    }

    /// Virtual list viewport `id` snaps to `count` rows, its list says.
    pub(crate) fn note_snap_count(&mut self, id: NodeId, count: usize) {
        if let Some(k) = self.snaps.find(id) {
            self.set_snap_count(k, count);
        }
    }

    /// Bring snapping viewport `id` to rest on item `index`, gliding there.
    pub fn snap_to_item(&mut self, id: NodeId, index: usize) {
        let (Some(k), Some(axis)) = (self.snaps.find(id), self.scroll_axis(id)) else {
            return;
        };
        if self.snaps_rows(id) {
            self.snaps.list[k].aim = Some(SnapAim::Item(index));
            return;
        }
        let snap = self.snaps.list[k].snap;
        self.gather_points(id, axis, snap);
        if let Some(&(to, index)) = self.snaps.points.iter().find(|p| p.1 == index) {
            self.snap_to(id, to, index, 0.0);
        }
    }

    /// Snapping viewport `id`, along `axis`, takes key step `step`: an item
    /// on or back from the one it heads for, the item nearest a page on or
    /// back (the next page, paging), or its first or last item.
    pub(crate) fn snap_by_key(&mut self, id: NodeId, axis: Axis, step: crate::key_scroll::Step) {
        use crate::key_scroll::Step;
        let Some(k) = self.snaps.find(id) else {
            return;
        };
        let s = self.snaps.list[k];
        let index = s.index.unwrap_or(0);
        let last = s.count.saturating_sub(1);
        let item = match step {
            Step::Line(on) => Some(on),
            Step::Page(on) if s.snap.paging => Some(on),
            Step::Page(on) => {
                let page = self.page_step(id, axis);
                let from = match self.snap_aim_at(id, axis, index) {
                    Some(at) => at,
                    None => self.scroll_distance(id, axis),
                };
                let rest = from + if on { page } else { -page };
                self.snap_near(id, axis, from, rest);
                return;
            }
            Step::End(on) => {
                self.snap_to_item(id, if on { last.max(index) } else { 0 });
                return;
            }
        };
        if let Some(on) = item {
            let next = if on {
                index + 1
            } else {
                index.saturating_sub(1)
            };
            if next != index && (!on || s.count == 0 || next <= last) {
                self.snap_to_item(id, next);
            }
        }
    }

    /// Where snapping viewport `id` rests on item `index`, from its start
    /// along `axis`, when its content's children are its items.
    fn snap_aim_at(&mut self, id: NodeId, axis: Axis, index: usize) -> Option<f32> {
        if self.snaps_rows(id) {
            return None;
        }
        let snap = self.snaps.list[self.snaps.find(id)?].snap;
        self.gather_points(id, axis, snap);
        self.snaps.points.iter().find(|p| p.1 == index).map(|p| p.0)
    }

    /// Snapping viewport `id`, at `from` along `axis`, glides to the item
    /// nearest `rest`, past `from` the way `rest` lies.
    fn snap_near(&mut self, id: NodeId, axis: Axis, from: f32, rest: f32) {
        let Some(k) = self.snaps.find(id) else {
            return;
        };
        if self.snaps_rows(id) {
            self.snaps.list[k].aim = Some(SnapAim::Release {
                from,
                rest,
                velocity: 0.0,
            });
            return;
        }
        let snap = self.snaps.list[k].snap;
        self.gather_points(id, axis, snap);
        let on = rest > from;
        let pick = self
            .snaps
            .points
            .iter()
            .copied()
            .filter(|p| {
                if on {
                    p.0 > from + ON_POINT
                } else {
                    p.0 < from - ON_POINT
                }
            })
            .min_by(|a, b| (a.0 - rest).abs().total_cmp(&(b.0 - rest).abs()));
        if let Some((to, index)) = pick {
            self.snap_to(id, to, index, 0.0);
        }
    }

    /// How viewport `id` snaps, if it does.
    pub fn scroll_snap(&self, id: NodeId) -> Option<ScrollSnap> {
        self.snaps.find(id).map(|k| self.snaps.list[k].snap)
    }

    /// The item viewport `id` rests on, heads for, or shows nearest.
    pub fn snap_index(&self, id: NodeId) -> Option<usize> {
        self.snaps.find(id).and_then(|k| self.snaps.list[k].index)
    }

    /// Whether viewport `id` snaps.
    #[inline]
    pub(crate) fn snaps(&self, id: NodeId) -> bool {
        !self.snaps.list.is_empty() && self.snaps.find(id).is_some()
    }

    /// Whether viewport `id`'s content is a virtual list's canvas, whose rows
    /// the list knows.
    fn snaps_rows(&self, id: NodeId) -> bool {
        self.content_of(id).is_some_and(|c| {
            matches!(
                self.layout_input(c),
                crate::layout::LayoutInput::AbsoluteRows { .. }
            )
        })
    }

    /// The content child of viewport `id`, past its chrome.
    fn content_of(&self, id: NodeId) -> Option<NodeId> {
        self.arena().children(id).find(|&c| !self.is_chrome(c))
    }

    /// The sign that turns viewport `id`'s offsets along `axis` into
    /// distances from its start: a row read right to left runs negative.
    fn snap_sign(&self, id: NodeId, axis: Axis) -> f32 {
        if axis == Axis::Row && self.direction_at(id.index()) == LayoutDirection::Rtl {
            -1.0
        } else {
            1.0
        }
    }

    /// Gather viewport `id`'s snap points from its content's children into
    /// the reused buffer, by distance.
    fn gather_points(&mut self, id: NodeId, axis: Axis, snap: ScrollSnap) {
        let mut points = std::mem::take(&mut self.snaps.points);
        points.clear();
        if let Some(content) = self.content_of(id) {
            let rtl = self.snap_sign(id, axis) < 0.0;
            let extent = self.bounds_main(id, axis);
            let range = self.scroll_range(id, axis);
            let span = |b: viso_render::Rect| match axis {
                Axis::Column => (b.y, b.y + b.h),
                Axis::Row => (b.x, b.x + b.w),
            };
            let (c0, c1) = span(self.bounds(content));
            for (k, child) in self.arena().children(content).enumerate() {
                if self.is_chrome(child) || self.is_overlay(child) {
                    continue;
                }
                let (s0, s1) = span(self.bounds(child));
                let start = if rtl { c1 - s1 } else { s0 - c0 };
                points.push((snap_point(snap, start, s1 - s0, extent, range), k));
            }
        }
        points.sort_by(|a, b| a.0.total_cmp(&b.0));
        self.snaps.points = points;
    }

    /// Viewport `id`, released at `velocity` (pt/s, signed as its offset
    /// moves), comes to rest on a snap point: it glides there at the speed
    /// it left with, or, its content a virtual list's rows, has its list
    /// aim it. Returns `false` when a proximity snap reaches no point, to
    /// coast on.
    pub(crate) fn release_snap(&mut self, id: NodeId, velocity: f32) -> bool {
        let (Some(k), Some(axis)) = (self.snaps.find(id), self.scroll_axis(id)) else {
            return false;
        };
        let sign = self.snap_sign(id, axis);
        let from = self.scroll_distance(id, axis);
        let speed = velocity * sign;
        let rest = from + self.coast_travel(speed);
        let aim = SnapAim::Release {
            from,
            rest,
            velocity: speed,
        };
        if self.snaps_rows(id) {
            self.snaps.list[k].aim = Some(aim);
            self.snaps.list[k].rest = f32::NAN;
            return true;
        }
        let snap = self.snaps.list[k].snap;
        self.gather_points(id, axis, snap);
        let extent = self.bounds_main(id, axis);
        let flick = self.scroll_physics().min_velocity;
        let Some((to, index)) =
            choose(&self.snaps.points, snap, (from, rest, speed), flick, extent)
        else {
            return false;
        };
        self.snap_to(id, to, index, speed);
        true
    }

    /// Glide viewport `id` to `distance` from its start along its axis,
    /// resting on item `index`, from a speed of `velocity` (pt/s, along the
    /// distance) that never carries it past.
    pub(crate) fn snap_to(&mut self, id: NodeId, distance: f32, index: usize, velocity: f32) {
        let (Some(k), Some(axis)) = (self.snaps.find(id), self.scroll_axis(id)) else {
            return;
        };
        self.set_snap_index(k, index);
        let sign = self.snap_sign(id, axis);
        let now = self.scroll(id);
        let target = with_axis(now, axis, distance * sign);
        let e = f64::from((self.scroll_distance(id, axis) - distance) * sign);
        if e.abs() < f64::from(ON_POINT) {
            self.set_scroll(id, target);
            self.snaps.list[k].rest = target.on(axis);
            self.motions.glides.retain(|g| g.viewport != id);
            return;
        }
        self.snaps.list[k].rest = f32::NAN;
        // Toward the point no faster than the spring carries it without
        // passing it.
        let mut v = f64::from(velocity * sign);
        if v * e < 0.0 && v.abs() > SNAP_RATE * e.abs() {
            v = -SNAP_RATE * e;
        }
        self.stop_coast(id);
        let mut velocity = [0.0; 2];
        velocity[usize::from(axis == Axis::Column)] = v;
        self.motions.glides.push(Glide {
            viewport: id,
            target,
            velocity,
            rate: SNAP_RATE,
        });
    }

    /// A glide of viewport `id` arrived: a snapping viewport rests on its
    /// item.
    pub(crate) fn snap_arrived(&mut self, id: NodeId) {
        if let (Some(k), Some(axis)) = (self.snaps.find(id), self.scroll_axis(id)) {
            self.snaps.list[k].rest = self.scroll(id).on(axis);
        }
    }

    /// Whether a release waits on its list to aim it.
    #[inline]
    pub(crate) fn snap_aims(&self) -> bool {
        self.snaps.list.iter().any(|s| s.aim.is_some())
    }

    /// The release a virtual list's viewport `id` waits on its list to aim.
    pub(crate) fn take_snap_aim(&mut self, id: NodeId) -> Option<SnapAim> {
        let k = self.snaps.find(id)?;
        self.snaps.list[k].aim.take()
    }

    /// The item virtual list viewport `id` shows nearest, its list says.
    pub(crate) fn note_snap_index(&mut self, id: NodeId, index: usize) {
        if let Some(k) = self.snaps.find(id)
            && !self.snap_moving(id)
        {
            self.set_snap_index(k, index);
        }
    }

    /// Whether viewport `id` coasts, glides or springs back from a pull.
    fn snap_moving(&self, id: NodeId) -> bool {
        self.is_coasting(id) || self.is_gliding(id) || self.overscroll(id) != 0.0
    }

    /// Every snapping viewport at or above `from` that stands still snaps:
    /// a release too slow to coast, a lift after a catch.
    pub(crate) fn settle_snaps_over(&mut self, from: NodeId) {
        if self.snaps.list.is_empty() {
            return;
        }
        let mut node = Some(from);
        while let Some(n) = node {
            if self.snaps(n) && !self.snap_moving(n) {
                self.release_snap(n, 0.0);
            }
            node = self.arena().parent(n);
        }
    }

    /// Trackpad fingers lifted over `target` at `velocity` (pt/s, as the
    /// offsets move) while the system coasts on: the innermost snapping
    /// viewport enclosing it snaps instead. Returns it, which the system's
    /// coast moves no more.
    pub(crate) fn snap_lift(&mut self, target: NodeId, velocity: Vec2) -> Option<NodeId> {
        if self.snaps.list.is_empty() {
            return None;
        }
        let mut node = Some(target);
        while let Some(n) = node {
            if self.snaps(n)
                && let Some(axis) = self.scroll_axis(n)
            {
                return self.release_snap(n, velocity.on(axis)).then_some(n);
            }
            node = self.arena().parent(n);
        }
        None
    }

    /// A press over `target` caught a snapping viewport's glide: its lift
    /// snaps it.
    pub(crate) fn note_snap_catch(&mut self, target: NodeId) {
        if !self.snaps.list.is_empty() {
            self.snaps.caught = Some(target);
        }
    }

    /// The press that caught a glide lifted.
    pub(crate) fn settle_caught_snap(&mut self) {
        if let Some(target) = self.snaps.caught.take() {
            self.settle_snaps_over(target);
        }
    }

    /// A wheel notch scrolled over `target`: the snapping viewports enclosing
    /// it snap once the wheel stops turning.
    pub(crate) fn snap_after_wheel(&mut self, target: NodeId) {
        if self.snaps.list.is_empty() {
            return;
        }
        let mut node = Some(target);
        while let Some(n) = node {
            if let Some(k) = self.snaps.find(n) {
                let wheel = self.snaps.list[k].wheel.wrapping_add(1);
                self.snaps.list[k].wheel = wheel;
                self.snaps.list[k].rest = f32::NAN;
                self.queue_timer(TimerRequest::new(n, WHEEL_QUIET, move |store| {
                    if store
                        .snaps
                        .find(n)
                        .is_some_and(|k| store.snaps.list[k].wheel == wheel)
                        && !store.snap_moving(n)
                    {
                        store.release_snap(n, 0.0);
                    }
                }));
            }
            node = self.arena().parent(n);
        }
    }

    /// After a layout: a viewport resting on its item follows the item where
    /// the layout moved it; one anything else moved rests no more, and
    /// reports the item it shows nearest.
    pub(crate) fn sync_snaps(&mut self) {
        for k in 0..self.snaps.list.len() {
            let s = self.snaps.list[k];
            let id = s.viewport;
            let Some(axis) = self.scroll_axis(id).filter(|_| self.arena().is_live(id)) else {
                continue;
            };
            if self.snaps_rows(id) || self.snap_moving(id) {
                continue;
            }
            let offset = self.scroll(id).on(axis);
            let rests = !s.rest.is_nan() && (offset - s.rest).abs() < ON_POINT;
            self.gather_points(id, axis, s.snap);
            self.set_snap_count(k, self.snaps.points.len());
            let from = self.scroll_distance(id, axis);
            let pick = match s.index.filter(|_| rests) {
                Some(index) => self.snaps.points.iter().find(|p| p.1 == index).copied(),
                None => self
                    .snaps
                    .points
                    .iter()
                    .copied()
                    .min_by(|a, b| (a.0 - from).abs().total_cmp(&(b.0 - from).abs())),
            };
            let Some((to, index)) = pick else {
                continue;
            };
            self.set_snap_index(k, index);
            if rests && (to - from).abs() >= ON_POINT {
                let sign = self.snap_sign(id, axis);
                let target = with_axis(self.scroll(id), axis, to * sign);
                self.set_scroll(id, target);
                self.snaps.list[k].rest = target.on(axis);
            } else if !rests {
                self.snaps.list[k].rest = if (to - from).abs() < ON_POINT {
                    offset
                } else {
                    f32::NAN
                };
            }
        }
    }
}

fn with_axis(v: Vec2, axis: Axis, value: f32) -> Vec2 {
    match axis {
        Axis::Row => Vec2 { x: value, y: v.y },
        Axis::Column => Vec2 { x: v.x, y: value },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::component::{BuildCx, FlexStyle, LeafStyle, ScrollStyle};
    use crate::input::{ScrollEvent, ScrollPhase, ScrollRouter};
    use crate::layout::{Length, Size};
    use crate::scroll_motion::ScrollPhysics;
    use crate::style::BoxStyle;
    use viso_render::Rect;

    const STEP: Duration = Duration::from_millis(16);

    /// A 100-wide viewport over five 100-wide items, snapping as `snap`
    /// says, on Apple's coast.
    fn strip(snap: ScrollSnap) -> (NodeStore, NodeId, Vec<NodeId>) {
        strip_with(snap, |_, _| {})
    }

    /// The strip, with `more` building beside its viewport.
    fn strip_with(
        snap: ScrollSnap,
        more: impl FnOnce(&mut BuildCx<'_>, crate::component::Handle),
    ) -> (NodeStore, NodeId, Vec<NodeId>) {
        let mut store = NodeStore::new();
        store.set_scroll_physics(ScrollPhysics::APPLE);
        let mut items = Vec::new();
        let mut cx = BuildCx::new(&mut store);
        let viewport = cx.scroll(
            ScrollStyle {
                axis: Axis::Row,
                size: Size::fixed(100.0, 50.0),
                snap: Some(snap),
                ..ScrollStyle::default()
            },
            |cx| {
                cx.flex(
                    FlexStyle {
                        axis: Axis::Row,
                        ..FlexStyle::default()
                    },
                    |cx| {
                        for _ in 0..5 {
                            let item = cx.leaf(LeafStyle {
                                size: Size::fixed(100.0, 50.0),
                                style: BoxStyle::NONE,
                            });
                            items.push(item.id());
                        }
                    },
                );
            },
        );
        more(&mut cx, viewport);
        let viewport = viewport.id();
        lay_out(&mut store, viewport);
        (store, viewport, items)
    }

    fn lay_out(store: &mut NodeStore, viewport: NodeId) {
        let surface = Rect {
            x: 0.0,
            y: 0.0,
            w: 100.0,
            h: 50.0,
        };
        store.relayout_dirty(viewport, surface, &mut Vec::new(), &mut Vec::new());
        if store.bounds(viewport).w == 0.0 {
            store.layout(viewport, surface, &mut Vec::new());
        }
    }

    /// Frames until the viewport stands still; the offsets it showed.
    fn settle(store: &mut NodeStore, viewport: NodeId) -> Vec<f32> {
        let mut shown = Vec::new();
        for _ in 0..240 {
            if !store.is_scroll_moving() {
                break;
            }
            store.tick_scroll_motion(STEP);
            lay_out(store, viewport);
            shown.push(store.scroll(viewport).x);
        }
        shown
    }

    fn at(store: &mut NodeStore, viewport: NodeId, x: f32) {
        store.set_scroll(viewport, Vec2 { x, y: 0.0 });
        lay_out(store, viewport);
    }

    #[test]
    fn a_flick_glides_to_the_item_its_coast_nears_never_past_it() {
        let (mut store, viewport, _) = strip(ScrollSnap::START);
        at(&mut store, viewport, 130.0);
        // Apple's coast from 300 pt/s rests about 150 on: at 280.
        store.fling(viewport, 300.0);
        assert!(store.is_gliding(viewport) && !store.is_coasting(viewport));
        let shown = settle(&mut store, viewport);
        assert_eq!(store.scroll(viewport).x, 300.0);
        assert!(shown[0] > 130.0, "it carries on the way it went");
        assert!(shown.iter().all(|&x| x <= 300.0), "never past: {shown:?}");
        assert_eq!(store.snap_index(viewport), Some(3));
    }

    #[test]
    fn a_pager_moves_one_item_per_flick_and_a_slow_release_settles_nearest() {
        let (mut store, viewport, items) = strip(ScrollSnap::PAGES);
        at(&mut store, viewport, 130.0);
        store.fling(viewport, 5000.0);
        settle(&mut store, viewport);
        assert_eq!(store.scroll(viewport).x, 200.0, "one item on, however hard");
        at(&mut store, viewport, 270.0);
        store.fling(viewport, -80.0);
        settle(&mut store, viewport);
        assert_eq!(
            store.scroll(viewport).x,
            200.0,
            "back the way it was flicked"
        );
        at(&mut store, viewport, 260.0);
        store.settle_snaps_over(items[2]);
        settle(&mut store, viewport);
        assert_eq!(store.scroll(viewport).x, 300.0, "past halfway, the next");
        assert_eq!(store.snap_index(viewport), Some(3));
    }

    #[test]
    fn a_proximity_snap_lets_a_coast_far_from_any_item_run_on() {
        let (mut store, viewport, _) = strip(ScrollSnap {
            proximity: true,
            ..ScrollSnap::START
        });
        at(&mut store, viewport, 0.0);
        // Resting about 150 on: half an item from either.
        store.fling(viewport, 300.0);
        assert!(store.is_coasting(viewport));
        store.stop_coast(viewport);
        at(&mut store, viewport, 90.0);
        store.settle_snaps_over(viewport);
        settle(&mut store, viewport);
        assert_eq!(store.scroll(viewport).x, 100.0, "near one it snaps");
    }

    #[test]
    fn a_wheel_snaps_once_it_stops_turning() {
        let (mut store, viewport, _) = strip(ScrollSnap::CENTER);
        for _ in 0..3 {
            let ev = ScrollEvent {
                x: 50.0,
                y: 25.0,
                delta_x: 40.0,
                delta_y: 0.0,
                modifiers: Default::default(),
                phase: ScrollPhase::None,
            };
            ScrollRouter::route(&mut store, viewport, ev);
        }
        assert_eq!(store.scroll(viewport).x, 120.0);
        let mut timers = Vec::new();
        store.take_timer_requests(&mut timers);
        assert_eq!(timers.len(), 3);
        for timer in timers {
            (timer.on_fire)(&mut store);
        }
        settle(&mut store, viewport);
        assert_eq!(
            store.scroll(viewport).x,
            100.0,
            "only the last notch's timer snaps"
        );
    }

    #[test]
    fn a_layout_keeps_the_item_a_viewport_rests_on() {
        let (mut store, viewport, items) = strip(ScrollSnap::START);
        at(&mut store, viewport, 180.0);
        store.settle_snaps_over(viewport);
        settle(&mut store, viewport);
        assert_eq!(store.scroll(viewport).x, 200.0);
        for &item in &items {
            store.set_fixed_size(
                item,
                Size {
                    width: Length::Fixed(150.0),
                    height: Length::Fixed(50.0),
                },
            );
        }
        lay_out(&mut store, viewport);
        assert_eq!(store.scroll(viewport).x, 300.0, "item 2 still at the start");
        // Moved by anything else, it no longer rests there.
        at(&mut store, viewport, 320.0);
        assert_eq!(store.snap_index(viewport), Some(2));
    }

    #[test]
    fn asked_for_an_item_it_glides_there_and_its_dots_follow() {
        let mut dots = None;
        let (mut store, viewport, _) = strip_with(ScrollSnap::START, |cx, viewport| {
            dots = Some(cx.page_dots(viewport, Default::default()).id());
        });
        let dots = dots.unwrap();
        let shown = store.page_dots(dots).unwrap();
        assert_eq!((shown.count, shown.current), (5, 0));
        store.snap_to_item(viewport, 3);
        assert_eq!(
            store.page_dots(dots).unwrap().current,
            3,
            "lit as it heads there"
        );
        settle(&mut store, viewport);
        assert_eq!(store.scroll(viewport).x, 300.0);
        assert_eq!(store.snap_index(viewport), Some(3));
        let state = store.semantic_state(dots).unwrap();
        assert_eq!((state.value, state.range), (Some(4.0), Some((1.0, 5.0))));
        store.snap_to_item(viewport, 9);
        assert!(!store.is_scroll_moving(), "no such item");
    }

    #[test]
    fn choosing_reads_the_flick_and_the_coast() {
        let points = [(0.0, 0), (100.0, 1), (200.0, 2)];
        let pick = |snap, from, rest, v| choose(&points, snap, (from, rest, v), 50.0, 100.0);
        assert_eq!(
            pick(ScrollSnap::START, 10.0, 160.0, 300.0),
            Some((200.0, 2))
        );
        assert_eq!(
            pick(ScrollSnap::PAGES, 10.0, 160.0, 300.0),
            Some((100.0, 1))
        );
        assert_eq!(
            pick(ScrollSnap::PAGES, 200.0, 900.0, 3000.0),
            Some((200.0, 2))
        );
        assert_eq!(pick(ScrollSnap::PAGES, 140.0, 140.0, 0.0), Some((100.0, 1)));
        assert_eq!(
            snap_point(ScrollSnap::CENTER, 100.0, 50.0, 100.0, 400.0),
            75.0
        );
        let end = ScrollSnap {
            align: SnapAlign::End,
            ..ScrollSnap::START
        };
        assert_eq!(snap_point(end, 0.0, 50.0, 100.0, 400.0), 0.0);
        let inset = ScrollSnap {
            inset: 16.0,
            ..ScrollSnap::START
        };
        assert_eq!(snap_point(inset, 116.0, 80.0, 100.0, 400.0), 100.0);
        assert_eq!(
            snap_point(ScrollSnap { inset: 16.0, ..end }, 116.0, 80.0, 100.0, 400.0),
            112.0
        );
    }
}
