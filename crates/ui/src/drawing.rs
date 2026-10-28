//! A retained drawing: primitives built once — off the loop thread, say — in
//! a drawing space of their own, and mapped into whatever box the node is
//! given each time it paints.
//!
//! A drawing is a node's content ([`Content::Drawing`]). Its primitives are
//! shared, so setting the same drawing again, or moving and resizing its
//! node, never copies or rebuilds them; and a node that only moves with a
//! transform root (a pan, a zoom, a spin) does not even repaint. The drawing
//! space runs from `(0, 0)` to its `extent`, which maps onto the node's box —
//! stretched to fill it on each axis — so the same primitives draw at any
//! size. A mesh is placed, not copied: its shared buffers go to the
//! renderer as they are, and its vertices keep their screen extrusion,
//! unscaled, so a line built that way keeps its width at every size.
//!
//! A path's stroke keeps its width as it maps, as a mesh's extrusion does,
//! unless the drawing [scales its strokes](Drawing::scaling_strokes): then
//! the width and dashes grow with the box, as a page's lines do with its
//! zoom.
//!
//! The primitives a drawing maps are the geometric ones: quads, images,
//! paths and meshes. Anything else it holds is skipped.

use std::sync::Arc;

use viso_render::{ImageDraw, MeshPlace, Path, PathCmd, Point, Primitive, Quad, Rect, Stroke};

use crate::component::{BuildCx, Handle, LeafStyle, NodeStore};
use crate::content::Content;
use crate::dirty::DirtyClass;
use crate::layout::{Sizing, Vec2};
use crate::node::NodeId;
use crate::style::BoxStyle;

/// Primitives in a drawing space of `extent`, mapped into a node's box when
/// it paints.
#[derive(Debug, Clone)]
pub struct Drawing {
    primitives: Arc<[Primitive]>,
    extent: Vec2,
    scales_strokes: bool,
}

impl PartialEq for Drawing {
    /// Two drawings are the same when they share primitives and extent: a
    /// drawing is compared by identity, never vertex by vertex.
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.primitives, &other.primitives)
            && self.extent == other.extent
            && self.scales_strokes == other.scales_strokes
    }
}

impl Drawing {
    /// A drawing of `primitives`, whose space runs from the origin to
    /// `extent` and maps onto the node's box. A zero extent on an axis maps
    /// it unscaled, from the box's origin.
    pub fn new(primitives: impl Into<Arc<[Primitive]>>, extent: Vec2) -> Self {
        Self {
            primitives: primitives.into(),
            extent,
            scales_strokes: false,
        }
    }

    /// Scale its paths' strokes with the box: their widths by the mean of
    /// the two axes' scales, their dashes alike. A hairline stays one.
    pub fn scaling_strokes(mut self) -> Self {
        self.scales_strokes = true;
        self
    }

    /// The drawing's primitives, in drawing space.
    pub fn primitives(&self) -> &Arc<[Primitive]> {
        &self.primitives
    }

    /// The size of the drawing space that maps onto the node's box.
    pub fn extent(&self) -> Vec2 {
        self.extent
    }

    /// Emit the drawing mapped into `world`.
    pub(crate) fn paint(&self, world: Rect, out: &mut Vec<Primitive>) {
        let scale = |extent: f32, size: f32| if extent > 0.0 { size / extent } else { 1.0 };
        let (sx, sy) = (scale(self.extent.x, world.w), scale(self.extent.y, world.h));
        let map = Map {
            sx,
            sy,
            ox: world.x,
            oy: world.y,
            stroke: if self.scales_strokes {
                (sx * sy).abs().sqrt()
            } else {
                1.0
            },
        };
        out.reserve(self.primitives.len());
        out.extend(self.primitives.iter().filter_map(|p| map.primitive(p)));
    }
}

