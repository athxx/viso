//! Paint: lower a laid-out subtree to renderer primitives (§9 node → primitive,
//! §16 UI produces paint data, renderer owns batching).
//!
//! [`paint_tree`] walks the tree in pre-order (parent before children, so a
//! container's background paints behind its contents) and emits one
//! [`viso_render::Primitive::Quad`] per visible node from its resolved `bounds`
//! and [`crate::style::BoxStyle`]. A node with a transparent, borderless style
//! contributes nothing — a pure layout container. The walk pushes into a
//! caller-owned `Vec` so frames reuse the buffer (§7.1). The renderer decides
//! batching/ordering downstream; this only produces the primitive data.

use crate::component::NodeStore;
use crate::node::NodeId;
use viso_render::{Primitive, Quad};

/// Emit primitives for the subtree rooted at `root` into `out`, in pre-order.
/// `out` is not cleared — append semantics let a caller compose multiple trees.
pub fn paint_tree(store: &NodeStore, root: NodeId, out: &mut Vec<Primitive>) {
    let arena = store.arena();
    if !arena.is_live(root) {
        return;
    }

    let style = store.style(root);
    if style.is_visible() {
        let bounds = store.bounds(root);
        out.push(Primitive::Quad(Quad {
            rect: bounds,
            color: style.fill,
            radius: style.radius,
            border: style.border,
        }));
    }

    // Recurse into children in sibling order (pre-order: parent already painted).
    let mut child = arena.links(root).and_then(|l| l.first_child);
    while let Some(c) = child {
        paint_tree(store, c, out);
        child = arena.links(c).and_then(|l| l.next_sibling);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::component::{BuildCx, FlexStyle, LeafStyle, NodeStore};
    use crate::layout::{Axis, Size};
    use crate::style::BoxStyle;
    use viso_render::{Rect, Rgba};

    const RED: Rgba = Rgba {
        r: 1.0,
        g: 0.0,
        b: 0.0,
        a: 1.0,
    };

    #[test]
    fn transparent_container_emits_only_visible_leaves() {
        let mut store = NodeStore::new();
        let root = {
            let mut cx = BuildCx::new(&mut store);
            cx.flex(
                FlexStyle {
                    axis: Axis::Row,
                    // Transparent container: no background quad.
                    ..Default::default()
                },
                |cx| {
                    cx.leaf(LeafStyle {
                        size: Size::fixed(10.0, 10.0),
                        style: BoxStyle::solid(RED),
                    });
                    // A transparent leaf contributes nothing.
                    cx.leaf(LeafStyle {
                        size: Size::fixed(10.0, 10.0),
                        style: BoxStyle::NONE,
                    });
                },
            );
            cx.root().unwrap()
        };
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
        let mut out = Vec::new();
        paint_tree(&store, root, &mut out);
        // Only the one solid leaf paints.
        assert_eq!(out.len(), 1);
        assert!(matches!(out[0], Primitive::Quad(q) if q.color == RED));
    }
}
