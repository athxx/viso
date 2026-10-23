//! Sticky nodes: a node that stays at its scroll viewport's start edge while
//! its parent passes under it.
//!
//! A sticky node lays out where its parent puts it. Once its viewport scrolls
//! it past the start edge, a pin — a translate on top of its own — holds it
//! at the edge, until its parent's end pushes it on: a section's header
//! leaves with its section. A virtual list pins its section headers the same
//! way, each pushed on by the next ([`crate::virtual_list`]). Pinning only
//! moves the node (`TRANSFORM`); a pinned node paints and is hit above its
//! siblings.

use crate::adaptive::LayoutDirection;
use crate::component::{BuildCx, Handle, NodeStore};
use crate::layout::{Axis, OnAxis, Vec2};
use crate::node::NodeId;

/// The pinned nodes of a store.
#[derive(Debug, Default)]
pub(crate) struct Stickies {
    list: Vec<Sticky>,
}

#[derive(Debug, Clone, Copy)]
struct Sticky {
    node: NodeId,
    /// The translate the pin adds to the node's own.
    pin: Vec2,
    /// Whether the store pins it (`set_sticky`), not a virtual list.
    own: bool,
}

impl Stickies {
    pub(crate) fn clear(&mut self) {
        self.list.clear();
    }

    /// Forgets the node of the slot `index`, freed or reused.
    pub(crate) fn forget(&mut self, index: u32) {
        self.list.retain(|s| s.node.index() != index);
    }

    fn find(&self, id: NodeId) -> Option<usize> {
        self.list.iter().position(|s| s.node == id)
    }
}

impl BuildCx<'_> {
    /// Make `handle`'s node sticky, or not: see [`NodeStore::set_sticky`].
    pub fn sticky(&mut self, handle: Handle, on: bool) {
        self.store.set_sticky(handle.id(), on);
    }
}

impl NodeStore {
    /// Make node `id` stick to its scroll viewport's start edge while its
    /// parent passes under it, or let it scroll with the rest again.
    pub fn set_sticky(&mut self, id: NodeId, on: bool) {
        if !self.arena().is_live(id) {
            return;
        }
        match (self.stickies.find(id), on) {
            (Some(k), true) => self.stickies.list[k].own = true,
            (None, true) => {
                self.stickies.list.push(Sticky {
                    node: id,
                    pin: Vec2::ZERO,
                    own: true,
                });
                self.restack(id);
            }
            (Some(_), false) => self.unpin(id),
            (None, false) => {}
        }
    }

    /// Whether node `id` is sticky.
    pub fn is_sticky(&self, id: NodeId) -> bool {
        self.stickies
            .find(id)
            .is_some_and(|k| self.stickies.list[k].own)
    }

    /// The translate node `id`'s pin adds to its own.
    #[inline]
    pub(crate) fn pin_of(&self, id: NodeId) -> Vec2 {
        let pin = if self.stickies.list.is_empty() {
            Vec2::ZERO
        } else {
            self.stickies
                .find(id)
                .map_or(Vec2::ZERO, |k| self.stickies.list[k].pin)
        };
        pin + self.reorder_shift_of(id) + self.sheet_slide_of(id) + self.drag_preview_offset_of(id)
    }

    /// Pin node `id` `pin` past where it lays out, raising it above its
    /// siblings: a virtual list's header row.
    pub(crate) fn pin(&mut self, id: NodeId, pin: Vec2) {
        let k = match self.stickies.find(id) {
            Some(k) => k,
            None => {
                self.stickies.list.push(Sticky {
                    node: id,
                    pin: Vec2::ZERO,
                    own: false,
                });
                self.restack(id);
                self.stickies.list.len() - 1
            }
        };
        self.repin(k, pin);
    }

    /// Unpin node `id`: it lays out and paints among its siblings again.
    pub(crate) fn unpin(&mut self, id: NodeId) {
        let Some(k) = self.stickies.find(id) else {
            return;
        };
        self.repin(k, Vec2::ZERO);
        self.stickies.list.swap_remove(k);
        self.restack(id);
    }

    /// Move entry `k`'s pin to `pin`, keeping the node's own translate.
    fn repin(&mut self, k: usize, pin: Vec2) {
        let s = self.stickies.list[k];
        if s.pin == pin {
            return;
        }
        self.stickies.list[k].pin = pin;
        if self.arena().is_live(s.node) {
            let own = self.translate(s.node) - s.pin;
            self.put_translate(s.node, own + pin);
        }
    }

    /// Repaint node `id`'s parent, whose paint order its raise changed.
    fn restack(&mut self, id: NodeId) {
        if let Some(parent) = self.arena().parent(id) {
            self.restack_children(parent);
        }
    }