/// Drawing space to world: a scale on each axis, then a shift.
#[derive(Clone, Copy)]
struct Map {
    sx: f32,
    sy: f32,
    ox: f32,
    oy: f32,
    /// What stroke widths scale by.
    stroke: f32,
}

impl Map {
    #[inline]
    fn x(self, x: f32) -> f32 {
        x * self.sx + self.ox
    }

    #[inline]
    fn y(self, y: f32) -> f32 {
        y * self.sy + self.oy
    }

    fn point(self, p: Point) -> Point {
        Point::new(self.x(p.x), self.y(p.y))
    }

    fn rect(self, r: Rect) -> Rect {
        Rect {
            x: self.x(r.x),
            y: self.y(r.y),
            w: r.w * self.sx,
            h: r.h * self.sy,
        }
    }

    fn cmd(self, cmd: PathCmd) -> PathCmd {
        let t = |p| self.point(p);
        match cmd {
            PathCmd::MoveTo(p) => PathCmd::MoveTo(t(p)),
            PathCmd::LineTo(p) => PathCmd::LineTo(t(p)),
            PathCmd::QuadTo(c, p) => PathCmd::QuadTo(t(c), t(p)),
            PathCmd::CubicTo(c1, c2, p) => PathCmd::CubicTo(t(c1), t(c2), t(p)),
            PathCmd::Close => PathCmd::Close,
        }
    }

    fn stroke(self, mut stroke: Stroke) -> Stroke {
        if self.stroke != 1.0 {
            stroke.width *= self.stroke;
            if let Some(dash) = &mut stroke.dash {
                dash.segments.iter_mut().for_each(|s| *s *= self.stroke);
                dash.offset *= self.stroke;
            }
        }
        stroke
    }

    fn primitive(self, p: &Primitive) -> Option<Primitive> {
        Some(match p {
            Primitive::Quad(q) => Primitive::Quad(Quad {
                rect: self.rect(q.rect),
                ..*q
            }),
            Primitive::Image(image) => Primitive::Image(ImageDraw {
                rect: self.rect(image.rect),
                mask: None,
                ..*image
            }),
            Primitive::Path(path) => Primitive::Path(Box::new(Path {
                cmds: path.cmds.iter().map(|&c| self.cmd(c)).collect(),
                stroke: path.stroke.map(|s| self.stroke(s)),
                ..(**path).clone()
            })),
            // The shared buffers, placed: no vertex is copied.
            Primitive::Mesh(mesh) => Primitive::Mesh(mesh.clone().placed(MeshPlace {
                scale: [self.sx, self.sy],
                offset: [self.ox, self.oy],
            })),
            _ => return None,
        })
    }
}

impl NodeStore {
    /// What node `id` draws, if its content is a drawing.
    pub fn drawing(&self, id: NodeId) -> Option<&Drawing> {
        match self.content_payload(id) {
            Some(Content::Drawing(drawing)) => Some(drawing),
            _ => None,
        }
    }

    /// Make node `id` draw `drawing`, repainting it (`PAINT`). A drawing
    /// sizes to nothing of its own, so only a node taking it from other
    /// content re-measures; setting the drawing it already shows does
    /// nothing. A live-guarded write.
    pub fn set_drawing(&mut self, id: NodeId, drawing: Drawing) {
        if !self.arena().is_live(id) {
            return;
        }
        match self.drawing(id) {
            Some(shown) if *shown == drawing => {}
            Some(_) => {
                if let Some(Content::Drawing(shown)) = self.content_payload_mut(id) {
                    **shown = drawing;
                }
                self.mark_dirty(id, DirtyClass::PAINT);
            }
            None => self.set_content_payload(id, Content::Drawing(Box::new(drawing))),
        }
    }
}

