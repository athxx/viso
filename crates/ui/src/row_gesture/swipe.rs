//! Swipe rows: a row whose content a drag across slides aside, showing the
//! actions behind it.
//!
//! A swipe row ([`NodeStore::set_swipe_row`]) names its content and, for
//! either edge, a panel of actions. A panel fills the row and waits past
//! its edge, its actions packed against the content's side; as the content
//! slides off one way the panel on the other slides in after it, its edge
//! held to the content's. The row takes drags across alone, so a drag up or
//! down is the list's: the content follows the pointer as far as the
//! actions reach and eases past them. Let go past half the actions, or
//! flicked open, it springs open on them; else it springs shut. An edge
//! whose first action runs on a full swipe lets the content travel on, and
//! one let go past the mark carries off and tells the row
//! ([`RowGesture::Swiped`]) before it springs back. A trackpad's fingers
//! swiping across it slide it the same way, and a wheel turned elsewhere
//! closes it.
//!
//! One row stands open at a time: a drag on another closes it, and a press
//! anywhere but on its actions does, the press clicking nothing within the
//! row's list; a press on one of its actions shuts it as it lifts. The focus coming to one of its actions opens the row on them
//! and leaving them closes it again; Escape closes the row the focus is in.
//! Leading is the left edge in a row read left to right, the right one read
//! right to left. Under reduced motion it moves at once.

use core::time::Duration;

use crate::adaptive::LayoutDirection;
use crate::component::NodeStore;
use crate::drag::{DragPhase, DragSample, SWIPE_SPEED};
use crate::input::{Key, KeyEvent, ScrollPhase};
use crate::layout::{Alignment2D, Justify, Placement, Size, StackPlacement, Vec2};
use crate::node::NodeId;
use crate::row_gesture::RowGesture;
use crate::scroll_motion::{MAX_STEP, SPRING, VelocityTracker};
use crate::viewer::{eased, uneased};

/// The share of the row's width a full swipe must carry the content past.
const FULL_SHARE: f32 = 0.55;
/// How far past its actions at least.
const FULL_PAST: f32 = 48.0;
/// How far past its actions the content eases at most, as a share of the
/// row's width, on an edge with no full swipe.
const STRETCH_SHARE: f32 = 0.15;
/// How long a full swipe holds the content off before it springs back.
const FULL_HOLD: f64 = 0.15;
/// Within this many points of its rest, and slower than [`SETTLE_STILL`],
/// a spring arrived.
pub(super) const SETTLE_NEAR: f64 = 0.5;
pub(super) const SETTLE_STILL: f64 = 10.0;
/// How much more across than along a trackpad's swipe must start to be the
/// row's.
const PAD_ACROSS: f32 = 1.5;

/// An edge of a swipe row, in the way it reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwipeEdge {
    /// Where its lines start: the left in a row read left to right.
    Leading,
    /// Where they end.
    Trailing,
}

/// A swipe row's parts ([`NodeStore::set_swipe_row`]): children of the row,
/// the panels below the content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SwipeParts {
    /// What slides.
    pub content: NodeId,
    /// The actions it shows slid toward its trailing edge, packed against
    /// its end; `None` for none.
    pub leading: Option<NodeId>,
    /// The actions it shows slid toward its leading edge, packed against
    /// its start.
    pub trailing: Option<NodeId>,
    /// Whether a full swipe runs the first leading action.
    pub full_leading: bool,
    /// Whether a full swipe runs the first trailing action.
    pub full_trailing: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Run {
    Still,
    /// Springing to `to` at `v` (pt/s).
    Spring {
        to: f32,
        v: f64,
    },
    /// Held carried off for `left` seconds more, then springing back.
    Hold {
        left: f64,
    },
}

#[derive(Debug, Clone, Copy)]
struct Swipe {
    node: NodeId,
    parts: SwipeParts,
    /// How far the content stands slid, in window x: positive to the right.
    offset: f32,
    /// The travel a drag or a trackpad gave it, before the ease past the
    /// actions.
    raw: f32,
    run: Run,
    /// The edge a full swipe under way carries it off toward.
    full: Option<SwipeEdge>,
    /// Opened as the focus came to its actions.
    by_focus: bool,
    /// The pointer pressing one of its actions, which shuts it once lifted.
    acting: Option<crate::input::PointerId>,
}

impl Swipe {
    /// Where it heads: a spring's rest, else where it stands.
    fn aim(&self) -> f32 {
        match self.run {
            Run::Spring { to, .. } => to,
            Run::Hold { .. } => self.offset,
            Run::Still => self.offset,
        }
    }

    fn moving(&self) -> bool {
        self.run != Run::Still
    }
}

/// Who a trackpad's swipe under way belongs to.
#[derive(Debug, Clone, Copy)]
enum Pad {
    /// Undecided until it moves.
    Fresh,
    /// The row `node`, which the fingers slide.
    Row(NodeId),
    /// Not a row's.
    Elsewhere,
}

