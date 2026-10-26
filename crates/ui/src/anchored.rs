//! Anchored layers: a floating box — a menu, a list, a tooltip, a popover —
//! laid out against another node, its anchor, rather than in its parent's
//! flow.
//!
//! An anchored layer is a [layer](crate::layer): out of its parent's flow,
//! painted in the top layer, and still hanging where it was authored. Where
//! an ordinary layer covers the surface, an anchored one takes its natural
//! size and sits on a side of its anchor ([`Anchoring`]): aligned to the
//! anchor's start, middle or end, a gap away, flipped to the other side when
//! its own lacks the room and the other has more, shifted along the anchor
//! to stay inside the window, held within the room on its side so a long
//! list scrolls, and as wide as its anchor if asked. The window is the
//! surface less the system's insets ([`NodeStore::set_keep_out`]) and a
//! margin. Where it went is its [`AnchorPlacement`], with where an arrow pointing
//! at the anchor's middle goes; a node named as its arrow is moved there.
//!
//! Layout places an anchored layer after the tree, so it follows its anchor
//! and its own content as they change size; a scroll or a translate moves
//! the anchor without a layout, so the frame re-places the layers after
//! transforms ([`NodeStore::follow_anchors`]). The table is cold: one entry
//! for each anchored layer.

use viso_render::Rect;

use crate::component::NodeStore;
use crate::layout::{Inset, LayoutTree, Length, Vec2};
use crate::node::NodeId;

/// The side of its anchor a box sits on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Side {
    Top,
    #[default]
    Bottom,
    /// The side lines start from: the left, the right right to left.
    Start,
    End,
}

/// Where along its anchor's side a box lines up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AnchorAlign {
    /// Its start with the anchor's.
    #[default]
    Start,
    Center,
    End,
}

/// How an anchored box sits against its anchor.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Anchoring {
    pub side: Side,
    pub align: AnchorAlign,
    /// The room between the anchor and the box; negative overlaps them, as a
    /// list whose chosen row covers its button does.
    pub gap: f32,
    /// The room kept from the window's edges.
    pub margin: f32,
    /// Go to the other side when this one lacks the room and the other has
    /// more.
    pub flip: bool,
    /// Slide to stay inside the window.
    pub shift: bool,
    /// Hold the box within the room on its side, and the window's breadth.
    pub fit: bool,
    /// At least as broad as the anchor along its side.
    pub match_anchor: bool,
    /// How far from the box's corners an arrow stays.
    pub arrow_inset: f32,
    /// How far the box moves along the side after it lines up: toward the
    /// end of lines on the top and bottom, down on the start and end. A
    /// submenu moves up by its padding so its first item lines up with the
    /// item that opened it.
    pub offset: f32,
}

impl Default for Anchoring {
    fn default() -> Self {
        Anchoring {
            side: Side::Bottom,
            align: AnchorAlign::Start,
            gap: 4.0,
            margin: 8.0,
            flip: true,
            shift: true,
            fit: true,
            match_anchor: false,
            arrow_inset: 0.0,
            offset: 0.0,
        }
    }
}

/// Where an anchored box went.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AnchorPlacement {
    /// Its box, in the window.
    pub rect: Rect,
    /// The side it went to, after any flip.
    pub side: Side,
    /// Where an arrow pointing at the anchor's middle goes, along the box's
    /// edge that faces the anchor, from that edge's left or top end.
    pub arrow: f32,
    /// The room on its side: the most it could take.
    pub room: Vec2,
}

/// The side's screen edge: whether it is above or below (`vertical`), and
/// whether it is the far one (below, or right).
fn edge(side: Side, rtl: bool) -> (bool, bool) {
    match side {
        Side::Top => (true, false),
        Side::Bottom => (true, true),
        Side::Start => (false, rtl),
        Side::End => (false, !rtl),
    }
}

fn opposite(side: Side) -> Side {
    match side {
        Side::Top => Side::Bottom,
        Side::Bottom => Side::Top,
        Side::Start => Side::End,
        Side::End => Side::Start,
    }
}

