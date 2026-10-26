//! Sheets: a layer whose panel slides in from an edge of the surface, rests
//! at one of its detents, and a drag carries between them or off.
//!
//! A sheet ([`NodeStore::set_sheet`]) is a layer covering the surface, its
//! own box the scrim, its first child the panel, laid out against an edge
//! ([`SheetEdge`]) at its full extent. The panel shows as much of itself as
//! its detent asks ([`SheetDetent`]: all of it, a share of the surface, or
//! a length), the rest past the edge: a sheet shows only translates, and
//! marks only `TRANSFORM` (and its scrim's `PAINT`). Presented
//! ([`NodeStore::present_sheet`]) it shows, traps the focus and springs in
//! to its detent, the scrim darkening as it comes; put away, it springs
//! out, and once out it hides and gives the focus back.
//!
//! A drag on the panel along the edge's axis carries it — a finger
//! anywhere on it, and from a scroll viewport within it once that viewport
//! has nowhere to go the way the finger set out; the sheet stops at its
//! full extent and, where a drag cannot dismiss it, eases below its least
//! detent. Let go, it springs to the detent nearest to where its speed
//! would carry it, the list told the detent it settles on
//! ([`KeyRouter::route_sheets`](crate::KeyRouter::route_sheets)); one that
//! a drag dismisses heads off past its edge and asks its owner to close it
//! as a press on its scrim does ([`EventCx::dismissed`]). An owner that
//! keeps it open sees it spring back. Under reduced motion it moves at
//! once.

use core::time::Duration;

use crate::adaptive::LayoutDirection;
use crate::component::NodeStore;
use crate::drag::{DragPhase, DragSample};
use crate::layout::{Axis, OnAxis, Vec2};
use crate::node::NodeId;
use crate::scroll_motion::{MAX_STEP, SPRING};
use crate::viewer::eased;
use viso_render::Rgba;
pub use viso_schema::layout::{SheetDetent, SheetEdge};

/// How far (a share of its least detent) a sheet a drag cannot dismiss
/// eases below that detent at most.
const BELOW_SHARE: f32 = 0.15;
/// How long a sheet let go keeps travelling at its speed, for the detent
/// it heads for.
const PROJECTION: f32 = 0.2;
/// Within this many points of its rest, and slower than [`SETTLE_STILL`],
/// a spring arrived.
const SETTLE_NEAR: f64 = 0.5;
const SETTLE_STILL: f64 = 10.0;

/// What a sheet is ([`NodeStore::set_sheet`]).
#[derive(Debug, Clone, PartialEq)]
pub struct SheetSpec {
    /// The edge its panel lies against.
    pub edge: SheetEdge,
    /// Where it rests; none for all of the panel. A drag carries it between
    /// them.
    pub detents: Vec<SheetDetent>,
    /// Whether a drag past its least detent dismisses it.
    pub dismissible: bool,
}

impl Default for SheetSpec {
    fn default() -> Self {
        SheetSpec {
            edge: SheetEdge::Bottom,
            detents: Vec::new(),
            dismissible: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Run {
    Still,
    /// Springing to its aim at `v` (pt/s of shown extent).
    Spring {
        v: f64,
    },
    /// Carried by a drag that travelled `raw` of extent.
    Drag {
        raw: f32,
    },
}

/// Whether a dismiss a drag asked for was heard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ask {
    None,
    /// Told, not yet heard.
    Told,
    /// Heard: an owner that keeps it open sees it spring back.
    Heard,
}

#[derive(Debug, Clone)]
struct Sheet {
    node: NodeId,
    spec: SheetSpec,
    /// Whether its owner presents it.
    open: bool,
    /// The detent it rests at while open, by its place among them least
    /// first.
    detent: usize,
    /// How much of the panel shows along its axis.
    shown: f32,
    run: Run,
    ask: Ask,
    /// Its scrim's color at full dim.
    scrim: Option<Rgba>,
    /// The slide written into the panel's translate, and the panel.
    slid: Vec2,
    panel: Option<NodeId>,
}

/// A detent or a sheet's choice told to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SheetTell {
    /// A drag asked to dismiss it.
    Dismiss,
    /// It settled on detent `.0`.
    Detent(usize),
}

/// A window's sheets. Cold: a store with none spends nothing on them.
#[derive(Debug, Default)]
pub(crate) struct Sheets {
    list: Vec<Sheet>,
    told: Vec<(NodeId, SheetTell)>,
}

impl Sheets {
    pub(crate) fn clear(&mut self) {
        self.list.clear();
        self.told.clear();
    }

