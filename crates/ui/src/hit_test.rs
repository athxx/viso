//! Hit testing: map a point to the topmost node that owns it.
//!
//! [`HitTestTree`] is a stateless query over a [`NodeStore`] — the retained
//! arena *is* the tree, so there is no separate hit structure to keep in sync.
//! Given a point, it returns the topmost hittable [`NodeId`] whose rect contains
//! that point, or `None` when the point misses the whole subtree.
//!
//! It descends children in **reverse** sibling order — the mirror of
//! `paint_tree`'s forward walk, where a later sibling paints on top — so the
//! visually-topmost child is tested first and wins. The descent prunes: it only
//! recurses into a subtree whose box contains the point, so it is a targeted
//! route, not a full-tree scan.
//!
//! This slice uses `bounds` directly as the hittable rect: layout already bakes
//! absolute positions into `bounds`, so a placed subtree hit-tests at its world
//! position with no separate transform to accumulate. When scroll/clip/transform
//! containers arrive, the descent gains a distinct world rect and a clip carried
//! down the recursion; that is localized to the two reads below (the `bounds`
//! lookup and the `contains` test) and deferred until those containers exist.

use crate::component::NodeStore;
use crate::node::NodeId;

/// A stateless hit-test query over a [`NodeStore`].
///
/// It owns no data; the struct is a home for future capture/clip state so the
/// call site stays stable as the input subsystem grows.
pub struct HitTestTree;

impl HitTestTree {
    /// The topmost hittable node whose rect contains `(px, py)`, or `None` if
    /// the point misses the whole subtree at `root`.
    ///
    /// Descends topmost-first (reverse sibling order) and prunes any subtree
    /// whose box does not contain the point — no allocation, worst case the tree
    /// depth.
    pub fn hit(store: &NodeStore, root: NodeId, px: f32, py: f32) -> Option<NodeId> {
        let arena = store.arena();
        if !arena.is_live(root) {
            return None;
        }
        // A child cannot escape its container's box, so a point outside this box
        // is outside the whole subtree — prune without recursing.
        if !store.bounds(root).contains(px, py) {
            return None;
        }

        // Test children topmost-first: the last-painted (last) child is tested
        // first, walking back toward the first-painted (bottom) one.
        let mut child = arena.links(root).and_then(|l| l.last_child);
        while let Some(c) = child {
            if let Some(hit) = Self::hit(store, c, px, py) {
                return Some(hit);
            }
            child = arena.links(c).and_then(|l| l.prev_sibling);
        }

        // No child claimed the point: this node is the hit only if it is itself
        // hittable. A pass-through container with no hittable child under the
        // point returns `None`, letting a lower sibling be tried by the caller.
        if store.hittable(root) {
            Some(root)
        } else {
            None
        }
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
}