/// Place a box of `natural` size against `anchor` within `bounds`, right to
/// left when `rtl`.
pub fn place(
    anchor: Rect,
    natural: Vec2,
    bounds: Rect,
    how: &Anchoring,
    rtl: bool,
) -> AnchorPlacement {
    let inner = Rect {
        x: bounds.x + how.margin,
        y: bounds.y + how.margin,
        w: (bounds.w - 2.0 * how.margin).max(0.0),
        h: (bounds.h - 2.0 * how.margin).max(0.0),
    };
    // The room on each side of the anchor, the gap taken.
    let room = |side: Side| {
        let (vertical, far) = edge(side, rtl);
        match (vertical, far) {
            (true, false) => anchor.y - how.gap - inner.y,
            (true, true) => inner.y + inner.h - (anchor.y + anchor.h + how.gap),
            (false, false) => anchor.x - how.gap - inner.x,
            (false, true) => inner.x + inner.w - (anchor.x + anchor.w + how.gap),
        }
        .max(0.0)
    };
    let need = |side: Side| {
        if edge(side, rtl).0 {
            natural.y
        } else {
            natural.x
        }
    };
    let mut side = how.side;
    if how.flip && room(side) < need(side) && room(opposite(side)) > room(side) {
        side = opposite(side);
    }
    let (vertical, far) = edge(side, rtl);
    // Main runs away from the anchor, cross along its side.
    let (anchor_main, anchor_main_len, anchor_cross, anchor_cross_len) = if vertical {
        (anchor.y, anchor.h, anchor.x, anchor.w)
    } else {
        (anchor.x, anchor.w, anchor.y, anchor.h)
    };
    let (inner_main, inner_main_len, inner_cross, inner_cross_len) = if vertical {
        (inner.y, inner.h, inner.x, inner.w)
    } else {
        (inner.x, inner.w, inner.y, inner.h)
    };
    let (mut main_len, mut cross_len) = if vertical {
        (natural.y, natural.x)
    } else {
        (natural.x, natural.y)
    };
    if how.match_anchor {
        cross_len = cross_len.max(anchor_cross_len);
    }
    let main_room = room(side);
    if how.fit {
        main_len = main_len.min(main_room);
        cross_len = cross_len.min(inner_cross_len);
    }
    let mut main = if far {
        anchor_main + anchor_main_len + how.gap
    } else {
        anchor_main - how.gap - main_len
    };
    // Along the side, start and end follow the reading direction where the
    // side runs across it.
    let flipped = vertical && rtl;
    let mut cross = match (how.align, flipped) {
        (AnchorAlign::Start, false) | (AnchorAlign::End, true) => anchor_cross,
        (AnchorAlign::Center, _) => anchor_cross + (anchor_cross_len - cross_len) * 0.5,
        (AnchorAlign::End, false) | (AnchorAlign::Start, true) => {
            anchor_cross + anchor_cross_len - cross_len
        }
    } + if flipped { -how.offset } else { how.offset };
    if how.shift {
        let keep = |v: f32, len: f32, start: f32, extent: f32| {
            if len >= extent {
                start
            } else {
                v.clamp(start, start + extent - len)
            }
        };
        cross = keep(cross, cross_len, inner_cross, inner_cross_len);
        main = keep(main, main_len, inner_main, inner_main_len);
    }
    let middle = anchor_cross + anchor_cross_len * 0.5 - cross;
    let arrow = if cross_len > 2.0 * how.arrow_inset {
        middle.clamp(how.arrow_inset, cross_len - how.arrow_inset)
    } else {
        cross_len * 0.5
    };
    let (rect, room) = if vertical {
        (
            Rect::new(cross, main, cross_len, main_len),
            Vec2::new(inner_cross_len, main_room),
        )
    } else {
        (
            Rect::new(main, cross, main_len, cross_len),
            Vec2::new(main_room, inner_cross_len),
        )
    };
    AnchorPlacement {
        rect,
        side,
        arrow,
        room,
    }
}

/// One anchored layer.
#[derive(Debug, Clone, Copy)]
struct Entry {
    layer: NodeId,
    anchor: NodeId,
    how: Anchoring,
    /// The node moved to where the arrow goes.
    arrow: Option<NodeId>,
    /// The point of the anchor the layer sits against instead of its box,
    /// from the anchor's top-left.
    point: Option<Vec2>,
    /// The node of the layer put level with the anchor, over it.
    over: Option<NodeId>,
    placed: Option<AnchorPlacement>,
}

/// The anchored layers of a store, and the insets the system keeps. Cold.
#[derive(Debug, Default)]
pub(crate) struct Anchors {
    entries: Vec<Entry>,
    keep_out: Inset,
    rtl: bool,
}

impl Anchors {
    pub(crate) fn clear(&mut self) {
        self.entries.clear();
    }