/// A window's swipe rows.
#[derive(Debug, Default)]
pub(crate) struct Swipes {
    list: Vec<Swipe>,
    /// The nodes marked as an edge's panel of the row they are a child of.
    panels: Vec<(NodeId, SwipeEdge)>,
    /// The rows made of their own children as they come and go, with the
    /// edges a full swipe runs on.
    auto: Vec<(NodeId, bool, bool)>,
    pad: Option<Pad>,
    /// The travel a trackpad's swipe of a row summed, and its speed.
    pad_at: f32,
    pad_track: VelocityTracker,
}

impl Swipes {
    pub(crate) fn clear(&mut self) {
        self.list.clear();
        self.panels.clear();
        self.auto.clear();
        self.pad = None;
    }

    pub(crate) fn moving(&self) -> bool {
        self.list.iter().any(Swipe::moving)
    }
}

impl NodeStore {
    /// Make node `id` a swipe row of `parts`, or no longer one with
    /// `None`: it takes drags across, and its panels wait past its edges. A
    /// row given new parts keeps its content where it stands, shut if the
    /// edge it is open on lost its panel.
    pub fn set_swipe_row(&mut self, id: NodeId, parts: Option<SwipeParts>) {
        let at = self.swipe_index(id);
        if let Some(i) = at {
            let old = self.row_gestures.swipes.list[i].parts;
            if parts != Some(old) {
                for part in [Some(old.content), old.leading, old.trailing]
                    .into_iter()
                    .flatten()
                {
                    self.set_translate(part, Vec2::ZERO);
                }
            }
        }
        let Some(parts) = parts.filter(|_| self.arena().is_live(id)) else {
            if let Some(i) = at {
                self.row_gestures.swipes.list.swap_remove(i);
            }
            self.set_draggable(id, false);
            self.set_drag_axis(id, None);
            return;
        };
        let i = match at {
            Some(i) => {
                self.row_gestures.swipes.list[i].parts = parts;
                i
            }
            None => {
                self.set_draggable(id, true);
                self.set_drag_axis(id, Some(crate::layout::Axis::Row));
                self.row_gestures.swipes.list.push(Swipe {
                    node: id,
                    parts,
                    offset: 0.0,
                    raw: 0.0,
                    run: Run::Still,
                    full: None,
                    by_focus: false,
                    acting: None,
                });
                self.row_gestures.swipes.list.len() - 1
            }
        };
        let s = self.row_gestures.swipes.list[i];
        if s.offset != 0.0 && self.swipe_reach(i, self.swipe_edge_of(i, s.offset)) == 0.0 {
            let s = &mut self.row_gestures.swipes.list[i];
            s.offset = 0.0;
            s.run = Run::Still;
        }
        self.show_swipe(i);
    }

    /// Mark node `id` as the panel of `edge`'s actions of the swipe row it
    /// is a child of, or no longer one with `None`; a row already set up
    /// takes it among its parts ([`swipe_parts_of`](Self::swipe_parts_of)).
    /// A panel lies below the row's content filling the row, and a flex one
    /// packs its actions against the content's side.
    pub fn set_swipe_panel(&mut self, id: NodeId, edge: Option<SwipeEdge>) {
        let panels = &mut self.row_gestures.swipes.panels;
        let was = panels.iter().find(|&&(n, _)| n == id).map(|&(_, e)| e);
        if was == edge {
            return;
        }
        let mut panels = std::mem::take(panels);
        panels.retain(|&(n, _)| n != id && self.arena().is_live(n));
        if let Some(edge) = edge.filter(|_| self.arena().is_live(id)) {
            panels.push((id, edge));
        }
        self.row_gestures.swipes.panels = panels;
        if let Some(row) = self.arena().parent(id) {
            self.refit_swipe_row(row);
        }
        if let Some(edge) = edge {
            self.set_placement(
                id,
                Placement::Stack(StackPlacement {
                    align: Some(Alignment2D::TopStart),
                    layer: -1,
                }),
            );
            self.set_fixed_size(id, Size::fill());
            if let Some((axis, _, align)) = self.flex_arrangement(id) {
                let justify = match edge {
                    SwipeEdge::Leading => Justify::End,
                    SwipeEdge::Trailing => Justify::Start,
                };
                self.set_flex_arrangement(id, (axis, justify, align));
            }
        }
    }

    /// Make node `id` a swipe row of its own children
    /// ([`swipe_parts_of`](Self::swipe_parts_of)), a full swipe running on
    /// the leading and the trailing edge as `full` says, re-reading them as
    /// they come and go; or no longer one with `None`. Until it has content
    /// it is none.
    pub fn set_swipe_row_of_children(&mut self, id: NodeId, full: Option<(bool, bool)>) {
        let mut auto = std::mem::take(&mut self.row_gestures.swipes.auto);
        auto.retain(|&(n, ..)| n != id && self.arena().is_live(n));
        if let Some((leading, trailing)) = full.filter(|_| self.arena().is_live(id)) {
            auto.push((id, leading, trailing));
        }
        self.row_gestures.swipes.auto = auto;
        if full.is_some() {
            self.refit_swipe_row(id);
        } else {
            self.set_swipe_row(id, None);
        }
    }