impl BuildCx<'_> {
    /// A drawing leaf asking `size` of its parent and drawing `drawing` into
    /// its box (see [`NodeStore::set_drawing`]).
    pub fn drawing(&mut self, size: Sizing, drawing: Drawing) -> Handle {
        let handle = self.leaf(LeafStyle {
            size,
            style: BoxStyle::NONE,
        });
        self.store.set_drawing(handle.id(), drawing);
        handle
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use viso_render::{Border, MeshVertex, Rgba};

    const INK: Rgba = Rgba::new(0.0, 0.0, 0.0, 1.0);

    fn square() -> Drawing {
        let v = |x: f32, y: f32| MeshVertex::new([x, y], [0.0, 0.0, 0.0, 1.0], 1.0);
        Drawing::new(
            vec![
                Primitive::Quad(Quad {
                    rect: Rect {
                        x: 1.0,
                        y: 1.0,
                        w: 2.0,
                        h: 2.0,
                    },
                    color: INK,
                    radius: 0.0,
                    border: Border::NONE,
                }),
                Primitive::Mesh(viso_render::Mesh::new(
                    vec![v(0.0, 0.0), v(4.0, 0.0), v(4.0, 4.0)],
                    vec![0, 1, 2],
                )),
                Primitive::LayerEnd,
            ],
            Vec2 { x: 4.0, y: 4.0 },
        )
    }

    #[test]
    fn maps_drawing_space_onto_the_box() {
        let mut out = Vec::new();
        let world = Rect {
            x: 10.0,
            y: 20.0,
            w: 40.0,
            h: 8.0,
        };
        square().paint(world, &mut out);
        assert_eq!(out.len(), 2, "a layer end is not geometry");
        let Primitive::Quad(q) = &out[0] else {
            panic!("a quad")
        };
        assert_eq!(
            q.rect,
            Rect {
                x: 20.0,
                y: 22.0,
                w: 20.0,
                h: 4.0
            }
        );
        let Primitive::Mesh(m) = &out[1] else {
            panic!("a mesh")
        };
        assert_eq!(m.place.apply(m.vertices[2].pos), [50.0, 28.0]);
    }

    #[test]
    fn strokes_scale_only_when_asked() {
        let line = || {
            vec![Primitive::Path(Box::new(Path {
                cmds: vec![
                    PathCmd::MoveTo(Point::new(0.0, 0.0)),
                    PathCmd::LineTo(Point::new(4.0, 4.0)),
                ],
                fill: None,
                stroke: Some(Stroke {
                    dash: Some(viso_render::DashPattern::new(&[1.0, 2.0], 0.5)),
                    ..Stroke::new(1.0, INK)
                }),
                shadow: None,
            }))]
        };
        let world = Rect {
            x: 0.0,
            y: 0.0,
            w: 8.0,
            h: 32.0,
        };
        let stroke_of = |drawing: Drawing| {
            let mut out = Vec::new();
            drawing.paint(world, &mut out);
            let Primitive::Path(path) = &out[0] else {
                panic!("a path")
            };
            path.stroke.unwrap()
        };
        let extent = Vec2 { x: 4.0, y: 4.0 };
        assert_eq!(stroke_of(Drawing::new(line(), extent)).width, 1.0);
        let scaled = stroke_of(Drawing::new(line(), extent).scaling_strokes());
        assert_eq!(scaled.width, 4.0);
        let dash = scaled.dash.unwrap();
        assert_eq!(
            (dash.segments[0], dash.segments[1], dash.offset),
            (4.0, 8.0, 2.0)
        );
    }

    #[test]
    fn the_same_drawing_again_marks_nothing() {
        let mut store = NodeStore::new();
        let drawing = square();
        let id = BuildCx::new(&mut store)
            .drawing(Sizing::fixed(4.0, 4.0), drawing.clone())
            .id();
        store.clear_dirty();
        store.set_drawing(id, drawing.clone());
        assert!(store.dirty(id).is_empty());
        store.set_drawing(id, square());
        assert_eq!(store.dirty(id), DirtyClass::PAINT);
        assert!(store.drawing(id).is_some());
    }
}
