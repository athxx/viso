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
    Rect, Rgba,
};

/// The opaque white tint that passes a premultiplied color-atlas texel through
/// the Image shader unchanged (`texel * float4(1,1,1,1)`), so a color-bitmap
/// glyph paints exactly as its emoji artwork stores it.
const WHITE: Rgba = Rgba {
    r: 1.0,
    g: 1.0,
    b: 1.0,
    a: 1.0,
};

/// Emit primitives for the subtree rooted at `root` into `out`, in pre-order.
/// `out` is not cleared — append semantics let a caller compose multiple trees.
///
/// Overlay nodes (see [`NodeStore::is_overlay`]) paint in a deferred top layer:
/// the main walk skips an overlay subtree where it meets it and records the root;
/// once the main walk finishes, each recorded overlay root paints, in the order
/// they were met (author order). So a popup declared deep in the tree still draws
/// over the whole scene, with no z-index sort. A tree with no overlay node emits
/// exactly the pre-order stream it did before — the deferred pass is empty.
pub fn paint_tree(store: &NodeStore, root: NodeId, out: &mut Vec<Primitive>) {
    // The top-layer roots met during the main walk. A thread/scene-local scratch
    // that a hot caller could hoist; here it is a small once-per-paint Vec, empty
    // (never heap-allocating) in the common no-overlay case.
    let mut overlays: Vec<NodeId> = Vec::new();
    // The `root` itself paints even if flagged overlay — it is the entry, not a
    // child boundary — so the main walk starts unconditionally.
    paint_subtree(store, root, out, &mut overlays);
    // Deferred top layer: paint each overlay root in the order it was met. An
    // overlay's own subtree may in turn contain further overlays, appended past
    // the current end, so the index walk naturally drains nested ones too.
    let mut i = 0;
    while i < overlays.len() {
        paint_subtree(store, overlays[i], out, &mut overlays);
        i += 1;
    }
}

