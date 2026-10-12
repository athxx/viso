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

use std::sync::Arc;

use crate::asset::{AssetDraw, AssetFit};
use crate::component::NodeStore;
use crate::content::Content;
use crate::layout::Vec2;
use crate::node::NodeId;
use viso_render::{
    GlyphInstanceData, GlyphLane, GlyphRunDraw, ImageDraw, LayerClip, Path, PathCmd, Point,
    Primitive, Quad, Rect, Rgba, SamplerDesc, ShaderDraw,
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
    // A hidden node paints neither its own quad/content nor any descendant, and
    // neither does a fully transparent one.
    let opacity = store.opacity(root);
    if store.hidden(root) || opacity <= 0.0 {
        return;
    }

    let world = store.world(root);
    // A translucent node composites itself and its subtree as one layer over
    // its own box.
    let faded = opacity < 1.0;
    if faded {
        out.push(Primitive::Layer(LayerClip {
            clip: world,
            opacity,
            blur_sigma: 0.0,
            backdrop_sigma: 0.0,
        }));
    }
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
            blur_sigma: 0.0,
            backdrop_sigma: 0.0,
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
    if faded {
        out.push(Primitive::LayerEnd);
    }
}

/// Lower one node's [`Content`] to a primitive, translating its node-local
/// coordinates by the node's `world` origin. The image variant fills the node's
/// whole `world` box (a content leaf sizes to the image's intrinsic size, so the
/// box already matches unless a fixed size overrides it).
pub(crate) fn paint_content(content: &Content, world: Rect, out: &mut Vec<Primitive>) {
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
                lane: GlyphLane::CoverageA8,
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
                        sampler: SamplerDesc::LINEAR_CLAMP,
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
                sampler: SamplerDesc::LINEAR_CLAMP,
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
                shadow: None,
            }));
        }
        Content::Shader { program, params } => {
            out.push(Primitive::Shader(ShaderDraw {
                rect: world,
                program: *program,
                params: Arc::clone(params),
            }));
        }
        Content::Asset {
            draw,
            natural,
            fit,
            tint,
            ..
        } => paint_asset(draw, *natural, *fit, *tint, world, out),
    }
}

