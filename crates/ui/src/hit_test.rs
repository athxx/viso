//! Hit testing: map a point to the topmost node that owns it.
//!
//! [`HitTestTree`] is a stateless query over a [`NodeStore`] — the retained
//! arena *is* the tree, so there is no separate hit structure to keep in sync.
//! Given a point, it returns the topmost hittable [`NodeId`] whose rect contains
//! that point, or `None` when the point misses the whole subtree.
//!
//! What is hit is what is seen. The top layer paints over the scene, so the
//! overlays the last paint drew are tried first, the last drawn first, each
//! free of the scroll clips above it as its paint is. The scene is then
//! descended in **reverse** sibling order — the mirror of `paint_tree`'s
//! forward walk, where a later sibling paints on top — so the visually-topmost
//! child is tested first and wins; a hidden node is folded out and neither it
//! nor anything under it is hit, while a fully transparent one still holds its
//! place and is. The descent prunes on
//! each node's [`reach`](NodeStore::reach) — its box joined with everything
//! its subtree places outside it — so a child that overflows a container that
//! does not clip is hit where it is drawn, and a subtree whose reach misses the
//! point is skipped: a targeted route, not a full-tree scan.
//!
//! Hit testing tests a node's on-screen rect (not `bounds`): the descent carries
//! the displacement the scrolls and translates above apply — the pointer mapped
//! through each transform root's inverse — so a scrolled child hit-tests where
//! it visibly sits with nothing recomputed after the scroll. The descent also
//! carries a `clip` rect — the intersection of every enclosing scroll
//! viewport's box — so a point over content scrolled past a viewport's edge
//! misses, matching what paint clips away. Entering a scroll viewport narrows
//! the clip to its box before descending.

use crate::component::NodeStore;
use crate::layout::Vec2;
use crate::node::NodeId;
use viso_render::Rect;

/// A stateless hit-test query over a [`NodeStore`].
///
/// It owns no data; the struct is a home for future capture/clip state so the
/// call site stays stable as the input subsystem grows.
pub struct HitTestTree;

impl HitTestTree {
    /// The topmost hittable node whose world rect contains `(px, py)`, or `None`
    /// if the point misses the whole subtree at `root`.
    ///
    /// Tries the top layer under `root`, then descends the scene topmost-first
    /// (reverse sibling order), pruning any subtree whose (clipped) reach does
    /// not contain the point — no allocation, worst case the tree depth.
    pub fn hit(store: &NodeStore, root: NodeId, px: f32, py: f32) -> Option<NodeId> {
        if !store.arena().is_live(root) {
            return None;
        }
        for overlay in store.top_layer().rev() {
            if overlay != root
                && Self::holds(store, root, overlay)
                && let Some(hit) = Self::hit_clipped(
                    store,
                    overlay,
                    px,
                    py,
                    Rect::INFINITE,
                    store.shift_above(overlay),
                )
            {
                return Some(hit);
            }
        }
        // The initial clip is unbounded: nothing above the root clips it.
        let above = store.shift_above(root);
        Self::hit_clipped(store, root, px, py, Rect::INFINITE, above)
    }

    /// Whether live `node` sits in `root`'s subtree.
    fn holds(store: &NodeStore, root: NodeId, node: NodeId) -> bool {
        let arena = store.arena();
        let mut at = Some(node);
        while let Some(n) = at {
            if n == root {
                return true;
            }
            at = arena.parent(n);
        }
        false
    }

    /// Descend `root` for the topmost hit, with the point required to fall inside
    /// `clip` (the running intersection of enclosing scroll viewports), the
    /// scrolls and translates above `root` displacing it by `above`. An overlay
    /// below `root` is the top layer's, tried before the scene, and is skipped.
    fn hit_clipped(
        store: &NodeStore,
        root: NodeId,
        px: f32,
        py: f32,
        clip: Rect,
        above: Vec2,
    ) -> Option<NodeId> {
        let arena = store.arena();
        if !arena.is_live(root) || store.hidden(root) {
            return None;
        }
        let own = above + store.self_shift(root);
        let shift = store.snap_shift(own);
        // Nothing of the subtree is seen outside its reach, so a point there —
        // or outside an enclosing viewport — misses the whole subtree. Prune
        // without recursing.
        if !store
            .reach(root)
            .translate(shift)
            .intersect(clip)
            .contains(px, py)
        {
            return None;
        }
        let visible = store.bounds(root).translate(shift).intersect(clip);

        // A scroll viewport clips its children to its own box: narrow the clip
        // handed to the descent so content scrolled past the edge is unhittable.
        let (child_clip, inner) = if store.is_scroll(root) {
            (visible, own + store.content_shift(root))
        } else {
            (clip, own)
        };

        // Test children topmost-first: the last-painted (last) child is tested
        // first, walking back toward the first-painted (bottom) one.
        let mut child = arena.last_child(root);
        while let Some(c) = child {
            if !store.is_overlay(c)
                && let Some(hit) = Self::hit_clipped(store, c, px, py, child_clip, inner)
            {
                return Some(hit);
            }
            child = arena.prev_sibling(c);
        }

        // No child claimed the point: this node is the hit only if it is itself
        // hittable and its own box holds the point. A pass-through container
        // with no hittable child under the point returns `None`, letting a
        // lower sibling be tried by the caller.
        (store.hittable(root) && visible.contains(px, py)).then_some(root)
    }
}