    /// Re-reads the parts of row `id` if it is made of its own children.
    fn refit_swipe_row(&mut self, id: NodeId) {
        let Some(&(_, leading, trailing)) = self
            .row_gestures
            .swipes
            .auto
            .iter()
            .find(|&&(n, ..)| n == id)
        else {
            return;
        };
        let parts = self.swipe_parts_of(id, leading, trailing);
        let now = self
            .swipe_index(id)
            .map(|i| self.row_gestures.swipes.list[i].parts);
        if parts != now {
            self.set_swipe_row(id, parts);
        }
    }

    /// The edge whose panel node `id` is marked as.
    pub fn swipe_panel_edge(&self, id: NodeId) -> Option<SwipeEdge> {
        self.row_gestures
            .swipes
            .panels
            .iter()
            .find(|&&(n, _)| n == id)
            .map(|&(_, e)| e)
    }

    /// The parts node `id`'s children make as a swipe row: the first child
    /// marked as each edge's panel ([`set_swipe_panel`](Self::set_swipe_panel)),
    /// and as its content the first other child; `None` without one.
    pub fn swipe_parts_of(
        &self,
        id: NodeId,
        full_leading: bool,
        full_trailing: bool,
    ) -> Option<SwipeParts> {
        let (mut content, mut leading, mut trailing) = (None, None, None);
        for child in self.arena().children(id) {
            if self.is_chrome(child) {
                continue;
            }
            match self.swipe_panel_edge(child) {
                Some(SwipeEdge::Leading) => leading = leading.or(Some(child)),
                Some(SwipeEdge::Trailing) => trailing = trailing.or(Some(child)),
                None => content = content.or(Some(child)),
            }
        }
        Some(SwipeParts {
            content: content?,
            leading,
            trailing,
            full_leading,
            full_trailing,
        })
    }

    /// Whether node `id` is a swipe row.
    pub fn is_swipe_row(&self, id: NodeId) -> bool {
        self.swipe_index(id).is_some()
    }

    /// How far swipe row `id`'s content stands slid, in window x.
    pub fn swipe_offset(&self, id: NodeId) -> Option<f32> {
        self.swipe_index(id)
            .map(|i| self.row_gestures.swipes.list[i].offset)
    }

    /// The edge whose actions swipe row `id` shows or springs to show.
    pub fn swipe_open(&self, id: NodeId) -> Option<SwipeEdge> {
        let i = self.swipe_index(id)?;
        let aim = self.row_gestures.swipes.list[i].aim();
        (aim != 0.0).then(|| self.swipe_edge_of(i, aim))
    }

    /// Open swipe row `id` on `edge`'s actions, or shut it with `None`,
    /// springing there (at once under reduced motion). Opening one shuts
    /// the others.
    pub fn open_swipe_row(&mut self, id: NodeId, edge: Option<SwipeEdge>) {
        let Some(i) = self.swipe_index(id) else {
            return;
        };
        let to = match edge {
            Some(edge) => {
                self.shut_swipes_but(Some(id));
                self.swipe_sign(i, edge) * self.swipe_reach(i, edge)
            }
            None => 0.0,
        };
        let s = &mut self.row_gestures.swipes.list[i];
        s.full = None;
        if edge.is_none() {
            s.by_focus = false;
        }
        self.spring_swipe(i, to, 0.0);
    }

    fn swipe_index(&self, id: NodeId) -> Option<usize> {
        let list = &self.row_gestures.swipes.list;
        if list.is_empty() {
            return None;
        }
        list.iter().position(|s| s.node == id)
    }

    /// The swipe row at or above `node`.
    fn swipe_over(&self, node: NodeId) -> Option<usize> {
        if self.row_gestures.swipes.list.is_empty() {
            return None;
        }
        let mut at = Some(node);
        while let Some(n) = at {
            if let Some(i) = self.swipe_index(n) {
                return Some(i);
            }
            at = self.arena().parent(n);
        }
        None
    }

    /// The way the content slides to show `edge`'s actions: `1` to the right.
    fn swipe_sign(&self, i: usize, edge: SwipeEdge) -> f32 {
        let node = self.row_gestures.swipes.list[i].node;
        let rtl = self.direction_at(node.index()) == LayoutDirection::Rtl;
        match (edge, rtl) {
            (SwipeEdge::Leading, false) | (SwipeEdge::Trailing, true) => 1.0,
            _ => -1.0,
        }
    }

    /// The edge whose actions the content slid `offset` shows.
    fn swipe_edge_of(&self, i: usize, offset: f32) -> SwipeEdge {
        if offset * self.swipe_sign(i, SwipeEdge::Leading) > 0.0 {
            SwipeEdge::Leading
        } else {
            SwipeEdge::Trailing
        }
    }

    fn swipe_panel(&self, i: usize, edge: SwipeEdge) -> Option<NodeId> {
        let parts = self.row_gestures.swipes.list[i].parts;
        match edge {
            SwipeEdge::Leading => parts.leading,
            SwipeEdge::Trailing => parts.trailing,
        }
        .filter(|&p| self.arena().is_live(p))
    }

