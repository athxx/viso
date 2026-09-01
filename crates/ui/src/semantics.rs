//! Accessibility semantics derived from the retained node tree.
//!
//! A node carries *authored* semantics — its [`Role`] and an optional label — as
//! cold side-storage; the live state (focused, its bounds) is *derived* from the
//! ordinary node columns at derive time, never stored twice.
//! [`NodeStore::derive_semantics`](crate::component::NodeStore::derive_semantics)
//! folds both into a flat [`SemanticsTree`]: a snapshottable, tree-shaped view an
//! assistive technology (or a headless test) reads. Generated from the node
//! model, not a parallel hand-maintained structure.

use crate::node::NodeId;
use viso_render::Rect;

/// The accessibility role of a node — what kind of thing it is to an assistive
/// technology. Coarse this slice (the roles the current widgets need); grows as
/// widgets land. [`Group`](Role::Group) is the default for a pure
/// layout/decoration node that carries no authored semantics and is not
/// interactive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Role {
    /// A structural grouping with no interaction (a layout container).
    #[default]
    Group,
    /// An activatable control (the default role an interactive node takes when
    /// it has a handler but no authored role).
    Button,
    /// A static text label.
    Label,
}

/// A node's *authored* semantics: the facts a builder sets, distinct from the
/// live state derived from other columns. Cold — holds the only heap data (the
/// label), read only by the derive pass.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Semantics {
    /// The node's role. [`Group`](Role::Group) for a plain container.
    pub role: Role,
    /// The accessible name, if any.
    pub label: Option<String>,
}

impl Semantics {
    /// Authored semantics with `role` and no label.
    pub fn role(role: Role) -> Self {
        Self { role, label: None }
    }

    /// This with an accessible label.
    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }
}

/// One node's place in the derived tree: its identity, resolved role and label,
/// live state, bounds, and its children (indices into the owning
/// [`SemanticsTree::nodes`]). Snapshottable.
#[derive(Debug, Clone, PartialEq)]
pub struct SemanticsNode {
    /// The node this row derives from.
    pub id: NodeId,
    /// The resolved role (authored, else a default from interactivity).
    pub role: Role,
    /// The resolved accessible label, if any.
    pub label: Option<String>,
    /// Whether this node currently holds focus (derived from the focus slot).
    pub focused: bool,
    /// The node's layout box.
    pub bounds: Rect,
    /// Indices of this node's children within [`SemanticsTree::nodes`], in tree
    /// order. Indices (not [`NodeId`]s) so a snapshot is self-contained.
    pub children: Vec<usize>,
}

/// A flat, tree-shaped accessibility view: `nodes[0]` is the root, each node
/// naming its children by index. Flat storage keeps it snapshot-friendly and
/// avoids per-node heap nodes. Rebuilt from the node model when a semantic
/// invalidation is pending.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SemanticsTree {
    /// The nodes in pre-order; `nodes[0]` is the root when non-empty.
    pub nodes: Vec<SemanticsNode>,
}

impl SemanticsTree {
    /// The root semantics node, if the tree is non-empty.
    pub fn root(&self) -> Option<&SemanticsNode> {
        self.nodes.first()
    }

    /// The number of nodes in the tree.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Whether the tree has no nodes.
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Find a node by its [`NodeId`], if present.
    pub fn get(&self, id: NodeId) -> Option<&SemanticsNode> {
        self.nodes.iter().find(|n| n.id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::NodeArena;

    const ZERO_RECT: Rect = Rect {
        x: 0.0,
        y: 0.0,
        w: 0.0,
        h: 0.0,
    };

    #[test]
    fn authored_semantics_carries_role_and_label() {
        let s = Semantics::role(Role::Button).with_label("Add");
        assert_eq!(s.role, Role::Button);
        assert_eq!(s.label.as_deref(), Some("Add"));
    }

    #[test]
    fn default_semantics_is_group_no_label() {
        let s = Semantics::default();
        assert_eq!(s.role, Role::Group);
        assert_eq!(s.label, None);
    }

    #[test]
    fn empty_tree_has_no_root() {
        let tree = SemanticsTree::default();
        assert!(tree.is_empty());
        assert_eq!(tree.len(), 0);
        assert!(tree.root().is_none());
    }

    #[test]
    fn hand_built_tree_round_trips_through_accessors() {
        // Value types don't mint ids; borrow two real ones from an arena.
        let mut arena = NodeArena::new();
        let root_id = arena.alloc();
        let child_id = arena.alloc();
        let tree = SemanticsTree {
            nodes: vec![
                SemanticsNode {
                    id: root_id,
                    role: Role::Group,
                    label: None,
                    focused: false,
                    bounds: ZERO_RECT,
                    children: vec![1],
                },
                SemanticsNode {
                    id: child_id,
                    role: Role::Button,
                    label: Some("Add".to_string()),
                    focused: true,
                    bounds: ZERO_RECT,
                    children: Vec::new(),
                },
            ],
        };
        assert_eq!(tree.len(), 2);
        assert_eq!(tree.root().map(|n| n.id), Some(root_id));
        assert_eq!(tree.get(child_id).map(|n| n.role), Some(Role::Button));
        assert!(tree.get(child_id).unwrap().focused);
    }
}
