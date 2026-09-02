//! Paint: lower a laid-out subtree to renderer primitives. UI produces paint
//! data; the renderer owns batching and ordering downstream.
//!
//! [`paint_tree`] walks the tree in pre-order (parent before children, so a
//! container's background paints behind its contents) and emits one
//! [`viso_render::Primitive::Quad`] per visible node from its resolved `world`
//! rect and [`crate::style::BoxStyle`]. A node with a transparent, borderless
//! style contributes nothing — a pure layout container. The walk pushes into a
//! caller-owned `Vec` so frames reuse the buffer. The renderer decides
//! batching/ordering downstream; this only produces the primitive data.
//!
//! Node rects come from `world`, not `bounds`: a scrolling ancestor shifts the
//! world rect so a scrolled child paints at its on-screen position. A scroll
//! viewport additionally wraps its children in a
//! [`viso_render::Primitive::Layer`]/[`viso_render::Primitive::LayerEnd`] pair
//! clipped to the viewport's own world rect, so content scrolled past the edge
//! is clipped away. The viewport's own background quad paints before the clip is
//! pushed, so it fills the whole box regardless of scroll.

use crate::component::NodeStore;
use crate::node::NodeId;
use viso_render::{LayerClip, Primitive, Quad};

/// Emit primitives for the subtree rooted at `root` into `out`, in pre-order.
/// `out` is not cleared — append semantics let a caller compose multiple trees.
pub fn paint_tree(store: &NodeStore, root: NodeId, out: &mut Vec<Primitive>) {
    let arena = store.arena();
    if !arena.is_live(root) {
        return;
    }

    let world = store.world(root);
    let style = store.style(root);
    if style.is_visible() {
        out.push(Primitive::Quad(Quad {
            rect: world,
            color: style.fill,
            radius: style.radius,
            border: style.border,
        }));
    }

    // A scroll viewport clips its content to its own box: push a clip layer
    // around the children (the viewport's background already painted above, so
    // it is not clipped). An ordinary container pushes nothing.
    let scroll_clip = store.is_scroll(root);
    if scroll_clip {
        out.push(Primitive::Layer(LayerClip {
            clip: world,
            opacity: 1.0,
        }));
    }

    // Recurse into children in sibling order (pre-order: parent already painted).
    let mut child = arena.links(root).and_then(|l| l.first_child);
    while let Some(c) = child {
        paint_tree(store, c, out);
        child = arena.links(c).and_then(|l| l.next_sibling);
    }

    if scroll_clip {
        out.push(Primitive::LayerEnd);
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

    #[test]
    fn scroll_viewport_clips_content_and_offsets_by_scroll() {
        use crate::component::ScrollStyle;
        use crate::layout::Vec2;

        let mut store = NodeStore::new();
        let mut content = None;
        let root = {
            let mut cx = BuildCx::new(&mut store);
            cx.scroll(
                ScrollStyle {
                    axis: Axis::Column,
                    size: Size::fixed(100.0, 100.0),
                    // A solid viewport background so its quad paints before the clip.
                    style: BoxStyle::solid(RED),
                },
                |cx| {
                    content = Some(
                        cx.leaf(LeafStyle {
                            size: Size::fixed(100.0, 300.0),
                            style: BoxStyle::solid(RED),
                        })
                        .id(),
                    );
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
        store.scroll_by(root, Vec2 { x: 0.0, y: 40.0 });
        store.resolve_transforms(root);

        let mut out = Vec::new();
        paint_tree(&store, root, &mut out);

        // viewport background quad, then the clip layer, then the content quad,
        // then the layer end — the background is outside the clip, the content in.
        assert_eq!(out.len(), 4);
        assert!(matches!(out[0], Primitive::Quad(q) if q.rect == store.world(root)));
        let clip = match out[1] {
            Primitive::Layer(l) => l,
            _ => panic!("expected a clip layer around the content"),
        };
        assert_eq!(clip.clip, store.world(root), "clip is the viewport box");
        // The content quad is shifted up by the scroll offset.
        let cw = store.world(content.unwrap());
        assert!(matches!(out[2], Primitive::Quad(q) if q.rect == cw));
        assert_eq!(cw.y, store.bounds(content.unwrap()).y - 40.0);
        assert!(matches!(out[3], Primitive::LayerEnd));
    }
}