    /// How far `edge`'s actions reach across: the span of its panel's
    /// children, `0` for none.
    fn swipe_reach(&self, i: usize, edge: SwipeEdge) -> f32 {
        let Some(panel) = self.swipe_panel(i, edge) else {
            return 0.0;
        };
        let (mut lo, mut hi) = (f32::INFINITY, f32::NEG_INFINITY);
        for child in self.arena().children(panel) {
            if self.is_chrome(child) {
                continue;
            }
            let b = self.bounds(child);
            lo = lo.min(b.x);
            hi = hi.max(b.x + b.w);
        }
        if hi > lo {
            (hi - lo).min(self.swipe_width(i))
        } else {
            0.0
        }
    }

    fn swipe_width(&self, i: usize) -> f32 {
        self.bounds(self.row_gestures.swipes.list[i].node).w
    }

    /// Whether a full swipe toward `edge` runs its first action.
    fn swipe_full(&self, i: usize, edge: SwipeEdge) -> bool {
        let parts = self.row_gestures.swipes.list[i].parts;
        let full = match edge {
            SwipeEdge::Leading => parts.full_leading,
            SwipeEdge::Trailing => parts.full_trailing,
        };
        full && self.swipe_reach(i, edge) > 0.0
    }

    /// How far a full swipe must carry the content.
    fn swipe_mark(&self, i: usize, edge: SwipeEdge) -> f32 {
        let w = self.swipe_width(i);
        (w * FULL_SHARE)
            .max(self.swipe_reach(i, edge) + FULL_PAST)
            .min(w * 0.95)
    }

    /// Where the content shows for a travel of `raw`: as far as the actions
    /// reach, eased past them, on to the row's width where a full swipe
    /// runs; not at all toward an edge with none.
    fn swipe_shape(&self, i: usize, raw: f32) -> f32 {
        if raw == 0.0 {
            return 0.0;
        }
        let edge = self.swipe_edge_of(i, raw);
        let r = self.swipe_reach(i, edge);
        if r == 0.0 {
            return 0.0;
        }
        let m = raw.abs();
        let shown = if self.swipe_full(i, edge) {
            m.min(self.swipe_width(i))
        } else if m <= r {
            m
        } else {
            r + eased(m - r, self.swipe_width(i) * STRETCH_SHARE)
        };
        shown.copysign(raw)
    }

    /// The travel that shows as `offset`, as [`shape`](Self::swipe_shape) shows it.
    fn swipe_unshape(&self, i: usize, offset: f32) -> f32 {
        if offset == 0.0 {
            return 0.0;
        }
        let edge = self.swipe_edge_of(i, offset);
        let r = self.swipe_reach(i, edge);
        let m = offset.abs();
        let raw = if self.swipe_full(i, edge) || m <= r {
            m
        } else {
            r + uneased(m - r, self.swipe_width(i) * STRETCH_SHARE)
        };
        raw.copysign(offset)
    }

    /// Writes swipe row `i`'s offset as its content's and panels'
    /// translates.
    fn show_swipe(&mut self, i: usize) {
        let s = self.row_gestures.swipes.list[i];
        let w = self.swipe_width(i);
        if self.arena().is_live(s.parts.content) {
            self.set_translate(s.parts.content, Vec2::new(s.offset, 0.0));
        }
        for edge in [SwipeEdge::Leading, SwipeEdge::Trailing] {
            if let Some(panel) = self.swipe_panel(i, edge) {
                let away = self.swipe_sign(i, edge) * w;
                self.set_translate(panel, Vec2::new(s.offset - away, 0.0));
            }
        }
    }

    /// Springs swipe row `i` to `to`, from `v` (pt/s); at once under
    /// reduced motion.
    fn spring_swipe(&mut self, i: usize, to: f32, v: f64) {
        let instant = self.reduced_motion();
        let s = &mut self.row_gestures.swipes.list[i];
        if instant || (s.offset == to && v == 0.0) {
            s.offset = to;
            s.run = Run::Still;
            self.arrive_swipe(i);
        } else {
            s.run = Run::Spring { to, v };
        }
        self.show_swipe(i);
    }

    /// Swipe row `i` came to rest: a full swipe tells the row, and holds the
    /// content off before it springs back.
    fn arrive_swipe(&mut self, i: usize) {
        let instant = self.reduced_motion();
        let s = &mut self.row_gestures.swipes.list[i];
        let Some(edge) = s.full.take() else {
            return;
        };
        let node = s.node;
        if instant {
            s.offset = 0.0;
        } else {
            s.run = Run::Hold { left: FULL_HOLD };
        }
        self.tell_row_gesture(node, RowGesture::Swiped(edge));
    }

    /// Lets swipe row `i` go moving at `v` (pt/s across): carried off past
    /// a full swipe's mark, open on the actions it was flicked toward or
    /// stood past half of, else shut.
    fn release_swipe(&mut self, i: usize, v: f32) {
        let o = self.row_gestures.swipes.list[i].offset;
        let way = if o != 0.0 { o } else { v };
        if way == 0.0 {
            self.spring_swipe(i, 0.0, 0.0);
            return;
        }
        let edge = self.swipe_edge_of(i, way);
        let sign = self.swipe_sign(i, edge);
        let r = self.swipe_reach(i, edge);
        let to = if o != 0.0 && self.swipe_full(i, edge) && o.abs() >= self.swipe_mark(i, edge) {
            self.row_gestures.swipes.list[i].full = Some(edge);
            sign * self.swipe_width(i)
        } else if v.abs() >= SWIPE_SPEED {
            if o == 0.0 || v * o > 0.0 {
                sign * r
            } else {
                0.0
            }
        } else if r > 0.0 && o.abs() >= r * 0.5 {
            sign * r
        } else {
            0.0
        };
        if to != 0.0 {
            let node = self.row_gestures.swipes.list[i].node;
            self.shut_swipes_but(Some(node));
        }
        self.spring_swipe(i, to, f64::from(v));
    }