    /// Forgets the slot `index`, freed or reused.
    pub(crate) fn forget(&mut self, index: u32) {
        if !self.entries.is_empty() {
            self.entries.retain(|e| e.layer.index() != index);
        }
    }

    fn entry(&self, layer: NodeId) -> Option<&Entry> {
        self.entries.iter().find(|e| e.layer == layer)
    }
}

impl NodeStore {
    /// Anchors node `id` to `anchor` the way `how` says, making it a layer;
    /// `None` puts it back in its parent's flow.
    pub fn set_anchor(&mut self, id: NodeId, anchor: Option<(NodeId, Anchoring)>) {
        if !self.arena().is_live(id) {
            return;
        }
        let at = self.anchors.entries.iter().position(|e| e.layer == id);
        match (anchor, at) {
            (Some((anchor, how)), Some(at)) => {
                let entry = &mut self.anchors.entries[at];
                if entry.anchor != anchor || entry.how != how {
                    entry.anchor = anchor;
                    entry.how = how;
                    self.place_anchored_layer(id);
                }
            }
            (Some((anchor, how)), None) => {
                self.anchors.entries.push(Entry {
                    layer: id,
                    anchor,
                    how,
                    arrow: None,
                    point: None,
                    over: None,
                    placed: None,
                });
                self.set_layer(id, true);
            }
            (None, Some(at)) => {
                self.anchors.entries.swap_remove(at);
                self.set_layer(id, false);
            }
            (None, None) => {}
        }
    }

    /// Moves node `arrow`, laid out at the top-left of anchored layer `id`,
    /// to where the layer's arrow goes, centred there.
    pub fn set_anchor_arrow(&mut self, id: NodeId, arrow: Option<NodeId>) {
        if let Some(entry) = self.anchors.entries.iter_mut().find(|e| e.layer == id) {
            entry.arrow = arrow;
        }
    }

    /// Sets anchored layer `id` against the window point `at` of its
    /// anchor rather than its box — a context menu at the press that asked
    /// for it — or, `None`, against the box again. The point moves with the
    /// anchor.
    pub fn set_anchor_point(&mut self, id: NodeId, at: Option<Vec2>) {
        let Some(entry) = self.anchors.entries.iter().find(|e| e.layer == id) else {
            return;
        };
        let origin = self.world(entry.anchor);
        let point = at.map(|p| Vec2::new(p.x - origin.x, p.y - origin.y));
        let entry = self
            .anchors
            .entries
            .iter_mut()
            .find(|e| e.layer == id)
            .expect("found above");
        if entry.point != point {
            entry.point = point;
            self.place_anchored_layer(id);
        }
    }

    /// Puts node `over` of anchored layer `id` level with the anchor, the
    /// layer over it — a list whose chosen row covers the field that opened
    /// it — while the layer fits the window's height; `None` sets it on its
    /// side again.
    pub fn set_anchor_over(&mut self, id: NodeId, over: Option<NodeId>) {
        let Some(entry) = self.anchors.entries.iter_mut().find(|e| e.layer == id) else {
            return;
        };
        if entry.over != over {
            entry.over = over;
            self.place_anchored_layer(id);
        }
    }

    /// Whether anchored layer `id` sits against a point of its anchor.
    pub(crate) fn anchored_at_point(&self, id: NodeId) -> bool {
        self.anchors.entry(id).is_some_and(|e| e.point.is_some())
    }

    /// The anchor of layer `id`, and how it sits against it.
    pub fn anchor_of(&self, id: NodeId) -> Option<(NodeId, Anchoring)> {
        self.anchors.entry(id).map(|e| (e.anchor, e.how))
    }

    /// Where anchored layer `id` went when it was last placed.
    pub fn anchor_placement(&self, id: NodeId) -> Option<AnchorPlacement> {
        self.anchors.entry(id)?.placed
    }

    /// Keep anchored layers out of `inset` of the surface — the system's
    /// insets, a notch's, a home indicator's — and lay their sides out right
    /// to left when `rtl`.
    pub fn set_keep_out(&mut self, inset: Inset, rtl: bool) {
        if self.anchors.keep_out == inset && self.anchors.rtl == rtl {
            return;
        }
        self.anchors.keep_out = inset;
        self.anchors.rtl = rtl;
        self.follow_anchors();
    }