    pub(crate) fn moving(&self) -> bool {
        self.list
            .iter()
            .any(|s| matches!(s.run, Run::Spring { .. }))
    }
}

impl NodeStore {
    /// Make layer `id` a sheet of `spec`, or no longer one with `None`: its
    /// first child is the panel. A new sheet starts put away and hidden.
    pub fn set_sheet(&mut self, id: NodeId, spec: Option<SheetSpec>) {
        let at = self.sheet_index(id);
        let Some(spec) = spec.filter(|_| self.arena().is_live(id)) else {
            if let Some(i) = at {
                let s = self.sheets.list.swap_remove(i);
                if let Some(panel) = s.panel.filter(|&p| self.arena().is_live(p)) {
                    let own = self.translate(panel) - s.slid;
                    self.put_translate(panel, own);
                    self.set_draggable(panel, false);
                    self.set_drag_axis(panel, None);
                }
            }
            return;
        };
        match at {
            Some(i) => {
                let edge = self.sheets.list[i].spec.edge;
                let s = &mut self.sheets.list[i];
                s.detent = s.detent.min(spec.detents.len().saturating_sub(1));
                s.spec = spec;
                if s.spec.edge != edge
                    && let Some(panel) = s.panel
                {
                    self.set_drag_axis(panel, Some(self.sheets.list[i].spec.edge.axis()));
                }
            }
            None => {
                self.sheets.list.push(Sheet {
                    node: id,
                    spec,
                    open: false,
                    detent: 0,
                    shown: 0.0,
                    run: Run::Still,
                    ask: Ask::None,
                    scrim: None,
                    slid: Vec2::ZERO,
                    panel: None,
                });
                self.set_hidden(id, true);
            }
        }
        self.fit_sheet(self.sheets.list.len() - 1);
    }

    /// Whether node `id` is a sheet.
    pub fn is_sheet(&self, id: NodeId) -> bool {
        self.sheet_index(id).is_some()
    }

    /// Whether node `id` is a sheet's panel, which a drag carries.
    pub fn is_sheet_panel(&self, id: NodeId) -> bool {
        self.sheet_of_panel(id).is_some()
    }

    /// Present sheet `id` — shown, the focus trapped in it, springing in to
    /// its detent — or put it away, springing out, hidden once out.
    pub fn present_sheet(&mut self, id: NodeId, open: bool) {
        let Some(i) = self.sheet_index(id) else {
            return;
        };
        let s = &mut self.sheets.list[i];
        if s.open == open {
            return;
        }
        s.open = open;
        s.ask = Ask::None;
        if open {
            self.set_hidden(id, false);
            self.trap_focus(id, true);
        }
        if !matches!(self.sheets.list[i].run, Run::Drag { .. }) {
            self.spring_sheet(i, 0.0);
        }
    }

    /// Bring sheet `id` to rest at detent `detent`, by its place among its
    /// detents least first.
    pub fn set_sheet_detent(&mut self, id: NodeId, detent: usize) {
        let Some(i) = self.sheet_index(id) else {
            return;
        };
        let last = self.sheets.list[i].spec.detents.len().saturating_sub(1);
        let detent = detent.min(last);
        if self.sheets.list[i].detent == detent {
            return;
        }
        self.sheets.list[i].detent = detent;
        if self.sheets.list[i].open && !matches!(self.sheets.list[i].run, Run::Drag { .. }) {
            self.spring_sheet(i, 0.0);
        }
    }

