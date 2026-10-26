//! Backdrops: a box of glass, which blurs, saturates and tints what lies
//! behind it.
//!
//! A backdrop is drawn as the node's frosted surface under its own fill,
//! outside the layer a translucent node fades through, so the glass fades
//! by its own opacity and keeps its blur. Glass boxes over the same
//! background at the same blur share one capture of it and one blur. The
//! table is cold: one entry for each node with a backdrop, kept in slot
//! order so paint finds a node's in a binary search and a tree with none
//! checks an empty table.

use crate::component::NodeStore;
use crate::node::NodeId;
use viso_render::{ColorMatrix, ColorOp, Corners, FrostedMaterial, Rect, Rgba};

/// What a box does to the content behind it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Backdrop {
    /// How far the content behind blurs, as CSS's blur radius: twice the
    /// Gaussian's standard deviation.
    pub blur: f32,
    /// How its saturation scales: `1` keeps it, `0` greys it, more deepens
    /// it.
    pub saturation: f32,
    /// A colour laid over it by the colour's alpha, which keeps the glass
    /// readable over busy content.
    pub tint: Rgba,
    /// A grain over the glass, `0..=1`.
    pub noise: f32,
}

impl Backdrop {
    /// The standard glass: a strong blur, deepened colour and a light
    /// frosting.
    pub const GLASS: Backdrop = Backdrop {
        blur: 30.0,
        saturation: 1.8,
        tint: Rgba {
            r: 1.0,
            g: 1.0,
            b: 1.0,
            // `#ffffff40`, as the DSL's `Backdrop` defaults to.
            a: 64.0 / 255.0,
        },
        noise: 0.0,
    };

    /// The frosted surface it draws over a box of `rect` rounded by
    /// `radius`, faded to `opacity`.
    pub(crate) fn surface(self, rect: Rect, radius: f32, opacity: f32) -> FrostedMaterial {
        let tint = ColorMatrix::tint([self.tint.r, self.tint.g, self.tint.b], self.tint.a);
        let mut surface =
            FrostedMaterial::new(rect, Corners::uniform(radius), self.blur.max(0.0) * 0.5);
        surface.color = ColorOp {
            matrix: ColorMatrix::saturation(self.saturation.max(0.0)).then(&tint),
            gamma: 1.0,
        };
        surface.noise = self.noise.clamp(0.0, 1.0);
        surface.opacity = opacity;
        surface
    }
}

/// The nodes with backdrops, in slot order.
#[derive(Debug, Default)]
pub(crate) struct Backdrops {
    entries: Vec<(u32, Backdrop)>,
}

impl Backdrops {
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
    pub(crate) fn get(&self, index: u32) -> Option<Backdrop> {
        if self.entries.is_empty() {
            return None;
        }
        self.find(index).ok().map(|at| self.entries[at].1)
    }
}

impl NodeStore {
    /// The backdrop of node `id`'s box.
    pub fn backdrop(&self, id: NodeId) -> Option<Backdrop> {
        self.backdrops.get(id.index())
    }

    /// Makes node `id`'s box glass over what lies behind it, or an ordinary
    /// box; a change repaints it. A live-guarded write — a stale handle is a
    /// no-op.
    pub fn set_backdrop(&mut self, id: NodeId, backdrop: Option<Backdrop>) {
        if !self.arena().is_live(id) || self.backdrop(id) == backdrop {
            return;
        }
        let index = id.index();
        match (self.backdrops.find(index), backdrop) {
            (Ok(at), Some(b)) => self.backdrops.entries[at].1 = b,
            (Ok(at), None) => {
                self.backdrops.entries.remove(at);
            }
            (Err(at), Some(b)) => self.backdrops.entries.insert(at, (index, b)),
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
    use viso_render::Primitive;

    fn panel(store: &mut NodeStore, opacity: f32) -> NodeId {
        let mut cx = BuildCx::new(store);
        let panel = cx.leaf(LeafStyle {
            size: Sizing::fixed(200.0, 100.0),
            style: BoxStyle::solid(Rgba {
                r: 1.0,
                g: 1.0,
                b: 1.0,
                a: 0.1,
            })
            .with_radius(12.0),
        });
        cx.backdrop(panel, Backdrop::GLASS);
        cx.opacity(panel, opacity);
        let panel = panel.id();
        store.layout(panel, Rect::new(0.0, 0.0, 400.0, 300.0), &mut Vec::new());
        panel
    }

    fn paint(store: &NodeStore, root: NodeId) -> Vec<Primitive> {
        let mut out = Vec::new();
        crate::paint::paint_tree(store, root, &mut out);
        out
    }

    #[test]
    fn a_glass_box_draws_its_frosted_surface_under_its_fill() {
        let mut store = NodeStore::new();
        let panel = panel(&mut store, 1.0);
        let out = paint(&store, panel);
        let Primitive::Frosted(surface) = &out[0] else {
            panic!("the glass first: {out:?}");
        };
        assert_eq!(surface.rect, store.world(panel), "the box's rect");
        assert_eq!(surface.backdrop_sigma, 15.0);
        assert_eq!(surface.radius, Corners::uniform(12.0), "its corners");
        assert_eq!(surface.opacity, 1.0);
        assert!(matches!(out[1], Primitive::Quad(_)), "then the box's fill");

        store.set_backdrop(panel, None);
        let out = paint(&store, panel);
        assert!(!out.iter().any(|p| matches!(p, Primitive::Frosted(_))));
    }

    #[test]
    fn a_fading_glass_box_fades_its_surface_outside_its_layer() {
        let mut store = NodeStore::new();
        let panel = panel(&mut store, 0.5);
        let out = paint(&store, panel);
        let Primitive::Frosted(surface) = &out[0] else {
            panic!("the glass before the fade: {out:?}");
        };
        assert_eq!(surface.opacity, 0.5);
        assert!(
            matches!(out[1], Primitive::Layer(_)),
            "then the fading layer"
        );
    }
}
