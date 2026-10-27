//! Transform roots: what moves without a relayout.
//!
//! A node's on-screen box is its layout `bounds` plus a *displacement*: the
//! translate of the node itself and of every ancestor, less the scroll of
//! every scrolling ancestor. Nothing stores the displaced box; it is
//! derived on demand ([`NodeStore::world`]), carried down by the walks that need
//! it (paint, hit testing), and snapped to device pixels so a moved run of text
//! lands on the same pixel grid it was rasterized on.
//!
//! A *transform root* is where a displacement can change on its own: a scroll
//! viewport's content, and a node that has ever been translated. Paint brackets
//! each root's primitives in a [`Primitive::Transform`] and records the root in
//! the [`TransformTable`], in paint (pre-)order, so an enclosing root precedes
//! the roots inside it. A scroll step or a slide marks only `TRANSFORM`: the
//! frame recomputes the table — one add per root, independent of how much
//! content the roots hold — and the renderer moves each root's retained draws
//! by how far it moved (`Renderer::set_transforms`). Nothing is re-measured,
//! re-laid out, repainted or re-uploaded.
//!
//! [`NodeStore::world`]: crate::NodeStore::world
//! [`Primitive::Transform`]: viso_render::Primitive::Transform

use crate::component::NodeStore;
use crate::layout::Vec2;
use crate::node::NodeId;
use viso_render::Affine2;

/// How a node turns about a point of its box after layout: scaled, then
/// rotated clockwise, about `origin` — fractions of the box across and down,
/// across from the start edge, so it follows the reading direction.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Turn {
    pub scale: f32,
    /// Radians, clockwise on screen.
    pub rotation: f32,
    pub origin: crate::layout::Alignment2D,
}

impl Turn {
    /// Not turned: unit scale, no rotation, about the middle.
    pub const NONE: Turn = Turn {
        scale: 1.0,
        rotation: 0.0,
        origin: crate::layout::Alignment2D::Center,
    };

    /// Whether it scales or rotates anything.
    #[inline]
    pub fn turns(&self) -> bool {
        self.scale != 1.0 || self.rotation != 0.0
    }
}

impl Default for Turn {
    fn default() -> Turn {
        Turn::NONE
    }
}

/// One transform root of a painted frame.
#[derive(Debug, Clone, Copy)]
struct Root {
    node: NodeId,
    /// The enclosing root's slot, or `None` at the top.
    parent: Option<u32>,
    /// A scroll viewport's content (moved by its scroll) rather than a
    /// translated node (moved by its translate).
    content: bool,
}

/// The transform roots of the last paint, each one's displacement as
/// painted, and its place now: the affine from where it was painted to where
/// it draws.
#[derive(Debug, Default)]
pub struct TransformTable {
    roots: Vec<Root>,
    /// Each root's unsnapped affine from its layout space to the screen.
    raw: Vec<Affine2>,
    painted: Vec<[f32; 2]>,
    places: Vec<Affine2>,
    /// The overlay roots a paint meets, each with the space it was met in:
    /// scratch for the paint walk, kept so a frame that paints an overlay
    /// allocates nothing.
    pub(crate) overlays: Vec<(NodeId, crate::paint::Space)>,
    /// The children of the layered stacks a paint is inside, each keyed by
    /// its layer and source order: scratch for the paint walk, kept so a
    /// frame allocates nothing.
    pub(crate) layered: Vec<(i64, NodeId)>,
}

impl TransformTable {
    /// Forget every root, for a paint about to record them anew.
    pub fn clear(&mut self) {
        self.roots.clear();
        self.raw.clear();
        self.painted.clear();
        self.places.clear();
    }