    /// The detent sheet `id` rests at, or heads for.
    pub fn sheet_detent(&self, id: NodeId) -> Option<usize> {
        self.sheet_index(id).map(|i| self.sheets.list[i].detent)
    }

    /// How much of sheet `id`'s panel shows along its axis.
    pub fn sheet_shown(&self, id: NodeId) -> Option<f32> {
        self.sheet_index(id).map(|i| self.sheets.list[i].shown)
    }

    /// Dim sheet `id`'s scrim, its own box, with `color` as the sheet
    /// shows: none while out, all of it from its least detent on.
    pub fn set_sheet_scrim(&mut self, id: NodeId, color: Rgba) {
        let Some(i) = self.sheet_index(id) else {
            return;
        };
        self.sheets.list[i].scrim = Some(color);
        self.show_sheet(i);
    }

    fn sheet_index(&self, id: NodeId) -> Option<usize> {
        self.sheets.list.iter().position(|s| s.node == id)
    }

    /// The sheet whose panel node `id` is.
    fn sheet_of_panel(&self, id: NodeId) -> Option<usize> {
        if self.sheets.list.is_empty() {
            return None;
        }
        self.sheets.list.iter().position(|s| s.panel == Some(id))
    }

    /// The slide a sheet gives node `id`, its panel, past its own translate.
    #[inline]
    pub(crate) fn sheet_slide_of(&self, id: NodeId) -> Vec2 {
        if self.sheets.list.is_empty() {
            return Vec2::ZERO;
        }
        self.sheet_of_panel(id)
            .map_or(Vec2::ZERO, |i| self.sheets.list[i].slid)
    }

    /// Sheet `i`'s panel's full extent along its axis, and the surface's.
    fn sheet_extents(&self, i: usize) -> (f32, f32) {
        let s = &self.sheets.list[i];
        let main = |id: NodeId| self.bounds_main(id, s.spec.edge.axis());
        (s.panel.map_or(0.0, main), main(s.node))
    }

    /// How much of sheet `i`'s panel each detent shows, least first.
    fn sheet_detent_extents(&self, i: usize) -> impl Iterator<Item = f32> + '_ {
        let (full, surface) = self.sheet_extents(i);
        let s = &self.sheets.list[i];
        let all = s.spec.detents.is_empty().then_some(SheetDetent::Full);
        s.spec.detents.iter().copied().chain(all).map(move |d| {
            match d {
                SheetDetent::Full => full,
                SheetDetent::Fraction(f) => f * surface,
                SheetDetent::Fixed(v) => v,
            }
            .clamp(0.0, full)
        })
    }

    /// How much of sheet `i`'s panel its detent `detent` shows.
    fn sheet_detent_extent(&self, i: usize, detent: usize) -> f32 {
        self.sheet_detent_extents(i)
            .nth(detent)
            .or_else(|| self.sheet_detent_extents(i).last())
            .unwrap_or(0.0)
    }

    /// Where sheet `i` heads: its detent while open and not dismissed, out
    /// past its edge else.
    fn sheet_aim(&self, i: usize) -> f32 {
        let s = &self.sheets.list[i];
        if s.open && s.ask == Ask::None {
            self.sheet_detent_extent(i, s.detent)
        } else {
            0.0
        }
    }

    /// The way, in window space, sheet `i`'s panel slides in.
    fn sheet_inward(&self, i: usize) -> Vec2 {
        let s = &self.sheets.list[i];
        let rtl = self.direction_at(s.node.index()) == LayoutDirection::Rtl;
        match (s.spec.edge, rtl) {
            (SheetEdge::Bottom, _) => Vec2::new(0.0, -1.0),
            (SheetEdge::Top, _) => Vec2::new(0.0, 1.0),
            (SheetEdge::Leading, false) | (SheetEdge::Trailing, true) => Vec2::new(1.0, 0.0),
            (SheetEdge::Leading, true) | (SheetEdge::Trailing, false) => Vec2::new(-1.0, 0.0),
        }
    }