    /// Whether node `id` is pinned, which raises it above its siblings.
    #[inline]
    pub(crate) fn is_pinned(&self, id: NodeId) -> bool {
        !self.stickies.list.is_empty() && self.stickies.find(id).is_some()
    }

    /// Whether a child of node `id` is pinned.
    #[inline]
    pub(crate) fn raises_children(&self, id: NodeId) -> bool {
        !self.stickies.list.is_empty()
            && self
                .stickies
                .list
                .iter()
                .any(|s| self.arena().parent(s.node) == Some(id))
    }

    /// Pin each sticky node where its viewport's scroll and its parent's box
    /// put it. Runs after layout, each frame.
    pub(crate) fn sync_stickies(&mut self) {
        for k in 0..self.stickies.list.len() {
            let s = self.stickies.list[k];
            if s.own && self.arena().is_live(s.node) {
                let pin = self.sticky_pin(s.node);
                self.repin(k, pin);
            }
        }
    }

    /// The pin that holds node `id` at its viewport's start edge, within its
    /// parent's box.
    fn sticky_pin(&self, id: NodeId) -> Vec2 {
        let arena = self.arena();
        let Some(parent) = arena.parent(id) else {
            return Vec2::ZERO;
        };
        let mut at = parent;
        while !self.is_scroll(at) {
            match arena.parent(at) {
                Some(up) => at = up,
                None => return Vec2::ZERO,
            }
        }
        let viewport = at;
        let Some(axis) = self.scroll_axis(viewport) else {
            return Vec2::ZERO;
        };
        let span = |id: NodeId| {
            let b = self.bounds(id);
            match axis {
                Axis::Column => (b.y, b.y + b.h),
                Axis::Row => (b.x, b.x + b.w),
            }
        };
        let scroll = self.scroll(viewport).on(axis);
        let (seen0, seen1) = span(viewport);
        let (seen0, seen1) = (seen0 + scroll, seen1 + scroll);
        let (own0, own1) = span(id);
        let (room0, room1) = span(parent);
        let rtl = axis == Axis::Row && self.direction_at(viewport.index()) == LayoutDirection::Rtl;
        // Read right to left a row starts at its right edge.
        let pin = if rtl {
            (seen1 - own1).clamp((room0 - own0).min(0.0), 0.0)
        } else {
            (seen0 - own0).clamp(0.0, (room1 - own1).max(0.0))
        };
        match axis {
            Axis::Column => Vec2 { x: 0.0, y: pin },
            Axis::Row => Vec2 { x: pin, y: 0.0 },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::component::{FlexStyle, LeafStyle, ScrollStyle};
    use crate::dirty::DirtyClass;
    use crate::layout::{Length, Size};
    use crate::style::BoxStyle;
    use viso_render::{Rect, Rgba};

    /// A 100-tall viewport over two 300-tall sections, each led by a sticky
    /// 20-tall header.
    fn sections(store: &mut NodeStore) -> (NodeId, [NodeId; 2], [NodeId; 2]) {
        let mut cx = BuildCx::new(store);
        let mut headers = Vec::new();
        let mut rows = Vec::new();
        let viewport = cx
            .scroll(
                ScrollStyle {
                    axis: Axis::Column,
                    size: Size::fixed(100.0, 100.0),
                    ..ScrollStyle::default()
                },
                |cx| {
                    cx.flex(
                        FlexStyle {
                            axis: Axis::Column,
                            ..FlexStyle::default()
                        },
                        |cx| {
                            for _ in 0..2 {
                                cx.flex(
                                    FlexStyle {
                                        axis: Axis::Column,
                                        size: Size {
                                            width: Length::fill(),
                                            height: Length::Fixed(300.0),
                                        },
                                        ..FlexStyle::default()
                                    },
                                    |cx| {
                                        let header = cx.leaf(LeafStyle {
                                            size: Size::fixed(100.0, 20.0),
                                            style: BoxStyle::solid(Rgba::new(1.0, 0.0, 0.0, 1.0)),
                                        });
                                        cx.sticky(header, true);
                                        headers.push(header.id());
                                        let row = cx.leaf(LeafStyle {
                                            size: Size::fixed(100.0, 40.0),
                                            style: BoxStyle::solid(Rgba::new(0.0, 0.0, 1.0, 1.0)),
                                        });
                                        rows.push(row.id());
                                    },
                                );
                            }
                        },
                    );
                },
            )
            .id();
        let surface = Rect {
            x: 0.0,
            y: 0.0,
            w: 100.0,
            h: 100.0,
        };
        store.layout(viewport, surface, &mut Vec::new());
        (viewport, [headers[0], headers[1]], [rows[0], rows[1]])
    }

    fn relayout(store: &mut NodeStore, root: NodeId) {
        let surface = Rect {
            x: 0.0,
            y: 0.0,
            w: 100.0,
            h: 100.0,
        };
        store.relayout_dirty(root, surface, &mut Vec::new(), &mut Vec::new());
    }

    #[test]
    fn a_header_sticks_until_its_section_pushes_it_on() {
        let mut store = NodeStore::new();
        let (viewport, [first, second], _) = sections(&mut store);
        assert!(store.is_sticky(first));
        assert_eq!(store.translate(first), Vec2::ZERO);
        for (scroll, pinned) in [(50.0, 50.0), (280.0, 280.0), (290.0, 280.0)] {
            store.set_scroll(viewport, Vec2 { x: 0.0, y: scroll });
            relayout(&mut store, viewport);
            assert_eq!(store.translate(first).y, pinned, "at {scroll}");
        }
        assert_eq!(store.translate(second).y, 0.0, "not reached yet");
        store.set_scroll(viewport, Vec2 { x: 0.0, y: 350.0 });
        relayout(&mut store, viewport);
        assert_eq!(store.translate(second).y, 50.0);
    }

    #[test]
    fn pinning_only_moves_and_raises_the_header() {
        let mut store = NodeStore::new();
        let (viewport, [first, _], [row, _]) = sections(&mut store);
        let section = store.arena().parent(first).unwrap();
        assert!(store.is_layered(section), "the header paints over its rows");
        assert!(store.stack_layer(first) > store.stack_layer(row));
        store.clear_dirty();
        store.set_scroll(viewport, Vec2 { x: 0.0, y: 30.0 });
        relayout(&mut store, viewport);
        let dirty = store.dirty(first);
        assert!(dirty.contains(DirtyClass::TRANSFORM));
        assert!(!dirty.intersects(DirtyClass::MEASURE | DirtyClass::LAYOUT));
    }

    #[test]
    fn a_pinned_header_takes_the_hits_over_the_rows_under_it() {
        let mut store = NodeStore::new();
        let (viewport, [first, _], [row, _]) = sections(&mut store);
        store.set_hittable(first, true);
        store.set_hittable(row, true);
        let hit =
            |store: &NodeStore| crate::hit_test::HitTestTree::hit(store, viewport, 50.0, 10.0);
        assert_eq!(hit(&store), Some(first));
        // The row, 20 to 60, slides under the header pinned at 0 to 20.
        store.set_scroll(viewport, Vec2 { x: 0.0, y: 30.0 });
        relayout(&mut store, viewport);
        assert_eq!(hit(&store), Some(first));
        store.set_sticky(first, false);
        assert_eq!(hit(&store), Some(row));
    }

    #[test]
    fn a_header_raised_or_lowered_paints_as_a_whole_repaint_would() {
        let mut store = NodeStore::new();
        let (viewport, [first, second], _) = sections(&mut store);
        let mut out = Vec::new();
        store.repaint(viewport, &mut out);
        let fills = |out: &[viso_render::Primitive]| -> Vec<f32> {
            out.iter()
                .filter_map(|p| match p {
                    viso_render::Primitive::Quad(q) => Some(q.color.r),
                    _ => None,
                })
                .collect()
        };
        // Each section paints its row, then its header over it.
        assert_eq!(fills(&out), [0.0, 1.0, 0.0, 1.0]);
        for (node, on) in [(first, false), (second, false), (first, true)] {
            store.clear_dirty();
            store.set_sticky(node, on);
            store.repaint_dirty(viewport, &mut out);
            let mut whole = Vec::new();
            store.repaint(viewport, &mut whole);
            assert_eq!(out, whole);
        }
        assert_eq!(fills(&out), [0.0, 1.0, 1.0, 0.0]);
    }

    #[test]
    fn a_pin_keeps_the_nodes_own_translate() {
        let mut store = NodeStore::new();
        let (viewport, [first, _], _) = sections(&mut store);
        store.set_translate(first, Vec2 { x: 5.0, y: 1.0 });
        store.set_scroll(viewport, Vec2 { x: 0.0, y: 40.0 });
        relayout(&mut store, viewport);
        assert_eq!(store.translate(first), Vec2 { x: 5.0, y: 41.0 });
        store.set_translate(first, Vec2 { x: 5.0, y: 2.0 });
        assert_eq!(store.translate(first), Vec2 { x: 5.0, y: 42.0 });
        store.set_sticky(first, false);
        assert_eq!(store.translate(first), Vec2 { x: 5.0, y: 2.0 });
        assert!(!store.is_layered(store.arena().parent(first).unwrap()));
    }
}