    /// Shuts every open swipe row but `keep`.
    fn shut_swipes_but(&mut self, keep: Option<NodeId>) {
        for k in 0..self.row_gestures.swipes.list.len() {
            let s = self.row_gestures.swipes.list[k];
            if Some(s.node) == keep || !self.arena().is_live(s.node) {
                continue;
            }
            if s.aim() != 0.0 || s.offset != 0.0 {
                let s = &mut self.row_gestures.swipes.list[k];
                s.full = None;
                s.by_focus = false;
                self.spring_swipe(k, 0.0, 0.0);
            }
        }
    }

    /// After a sample of a drag on node `node` was dispatched: a swipe
    /// row's drag slides its content, and its end lets it go.
    pub(crate) fn steer_swipe_drag(&mut self, node: NodeId, s: DragSample) {
        let Some(i) = self.swipe_index(node) else {
            return;
        };
        if s.phase == DragPhase::Start {
            self.shut_swipes_but(Some(node));
            let offset = self.row_gestures.swipes.list[i].offset;
            let raw = self.swipe_unshape(i, offset);
            let row = &mut self.row_gestures.swipes.list[i];
            row.run = Run::Still;
            row.full = None;
            row.raw = raw;
        }
        let raw = self.row_gestures.swipes.list[i].raw + s.delta.x;
        let offset = self.swipe_shape(i, raw);
        let row = &mut self.row_gestures.swipes.list[i];
        row.raw = raw;
        row.offset = offset;
        self.show_swipe(i);
        if s.phase == DragPhase::End {
            self.release_swipe(i, s.velocity.x);
        }
    }

    /// Whether a swipe row stands open or opens.
    pub(crate) fn swipes_open(&self) -> bool {
        self.row_gestures
            .swipes
            .list
            .iter()
            .any(|s| s.aim() != 0.0 || s.offset != 0.0)
    }

    /// A press of `pointer` on `target`: an open swipe row shuts unless the
    /// press is on its actions, and then shuts once it lifts. Returns
    /// whether one within the row's list shut, so the press is only that.
    pub(crate) fn press_swipes(
        &mut self,
        target: NodeId,
        pointer: crate::input::PointerId,
    ) -> bool {
        let mut taken = false;
        for i in 0..self.row_gestures.swipes.list.len() {
            let s = self.row_gestures.swipes.list[i];
            if !self.arena().is_live(s.node) || (s.aim() == 0.0 && s.offset == 0.0) {
                continue;
            }
            let on_actions = [SwipeEdge::Leading, SwipeEdge::Trailing]
                .into_iter()
                .filter_map(|e| self.swipe_panel(i, e))
                .any(|p| self.within(target, p));
            if on_actions {
                self.row_gestures.swipes.list[i].acting = Some(pointer);
                continue;
            }
            taken |= self.within(target, self.swipe_list(s.node));
            let s = &mut self.row_gestures.swipes.list[i];
            s.full = None;
            s.by_focus = false;
            self.spring_swipe(i, 0.0, 0.0);
        }
        taken
    }

    /// Pointer `pointer` lifted, its sample dispatched: the row whose action
    /// it pressed shuts.
    pub(crate) fn lift_swipes(&mut self, pointer: crate::input::PointerId) {
        for i in 0..self.row_gestures.swipes.list.len() {
            if self.row_gestures.swipes.list[i].acting == Some(pointer) {
                self.row_gestures.swipes.list[i].acting = None;
                let node = self.row_gestures.swipes.list[i].node;
                self.open_swipe_row(node, None);
            }
        }
    }

    /// The scroll viewport above row `node`, else the row.
    fn swipe_list(&self, node: NodeId) -> NodeId {
        let mut at = self.arena().parent(node);
        while let Some(n) = at {
            if self.scroll_axis(n).is_some() {
                return n;
            }
            at = self.arena().parent(n);
        }
        node
    }

    /// The focus moved to `focused`: a swipe row opens on the actions it
    /// came to, and one it opened shuts once it left them.
    pub(crate) fn swipe_focus(&mut self, focused: Option<NodeId>) {
        for i in 0..self.row_gestures.swipes.list.len() {
            let s = self.row_gestures.swipes.list[i];
            if !self.arena().is_live(s.node) {
                continue;
            }
            let came = focused.and_then(|f| {
                [SwipeEdge::Leading, SwipeEdge::Trailing]
                    .into_iter()
                    .find(|&e| self.swipe_panel(i, e).is_some_and(|p| self.within(f, p)))
            });
            match came {
                Some(edge) => {
                    let opened = self.swipe_open(s.node) == Some(edge);
                    if !opened {
                        self.open_swipe_row(s.node, Some(edge));
                    }
                    self.row_gestures.swipes.list[i].by_focus |= !opened;
                }
                None if s.by_focus => self.open_swipe_row(s.node, None),
                None => {}
            }
        }
    }