/// An asset of `natural` size placed into `world` by `fit`.
fn paint_asset(
    draw: &AssetDraw,
    natural: Vec2,
    fit: AssetFit,
    tint: Option<Rgba>,
    world: Rect,
    out: &mut Vec<Primitive>,
) {
    if natural.x <= 0.0 || natural.y <= 0.0 || world.w <= 0.0 || world.h <= 0.0 {
        return;
    }
    let scale = match fit {
        AssetFit::Contain => (world.w / natural.x).min(world.h / natural.y),
        AssetFit::Cover => (world.w / natural.x).max(world.h / natural.y),
        AssetFit::Fill => 0.0,
    };
    // The box the whole asset takes, centered on the node's.
    let placed = if fit == AssetFit::Fill {
        world
    } else {
        let (w, h) = (natural.x * scale, natural.y * scale);
        Rect {
            x: world.x + (world.w - w) * 0.5,
            y: world.y + (world.h - h) * 0.5,
            w,
            h,
        }
    };
    match draw {
        AssetDraw::Nothing => {}
        AssetDraw::Raster(texture) => {
            // A cover crops in the texture rather than drawing past the box.
            let (rect, uv) = if fit == AssetFit::Cover {
                let (uw, uh) = (world.w / placed.w, world.h / placed.h);
                let uv = Rect {
                    x: (1.0 - uw) * 0.5,
                    y: (1.0 - uh) * 0.5,
                    w: uw,
                    h: uh,
                };
                (world, uv)
            } else {
                let full = Rect {
                    x: 0.0,
                    y: 0.0,
                    w: 1.0,
                    h: 1.0,
                };
                (placed, full)
            };
            out.push(Primitive::Image(ImageDraw {
                rect,
                uv,
                tint: tint.unwrap_or(WHITE),
                texture: *texture,
                sampler: SamplerDesc::LINEAR_CLAMP,
            }));
        }
        AssetDraw::Vector(scene) => {
            let crop = fit == AssetFit::Cover;
            if crop {
                out.push(Primitive::Layer(LayerClip {
                    clip: world,
                    opacity: 1.0,
                    blur_sigma: 0.0,
                    backdrop_sigma: 0.0,
                }));
            }
            out.extend(scene.place(placed, tint).map(Primitive::Path));
            if crop {
                out.push(Primitive::LayerEnd);
            }
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
    use crate::asset::AssetBytes;
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
    fn a_translucent_node_paints_its_subtree_as_one_layer() {
        let mut store = NodeStore::new();
        let (root, leaf) = {
            let mut cx = BuildCx::new(&mut store);
            let mut leaf = None;
            cx.flex(FlexStyle::default(), |cx| {
                leaf = Some(
                    cx.leaf(LeafStyle {
                        size: Size::fixed(10.0, 10.0),
                        style: BoxStyle::solid(RED),
                    })
                    .id(),
                );
            });
            (cx.root().unwrap(), leaf.unwrap())
        };
        let surface = Rect {
            x: 0.0,
            y: 0.0,
            w: 100.0,
            h: 100.0,
        };
        store.layout(root, surface, &mut Vec::new());
        store.set_opacity(root, 0.5);
        assert!(store.dirty(root).contains(DirtyClass::PAINT));
        let mut out = Vec::new();
        paint_tree(&store, root, &mut out);
        assert_eq!(out.len(), 3, "layer, leaf, layer end");
        assert!(matches!(out[0], Primitive::Layer(l) if l.opacity == 0.5));
        assert!(matches!(out[2], Primitive::LayerEnd));

        let green = Rgba { g: 1.0, ..RED };
        store.set_fill(leaf, green);
        store.set_opacity(root, 0.0);
        out.clear();
        paint_tree(&store, root, &mut out);
        assert!(out.is_empty(), "a transparent node paints nothing");
        store.set_opacity(root, f32::NAN);
        out.clear();
        paint_tree(&store, root, &mut out);
        assert!(matches!(out[..], [Primitive::Quad(q)] if q.color == green));
    }

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
                }],
                atlas: TextureId::new(7),
                color_glyphs: Vec::new(),
                color_atlas: None,
                color: RED,
                natural: Vec2 { x: 30.0, y: 20.0 },
                baseline: 16.0,
                shaped_at_width: None,
                soft_wrap: false,
            },
        );
        // An image whose Fit box comes from its intrinsic size.
        store.set_content_payload(
            image_id,
            Content::Image {
                texture: TextureId::new(9),
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
        assert_eq!(run.atlas, TextureId::new(7));
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
        assert_eq!(image.texture, TextureId::new(9));
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
                }],
                atlas: TextureId::new(3),
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
                }],
                color_atlas: Some(TextureId::new(5)),
                color: RED,
                natural: Vec2 { x: 40.0, y: 20.0 },
                baseline: 16.0,
                shaped_at_width: None,
                soft_wrap: false,
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
        assert_eq!(run.atlas, TextureId::new(3));
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
        assert_eq!(img.texture, TextureId::new(5));
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
                }],
                atlas: TextureId::new(3),
                color_glyphs: Vec::new(),
                color_atlas: None,
                color: RED,
                natural: Vec2 { x: 20.0, y: 20.0 },
                baseline: 16.0,
                shaped_at_width: None,
                soft_wrap: false,
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

    #[test]
    fn a_shader_paints_the_box_and_a_parameter_change_is_paint_only() {
        use std::sync::Arc;
        use viso_render::{BrushLayout, BrushProgram, ProgramSlot, ShaderDraw};

        let program = Arc::new(BrushProgram {
            name: "Ripple".into(),
            layout: BrushLayout {
                stride: 20,
                rect_pos: 0,
                rect_size: 8,
                uniform_size: 16,
                viewport_size: 0,
                params: Vec::new(),
            },
            code: Vec::new(),
        });
        let mut store = NodeStore::new();
        let (mut a, mut b) = (None, None);
        let root = {
            let mut cx = BuildCx::new(&mut store);
            cx.flex(FlexStyle::default(), |cx| {
                for slot in [&mut a, &mut b] {
                    *slot = Some(
                        cx.leaf(LeafStyle {
                            size: Size::fixed(30.0, 20.0),
                            style: BoxStyle::NONE,
                        })
                        .id(),
                    );
                }
            });
            cx.root().unwrap()
        };
        let (a, b) = (a.unwrap(), b.unwrap());
        let params: Arc<[u8]> = Arc::from(vec![7u8; 20]);
        store.set_shader(a, &program, Arc::clone(&params));
        store.set_shader(b, &program, Arc::clone(&params));
        assert_eq!(store.shaders().iter().count(), 1, "one program, one slot");
        assert!(store.dirty(a).contains(DirtyClass::MEASURE));

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
        let draws: Vec<&ShaderDraw> = out
            .iter()
            .filter_map(|p| match p {
                Primitive::Shader(d) => Some(d),
                _ => None,
            })
            .collect();
        assert_eq!(draws.len(), 2);
        assert_eq!(draws[1].rect, store.world(b));
        assert_eq!(draws[1].program, ProgramSlot(0));
        assert!(
            Arc::ptr_eq(&draws[0].params, &params),
            "re-emitted, not copied"
        );

        store.clear_dirty();
        store.set_shader_params(b, Arc::from(vec![9u8; 20]));
        assert_eq!(store.dirty(b), DirtyClass::PAINT);
        store.clear_dirty();
        store.set_shader(a, &program, Arc::from(vec![1u8; 20]));
        assert_eq!(
            store.dirty(a),
            DirtyClass::PAINT,
            "the same program, new params"
        );

        // The program leaves its slot with the last node painting with it.
        let r = store.shaders().revision();
        store.set_content_payload(
            a,
            crate::content::Content::Path {
                cmds: Vec::new(),
                fill: None,
                stroke: None,
                natural: crate::layout::Vec2::ZERO,
            },
        );
        assert_eq!(store.shaders().revision(), r, "b still paints with it");
        let mut effects = crate::reactive::EffectStore::default();
        store.free_tree(b, &mut effects, &mut Vec::new());
        assert!(store.shaders().revision() > r);
        assert_eq!(store.shaders().iter().count(), 0);
        // The slot is the next program's.
        let next = Arc::new(BrushProgram {
            name: "Next".into(),
            ..(*program).clone()
        });
        store.set_shader(a, &next, Arc::clone(&params));
        assert_eq!(
            store.content_payload(a).map(|c| match c {
                crate::content::Content::Shader { program, .. } => *program,
                _ => ProgramSlot(99),
            }),
            Some(ProgramSlot(0))
        );
    }

    #[test]
    fn an_asset_draws_what_its_table_holds_and_a_new_payload_dirties_only_its_nodes() {
        use crate::asset::{AssetFit, AssetPayload};
        use std::sync::Arc;
        use viso_render::TextureId;
        use viso_render::asset::{RasterImage, ResourceId, ResourceRevision, VectorScene};

        let raster = |w: u32, h: u32| {
            AssetPayload::Raster(Arc::new(RasterImage {
                width: w,
                height: h,
                pixels: vec![255; (w * h * 4) as usize],
            }))
        };
        let logo = ResourceId::of("images/logo.png");
        let other = ResourceId::of("images/other.png");
        let mut store = NodeStore::new();
        let (mut a, mut b, mut c) = (None, None, None);
        let root = {
            let mut cx = BuildCx::new(&mut store);
            cx.flex(FlexStyle::default(), |cx| {
                for slot in [&mut a, &mut b, &mut c] {
                    *slot = Some(
                        cx.leaf(LeafStyle {
                            size: Size::fixed(40.0, 40.0),
                            style: BoxStyle::NONE,
                        })
                        .id(),
                    );
                }
            });
            cx.root().unwrap()
        };
        let (a, b, c) = (a.unwrap(), b.unwrap(), c.unwrap());
        let r1 = ResourceRevision(1);
        store
            .assets_mut()
            .offer(logo, r1, AssetBytes::Static(b"png"));
        let offered = store.assets().revision();
        store
            .assets_mut()
            .offer(logo, r1, AssetBytes::Static(b"png"));
        assert_eq!(store.assets().revision(), offered, "the same bytes again");
        store.set_asset(a, logo, AssetFit::Fill, None);
        store.set_asset(b, logo, AssetFit::Contain, None);
        store.set_asset(c, other, AssetFit::Fill, None);
        assert_eq!(
            store
                .content_payload(a)
                .map_or(Vec2::ZERO, Content::natural),
            Vec2::ZERO,
            "nothing decoded yet"
        );
        assert_eq!(store.assets().undecoded().count(), 1);

        // Decoded: its size lays out, but no pixels draw before they are resident.
        store.clear_dirty();
        store.provide_asset(logo, r1, raster(20, 10));
        assert_eq!(
            store.assets().undecoded().count(),
            0,
            "the bytes are dropped"
        );
        assert_eq!(
            store
                .content_payload(a)
                .map_or(Vec2::ZERO, Content::natural),
            Vec2::new(20.0, 10.0)
        );
        assert!(store.dirty(a).contains(DirtyClass::MEASURE));
        assert_eq!(store.dirty(c), DirtyClass::EMPTY, "another resource");
        assert_eq!(store.assets().unresident().count(), 1);
        let layout = |store: &mut NodeStore| {
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
            paint_tree(store, root, &mut out);
            out
        };
        let images = |out: &[Primitive]| -> Vec<ImageDraw> {
            out.iter()
                .filter_map(|p| match p {
                    Primitive::Image(d) => Some(*d),
                    _ => None,
                })
                .collect()
        };
        assert!(images(&layout(&mut store)).is_empty());

        // Resident: the pixels draw, PAINT only, the size unchanged.
        store.clear_dirty();
        let first = TextureId::new(1);
        assert_eq!(store.attach_texture(logo, r1, first), None);
        assert!(
            store.assets().payload(logo).is_none(),
            "the texture holds the pixels now"
        );
        assert_eq!(store.dirty(a), DirtyClass::PAINT);
        assert_eq!(store.dirty(c), DirtyClass::EMPTY);
        let drawn = images(&layout(&mut store));
        assert_eq!(drawn.len(), 2);
        assert_eq!(drawn[0].rect, store.world(a), "fill stretches to the box");
        let wb = store.world(b);
        assert_eq!(
            drawn[1].rect,
            Rect {
                x: wb.x,
                y: wb.y + 10.0,
                w: 40.0,
                h: 20.0
            },
            "contain keeps the aspect, centered"
        );

        // A new payload of another size: the old pixels draw until the new
        // ones are resident, then the nodes remeasure and the old texture
        // comes back to release.
        store.clear_dirty();
        let r2 = ResourceRevision(2);
        store.provide_asset(logo, r2, raster(10, 10));
        assert_eq!(
            store.dirty(a),
            DirtyClass::EMPTY,
            "the old pixels still draw"
        );
        let second = TextureId::new(2);
        assert_eq!(store.attach_texture(logo, r2, second), Some(first));
        assert!(store.dirty(a).contains(DirtyClass::MEASURE));
        assert!(store.dirty(b).contains(DirtyClass::MEASURE));
        assert_eq!(store.dirty(c), DirtyClass::EMPTY);
        assert_eq!(
            store
                .content_payload(a)
                .map_or(Vec2::ZERO, Content::natural),
            Vec2::new(10.0, 10.0)
        );
        // A texture of a revision the table moved past is handed back.
        assert_eq!(store.attach_texture(logo, r1, first), Some(first));
        assert_eq!(store.assets().texture(logo), Some((r2, second)));

        // The same size again: PAINT only.
        store.clear_dirty();
        let r3 = ResourceRevision(3);
        store.provide_asset(logo, r3, raster(10, 10));
        store.attach_texture(logo, r3, TextureId::new(3));
        assert_eq!(store.dirty(a), DirtyClass::PAINT);
        store.clear_dirty();
        store.provide_asset(logo, r2, raster(30, 30));
        assert_eq!(
            store.dirty(a),
            DirtyClass::EMPTY,
            "older than the resident one"
        );
        assert!(store.assets().payload(logo).is_none());

        // A vector draws as soon as it is decoded, cover cropped to the box.
        let scene = Arc::new(VectorScene {
            paths: Arc::from(vec![Path {
                cmds: vec![
                    PathCmd::MoveTo(Point::new(0.0, 0.0)),
                    PathCmd::LineTo(Point::new(20.0, 10.0)),
                ],
                fill: Some(WHITE),
                stroke: None,
                shadow: None,
            }]),
            size: [20.0, 10.0],
        });
        store.set_asset(c, other, AssetFit::Cover, None);
        store.provide_asset(other, r1, AssetPayload::Vector(scene));
        let out = layout(&mut store);
        let wc = store.world(c);
        let layer = out
            .iter()
            .position(|p| matches!(p, Primitive::Layer(l) if l.clip == wc))
            .expect("a cover clips to the box");
        let Primitive::Path(path) = &out[layer + 1] else {
            panic!("{out:?}");
        };
        assert_eq!(
            path.cmds[1],
            PathCmd::LineTo(Point::new(wc.x + 60.0, wc.y + 40.0)),
            "scaled by 4 to cover the 40x40 box, centered: x from -20"
        );
        assert!(matches!(out[layer + 2], Primitive::LayerEnd));
    }
}
