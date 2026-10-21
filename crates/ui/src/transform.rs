//! Transform roots: what moves without a relayout (§21, ADR 0043).
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

/// The transform roots of the last paint, and each one's displacement: as
/// painted, and now.
#[derive(Debug, Default)]
pub struct TransformTable {
    roots: Vec<Root>,
    /// Unsnapped displacement of each root, scratch for [`update`](Self::update).
    raw: Vec<Vec2>,
    painted: Vec<[f32; 2]>,
    current: Vec<[f32; 2]>,
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
        self.current.clear();
    }

    /// Record a root at `node`, inside `parent`, displaced by `raw` (unsnapped)
    /// and `snapped` as painted. Returns its slot.
    pub(crate) fn push(
        &mut self,
        node: NodeId,
        parent: Option<u32>,
        content: bool,
        raw: Vec2,
        snapped: Vec2,
    ) -> u32 {
        let slot = self.roots.len() as u32;
        self.roots.push(Root {
            node,
            parent,
            content,
        });
        self.raw.push(raw);
        self.painted.push([snapped.x, snapped.y]);
        self.current.push([snapped.x, snapped.y]);
        slot
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

    /// Each root's displacement now, by slot.
    pub fn current(&self) -> &[[f32; 2]] {
        &self.current
    }

    /// Recompute every root's displacement from the scrolls and translates as
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
            let base = match root.parent {
                Some(parent) => self.raw[parent as usize],
                None => store.shift_above(root.node),
            };
            let raw = if root.content {
                base + store.content_shift(root.node)
            } else {
                base + store.self_shift(root.node)
            };
            self.raw[slot] = raw;
            let snapped = store.snap_shift(raw);
            let now = [snapped.x, snapped.y];
            if self.current[slot] != now {
                self.current[slot] = now;
                moved = true;
            }
        }
        moved
    }
}