    /// Where anchored layer `index` goes, and its box in layout's terms:
    /// `None` for no anchored layer, or one whose anchor is gone. Its content
    /// is measured.
    pub(crate) fn anchored_box(
        &self,
        index: u32,
        surface: Rect,
    ) -> Option<(AnchorPlacement, Rect)> {
        if self.anchors.entries.is_empty() {
            return None;
        }
        let entry = self
            .anchors
            .entries
            .iter()
            .find(|e| e.layer.index() == index)?;
        if !self.arena().is_live(entry.anchor) {
            return None;
        }
        let keep = self.anchors.keep_out;
        let bounds = Rect {
            x: surface.x + keep.left,
            y: surface.y + keep.top,
            w: (surface.w - keep.left - keep.right).max(0.0),
            h: (surface.h - keep.top - keep.bottom).max(0.0),
        };
        let measured = LayoutTree::measured(self, index);
        let size = LayoutTree::input(self, index).size();
        let natural = |length: Length, natural: f32, room: f32| match length {
            Length::Fixed(v) => v,
            Length::Fill { .. } => room,
            _ => natural,
        };
        let natural = Vec2::new(
            natural(size.width, measured.w, bounds.w),
            natural(size.height, measured.h, bounds.h),
        );
        let mut anchor = self.world(entry.anchor);
        if let Some(p) = entry.point {
            anchor = Rect::new(anchor.x + p.x, anchor.y + p.y, 0.0, 0.0);
        }
        let mut placed = place(anchor, natural, bounds, &entry.how, self.anchors.rtl);
        if let Some(top) = entry.over.and_then(|over| {
            self.over_top(entry.layer, over, anchor, natural.y, bounds, &entry.how)
        }) {
            placed.rect.y = top;
            placed.rect.h = natural.y;
        }
        // Layout's boxes are the world's before the ancestors' transforms.
        let shift = self.parent(entry.layer).map_or(Vec2::ZERO, |p| {
            let t = self.world_transform(p).translation();
            Vec2::new(t.x, t.y)
        });
        let rect = placed.rect;
        Some((
            placed,
            Rect::new(rect.x - shift.x, rect.y - shift.y, rect.w, rect.h),
        ))
    }

    /// Where the top of layer `layer`, `height` tall, goes to put its node
    /// `over` level with `anchor`, as the last layout placed `over` in it:
    /// `None` until it is laid out, when it does not fit the window, or when
    /// `over` sits in a viewport that scrolls.
    fn over_top(
        &self,
        layer: NodeId,
        over: NodeId,
        anchor: Rect,
        height: f32,
        bounds: Rect,
        how: &Anchoring,
    ) -> Option<f32> {
        if !self.arena().is_live(over) || !self.is_under(over, layer) {
            return None;
        }
        let (top, bottom) = (bounds.y + how.margin, bounds.y + bounds.h - how.margin);
        let (l, c) = (self.bounds(layer), self.bounds(over));
        if l.h <= 0.0 || height > bottom - top {
            return None;
        }
        // A list that scrolls keeps its place below the anchor: where its
        // node shows depends on how far it is scrolled.
        let mut at = self.parent(over);
        while let Some(id) = at.filter(|&id| id != layer) {
            if self
                .scroll_axis(id)
                .is_some_and(|axis| self.scroll_range(id, axis) > 0.0)
            {
                return None;
            }
            at = self.parent(id);
        }
        let y = anchor.y + anchor.h * 0.5 - (c.y - l.y + c.h * 0.5);
        Some(y.clamp(top, bottom - height))
    }

    /// Note where anchored layer `id` went, and move its arrow there.
    pub(crate) fn placed_anchored(&mut self, id: NodeId, placed: AnchorPlacement) {
        let Some(entry) = self.anchors.entries.iter_mut().find(|e| e.layer == id) else {
            return;
        };
        entry.placed = Some(placed);
        let Some(arrow) = entry.arrow.filter(|&a| self.arena().is_live(a)) else {
            return;
        };
        let size = self.bounds(arrow);
        let r = placed.rect;
        let (vertical, far) = edge(placed.side, self.anchors.rtl);
        // The arrow straddles the edge that faces the anchor.
        let at = match (vertical, far) {
            (true, true) => Vec2::new(placed.arrow - size.w * 0.5, -size.h * 0.5),
            (true, false) => Vec2::new(placed.arrow - size.w * 0.5, r.h - size.h * 0.5),
            (false, true) => Vec2::new(-size.w * 0.5, placed.arrow - size.h * 0.5),
            (false, false) => Vec2::new(r.w - size.w * 0.5, placed.arrow - size.h * 0.5),
        };
        let laid = self.bounds(id);
        let origin = self.bounds(arrow);
        self.set_translate(
            arrow,
            Vec2::new(laid.x + at.x - origin.x, laid.y + at.y - origin.y),
        );
    }

