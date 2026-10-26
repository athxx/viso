//! Folds: a node collapsed to its first child, as a disclosure is while it is
//! not expanded.
//!
//! A folded node keeps its first child, its header, and folds every other
//! child out of layout, paint, hit testing and the semantics tree, as hiding
//! each would; a child linked under it while it is folded is folded too, and
//! one a child's own `visible` hides stays hidden when the fold opens. The
//! table is cold: a store that folds nothing pays one emptiness check where a
//! node's visibility is read.

use crate::component::NodeStore;
use crate::dirty::DirtyClass;
use crate::node::{NodeId, NodeIndex};

/// The folded nodes, by slot. Cold and small: a handful of disclosures at
/// most collapse at once.
#[derive(Debug, Default)]
pub(crate) struct Folds {
    nodes: Vec<NodeId>,
}

impl Folds {
    pub(crate) fn clear(&mut self) {
        self.nodes.clear();
    }

    /// Whether the slot `index` is folded.
    #[inline]
    fn holds(&self, index: NodeIndex) -> bool {
        self.nodes.iter().any(|n| n.index() == index.get())
    }

    /// Forgets the slot `index`, freed or reused.
    pub(crate) fn forget(&mut self, index: u32) {
        self.nodes.retain(|n| n.index() != index);
    }
}

impl NodeStore {
    /// Folds node `id` to its first child, or opens it. Every other child
    /// leaves or rejoins layout, paint, hit testing and the semantics tree;
    /// the children are not rebuilt.
    pub fn set_folded(&mut self, id: NodeId, folded: bool) {
        if !self.arena().is_live(id) || self.folded(id) == folded {
            return;
        }
        if folded {
            self.folds.nodes.push(id);
        } else {
            self.folds.forget(id.index());
        }
        let header = self.arena().first_child(id);
        let mut child = header.and_then(|h| self.arena().next_sibling(h));
        while let Some(c) = child {
            child = self.arena().next_sibling(c);
            self.shown_changed(c);
        }
        self.mark_dirty(
            id,
            DirtyClass::MEASURE | DirtyClass::LAYOUT | DirtyClass::PAINT | DirtyClass::SEMANTICS,
        );
    }

    /// Whether node `id` is folded to its first child.
    pub fn folded(&self, id: NodeId) -> bool {
        self.arena().is_live(id) && self.folds.holds(id.node_index())
    }

    /// Whether the slot `index` lies under a folded parent past its first
    /// child.
    #[inline]
    pub(crate) fn folded_away(&self, index: NodeIndex) -> bool {
        if self.folds.nodes.is_empty() {
            return false;
        }
        let parent = self.arena().parent_of(index);
        !parent.is_none()
            && self.folds.holds(parent)
            && self.arena().first_child_of(parent) != index
    }
}

#[cfg(test)]
mod tests {
    use viso_render::Rect;

    use crate::component::{BuildCx, FlexStyle, LeafStyle, NodeStore};
    use crate::layout::{Axis, Sizing};

    fn heights(
        store: &mut NodeStore,
        root: crate::node::NodeId,
        kids: &[crate::node::NodeId],
    ) -> Vec<f32> {
        let surface = Rect {
            x: 0.0,
            y: 0.0,
            w: 100.0,
            h: 500.0,
        };
        store.layout(root, surface, &mut Vec::new());
        kids.iter().map(|&k| store.bounds(k).h).collect()
    }

    #[test]
    fn a_folded_node_keeps_its_header_and_folds_the_rest_even_children_linked_later() {
        let mut store = NodeStore::new();
        let mut kids = Vec::new();
        let root = {
            let mut cx = BuildCx::new(&mut store);
            let style = FlexStyle {
                axis: Axis::Column,
                ..FlexStyle::default()
            };
            cx.flex(style, |cx| {
                for _ in 0..3 {
                    let leaf = LeafStyle {
                        size: Sizing::fixed(10.0, 20.0),
                        ..LeafStyle::default()
                    };
                    kids.push(cx.leaf(leaf).id());
                }
            })
            .id()
        };
        assert_eq!(heights(&mut store, root, &kids), [20.0; 3]);
        store.set_folded(root, true);
        assert!(store.folded(root));
        assert!(!store.hidden(kids[0]), "the header shows");
        assert!(store.hidden(kids[1]) && store.hidden(kids[2]));
        assert_eq!(heights(&mut store, root, &kids), [20.0, 0.0, 0.0]);

        let late = {
            let mut cx = BuildCx::new(&mut store);
            cx.leaf(LeafStyle::default()).id()
        };
        store.arena_append_child(root, late);
        assert!(store.hidden(late), "a child linked while folded folds");

        store.set_hidden(kids[2], true);
        store.set_folded(root, false);
        assert!(!store.hidden(kids[1]) && !store.hidden(late));
        assert!(store.hidden(kids[2]), "its own hiding outlives the fold");
        assert_eq!(heights(&mut store, root, &kids), [20.0, 20.0, 0.0]);
    }
}