    /// Escape on the swipe row the focus is in shuts it. Returns whether it
    /// took the key.
    pub(crate) fn swipe_key(&mut self, ev: &KeyEvent) -> bool {
        if !ev.pressed || ev.key != Key::Escape || self.row_gestures.swipes.list.is_empty() {
            return false;
        }
        let Some(i) = self.focused().and_then(|f| self.swipe_over(f)) else {
            return false;
        };
        let node = self.row_gestures.swipes.list[i].node;
        if self.swipe_open(node).is_none() {
            return false;
        }
        // The focus leaves the actions sliding away for the row itself.
        let on_actions = self.focused().is_some_and(|f| {
            [SwipeEdge::Leading, SwipeEdge::Trailing]
                .into_iter()
                .filter_map(|e| self.swipe_panel(i, e))
                .any(|p| self.within(f, p))
        });
        if on_actions && self.focusable(node) {
            self.set_focused(Some(node));
        }
        self.open_swipe_row(node, None);
        true
    }

    /// A trackpad's scroll of `delta` (the offset's way) in `phase` over
    /// `target`: a swipe across a row slides it, wholly, and its lift lets
    /// it go; a scroll elsewhere shuts the open rows. Returns what of
    /// `delta` is left for the viewports.
    pub(crate) fn pad_swipe(&mut self, target: NodeId, delta: Vec2, phase: ScrollPhase) -> Vec2 {
        if self.row_gestures.swipes.list.is_empty() {
            return delta;
        }
        if phase == ScrollPhase::None {
            if delta != Vec2::ZERO {
                self.shut_swipes_but(None);
            }
            return delta;
        }
        if !matches!(
            phase,
            ScrollPhase::Began | ScrollPhase::Changed | ScrollPhase::Ended
        ) {
            return delta;
        }
        if phase == ScrollPhase::Began {
            self.row_gestures.swipes.pad = Some(Pad::Fresh);
        }
        let mut pad = self.row_gestures.swipes.pad.unwrap_or(Pad::Elsewhere);
        if matches!(pad, Pad::Fresh) && delta != Vec2::ZERO {
            pad = match self.swipe_over(target) {
                Some(i) if delta.x.abs() > delta.y.abs() * PAD_ACROSS => {
                    let node = self.row_gestures.swipes.list[i].node;
                    self.shut_swipes_but(Some(node));
                    let offset = self.row_gestures.swipes.list[i].offset;
                    let raw = self.swipe_unshape(i, offset);
                    let row = &mut self.row_gestures.swipes.list[i];
                    row.run = Run::Still;
                    row.full = None;
                    row.raw = raw;
                    self.row_gestures.swipes.pad_at = 0.0;
                    self.row_gestures.swipes.pad_track = VelocityTracker::default();
                    Pad::Row(node)
                }
                _ => {
                    self.shut_swipes_but(None);
                    Pad::Elsewhere
                }
            };
        }
        let left = match pad {
            Pad::Row(node) => {
                let now = self.input_time();
                let swipes = &mut self.row_gestures.swipes;
                swipes.pad_at -= delta.x;
                if let Some(now) = now {
                    swipes.pad_track.push(now, [swipes.pad_at, 0.0]);
                }
                let v = swipes.pad_track.velocity().x;
                if let Some(i) = self.swipe_index(node) {
                    let raw = self.row_gestures.swipes.list[i].raw - delta.x;
                    let offset = self.swipe_shape(i, raw);
                    let row = &mut self.row_gestures.swipes.list[i];
                    row.raw = raw;
                    row.offset = offset;
                    self.show_swipe(i);
                    if phase == ScrollPhase::Ended {
                        self.release_swipe(i, v);
                    }
                }
                Vec2::ZERO
            }
            _ => delta,
        };
        self.row_gestures.swipes.pad = (phase != ScrollPhase::Ended).then_some(pad);
        left
    }

    /// Advances every swipe row's spring by a frame of `delta`.
    pub(crate) fn tick_swipes(&mut self, delta: Duration) {
        let dt = delta.as_secs_f64().min(MAX_STEP);
        let mut i = self.row_gestures.swipes.list.len();
        while i > 0 {
            i -= 1;
            let s = self.row_gestures.swipes.list[i];
            if !self.arena().is_live(s.node) {
                self.row_gestures.swipes.list.swap_remove(i);
                continue;
            }
            match s.run {
                Run::Still => continue,
                Run::Hold { left } => {
                    if left > dt {
                        self.row_gestures.swipes.list[i].run = Run::Hold { left: left - dt };
                    } else {
                        self.row_gestures.swipes.list[i].run = Run::Spring { to: 0.0, v: 0.0 };
                    }
                }
                Run::Spring { to, v } => {
                    // A critically damped spring about its rest:
                    // e(t) = (e + (v + λe)t)·e^(−λt).
                    let e = f64::from(s.offset - to);
                    let c = v + SPRING * e;
                    let decay = (-SPRING * dt).exp();
                    let e = (e + c * dt) * decay;
                    let v = (v - SPRING * c * dt) * decay;
                    let row = &mut self.row_gestures.swipes.list[i];
                    if e.abs() < SETTLE_NEAR && v.abs() < SETTLE_STILL {
                        row.offset = to;
                        row.run = Run::Still;
                        self.arrive_swipe(i);
                    } else {
                        row.offset = to + e as f32;
                        row.run = Run::Spring { to, v };
                    }
                    self.show_swipe(i);
                }
            }
        }
    }

