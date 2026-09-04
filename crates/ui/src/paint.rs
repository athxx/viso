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
use crate::content::Content;
use crate::node::NodeId;
use viso_render::{
    GlyphInstanceData, GlyphRunDraw, ImageDraw, LayerClip, Path, PathCmd, Point, Primitive, Quad,
    Rect,
};

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

    // Drawable content (text/image/path) paints over the node's background. Its
    // coordinates are node-local, so shift them by the node's world origin — a
    // scrolled or repositioned node then draws its content in the right place.
    if let Some(content) = store.content_payload(root) {
        paint_content(content, world, out);
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

/// Lower one node's [`Content`] to a primitive, translating its node-local
/// coordinates by the node's `world` origin. The image variant fills the node's
/// whole `world` box (a content leaf sizes to the image's intrinsic size, so the
/// box already matches unless a fixed size overrides it).
fn paint_content(content: &Content, world: Rect, out: &mut Vec<Primitive>) {
    let ox = world.x;
    let oy = world.y;
    match content {
        Content::Text {
            glyphs,
            atlas,
            color,
            ..
        } => {
            let glyphs = glyphs
                .iter()
                .map(|g| GlyphInstanceData {
                    rect: Rect {
                        x: g.rect.x + ox,
                        y: g.rect.y + oy,
                        w: g.rect.w,
                        h: g.rect.h,
                    },
                    ..*g
                })
                .collect();
            out.push(Primitive::GlyphRun(GlyphRunDraw {
                glyphs,
                atlas: *atlas,
                color: *color,
            }));
        }
        Content::Image {
            texture, uv, tint, ..
        } => {
            out.push(Primitive::Image(ImageDraw {
                rect: world,
                uv: *uv,
                tint: *tint,
                texture: *texture,
            }));
        }
        Content::Path {
            cmds, fill, stroke, ..
        } => {
            let cmds = cmds.iter().map(|c| translate_cmd(*c, ox, oy)).collect();
            out.push(Primitive::Path(Path {
                cmds,
                fill: *fill,
                stroke: *stroke,
            }));
        }
    }
}

/// Translate a path command's points by `(dx, dy)`.
#[inline]
fn translate_cmd(cmd: PathCmd, dx: f32, dy: f32) -> PathCmd {
    let t = |p: Point| Point::new(p.x + dx, p.y + dy);
    match cmd {
        PathCmd::MoveTo(p) => PathCmd::MoveTo(t(p)),
        PathCmd::LineTo(p) => PathCmd::LineTo(t(p)),
        PathCmd::QuadTo(c, p) => PathCmd::QuadTo(t(c), t(p)),
        PathCmd::CubicTo(c1, c2, p) => PathCmd::CubicTo(t(c1), t(c2), t(p)),
        PathCmd::Close => PathCmd::Close,
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

    #[test]
    fn content_emits_after_background_and_offsets_by_world() {
        use crate::content::Content;
        use crate::layout::{Length, Vec2};
        use viso_render::{GlyphInstanceData, PathCmd, Point, TextureId};

        // A row so the leaf sits at a nonzero world x, exercising the local→world
        // translation of glyph and path coordinates.
        let mut store = NodeStore::new();
        let mut text_id = None;
        let mut path_id = None;
        let mut image_id = None;
        let root = {
            let mut cx = BuildCx::new(&mut store);
            cx.flex(FlexStyle::default(), |cx| {
                text_id = Some(
                    cx.leaf(LeafStyle {
                        size: Size::fixed(30.0, 20.0),
                        style: BoxStyle::solid(RED),
                    })
                    .id(),
                );
                image_id = Some(
                    cx.leaf(LeafStyle {
                        size: Size {
                            width: Length::Fit,
                            height: Length::Fit,
                        },
                        style: BoxStyle::NONE,
                    })
                    .id(),
                );
                path_id = Some(
                    cx.leaf(LeafStyle {
                        size: Size::fixed(16.0, 16.0),
                        style: BoxStyle::NONE,
                    })
                    .id(),
                );
            });
            cx.root().unwrap()
        };
        let text_id = text_id.unwrap();
        let image_id = image_id.unwrap();
        let path_id = path_id.unwrap();

        // A one-glyph run positioned at local (2,3).
        store.set_content_payload(
            text_id,
            Content::Text {
                glyphs: vec![GlyphInstanceData {
                    rect: Rect {
                        x: 2.0,
                        y: 3.0,
                        w: 8.0,
                        h: 10.0,
                    },
                    uv: Rect {
                        x: 0.0,
                        y: 0.0,
                        w: 1.0,
                        h: 1.0,
                    },
                    px_range: 2.0,
                }],
                atlas: TextureId(7),
                color: RED,
                natural: Vec2 { x: 30.0, y: 20.0 },
            },
        );
        // An image whose Fit box comes from its intrinsic size.
        store.set_content_payload(
            image_id,
            Content::Image {
                texture: TextureId(9),
                uv: Rect {
                    x: 0.0,
                    y: 0.0,
                    w: 1.0,
                    h: 1.0,
                },
                tint: Rgba {
                    r: 1.0,
                    g: 1.0,
                    b: 1.0,
                    a: 1.0,
                },
                natural: Vec2 { x: 40.0, y: 20.0 },
            },
        );
        // A path with one moveto/lineto at local coords.
        store.set_content_payload(
            path_id,
            Content::Path {
                cmds: vec![
                    PathCmd::MoveTo(Point::new(1.0, 1.0)),
                    PathCmd::LineTo(Point::new(5.0, 5.0)),
                ],
                fill: Some(RED),
                stroke: None,
                natural: Vec2 { x: 16.0, y: 16.0 },
            },
        );

        let mut scratch = Vec::new();
        store.layout(
            root,
            Rect {
                x: 0.0,
                y: 0.0,
                w: 200.0,
                h: 100.0,
            },
            &mut scratch,
        );

        // The image leaf's box is its intrinsic size (Fit both axes).
        let img_box = store.bounds(image_id);
        assert_eq!(img_box.w, 40.0);
        assert_eq!(img_box.h, 20.0);

        let mut out = Vec::new();
        paint_tree(&store, root, &mut out);

        // text leaf: background quad, then a glyph run shifted to world origin.
        let text_world = store.world(text_id);
        let run = out
            .iter()
            .find_map(|p| match p {
                Primitive::GlyphRun(r) => Some(r),
                _ => None,
            })
            .expect("a glyph run");
        assert_eq!(run.atlas, TextureId(7));
        assert_eq!(run.glyphs[0].rect.x, text_world.x + 2.0);
        assert_eq!(run.glyphs[0].rect.y, text_world.y + 3.0);

        // image: fills its world box.
        let image = out
            .iter()
            .find_map(|p| match p {
                Primitive::Image(i) => Some(i),
                _ => None,
            })
            .expect("an image");
        assert_eq!(image.texture, TextureId(9));
        assert_eq!(image.rect, store.world(image_id));

        // path: commands translated by the path leaf's world origin.
        let path_world = store.world(path_id);
        let path = out
            .iter()
            .find_map(|p| match p {
                Primitive::Path(p) => Some(p),
                _ => None,
            })
            .expect("a path");
        assert_eq!(
            path.cmds[0],
            PathCmd::MoveTo(Point::new(path_world.x + 1.0, path_world.y + 1.0))
        );
        assert_eq!(
            path.cmds[1],
            PathCmd::LineTo(Point::new(path_world.x + 5.0, path_world.y + 5.0))
        );
    }
}
