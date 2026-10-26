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
//! The primitives a drawing maps are the geometric ones: quads, images,
//! paths and meshes. Anything else it holds is skipped.

use std::sync::Arc;

use viso_render::{ImageDraw, MeshPlace, Path, PathCmd, Point, Primitive, Quad, Rect};

use crate::component::{BuildCx, Handle, LeafStyle, NodeStore};
use crate::content::Content;
use crate::dirty::DirtyClass;
use crate::layout::{Size, Vec2};
use crate::node::NodeId;
use crate::style::BoxStyle;

/// Primitives in a drawing space of `extent`, mapped into a node's box when
/// it paints.
#[derive(Debug, Clone)]
pub struct Drawing {
    primitives: Arc<[Primitive]>,
    extent: Vec2,
}

impl PartialEq for Drawing {
    /// Two drawings are the same when they share primitives and extent: a
    /// drawing is compared by identity, never vertex by vertex.
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.primitives, &other.primitives) && self.extent == other.extent
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
        }
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
        let map = Map {
            sx: scale(self.extent.x, world.w),
            sy: scale(self.extent.y, world.h),
            ox: world.x,
            oy: world.y,
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
    pub fn drawing(&mut self, size: Size, drawing: Drawing) -> Handle {
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
    fn the_same_drawing_again_marks_nothing() {
        let mut store = NodeStore::new();
        let drawing = square();
        let id = BuildCx::new(&mut store)
            .drawing(Size::fixed(4.0, 4.0), drawing.clone())
            .id();
        store.clear_dirty();
        store.set_drawing(id, drawing.clone());
        assert!(store.dirty(id).is_empty());
        store.set_drawing(id, square());
        assert_eq!(store.dirty(id), DirtyClass::PAINT);
        assert!(store.drawing(id).is_some());
    }
}