    /// Once layout placed the tree: a row open at rest stays open on as
    /// far as its actions now reach, and each shows where it stands in the
    /// width it now has.
    pub(crate) fn fit_swipes(&mut self) {
        for k in 0..self.row_gestures.swipes.auto.len() {
            let row = self.row_gestures.swipes.auto[k].0;
            if self.arena().is_live(row) {
                self.refit_swipe_row(row);
            }
        }
        if self.row_gestures.swipes.list.is_empty() {
            return;
        }
        let mut list = std::mem::take(&mut self.row_gestures.swipes.list);
        list.retain(|s| self.arena().is_live(s.node));
        self.row_gestures.swipes.list = list;
        for i in 0..self.row_gestures.swipes.list.len() {
            let s = self.row_gestures.swipes.list[i];
            if s.run == Run::Still && s.offset != 0.0 && !self.dragging(s.node) {
                let edge = self.swipe_edge_of(i, s.offset);
                let to = self.swipe_sign(i, edge) * self.swipe_reach(i, edge);
                self.row_gestures.swipes.list[i].offset = to;
            }
            self.show_swipe(i);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::component::{BuildCx, FlexStyle, LeafStyle, StackStyle};
    use crate::layout::{Alignment2D, Axis, Justify, Size};
    use crate::style::BoxStyle;
    use viso_render::Rect;

    const FRAME: Duration = Duration::from_millis(16);

    /// A 300×50 row: its content, a leading panel of one 60-wide action and
    /// a trailing one of two 70-wide ones.
    fn row(full_trailing: bool) -> (NodeStore, NodeId, SwipeParts) {
        let mut store = NodeStore::new();
        let mut parts = [None; 3];
        let leaf = |cx: &mut BuildCx<'_>, w: f32| {
            cx.leaf(LeafStyle {
                size: Size::fixed(w, 50.0),
                style: BoxStyle::NONE,
            })
            .id()
        };
        let root = {
            let mut cx = BuildCx::new(&mut store);
            let root = cx.stack(
                StackStyle {
                    align: Alignment2D::TopStart,
                    size: Size::fixed(300.0, 50.0),
                    ..StackStyle::default()
                },
                |cx| {
                    parts[0] = Some(leaf(cx, 300.0));
                    let panel = |cx: &mut BuildCx<'_>, end: bool, widths: &[f32]| {
                        cx.flex(
                            FlexStyle {
                                axis: Axis::Row,
                                justify: if end { Justify::End } else { Justify::Start },
                                size: Size::fill(),
                                ..FlexStyle::default()
                            },
                            |cx| {
                                for &w in widths {
                                    leaf(cx, w);
                                }
                            },
                        )
                        .id()
                    };
                    parts[1] = Some(panel(cx, true, &[60.0]));
                    parts[2] = Some(panel(cx, false, &[70.0, 70.0]));
                },
            );
            cx.clip(root, true);
            root.id()
        };
        let row = SwipeParts {
            content: parts[0].unwrap(),
            leading: parts[1],
            trailing: parts[2],
            full_leading: false,
            full_trailing,
        };
        store.set_swipe_row(root, Some(row));
        store.layout(root, Rect::new(0.0, 0.0, 300.0, 50.0), &mut Vec::new());
        store.fit_swipes();
        (store, root, row)
    }

    fn settle(store: &mut NodeStore) {
        for _ in 0..200 {
            if !store.row_gestures.moving() {
                return;
            }
            store.tick_swipes(FRAME);
        }
        panic!("the row never came to rest");
    }

    fn drag(store: &mut NodeStore, node: NodeId, by: &[f32], velocity: f32) {
        for (k, &d) in by.iter().enumerate() {
            let phase = if k == 0 {
                DragPhase::Start
            } else if k + 1 == by.len() {
                DragPhase::End
            } else {
                DragPhase::Move
            };
            store.steer_swipe_drag(
                node,
                DragSample {
                    phase,
                    at: Vec2::ZERO,
                    delta: Vec2::new(d, 0.0),
                    total: Vec2::ZERO,
                    velocity: Vec2::new(
                        if phase == DragPhase::End {
                            velocity
                        } else {
                            0.0
                        },
                        0.0,
                    ),
                },
            );
        }
    }