    /// Springs sheet `i` toward its aim from `v`; at once under reduced
    /// motion.
    fn spring_sheet(&mut self, i: usize, v: f64) {
        if self.reduced_motion() {
            self.sheets.list[i].shown = self.sheet_aim(i);
            self.sheets.list[i].run = Run::Still;
            self.arrive_sheet(i);
        } else {
            self.sheets.list[i].run = Run::Spring { v };
        }
        self.show_sheet(i);
    }

    /// Sheet `i` came to rest: put away and out, it hides and gives the
    /// focus back.
    fn arrive_sheet(&mut self, i: usize) {
        let s = &self.sheets.list[i];
        if !s.open && s.shown == 0.0 {
            let node = s.node;
            self.set_hidden(node, true);
            self.trap_focus(node, false);
        }
    }

    /// Writes how much of sheet `i` shows: its panel's slide past its edge
    /// and its scrim's dim.
    fn show_sheet(&mut self, i: usize) {
        let (full, _) = self.sheet_extents(i);
        let inward = self.sheet_inward(i);
        let least = self.sheet_detent_extent(i, 0);
        let s = &self.sheets.list[i];
        let slide = inward * -(full - s.shown).max(0.0);
        if let Some(panel) = s.panel.filter(|&p| self.arena().is_live(p))
            && slide != s.slid
        {
            let own = self.translate(panel) - s.slid;
            self.sheets.list[i].slid = slide;
            self.put_translate(panel, own + slide);
        }
        let s = &self.sheets.list[i];
        if let Some(color) = s.scrim {
            let dim = if least > 0.0 {
                (s.shown / least).clamp(0.0, 1.0)
            } else {
                f32::from(u8::from(s.open))
            };
            let node = s.node;
            self.set_fill(
                node,
                Rgba {
                    a: color.a * dim,
                    ..color
                },
            );
        }
    }

    /// Re-reads sheet `i`'s panel and shows it where it stands.
    fn fit_sheet(&mut self, i: usize) {
        let node = self.sheets.list[i].node;
        let panel = self.arena().children(node).find(|&c| !self.is_chrome(c));
        let s = &self.sheets.list[i];
        if panel != s.panel {
            if let Some(old) = s.panel.filter(|&p| self.arena().is_live(p)) {
                let own = self.translate(old) - s.slid;
                self.put_translate(old, own);
                self.set_draggable(old, false);
                self.set_drag_axis(old, None);
            }
            self.sheets.list[i].slid = Vec2::ZERO;
            self.sheets.list[i].panel = panel;
            if let Some(panel) = panel {
                self.set_draggable(panel, true);
                self.set_drag_axis(panel, Some(self.sheets.list[i].spec.edge.axis()));
            }
        }
        // At rest it stays at its aim as the layout moves its detents.
        if self.sheets.list[i].run == Run::Still {
            self.sheets.list[i].shown = self.sheet_aim(i);
        }
        self.show_sheet(i);
    }

    /// How much of sheet `i` a drag that travelled `raw` shows: no more than
    /// all of it, and below its least detent eased unless a drag dismisses
    /// it.
    fn sheet_shape(&self, i: usize, raw: f32) -> f32 {
        let (full, _) = self.sheet_extents(i);
        let least = self.sheet_detent_extent(i, 0);
        let raw = raw.min(full);
        if raw >= least || self.sheets.list[i].spec.dismissible {
            return raw.max(0.0);
        }
        least - eased(least - raw, least * BELOW_SHARE)
    }