    /// Record a root at `node`, inside `parent`, its content painted
    /// displaced by `snapped`. Returns its slot.
    pub(crate) fn push(
        &mut self,
        store: &NodeStore,
        node: NodeId,
        parent: Option<u32>,
        content: bool,
        snapped: Vec2,
    ) -> u32 {
        let slot = self.roots.len() as u32;
        let root = Root {
            node,
            parent,
            content,
        };
        self.roots.push(root);
        let raw = self.raw_of(store, root);
        self.raw.push(raw);
        self.painted.push([snapped.x, snapped.y]);
        self.places.push(place(store, raw, snapped));
        slot
    }

    /// `root`'s unsnapped affine from its layout space to the screen, its
    /// enclosing root's as recorded.
    fn raw_of(&self, store: &NodeStore, root: Root) -> Affine2 {
        let base = match root.parent {
            Some(parent) => self.raw[parent as usize],
            None => store.transform_above(root.node),
        };
        let own = if root.content {
            store.content_transform(root.node)
        } else {
            store.self_transform(root.node)
        };
        own.then(&base)
    }

    /// How many roots the last paint recorded.
    pub fn len(&self) -> usize {
        self.roots.len()
    }

    /// Whether the last paint recorded no root.
    pub fn is_empty(&self) -> bool {
        self.roots.is_empty()
    }

    /// Each root's displacement when its primitives were painted, by slot.
    pub fn painted(&self) -> &[[f32; 2]] {
        &self.painted
    }

    /// Each root's place now, by slot: the identity for a root where it was
    /// painted.
    pub fn places(&self) -> &[Affine2] {
        &self.places
    }

    /// Recompute every root's place from the scrolls, translates and turns as
    /// they stand, and report whether any root moved since the last call. A
    /// root whose node is gone keeps its value: its primitives are stale until
    /// the repaint its removal scheduled.
    pub fn update(&mut self, store: &NodeStore) -> bool {
        let mut moved = false;
        for slot in 0..self.roots.len() {
            let root = self.roots[slot];
            if !store.arena().is_live(root.node) {
                continue;
            }
            let raw = self.raw_of(store, root);
            self.raw[slot] = raw;
            let [x, y] = self.painted[slot];
            let now = place(store, raw, Vec2 { x, y });
            if self.places[slot] != now {
                self.places[slot] = now;
                moved = true;
            }
        }
        moved
    }
}

impl TransformTable {
    /// Every shown text node that draws scaled or rotated, with the affine
    /// from its layout space to the screen's linear part: the text under each
    /// root that turns, down to the next turned node, which is a root of its
    /// own. `stack` is scratch. A table whose roots only translate visits no
    /// node.
    pub fn turned_text(
        &self,
        store: &NodeStore,
        out: &mut Vec<(NodeId, Affine2)>,
        stack: &mut Vec<NodeId>,
    ) {
        out.clear();
        for (root, raw) in self.roots.iter().zip(&self.raw) {
            // A scroll or a translate inside a turned node is walked with it.
            if root.content
                || !store.arena().is_live(root.node)
                || !store.turn(root.node).turns()
                || raw.is_translation()
            {
                continue;
            }
            let linear = Affine2 {
                e: 0.0,
                f: 0.0,
                ..*raw
            };
            stack.clear();
            stack.push(root.node);
            while let Some(id) = stack.pop() {
                if store.hidden(id) {
                    continue;
                }
                if matches!(
                    store.content_payload(id),
                    Some(crate::content::Content::Text { .. })
                ) {
                    out.push((id, linear));
                }
                stack.extend(
                    store
                        .arena()
                        .children(id)
                        .filter(|&child| !store.turn(child).turns()),
                );
            }
        }
    }
}

