//! Shadows: a soft shadow a node's box casts under itself, lifting a menu,
//! a popover or a card off what lies beneath.
//!
//! A shadow is the box's rounded rect grown by its spread, moved by its
//! offset and blurred; it paints under the box, as part of the node's own
//! drawables, in closed form (no offscreen pass). An inset shadow darkens
//! the inside of the box near its edge instead, over its fill and within its
//! border. The table is cold: one
//! entry for each node that casts one, kept in slot order so paint finds a
//! node's in a binary search and a tree with none checks an empty table.

use crate::component::NodeStore;
use crate::layout::Vec2;
use crate::node::NodeId;
use viso_render::Rgba;

/// A soft shadow under a node's box.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Shadow {
    pub color: Rgba,
    /// How far it falls from the box.
    pub offset: Vec2,
    /// How far its edge fades, as CSS's blur radius: twice the Gaussian's
    /// standard deviation.
    pub blur: f32,
    /// How far it grows past the box before it fades; negative shrinks it.
    /// An inset shadow's spread grows it inward.
    pub spread: f32,
    /// Whether it falls inside the box rather than under it.
    pub inset: bool,
}

impl Shadow {
    /// A shadow of `color` falling `y` below the box and fading over `blur`.
    pub const fn below(color: Rgba, y: f32, blur: f32) -> Self {
        Shadow {
            color,
            offset: Vec2 { x: 0.0, y },
            blur,
            spread: 0.0,
            inset: false,
        }
    }

    /// The same shadow, falling inside the box.
    pub const fn inside(self) -> Self {
        Shadow {
            inset: true,
            ..self
        }
    }

    /// The shadow of a floating layer: a menu, a list, a popover.
    pub const FLOATING: Shadow = Shadow::below(
        Rgba {
            r: 0.0,
            g: 0.0,
            b: 0.0,
            a: 0.22,
        },
        6.0,
        20.0,
    );
}

/// The nodes that cast shadows, in slot order.
#[derive(Debug, Default)]
pub(crate) struct Shadows {
    entries: Vec<(u32, Shadow)>,
}

impl Shadows {
    pub(crate) fn clear(&mut self) {
        self.entries.clear();
    }

    /// Forgets the slot `index`, freed or reused.
    pub(crate) fn forget(&mut self, index: u32) {
        if let Ok(at) = self.find(index) {
            self.entries.remove(at);
        }
    }

    fn find(&self, index: u32) -> Result<usize, usize> {
        self.entries.binary_search_by_key(&index, |e| e.0)
    }

    #[inline]
    pub(crate) fn get(&self, index: u32) -> Option<Shadow> {
        if self.entries.is_empty() {
            return None;
        }
        self.find(index).ok().map(|at| self.entries[at].1)
    }
}

impl NodeStore {
    /// The shadow node `id` casts.
    pub fn shadow(&self, id: NodeId) -> Option<Shadow> {
        self.shadows.get(id.index())
    }

    /// Makes node `id` cast `shadow` under its box, or none; a change
    /// repaints it. A live-guarded write — a stale handle is a no-op.
    pub fn set_shadow(&mut self, id: NodeId, shadow: Option<Shadow>) {
        if !self.arena().is_live(id) || self.shadow(id) == shadow {
            return;
        }
        let index = id.index();
        match (self.shadows.find(index), shadow) {
            (Ok(at), Some(s)) => self.shadows.entries[at].1 = s,
            (Ok(at), None) => {
                self.shadows.entries.remove(at);
            }
            (Err(at), Some(s)) => self.shadows.entries.insert(at, (index, s)),
            (Err(_), None) => {}
        }
        self.mark_dirty(id, crate::dirty::DirtyClass::PAINT);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::component::{BuildCx, LeafStyle};
    use crate::layout::Sizing;
    use crate::style::BoxStyle;

    #[test]
    fn a_node_casts_its_shadow_under_its_box() {
        let mut store = NodeStore::new();
        let card = {
            let mut cx = BuildCx::new(&mut store);
            let card = cx.leaf(LeafStyle {
                size: Sizing::fixed(100.0, 40.0),
                style: BoxStyle::solid(Rgba {
                    r: 1.0,
                    g: 1.0,
                    b: 1.0,
                    a: 1.0,
                })
                .with_radius(8.0),
            });
            cx.shadow(card, Shadow::FLOATING);
            card.id()
        };
        assert_eq!(store.shadow(card), Some(Shadow::FLOATING));
        store.layout(
            card,
            viso_render::Rect::new(0.0, 0.0, 400.0, 300.0),
            &mut Vec::new(),
        );
        let mut out = Vec::new();
        crate::paint::paint_tree(&store, card, &mut out);
        let viso_render::Primitive::AnalyticShadow(s) = &out[0] else {
            panic!("the shadow first: {out:?}");
        };
        assert_eq!(s.rect, store.world(card), "the box's rect");
        assert_eq!(s.offset, [0.0, 6.0]);
        assert_eq!(s.sigma, 10.0);
        assert_eq!(s.radius, viso_render::Corners::uniform(8.0), "its corners");
        assert!(
            matches!(out[1], viso_render::Primitive::Quad(_)),
            "then the box"
        );
        store.set_shadow(card, None);
        assert_eq!(store.shadow(card), None);
    }

    #[test]
    fn an_inset_shadow_falls_inside_the_box_over_its_fill_and_within_its_border() {
        let mut store = NodeStore::new();
        let white = Rgba {
            r: 1.0,
            g: 1.0,
            b: 1.0,
            a: 1.0,
        };
        let card = {
            let mut cx = BuildCx::new(&mut store);
            let card = cx.leaf(LeafStyle {
                size: Sizing::fixed(100.0, 40.0),
                style: BoxStyle::solid(white).with_radius(8.0),
            });
            cx.shadow(card, Shadow::FLOATING.inside());
            card.id()
        };
        store.set_border(
            card,
            viso_render::Border {
                width: 2.0,
                color: white,
            },
        );
        store.layout(
            card,
            viso_render::Rect::new(0.0, 0.0, 400.0, 300.0),
            &mut Vec::new(),
        );
        let mut out = Vec::new();
        crate::paint::paint_tree(&store, card, &mut out);
        assert!(
            matches!(out[0], viso_render::Primitive::Quad(_)),
            "the box first: {out:?}"
        );
        let viso_render::Primitive::AnalyticShadow(s) = &out[1] else {
            panic!("then the shadow: {out:?}");
        };
        assert!(s.inner);
        let world = store.world(card);
        assert_eq!(
            s.rect,
            viso_render::Rect::new(world.x + 2.0, world.y + 2.0, world.w - 4.0, world.h - 4.0),
            "within the border"
        );
        assert_eq!(s.radius, viso_render::Corners::uniform(6.0));
    }
}