    #[test]
    fn the_panels_wait_past_the_edges_and_follow_the_content_in() {
        let (mut store, root, row) = row(false);
        let (lead, trail) = (row.leading.unwrap(), row.trailing.unwrap());
        assert_eq!(store.translate(lead), Vec2::new(-300.0, 0.0));
        assert_eq!(store.translate(trail), Vec2::new(300.0, 0.0));
        drag(&mut store, root, &[-40.0, -40.0], 0.0);
        assert_eq!(store.swipe_offset(root), Some(-80.0));
        assert_eq!(store.translate(row.content), Vec2::new(-80.0, 0.0));
        assert_eq!(
            store.translate(trail),
            Vec2::new(220.0, 0.0),
            "its edge held to the content's"
        );
        assert_eq!(store.swipe_open(root), Some(SwipeEdge::Trailing));
        settle(&mut store);
        assert_eq!(
            store.swipe_offset(root),
            Some(-140.0),
            "open on both actions"
        );
    }

    #[test]
    fn let_go_short_of_half_it_shuts_and_flicked_it_opens() {
        let (mut store, root, _) = row(false);
        drag(&mut store, root, &[-20.0, -20.0], 0.0);
        settle(&mut store);
        assert_eq!(store.swipe_offset(root), Some(0.0));
        drag(&mut store, root, &[-10.0, -10.0], -900.0);
        settle(&mut store);
        assert_eq!(store.swipe_offset(root), Some(-140.0));
        drag(&mut store, root, &[5.0, 5.0], 900.0);
        settle(&mut store);
        assert_eq!(store.swipe_offset(root), Some(0.0), "a flick back shuts it");
    }

    #[test]
    fn past_the_actions_it_eases_and_toward_an_edge_with_none_it_stays() {
        let (mut store, root, row) = row(false);
        drag(&mut store, root, &[200.0, 200.0], 0.0);
        let shown = store.swipe_offset(root).unwrap();
        assert!(shown > 60.0 && shown < 60.0 + 45.0, "{shown}");
        settle(&mut store);
        assert_eq!(store.swipe_offset(root), Some(60.0));
        store.set_swipe_row(
            root,
            Some(SwipeParts {
                leading: None,
                ..row
            }),
        );
        assert_eq!(
            store.swipe_offset(root),
            Some(0.0),
            "the open edge lost its panel"
        );
        drag(&mut store, root, &[80.0, 80.0], 0.0);
        assert_eq!(store.swipe_offset(root), Some(0.0));
    }

    #[test]
    fn a_full_swipe_carries_the_content_off_tells_the_row_and_springs_back() {
        let (mut store, root, _) = row(true);
        drag(&mut store, root, &[-100.0, -100.0], 0.0);
        assert_eq!(store.swipe_offset(root), Some(-200.0), "it follows on");
        for _ in 0..200 {
            store.tick_swipes(FRAME);
            if store.row_gestures_due() {
                break;
            }
        }
        assert_eq!(store.swipe_offset(root), Some(-300.0));
        let mut told = Vec::new();
        store.take_row_gestures(&mut told);
        assert_eq!(told, [(root, RowGesture::Swiped(SwipeEdge::Trailing))]);
        settle(&mut store);
        assert_eq!(store.swipe_offset(root), Some(0.0));
    }

    #[test]
    fn read_right_to_left_its_leading_edge_is_the_right() {
        let (mut store, root, _) = row(false);
        store.set_layout_direction(root, Some(LayoutDirection::Rtl));
        store.layout(root, Rect::new(0.0, 0.0, 300.0, 50.0), &mut Vec::new());
        drag(&mut store, root, &[-50.0, -50.0], 0.0);
        settle(&mut store);
        assert_eq!(store.swipe_open(root), Some(SwipeEdge::Leading));
        assert_eq!(store.swipe_offset(root), Some(-60.0));
    }

    #[test]
    fn the_focus_on_an_action_opens_the_row_and_leaving_shuts_it() {
        let (mut store, root, row) = row(false);
        store.set_reduced_motion(true);
        let action = store
            .arena()
            .children(row.trailing.unwrap())
            .next()
            .unwrap();
        store.set_focusable(action, true);
        store.set_focused(Some(action));
        assert_eq!(store.swipe_offset(root), Some(-140.0));
        store.set_focused(None);
        assert_eq!(store.swipe_offset(root), Some(0.0));
    }

    #[test]
    fn a_trackpad_swipe_across_slides_it_and_one_along_is_left_to_the_list() {
        let (mut store, root, row) = row(false);
        let t0 = viso_runtime::Instant::now();
        store.set_input_time(t0);
        let left = store.pad_swipe(row.content, Vec2::new(2.0, 0.0), ScrollPhase::Began);
        assert_eq!(left, Vec2::ZERO);
        for k in 1..=5 {
            store.set_input_time(t0 + Duration::from_millis(10 * k));
            store.pad_swipe(row.content, Vec2::new(20.0, 1.0), ScrollPhase::Changed);
        }
        assert_eq!(store.swipe_offset(root), Some(-102.0));
        store.pad_swipe(row.content, Vec2::ZERO, ScrollPhase::Ended);
        settle(&mut store);
        assert_eq!(store.swipe_offset(root), Some(-140.0));

        let left = store.pad_swipe(row.content, Vec2::new(0.0, 10.0), ScrollPhase::Began);
        assert_eq!(left, Vec2::new(0.0, 10.0));
        settle(&mut store);
        assert_eq!(
            store.swipe_offset(root),
            Some(0.0),
            "a scroll along shuts it"
        );
    }
}