/// Paint one subtree in pre-order, diverting any overlay child into `overlays`
/// instead of descending into it (the caller drains that list into a top layer).
fn paint_subtree(
    store: &NodeStore,
    root: NodeId,
    out: &mut Vec<Primitive>,
    overlays: &mut Vec<NodeId>,
) {
    let arena = store.arena();
    if !arena.is_live(root) {
        return;
    }
    // A hidden node paints neither its own quad/content nor any descendant.
    if store.hidden(root) {
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
    // An overlay child is diverted to the deferred top layer rather than painted
    // in place — it draws after the whole scene, so it must not descend here.
    let mut child = arena.links(root).and_then(|l| l.first_child);
    while let Some(c) = child {
        if store.is_overlay(c) {
            overlays.push(c);
        } else {
            paint_subtree(store, c, out, overlays);
        }
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
            color_glyphs,
            color_atlas,
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
            // Color-bitmap glyphs (emoji) reuse the Image pipeline: each is one
            // textured quad sampling the RGBA color atlas at its glyph sub-rect.
            // A white, opaque tint leaves the premultiplied emoji texel unchanged
            // (`texel * float4(1,1,1,1)`), so the color glyph paints as authored.
            // `color_atlas` is `Some` whenever `color_glyphs` is non-empty.
            if let Some(color_atlas) = color_atlas {
                for g in color_glyphs {
                    out.push(Primitive::Image(ImageDraw {
                        rect: Rect {
                            x: g.rect.x + ox,
                            y: g.rect.y + oy,
                            w: g.rect.w,
                            h: g.rect.h,
                        },
                        uv: g.uv,
                        tint: WHITE,
                        texture: *color_atlas,
                    }));
                }
            }
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
    use crate::dirty::DirtyClass;
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
    fn a_hidden_node_emits_no_primitive_for_itself_or_its_subtree() {
        // A row of two solid leaves; hiding the first must drop both its own quad
        // and (were it a container) its descendants — the paint walk early-returns.
        let mut store = NodeStore::new();
        let mut first = None;
        let root = {
            let mut cx = BuildCx::new(&mut store);
            cx.flex(
                FlexStyle {
                    axis: Axis::Row,
                    ..Default::default()
                },
                |cx| {
                    let h = cx.leaf(LeafStyle {
                        size: Size::fixed(10.0, 10.0),
                        style: BoxStyle::solid(RED),
                    });
                    first = Some(h.id());
                    cx.leaf(LeafStyle {
                        size: Size::fixed(10.0, 10.0),
                        style: BoxStyle::solid(RED),
                    });
                },
            );
            cx.root().unwrap()
        };
        store.set_hidden(first.unwrap(), true);
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
        // Only the still-shown leaf paints; the hidden one contributes nothing.
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
                color_glyphs: Vec::new(),
                color_atlas: None,
                color: RED,
                natural: Vec2 { x: 30.0, y: 20.0 },
                baseline: 16.0,
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

    #[test]
    fn color_glyphs_lower_to_white_tinted_images() {
        use crate::content::Content;
        use crate::layout::Vec2;
        use viso_render::{GlyphInstanceData, TextureId};

        // A text run mixing one SDF outline glyph and one color-bitmap glyph.
        // The SDF glyph must still emit the single GlyphRun; the color glyph must
        // additionally lower to one white-tinted Image sampling the color atlas,
        // both shifted to the node's world origin.
        let mut store = NodeStore::new();
        let mut text_id = None;
        let root = {
            let mut cx = BuildCx::new(&mut store);
            cx.flex(FlexStyle::default(), |cx| {
                text_id = Some(
                    cx.leaf(LeafStyle {
                        size: Size::fixed(40.0, 20.0),
                        style: BoxStyle::NONE,
                    })
                    .id(),
                );
            });
            cx.root().unwrap()
        };
        let text_id = text_id.unwrap();

        store.set_content_payload(
            text_id,
            Content::Text {
                glyphs: vec![GlyphInstanceData {
                    rect: Rect {
                        x: 1.0,
                        y: 2.0,
                        w: 8.0,
                        h: 10.0,
                    },
                    uv: Rect {
                        x: 0.0,
                        y: 0.0,
                        w: 0.1,
                        h: 0.1,
                    },
                    px_range: 2.0,
                }],
                atlas: TextureId(3),
                color_glyphs: vec![GlyphInstanceData {
                    rect: Rect {
                        x: 12.0,
                        y: 4.0,
                        w: 16.0,
                        h: 16.0,
                    },
                    uv: Rect {
                        x: 0.25,
                        y: 0.5,
                        w: 0.125,
                        h: 0.125,
                    },
                    // A color glyph carries no SDF decode factor; paint ignores it.
                    px_range: 0.0,
                }],
                color_atlas: Some(TextureId(5)),
                color: RED,
                natural: Vec2 { x: 40.0, y: 20.0 },
                baseline: 16.0,
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

        let mut out = Vec::new();
        paint_tree(&store, root, &mut out);
        let world = store.world(text_id);

        // The SDF run still lands once, sampling the R8 atlas.
        let run = out
            .iter()
            .find_map(|p| match p {
                Primitive::GlyphRun(r) => Some(r),
                _ => None,
            })
            .expect("the outline run");
        assert_eq!(run.atlas, TextureId(3));
        assert_eq!(run.glyphs.len(), 1);

        // The one color glyph lowers to one Image: white opaque tint (passes the
        // premultiplied emoji texel through unchanged), the color atlas texture,
        // its UV sub-rect verbatim, and its rect shifted to the world origin.
        let images: Vec<_> = out
            .iter()
            .filter_map(|p| match p {
                Primitive::Image(i) => Some(i),
                _ => None,
            })
            .collect();
        assert_eq!(images.len(), 1, "one color glyph → one image quad");
        let img = images[0];
        assert_eq!(img.texture, TextureId(5));
        assert_eq!(img.tint, WHITE);
        assert_eq!(img.uv.x, 0.25);
        assert_eq!(img.uv.y, 0.5);
        assert_eq!(img.uv.w, 0.125);
        assert_eq!(img.uv.h, 0.125);
        assert_eq!(img.rect.x, world.x + 12.0);
        assert_eq!(img.rect.y, world.y + 4.0);
        assert_eq!(img.rect.w, 16.0);
        assert_eq!(img.rect.h, 16.0);
    }

    #[test]
    fn pure_text_run_emits_no_image() {
        use crate::content::Content;
        use crate::layout::Vec2;
        use viso_render::{GlyphInstanceData, TextureId};

        // A pure-text run (empty color_glyphs, no color atlas) must not emit any
        // Image primitive — the color path costs nothing when unused.
        let mut store = NodeStore::new();
        let mut text_id = None;
        let root = {
            let mut cx = BuildCx::new(&mut store);
            cx.flex(FlexStyle::default(), |cx| {
                text_id = Some(
                    cx.leaf(LeafStyle {
                        size: Size::fixed(20.0, 20.0),
                        style: BoxStyle::NONE,
                    })
                    .id(),
                );
            });
            cx.root().unwrap()
        };
        let text_id = text_id.unwrap();
        store.set_content_payload(
            text_id,
            Content::Text {
                glyphs: vec![GlyphInstanceData {
                    rect: Rect {
                        x: 0.0,
                        y: 0.0,
                        w: 8.0,
                        h: 10.0,
                    },
                    uv: Rect {
                        x: 0.0,
                        y: 0.0,
                        w: 0.1,
                        h: 0.1,
                    },
                    px_range: 2.0,
                }],
                atlas: TextureId(3),
                color_glyphs: Vec::new(),
                color_atlas: None,
                color: RED,
                natural: Vec2 { x: 20.0, y: 20.0 },
                baseline: 16.0,
            },
        );

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
        assert!(
            !out.iter().any(|p| matches!(p, Primitive::Image(_))),
            "a pure-text run emits no image quad"
        );
    }

    const GREEN: Rgba = Rgba {
        r: 0.0,
        g: 1.0,
        b: 0.0,
        a: 1.0,
    };
    const BLUE: Rgba = Rgba {
        r: 0.0,
        g: 0.0,
        b: 1.0,
        a: 1.0,
    };

    // The fill colors of every quad in the primitive stream, in paint order —
    // enough to assert draw order for the overlay tests below.
    fn quad_colors(out: &[Primitive]) -> Vec<Rgba> {
        out.iter()
            .filter_map(|p| match p {
                Primitive::Quad(q) => Some(q.color),
                _ => None,
            })
            .collect()
    }

    // Author a row of three solid leaves (RED, GREEN, BLUE) into a fresh store,
    // returning the store, the root, and the three leaf ids in author order. The
    // caller flags whichever leaves it wants as overlay before laying out.
    fn three_leaf_row() -> (NodeStore, NodeId, [NodeId; 3]) {
        let mut store = NodeStore::new();
        let mut ids = [None; 3];
        let root = {
            let mut cx = BuildCx::new(&mut store);
            cx.flex(
                FlexStyle {
                    axis: Axis::Row,
                    ..Default::default()
                },
                |cx| {
                    for (i, color) in [RED, GREEN, BLUE].into_iter().enumerate() {
                        ids[i] = Some(
                            cx.leaf(LeafStyle {
                                size: Size::fixed(10.0, 10.0),
                                style: BoxStyle::solid(color),
                            })
                            .id(),
                        );
                    }
                },
            );
            cx.root().unwrap()
        };
        (store, root, ids.map(|id| id.unwrap()))
    }

    fn layout_full(store: &mut NodeStore, root: NodeId) {
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
    }

    #[test]
    fn an_overlay_child_paints_after_its_later_siblings() {
        // Flag the first leaf (RED) as overlay: it is authored before GREEN and
        // BLUE, so pre-order would paint it first — instead it must paint last,
        // after both later siblings, in the deferred top layer.
        let (mut store, root, [red, _green, _blue]) = three_leaf_row();
        store.set_overlay(red, true);
        layout_full(&mut store, root);

        let mut out = Vec::new();
        paint_tree(&store, root, &mut out);
        assert_eq!(quad_colors(&out), vec![GREEN, BLUE, RED]);
    }

    #[test]
    fn an_overlay_paints_over_an_earlier_non_overlay_node() {
        // Flag only the last leaf (BLUE) as overlay. It already paints last in
        // pre-order, so the stream is unchanged — but this pins the guarantee
        // that a top-layer node draws over the nodes authored before it.
        let (mut store, root, [_red, _green, blue]) = three_leaf_row();
        store.set_overlay(blue, true);
        layout_full(&mut store, root);

        let mut out = Vec::new();
        paint_tree(&store, root, &mut out);
        let colors = quad_colors(&out);
        assert_eq!(*colors.last().unwrap(), BLUE, "overlay draws on top");
        assert_eq!(colors, vec![RED, GREEN, BLUE]);
    }

    #[test]
    fn no_overlay_emits_the_same_stream_as_the_plain_pre_order_walk() {
        // The regression guard: with no node flagged overlay, the primitive
        // stream must be byte-identical to the pre-order walk it produced before
        // the top-layer pass existed.
        let (mut store, root, _ids) = three_leaf_row();
        layout_full(&mut store, root);

        let mut out = Vec::new();
        paint_tree(&store, root, &mut out);
        assert_eq!(quad_colors(&out), vec![RED, GREEN, BLUE]);
        // No layer/clip primitives crept in for a plain row.
        assert_eq!(out.len(), 3);
    }

    #[test]
    fn nested_overlays_paint_in_author_order() {
        // A top-layer node whose own subtree contains a further overlay: the
        // outer overlay is collected during the main walk and drained; draining
        // it re-enters paint_subtree, which collects the inner overlay past the
        // current end, so the index walk paints it after — author (collection)
        // order, with no z sort.
        let mut store = NodeStore::new();
        let mut outer = None;
        let mut inner = None;
        let root = {
            let mut cx = BuildCx::new(&mut store);
            cx.flex(
                FlexStyle {
                    axis: Axis::Column,
                    ..Default::default()
                },
                |cx| {
                    // A plain in-place leaf, painted in the main walk.
                    cx.leaf(LeafStyle {
                        size: Size::fixed(10.0, 10.0),
                        style: BoxStyle::solid(RED),
                    });
                    // An overlay panel (GREEN) that itself holds an overlay child
                    // (BLUE) — both must land in the deferred top layer.
                    let panel = cx.flex(
                        FlexStyle {
                            axis: Axis::Column,
                            style: BoxStyle::solid(GREEN),
                            ..Default::default()
                        },
                        |cx| {
                            inner = Some(
                                cx.leaf(LeafStyle {
                                    size: Size::fixed(10.0, 10.0),
                                    style: BoxStyle::solid(BLUE),
                                })
                                .id(),
                            );
                        },
                    );
                    outer = Some(panel.id());
                },
            );
            cx.root().unwrap()
        };
        store.set_overlay(outer.unwrap(), true);
        store.set_overlay(inner.unwrap(), true);
        layout_full(&mut store, root);

        let mut out = Vec::new();
        paint_tree(&store, root, &mut out);
        // RED (in place) first, then the outer overlay panel GREEN, then its
        // nested overlay BLUE — the whole top layer over the main content.
        assert_eq!(quad_colors(&out), vec![RED, GREEN, BLUE]);
    }

    #[test]
    fn set_overlay_marks_paint_only_not_layout() {
        // An overlay lays out in place; flipping the flag must repaint but must
        // not force a re-measure/re-layout of the node. Contrast set_hidden,
        // which marks LAYOUT | PAINT.
        let (mut store, root, [red, _green, _blue]) = three_leaf_row();
        layout_full(&mut store, root);
        // Layout clears the dirty flags; a fresh set_overlay is the only mark.
        store.set_overlay(red, true);
        let d = store.dirty(red);
        assert!(d.contains(DirtyClass::PAINT), "overlay flip repaints");
        assert!(
            !d.intersects(DirtyClass::LAYOUT | DirtyClass::MEASURE),
            "overlay lays out in place — no relayout"
        );
    }
}