    /// Re-place every shown anchored layer whose anchor or size moved it,
    /// as a scroll or a translate of the anchor does.
    pub fn follow_anchors(&mut self) {
        if self.anchors.entries.is_empty() {
            return;
        }
        let layers: Vec<NodeId> = self.anchors.entries.iter().map(|e| e.layer).collect();
        for layer in layers {
            self.place_anchored_layer(layer);
        }
    }

    /// Lay anchored layer `id` out again where it goes now, when that moved.
    fn place_anchored_layer(&mut self, id: NodeId) {
        let Some(surface) = self.layer_surface() else {
            return;
        };
        if !self.arena().is_live(id) || self.hidden(id) {
            return;
        }
        let Some((placed, laid)) = self.anchored_box(id.index(), surface) else {
            return;
        };
        if self.bounds(id) != laid || self.anchor_placement(id) != Some(placed) {
            self.lay_layer(id, surface, &mut Vec::new());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WINDOW: Rect = Rect {
        x: 0.0,
        y: 0.0,
        w: 400.0,
        h: 300.0,
    };

    fn below() -> Anchoring {
        Anchoring {
            gap: 4.0,
            margin: 8.0,
            ..Anchoring::default()
        }
    }

    #[test]
    fn a_box_sits_below_its_anchor_lined_up_with_its_start() {
        let anchor = Rect::new(50.0, 40.0, 80.0, 20.0);
        let p = place(anchor, Vec2::new(120.0, 60.0), WINDOW, &below(), false);
        assert_eq!(p.side, Side::Bottom);
        assert_eq!(p.rect, Rect::new(50.0, 64.0, 120.0, 60.0));
        assert_eq!(p.arrow, 40.0, "the anchor's middle");
        let rtl = place(anchor, Vec2::new(120.0, 60.0), WINDOW, &below(), true);
        assert_eq!(rtl.rect.x, 130.0 - 120.0, "its end with the anchor's");
        let centred = Anchoring {
            align: AnchorAlign::Center,
            ..below()
        };
        let c = place(anchor, Vec2::new(120.0, 60.0), WINDOW, &centred, false);
        assert_eq!(c.rect.x, 30.0);
    }

    #[test]
    fn a_box_without_room_below_flips_above_and_one_too_wide_shifts_in() {
        let anchor = Rect::new(350.0, 250.0, 40.0, 20.0);
        let p = place(anchor, Vec2::new(120.0, 100.0), WINDOW, &below(), false);
        assert_eq!(p.side, Side::Top, "no room below");
        assert_eq!(p.rect.y, 250.0 - 4.0 - 100.0);
        assert_eq!(p.rect.x, 400.0 - 8.0 - 120.0, "shifted inside the window");
        assert_eq!(p.arrow, 370.0 - p.rect.x, "still at the anchor's middle");
        let fixed = Anchoring {
            flip: false,
            shift: false,
            fit: false,
            ..below()
        };
        let q = place(anchor, Vec2::new(120.0, 100.0), WINDOW, &fixed, false);
        assert_eq!(q.rect, Rect::new(350.0, 274.0, 120.0, 100.0));
    }

    #[test]
    fn a_long_box_fits_the_room_on_its_side_and_can_match_its_anchor() {
        let anchor = Rect::new(20.0, 100.0, 200.0, 24.0);
        let how = Anchoring {
            match_anchor: true,
            ..below()
        };
        let p = place(anchor, Vec2::new(150.0, 500.0), WINDOW, &how, false);
        assert_eq!(p.side, Side::Bottom, "more room below than above");
        assert_eq!(p.rect.w, 200.0, "as wide as the anchor");
        assert_eq!(p.rect.h, 300.0 - 8.0 - 128.0, "the room below");
        assert_eq!(p.room.y, p.rect.h);
    }

    #[test]
    fn a_side_box_centres_on_its_anchor_and_keeps_its_arrow_off_the_corners() {
        let anchor = Rect::new(100.0, 10.0, 20.0, 20.0);
        let how = Anchoring {
            side: Side::End,
            align: AnchorAlign::Center,
            arrow_inset: 12.0,
            ..below()
        };
        let p = place(anchor, Vec2::new(80.0, 120.0), WINDOW, &how, false);
        assert_eq!(p.side, Side::End);
        assert_eq!(p.rect.x, 124.0);
        assert_eq!(p.rect.y, 8.0, "shifted down into the window");
        assert_eq!(p.arrow, 12.0, "held off the corner");
        let rtl = place(anchor, Vec2::new(80.0, 120.0), WINDOW, &how, true);
        assert_eq!(rtl.rect.x, 100.0 - 4.0 - 80.0, "the end is the left");
    }

    /// A 400 by 300 column holding a row (whose translate moves the anchor
    /// within it) with an 80 by 20 anchor at 50, 40, and a floating box of
    /// one 120 by 60 child.
    fn scene() -> (NodeStore, NodeId, NodeId, NodeId, NodeId) {
        use crate::component::BuildCx;
        use crate::{FlexStyle, Inset, LeafStyle, Sizing};
        let mut store = NodeStore::new();
        let (mut row, mut anchor, mut floater) = (None, None, None);
        let root = {
            let mut cx = BuildCx::new(&mut store);
            cx.flex(
                FlexStyle {
                    size: Sizing::fixed(400.0, 300.0),
                    padding: Inset {
                        left: 50.0,
                        top: 40.0,
                        ..Inset::default()
                    },
                    axis: crate::Axis::Column,
                    ..FlexStyle::default()
                },
                |cx| {
                    row = Some(
                        cx.flex(
                            FlexStyle {
                                size: Sizing {
                                    width: Length::Fit,
                                    height: Length::Fit,
                                },
                                ..FlexStyle::default()
                            },
                            |cx| {
                                anchor = Some(
                                    cx.leaf(LeafStyle {
                                        size: Sizing::fixed(80.0, 20.0),
                                        ..LeafStyle::default()
                                    })
                                    .id(),
                                );
                            },
                        )
                        .id(),
                    );
                    let fit = Sizing {
                        width: Length::Fit,
                        height: Length::Fit,
                    };
                    floater = Some(
                        cx.flex(
                            FlexStyle {
                                size: fit,
                                ..FlexStyle::default()
                            },
                            |cx| {
                                cx.leaf(LeafStyle {
                                    size: Sizing::fixed(120.0, 60.0),
                                    ..LeafStyle::default()
                                });
                            },
                        )
                        .id(),
                    );
                },
            )
            .id()
        };
        let (anchor, floater) = (anchor.unwrap(), floater.unwrap());
        store.set_anchor(floater, Some((anchor, below())));
        store.layout(root, WINDOW, &mut Vec::new());
        (store, root, row.unwrap(), anchor, floater)
    }

    #[test]
    fn an_anchored_layer_lays_out_against_its_anchor_and_follows_it() {
        let (mut store, root, row, anchor, floater) = scene();
        assert!(store.is_layer(floater));
        assert_eq!(store.world(anchor), Rect::new(50.0, 40.0, 80.0, 20.0));
        assert_eq!(store.world(floater), Rect::new(50.0, 64.0, 120.0, 60.0));
        assert_eq!(
            store.world(store.arena().children(floater).next().unwrap()),
            Rect::new(50.0, 64.0, 120.0, 60.0),
            "its content with it"
        );
        assert_eq!(store.world(row).h, 20.0, "it takes no room in the column");
        store.set_translate(row, Vec2::new(0.0, 200.0));
        store.follow_anchors();
        let p = store.anchor_placement(floater).unwrap();
        assert_eq!(p.side, Side::Top, "the anchor moved down: no room below");
        assert_eq!(store.world(floater), Rect::new(50.0, 176.0, 120.0, 60.0));
        store.set_anchor(floater, None);
        store.layout(root, WINDOW, &mut Vec::new());
        assert!(!store.is_layer(floater));
    }

    #[test]
    fn the_system_s_insets_keep_an_anchored_layer_out() {
        let (mut store, _, _, _, floater) = scene();
        store.set_keep_out(
            Inset {
                left: 0.0,
                top: 0.0,
                right: 0.0,
                bottom: 200.0,
            },
            false,
        );
        let p = store.anchor_placement(floater).unwrap();
        assert_eq!(p.side, Side::Bottom);
        assert_eq!(p.rect.h, 100.0 - 8.0 - 64.0, "fitted above the inset");
    }
}