/// Where content painted displaced by `painted` draws under `raw`: moved by
/// the difference, snapped to device pixels, while `raw` only translates;
/// mapped by it, from where it was painted, once it turns.
fn place(store: &NodeStore, raw: Affine2, painted: Vec2) -> Affine2 {
    if raw.is_translation() {
        let now = store.snap_shift(raw.translation());
        return Affine2::translate(now.x - painted.x, now.y - painted.y);
    }
    Affine2::translate(-painted.x, -painted.y).then(&raw)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::component::{BuildCx, FlexStyle, LeafStyle};
    use crate::dirty::DirtyClass;
    use crate::hit_test::hit_test;
    use crate::layout::{Alignment2D, Axis, Sizing};
    use crate::style::BoxStyle;
    use viso_render::{Rect, Rgba};

    const QUARTER: f32 = std::f32::consts::FRAC_PI_2;

    /// A 100×100 row holding a tall bar, 20 across and 60 down, at the top
    /// left.
    fn bar() -> (NodeStore, NodeId, NodeId) {
        let mut store = NodeStore::new();
        let mut bar = None;
        let root = {
            let mut cx = BuildCx::new(&mut store);
            cx.flex(
                FlexStyle {
                    axis: Axis::Row,
                    size: Sizing::fixed(100.0, 100.0),
                    ..Default::default()
                },
                |cx| {
                    bar = Some(cx.leaf(LeafStyle {
                        size: Sizing::fixed(20.0, 60.0),
                        style: BoxStyle::solid(Rgba::new(1.0, 0.0, 0.0, 1.0)),
                    }));
                },
            );
            cx.root().unwrap()
        };
        let surface = Rect::new(0.0, 0.0, 100.0, 100.0);
        store.layout(root, surface, &mut Vec::new());
        (store, root, bar.unwrap().id())
    }

    fn near(a: Rect, b: Rect) -> bool {
        (a.x - b.x).abs() < 1e-3
            && (a.y - b.y).abs() < 1e-3
            && (a.w - b.w).abs() < 1e-3
            && (a.h - b.h).abs() < 1e-3
    }

    #[test]
    fn a_turned_node_lands_turned_about_its_origin() {
        let (mut store, _, bar) = bar();
        store.set_rotation(bar, QUARTER);
        // About its middle (10, 30): it lies across, 60 by 20.
        assert!(near(store.world(bar), Rect::new(-20.0, 20.0, 60.0, 20.0)));
        store.set_transform_origin(bar, Alignment2D::TopStart);
        // About its top left: it swings left of the origin.
        assert!(near(store.world(bar), Rect::new(-60.0, 0.0, 60.0, 20.0)));
        store.set_turn(bar, Turn::NONE);
        store.set_scale(bar, 2.0);
        assert!(near(store.world(bar), Rect::new(-10.0, -30.0, 40.0, 120.0)));
        let local = store.to_local(bar, 30.0, 90.0);
        assert!((local.x - 20.0).abs() < 1e-3 && (local.y - 60.0).abs() < 1e-3);
    }

    #[test]
    fn a_turned_node_is_hit_where_it_lands() {
        let (mut store, root, bar) = bar();
        assert_eq!(hit_test(&store, root, 10.0, 50.0), Some(bar));
        assert_eq!(hit_test(&store, root, 35.0, 30.0), Some(root));
        store.set_rotation(bar, QUARTER);
        assert_eq!(hit_test(&store, root, 35.0, 30.0), Some(bar), "across");
        assert_eq!(
            hit_test(&store, root, 10.0, 50.0),
            Some(root),
            "no longer down"
        );
    }

    #[test]
    fn a_turned_root_draws_through_its_place_and_follows_its_box() {
        let (mut store, root, bar) = bar();
        store.set_rotation(bar, QUARTER);
        assert!(
            store.any_dirty_class(DirtyClass::PAINT),
            "a first turn paints"
        );
        let mut out = Vec::new();
        store.repaint(root, &mut out);
        store.clear_dirty();
        let place = store.transforms().places()[0];
        assert!(!place.is_translation(), "the turn is the place's");
        let p = place.transform_point(Vec2 { x: 10.0, y: 0.0 });
        assert!((p.x - 40.0).abs() < 1e-3 && (p.y - 30.0).abs() < 1e-3);

        store.set_rotation(bar, 2.0 * QUARTER);
        assert!(
            !store.any_dirty_class(DirtyClass::PAINT),
            "a later turn only moves"
        );
        assert!(store.transforms_pending());
        assert!(store.update_transforms());
        let p = store.transforms().places()[0].transform_point(Vec2 { x: 10.0, y: 0.0 });
        assert!((p.x - 10.0).abs() < 1e-3 && (p.y - 60.0).abs() < 1e-3);
        store.clear_dirty();
        assert!(!store.transforms_pending());

        store.set_fixed_size(bar, Sizing::fixed(20.0, 80.0));
        store.layout(root, Rect::new(0.0, 0.0, 100.0, 100.0), &mut Vec::new());
        assert!(store.transforms_pending(), "its pivot moved with its box");
    }

    fn text() -> crate::content::Content {
        crate::content::Content::Text {
            glyphs: Vec::new(),
            atlas: viso_render::TextureId::new(1),
            lane: viso_render::GlyphLane::CoverageA8,
            color_glyphs: Vec::new(),
            color_atlas: None,
            color: Rgba::TRANSPARENT,
            natural: Vec2 { x: 10.0, y: 10.0 },
            baseline: 8.0,
            shaped_at_width: None,
            width_use: crate::text_style::WidthUse::Ignored,
            min_content: 10.0,
            max_content: 10.0,
            ink: None,
        }
    }

    #[test]
    fn turned_text_is_the_text_under_each_turn_with_its_linear_part() {
        let mut store = NodeStore::new();
        let (mut card, mut inner, mut texts) = (None, None, Vec::new());
        let root = {
            let mut cx = BuildCx::new(&mut store);
            cx.flex(FlexStyle::default(), |cx| {
                card = Some(
                    cx.flex(FlexStyle::default(), |cx| {
                        texts.push(cx.leaf(LeafStyle::default()).id());
                        texts.push(cx.leaf(LeafStyle::default()).id());
                        inner = Some(
                            cx.flex(FlexStyle::default(), |cx| {
                                texts.push(cx.leaf(LeafStyle::default()).id());
                            })
                            .id(),
                        );
                    })
                    .id(),
                );
                texts.push(cx.leaf(LeafStyle::default()).id());
            })
            .id()
        };
        let (card, inner) = (card.unwrap(), inner.unwrap());
        for &id in &texts {
            store.set_content_payload(id, text());
        }
        let surface = Rect::new(0.0, 0.0, 400.0, 300.0);
        let (mut out, mut stack, mut prims) = (Vec::new(), Vec::new(), Vec::new());
        let mut frame = |store: &mut NodeStore, out: &mut Vec<(NodeId, Affine2)>| {
            store.layout(root, surface, &mut Vec::new());
            store.repaint(root, &mut prims);
            store.clear_dirty();
            store.transforms().turned_text(store, out, &mut stack);
        };

        frame(&mut store, &mut out);
        assert!(out.is_empty(), "nothing turns");

        store.set_scale(card, 2.0);
        store.set_rotation(inner, QUARTER);
        store.set_hidden(texts[1], true);
        frame(&mut store, &mut out);
        out.sort_by_key(|(id, _)| id.index());
        assert_eq!(out.len(), 2, "the shown text under the turns, once each");
        let (scaled, turned) = (out[0], out[1]);
        assert_eq!(scaled.0, texts[0]);
        assert!((scaled.1.a - 2.0).abs() < 1e-5 && scaled.1.is_scale_translate());
        assert_eq!((scaled.1.e, scaled.1.f), (0.0, 0.0));
        assert_eq!(turned.0, texts[2]);
        assert!(!turned.1.is_scale_translate());
        assert!((turned.1.mean_scale() - 2.0).abs() < 1e-4);

        store.set_turn(card, Turn::NONE);
        store.set_turn(inner, Turn::NONE);
        frame(&mut store, &mut out);
        assert!(out.is_empty(), "untouched again");
    }
}