    /// After a sample of a drag on node `node` was dispatched: a sheet's
    /// panel follows it, and its end lets it go.
    pub(crate) fn steer_sheet_drag(&mut self, node: NodeId, s: DragSample) {
        let Some(i) = self.sheet_of_panel(node) else {
            return;
        };
        if !self.sheets.list[i].open {
            return;
        }
        let inward = self.sheet_inward(i);
        let along = |v: Vec2| v.x * inward.x + v.y * inward.y;
        let raw = match self.sheets.list[i].run {
            Run::Drag { raw } if s.phase != DragPhase::Start => raw,
            _ => self.sheets.list[i].shown,
        } + along(s.delta);
        self.sheets.list[i].shown = self.sheet_shape(i, raw);
        self.sheets.list[i].run = Run::Drag { raw };
        self.show_sheet(i);
        if s.phase == DragPhase::End {
            self.release_sheet(i, along(s.velocity));
        }
    }

    /// Lets sheet `i` go moving at `v` (pt/s, inward): to the detent
    /// nearest where its speed would carry it, or off.
    fn release_sheet(&mut self, i: usize, v: f32) {
        let s = &self.sheets.list[i];
        let projected = s.shown + v * PROJECTION;
        // The nearest rest, off past the edge among them where a drag
        // dismisses it.
        let mut best = if s.spec.dismissible {
            (None, projected.abs())
        } else {
            (Some(0), f32::INFINITY)
        };
        for (k, e) in self.sheet_detent_extents(i).enumerate() {
            let d = (projected - e).abs();
            if d < best.1 {
                best = (Some(k), d);
            }
        }
        let node = self.sheets.list[i].node;
        match best.0 {
            Some(k) => {
                if k != self.sheets.list[i].detent {
                    self.sheets.list[i].detent = k;
                    self.sheets.told.push((node, SheetTell::Detent(k)));
                }
            }
            None => {
                self.sheets.list[i].ask = Ask::Told;
                self.sheets.told.push((node, SheetTell::Dismiss));
            }
        }
        self.spring_sheet(i, f64::from(v));
    }

    /// Whether a touch drag of node `node` pressed at `target` that
    /// travelled `total` leads ahead of the scroll viewports between them:
    /// `node` is a sheet's panel, and it grows while it can, or every
    /// viewport has nowhere to go the way the finger set out.
    pub(crate) fn sheet_leads(&self, node: NodeId, target: NodeId, total: Vec2) -> bool {
        let Some(i) = self.sheet_of_panel(node) else {
            return false;
        };
        let s = &self.sheets.list[i];
        let axis = s.spec.edge.axis();
        let inward = self.sheet_inward(i);
        let along = total.x * inward.x + total.y * inward.y;
        let across = match axis {
            Axis::Row => total.y,
            Axis::Column => total.x,
        };
        if along.abs() < across.abs() {
            return false;
        }
        let (full, _) = self.sheet_extents(i);
        if along > 0.0 && s.shown < full - SETTLE_NEAR as f32 {
            return true;
        }
        // The content moves with the finger: a viewport scrolls the other way.
        let delta = -total.on(axis);
        let mut at = Some(target);
        while let Some(n) = at.filter(|&n| n != node) {
            if self.scrolls_on(n, axis) {
                let offset = self.scroll(n).on(axis);
                let (least, most) = self.scroll_span(n, axis);
                if (delta > 0.0 && offset < most) || (delta < 0.0 && offset > least) {
                    return false;
                }
            }
            at = self.arena().parent(n);
        }
        true
    }

    /// Whether a sheet's choice waits to be told.
    pub fn sheets_due(&self) -> bool {
        !self.sheets.told.is_empty()
    }

    /// Moves the sheets' choices waiting to be told into `out`.
    pub(crate) fn take_sheet_tells(&mut self, out: &mut Vec<(NodeId, SheetTell)>) {
        for &(node, tell) in &self.sheets.told {
            if tell == SheetTell::Dismiss
                && let Some(i) = self.sheet_index(node)
                && self.sheets.list[i].ask == Ask::Told
            {
                self.sheets.list[i].ask = Ask::Heard;
            }
        }
        out.append(&mut self.sheets.told);
    }