/// The topmost hittable node whose rect contains `(px, py)`, or `None`. A free
/// function mirroring [`HitTestTree::hit`] for ergonomic call sites.
#[inline]
pub fn hit_test(store: &NodeStore, root: NodeId, px: f32, py: f32) -> Option<NodeId> {
    HitTestTree::hit(store, root, px, py)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::component::{BuildCx, FlexStyle, LeafStyle, NodeStore};
    use crate::layout::{Align, Axis, Inset, Length, Size};
    use crate::style::BoxStyle;
    use viso_render::{Rect, Rgba};

    const SURFACE: Rect = Rect {
        x: 0.0,
        y: 0.0,
        w: 200.0,
        h: 200.0,
    };

    const SOLID: Rgba = Rgba {
        r: 0.5,
        g: 0.5,
        b: 0.5,
        a: 1.0,
    };

    #[test]
    fn point_in_leaf_returns_that_leaf() {
        let mut store = NodeStore::new();
        let (root, first, second) = {
            let mut cx = BuildCx::new(&mut store);
            let mut ids = (None, None);
            cx.flex(
                FlexStyle {
                    axis: Axis::Row,
                    gap: 10.0,
                    size: Size::fill(),
                    ..Default::default()
                },
                |cx| {
                    ids.0 = Some(cx.leaf(LeafStyle {
                        size: Size::fixed(40.0, 40.0),
                        style: BoxStyle::solid(SOLID),
                    }));
                    ids.1 = Some(cx.leaf(LeafStyle {
                        size: Size::fixed(40.0, 40.0),
                        style: BoxStyle::solid(SOLID),
                    }));
                },
            );
            (cx.root().unwrap(), ids.0.unwrap(), ids.1.unwrap())
        };
        let mut scratch = Vec::new();
        store.layout(root, SURFACE, &mut scratch);

        // First leaf occupies [0,40); second, after a 10px gap, [50,90).
        assert_eq!(hit_test(&store, root, 20.0, 20.0), Some(first.id()));
        assert_eq!(hit_test(&store, root, 60.0, 20.0), Some(second.id()));
    }

    #[test]
    fn overlapping_children_return_topmost() {
        // A Stack-like overlap: both children fill the same box (a container with
        // no gap/padding and two fill-sized leaves stacked by absolute placement
        // is not available this slice, so overlap them by fixing both to the full
        // surface and axis Row with zero gap — the layout places them at the same
        // origin only if the container lets them; instead build a container whose
        // two children both start at x=0 by giving the container column axis so
        // they stack on the cross axis at the same x, then pick a point in the
        // shared x-range where both boxes cover the y).
        //
        // Simpler and deterministic: nest a child fully inside the parent's box;
        // the (topmost) inner child wins over the (bottom) parent for a shared
        // point.
        let mut store = NodeStore::new();
        let (root, inner) = {
            let mut cx = BuildCx::new(&mut store);
            let mut inner = None;
            cx.flex(
                FlexStyle {
                    axis: Axis::Row,
                    padding: Inset::all(20.0),
                    size: Size::fixed(100.0, 100.0),
                    style: BoxStyle::solid(SOLID),
                    ..Default::default()
                },
                |cx| {
                    inner = Some(cx.leaf(LeafStyle {
                        size: Size::fixed(40.0, 40.0),
                        style: BoxStyle::solid(SOLID),
                    }));
                },
            );
            (cx.root().unwrap(), inner.unwrap())
        };
        let mut scratch = Vec::new();
        store.layout(root, SURFACE, &mut scratch);

        // A point inside the inner leaf (which sits atop the container) returns
        // the inner leaf, not the container beneath it.
        assert_eq!(hit_test(&store, root, 30.0, 30.0), Some(inner.id()));
        // A point in the container's padding (no child there) returns the
        // container.
        assert_eq!(hit_test(&store, root, 5.0, 5.0), Some(root));
    }

    #[test]
    fn point_in_padding_returns_container_or_nothing_when_passthrough() {
        let mut store = NodeStore::new();
        let root = {
            let mut cx = BuildCx::new(&mut store);
            cx.flex(
                FlexStyle {
                    axis: Axis::Row,
                    padding: Inset::all(20.0),
                    align: Align::Center,
                    size: Size::fixed(100.0, 100.0),
                    style: BoxStyle::solid(SOLID),
                    ..Default::default()
                },
                |cx| {
                    cx.leaf(LeafStyle {
                        size: Size::fixed(20.0, 20.0),
                        style: BoxStyle::solid(SOLID),
                    });
                },
            );
            cx.root().unwrap()
        };
        let mut scratch = Vec::new();
        store.layout(root, SURFACE, &mut scratch);

        // A point in the padding lands on the container.
        assert_eq!(hit_test(&store, root, 5.0, 5.0), Some(root));

        // After marking the container pass-through, the same point (no hittable
        // child under it) resolves to nothing.
        store.set_hittable(root, false);
        assert_eq!(hit_test(&store, root, 5.0, 5.0), None);
    }

    #[test]
    fn point_outside_root_box_returns_none() {
        // The root fills the surface it is laid into, so a point outside the
        // surface is outside the root box — the whole subtree is pruned at the
        // root without recursing into any child (the targeted-route guarantee).
        let mut store = NodeStore::new();
        let root = {
            let mut cx = BuildCx::new(&mut store);
            cx.flex(
                FlexStyle {
                    axis: Axis::Row,
                    size: Size::fill(),
                    style: BoxStyle::solid(SOLID),
                    ..Default::default()
                },
                |cx| {
                    cx.leaf(LeafStyle {
                        size: Size::fixed(20.0, 20.0),
                        style: BoxStyle::solid(SOLID),
                    });
                },
            );
            cx.root().unwrap()
        };
        let mut scratch = Vec::new();
        store.layout(root, SURFACE, &mut scratch);

        // Beyond the 200x200 surface: pruned at the root, no hit.
        assert_eq!(hit_test(&store, root, 250.0, 250.0), None);
    }

    #[test]
    fn seam_between_tiling_siblings_hits_exactly_one() {
        // Two zero-gap fixed leaves tile [0,40) and [40,80) on the main axis; the
        // point at x=40 is on their shared seam. Half-open far edges mean the
        // first leaf (ending at 40, exclusive) misses and the second (starting at
        // 40, inclusive) claims it — exactly one hit, no double-claim.
        let mut store = NodeStore::new();
        let (root, first, second) = {
            let mut cx = BuildCx::new(&mut store);
            let mut ids = (None, None);
            cx.flex(
                FlexStyle {
                    axis: Axis::Row,
                    gap: 0.0,
                    size: Size {
                        width: Length::Fixed(80.0),
                        height: Length::Fixed(40.0),
                    },
                    ..Default::default()
                },
                |cx| {
                    ids.0 = Some(cx.leaf(LeafStyle {
                        size: Size::fixed(40.0, 40.0),
                        style: BoxStyle::solid(SOLID),
                    }));
                    ids.1 = Some(cx.leaf(LeafStyle {
                        size: Size::fixed(40.0, 40.0),
                        style: BoxStyle::solid(SOLID),
                    }));
                },
            );
            (cx.root().unwrap(), ids.0.unwrap(), ids.1.unwrap())
        };
        let mut scratch = Vec::new();
        store.layout(root, SURFACE, &mut scratch);

        assert_eq!(hit_test(&store, root, 20.0, 20.0), Some(first.id()));
        // On the seam: the far edge of the first is exclusive, so the second wins.
        assert_eq!(hit_test(&store, root, 40.0, 20.0), Some(second.id()));
        assert_eq!(hit_test(&store, root, 60.0, 20.0), Some(second.id()));
    }

    #[test]
    fn scroll_clips_hits_to_the_viewport_and_follows_the_offset() {
        use crate::component::ScrollStyle;
        use crate::layout::Vec2;

        // A 100×100 vertical viewport over a 100×300 content leaf.
        let mut store = NodeStore::new();
        let mut content = None;
        let root = {
            let mut cx = BuildCx::new(&mut store);
            cx.scroll(
                ScrollStyle {
                    axis: Axis::Column,
                    size: Size::fixed(100.0, 100.0),
                    ..Default::default()
                },
                |cx| {
                    content = Some(
                        cx.leaf(LeafStyle {
                            size: Size::fixed(100.0, 300.0),
                            style: BoxStyle::solid(SOLID),
                        })
                        .id(),
                    );
                },
            );
            cx.root().unwrap()
        };
        let content = content.unwrap();
        let mut scratch = Vec::new();
        store.layout(
            root,
            Rect {
                x: 0.0,
                y: 0.0,
                w: 100.0,
                h: 100.0,
            },
            &mut scratch,
        );

        // Unscrolled: a point inside the viewport hits the content; a point below
        // the viewport (content overflows there) is clipped away — a miss.
        assert_eq!(hit_test(&store, root, 50.0, 50.0), Some(content));
        assert_eq!(hit_test(&store, root, 50.0, 150.0), None);

        // Scroll down 100px: content y-range [100,200) now shows at [0,100). A
        // hit at viewport y=10 lands on content world-y 110, still the content.
        store.scroll_by(root, Vec2 { x: 0.0, y: 100.0 });
        assert_eq!(hit_test(&store, root, 50.0, 10.0), Some(content));
        // Above the viewport (negative side) stays a miss.
        assert_eq!(hit_test(&store, root, 50.0, 150.0), None);
    }

    /// A row over the surface holding a 100×40 card whose column of two
    /// 100×40 leaves runs 40 past its box, and a leaf beside it. Returns the
    /// row, the card and its two leaves.
    fn overflowing(store: &mut NodeStore) -> (NodeId, NodeId, NodeId, NodeId) {
        let mut ids = (None, None, None);
        let root = {
            let mut cx = BuildCx::new(store);
            cx.flex(
                FlexStyle {
                    axis: Axis::Row,
                    size: Size::fill(),
                    ..Default::default()
                },
                |cx| {
                    ids.0 = Some(
                        cx.flex(
                            FlexStyle {
                                axis: Axis::Column,
                                size: Size::fixed(100.0, 40.0),
                                ..Default::default()
                            },
                            |cx| {
                                ids.1 = Some(cx.leaf(LeafStyle {
                                    size: Size::fixed(100.0, 40.0),
                                    style: BoxStyle::solid(SOLID),
                                }));
                                ids.2 = Some(cx.leaf(LeafStyle {
                                    size: Size::fixed(100.0, 40.0),
                                    style: BoxStyle::solid(SOLID),
                                }));
                            },
                        )
                        .id(),
                    );
                    cx.leaf(LeafStyle {
                        size: Size::fixed(40.0, 40.0),
                        style: BoxStyle::solid(SOLID),
                    });
                },
            );
            cx.root().unwrap()
        };
        store.layout(root, SURFACE, &mut Vec::new());
        (
            root,
            ids.0.unwrap(),
            ids.1.unwrap().id(),
            ids.2.unwrap().id(),
        )
    }

    #[test]
    fn a_child_past_its_containers_box_is_hit_where_it_is_drawn() {
        let mut store = NodeStore::new();
        let (root, card, _, below) = overflowing(&mut store);
        assert_eq!(store.reach(card), Rect::new(0.0, 0.0, 100.0, 80.0));
        assert_eq!(hit_test(&store, root, 50.0, 60.0), Some(below));
        // Past everything the card reaches: the row itself.
        assert_eq!(hit_test(&store, root, 50.0, 90.0), Some(root));
    }

    #[test]
    fn a_child_relaid_past_its_container_is_hit_there() {
        let mut store = NodeStore::new();
        let (root, card, top, _) = overflowing(&mut store);
        store.set_fixed_size(top, Size::fixed(100.0, 120.0));
        store.relayout_dirty(root, SURFACE, &mut Vec::new(), &mut Vec::new());
        assert_eq!(store.reach(card), Rect::new(0.0, 0.0, 100.0, 160.0));
        assert_eq!(hit_test(&store, root, 50.0, 100.0), Some(top));
        assert!(store.reach(root).h >= 160.0);
    }

    #[test]
    fn a_child_translated_out_of_its_container_is_hit_where_it_slid_to() {
        use crate::layout::Vec2;

        let mut store = NodeStore::new();
        let (root, card, top, below) = overflowing(&mut store);
        store.set_translate(top, Vec2 { x: 0.0, y: -100.0 });
        let world = store.world(top);
        assert!(world.y >= 80.0, "slid past the card: {world:?}");
        let (x, y) = (world.x + world.w / 2.0, world.y + world.h / 2.0);
        assert_eq!(hit_test(&store, root, x, y), Some(top));
        assert!(store.reach(card).contains(x, y));
        // Where it was, nothing of it is left.
        assert_eq!(hit_test(&store, root, 50.0, 20.0), Some(card));
        assert_eq!(hit_test(&store, root, 50.0, 60.0), Some(below));
    }

    #[test]
    fn a_hidden_subtree_is_not_hit_and_a_transparent_one_is() {
        let mut store = NodeStore::new();
        let (root, card, top, below) = overflowing(&mut store);
        store.set_opacity(card, 0.0);
        assert_eq!(hit_test(&store, root, 50.0, 20.0), Some(top));
        assert_eq!(hit_test(&store, root, 50.0, 60.0), Some(below));
        store.set_hidden(card, true);
        store.relayout_dirty(root, SURFACE, &mut Vec::new(), &mut Vec::new());
        assert_eq!(hit_test(&store, root, 50.0, 20.0), Some(root));
        assert_eq!(hit_test(&store, root, 50.0, 60.0), Some(root));
    }

    #[test]
    fn an_overlay_is_hit_over_the_later_sibling_it_paints_over() {
        let mut store = NodeStore::new();
        let mut ids = (None, None);
        let root = {
            let mut cx = BuildCx::new(&mut store);
            cx.flex(
                FlexStyle {
                    axis: Axis::Column,
                    size: Size::fill(),
                    ..Default::default()
                },
                |cx| {
                    cx.flex(
                        FlexStyle {
                            axis: Axis::Column,
                            size: Size::fixed(100.0, 40.0),
                            ..Default::default()
                        },
                        |cx| {
                            let menu = cx.leaf(LeafStyle {
                                size: Size::fixed(100.0, 80.0),
                                style: BoxStyle::solid(SOLID),
                            });
                            ids.0 = Some(cx.set_overlay(menu, true).id());
                        },
                    );
                    ids.1 = Some(
                        cx.leaf(LeafStyle {
                            size: Size::fixed(100.0, 40.0),
                            style: BoxStyle::solid(SOLID),
                        })
                        .id(),
                    );
                },
            );
            cx.root().unwrap()
        };
        let (menu, field) = (ids.0.unwrap(), ids.1.unwrap());
        store.layout(root, SURFACE, &mut Vec::new());
        store.repaint(root, &mut Vec::new());
        assert_eq!(hit_test(&store, root, 50.0, 60.0), Some(menu));

        store.set_overlay(menu, false);
        store.repaint(root, &mut Vec::new());
        assert_eq!(hit_test(&store, root, 50.0, 60.0), Some(field));
    }

    #[test]
    fn a_fade_composites_over_all_its_subtree_draws() {
        use viso_render::Primitive;

        let mut store = NodeStore::new();
        let (root, card, _, _) = overflowing(&mut store);
        store.set_opacity(card, 0.5);
        let mut out = Vec::new();
        store.repaint(root, &mut out);
        let clip = out.iter().find_map(|p| match p {
            Primitive::Layer(layer) if layer.opacity < 1.0 => Some(layer.clip),
            _ => None,
        });
        assert_eq!(clip, Some(Rect::new(0.0, 0.0, 100.0, 80.0)));
    }

    #[test]
    fn a_fade_follows_a_child_that_grows_past_it() {
        use viso_render::Primitive;

        let mut store = NodeStore::new();
        let (root, card, top, _) = overflowing(&mut store);
        store.set_opacity(card, 0.5);
        let mut out = Vec::new();
        store.repaint(root, &mut out);
        store.clear_dirty();
        store.set_fixed_size(top, Size::fixed(100.0, 120.0));
        store.relayout_dirty(root, SURFACE, &mut Vec::new(), &mut Vec::new());
        store.repaint_dirty(root, &mut out);
        let clip = out.iter().find_map(|p| match p {
            Primitive::Layer(layer) if layer.opacity < 1.0 => Some(layer.clip),
            _ => None,
        });
        assert_eq!(clip, Some(Rect::new(0.0, 0.0, 100.0, 160.0)));
        let mut whole = Vec::new();
        store.repaint(root, &mut whole);
        assert_eq!(out, whole);
    }
}