    /// Advance the sheets' springs by `delta`.
    pub(crate) fn tick_sheets(&mut self, delta: Duration) {
        let dt = delta.as_secs_f64().min(MAX_STEP);
        for i in 0..self.sheets.list.len() {
            let Run::Spring { v } = self.sheets.list[i].run else {
                continue;
            };
            let to = self.sheet_aim(i);
            // A critically damped spring about its rest:
            // e(t) = (e + (v + λe)t)·e^(−λt).
            let e = f64::from(self.sheets.list[i].shown - to);
            let c = v + SPRING * e;
            let decay = (-SPRING * dt).exp();
            let e = (e + c * dt) * decay;
            let v = (v - SPRING * c * dt) * decay;
            if e.abs() < SETTLE_NEAR && v.abs() < SETTLE_STILL {
                self.sheets.list[i].shown = to;
                self.sheets.list[i].run = Run::Still;
                self.arrive_sheet(i);
            } else {
                self.sheets.list[i].shown = to + e as f32;
                self.sheets.list[i].run = Run::Spring { v };
            }
            self.show_sheet(i);
        }
    }

    /// Once layout placed the tree: each sheet shows where it stands as
    /// large as its panel now is; one whose owner heard its dismiss and
    /// kept it open springs back.
    pub(crate) fn fit_sheets(&mut self) {
        if self.sheets.list.is_empty() {
            return;
        }
        let mut list = std::mem::take(&mut self.sheets.list);
        list.retain(|s| self.arena().is_live(s.node));
        self.sheets.list = list;
        for i in 0..self.sheets.list.len() {
            let s = &self.sheets.list[i];
            if s.ask == Ask::Heard {
                self.sheets.list[i].ask = Ask::None;
                if self.sheets.list[i].open {
                    self.spring_sheet(i, 0.0);
                }
            }
            self.fit_sheet(i);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::component::{BuildCx, LeafStyle, StackStyle};
    use crate::layout::{Alignment2D, Sizing};
    use crate::style::BoxStyle;
    use viso_render::Rect;

    const FRAME: Duration = Duration::from_millis(16);

    /// A 400×800 sheet whose panel is 400×600, resting at half the surface
    /// and at all of the panel.
    fn sheet(dismissible: bool) -> (NodeStore, NodeId, NodeId) {
        let mut store = NodeStore::new();
        let mut panel = None;
        let root = {
            let mut cx = BuildCx::new(&mut store);
            cx.stack(
                StackStyle {
                    align: Alignment2D::Bottom,
                    size: Sizing::fixed(400.0, 800.0),
                    ..StackStyle::default()
                },
                |cx| {
                    panel = Some(
                        cx.leaf(LeafStyle {
                            size: Sizing::fixed(400.0, 600.0),
                            style: BoxStyle::NONE,
                        })
                        .id(),
                    );
                },
            )
            .id()
        };
        store.set_sheet(
            root,
            Some(SheetSpec {
                edge: SheetEdge::Bottom,
                detents: vec![SheetDetent::Fraction(0.5), SheetDetent::Full],
                dismissible,
            }),
        );
        store.layout(root, Rect::new(0.0, 0.0, 400.0, 800.0), &mut Vec::new());
        store.fit_sheets();
        (store, root, panel.unwrap())
    }

    /// Presents it and lays it out shown, as the next frame does.
    fn present(store: &mut NodeStore, root: NodeId) {
        store.present_sheet(root, true);
        store.layout(root, Rect::new(0.0, 0.0, 400.0, 800.0), &mut Vec::new());
        store.fit_sheets();
    }

    fn settle(store: &mut NodeStore) {
        for _ in 0..300 {
            if !store.sheets.moving() {
                return;
            }
            store.tick_sheets(FRAME);
        }
        panic!("the sheet never came to rest");
    }

    fn drag(store: &mut NodeStore, panel: NodeId, dy: f32, vy: f32) {
        for (phase, delta, velocity) in [
            (DragPhase::Start, 0.0, 0.0),
            (DragPhase::Move, dy, 0.0),
            (DragPhase::End, 0.0, vy),
        ] {
            store.steer_sheet_drag(
                panel,
                DragSample {
                    phase,
                    at: Vec2::ZERO,
                    delta: Vec2::new(0.0, delta),
                    total: Vec2::ZERO,
                    velocity: Vec2::new(0.0, velocity),
                },
            );
        }
    }

    fn told(store: &mut NodeStore) -> Vec<SheetTell> {
        let mut out = Vec::new();
        store.take_sheet_tells(&mut out);
        out.into_iter().map(|(_, t)| t).collect()
    }

    #[test]
    fn presented_it_springs_in_to_its_detent_and_put_away_it_hides() {
        let (mut store, root, panel) = sheet(true);
        assert!(store.hidden(root), "a new sheet is put away");
        present(&mut store, root);
        assert!(!store.hidden(root));
        assert_eq!(store.translate(panel).y, 600.0, "past the edge");
        settle(&mut store);
        assert_eq!(store.sheet_shown(root), Some(400.0), "half the surface");
        assert_eq!(store.translate(panel).y, 200.0);
        store.set_sheet_detent(root, 1);
        settle(&mut store);
        assert_eq!(store.translate(panel).y, 0.0, "all of the panel");
        store.present_sheet(root, false);
        assert!(!store.hidden(root), "shown as it slides out");
        settle(&mut store);
        assert!(store.hidden(root));
        assert_eq!(store.translate(panel).y, 600.0);
    }

    #[test]
    fn a_drag_carries_it_to_the_nearest_detent_its_speed_heads_for() {
        let (mut store, root, panel) = sheet(true);
        present(&mut store, root);
        settle(&mut store);
        drag(&mut store, panel, -50.0, -600.0);
        assert_eq!(told(&mut store), [SheetTell::Detent(1)], "flicked up");
        settle(&mut store);
        assert_eq!(store.sheet_shown(root), Some(600.0));
        drag(&mut store, panel, 120.0, 0.0);
        assert_eq!(told(&mut store), [SheetTell::Detent(0)]);
        settle(&mut store);
        assert_eq!(store.sheet_shown(root), Some(400.0));
    }

    #[test]
    fn a_drag_off_asks_to_dismiss_and_a_sheet_kept_open_springs_back() {
        let (mut store, root, panel) = sheet(true);
        present(&mut store, root);
        settle(&mut store);
        drag(&mut store, panel, 250.0, 300.0);
        settle(&mut store);
        assert_eq!(store.sheet_shown(root), Some(0.0), "off past its edge");
        assert!(!store.hidden(root), "until its owner closes it");
        assert_eq!(told(&mut store), [SheetTell::Dismiss]);
        store.fit_sheets();
        settle(&mut store);
        assert_eq!(store.sheet_shown(root), Some(400.0), "kept open, back");

        drag(&mut store, panel, 300.0, 0.0);
        let _ = told(&mut store);
        store.present_sheet(root, false);
        settle(&mut store);
        assert!(store.hidden(root), "closed, hidden");
    }

    #[test]
    fn one_no_drag_dismisses_eases_below_its_least_detent() {
        let (mut store, root, panel) = sheet(false);
        present(&mut store, root);
        settle(&mut store);
        store.steer_sheet_drag(
            panel,
            DragSample {
                phase: DragPhase::Start,
                at: Vec2::ZERO,
                delta: Vec2::new(0.0, 300.0),
                total: Vec2::ZERO,
                velocity: Vec2::ZERO,
            },
        );
        let shown = store.sheet_shown(root).unwrap();
        assert!(shown > 340.0 && shown < 400.0, "eased: {shown}");
        drag(&mut store, panel, 0.0, 0.0);
        assert!(told(&mut store).is_empty());
        settle(&mut store);
        assert_eq!(store.sheet_shown(root), Some(400.0));
    }
}
