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

pub use viso_schema::semantics::{InspectKind, LiveRegion, Role, SemanticState};

/// A node's *authored* semantics: the facts a builder sets, distinct from the
/// live state derived from other columns. Cold — holds the only heap data (the
/// label, hint and value text), read only by the derive pass.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Semantics {
    /// The node's role. [`Group`](Role::Group) for a plain container.
    pub role: Role,
    /// The accessible name, if any.
    pub label: Option<String>,
    /// What acting on the node does, read after its name.
    pub hint: Option<String>,
    /// The value it holds, as text: what its role's live state does not say
    /// (a step of a wizard, a rating).
    pub value: Option<String>,
    /// How a change to its content is announced.
    pub live: LiveRegion,
    /// Whether it and its subtree are left out of what assistive technology
    /// reads, though still shown.
    pub hidden: bool,
    /// The level of a [`Heading`](Role::Heading), 1 the topmost.
    pub heading_level: Option<u8>,
}

impl Semantics {
    /// Authored semantics with `role` and nothing else.
    pub fn role(role: Role) -> Self {
        Self {
            role,
            ..Self::default()
        }
    }

    /// This with an accessible label.
    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// This with a hint on what acting on the node does.
    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    /// This with a value as text.
    pub fn with_value(mut self, value: impl Into<String>) -> Self {
        self.value = Some(value.into());
        self
    }

    /// This announcing a change to its content as `live` says.
    pub fn with_live(mut self, live: LiveRegion) -> Self {
        self.live = live;
        self
    }

    /// This left out of what assistive technology reads when `hidden`.
    pub fn with_hidden(mut self, hidden: bool) -> Self {
        self.hidden = hidden;
        self
    }

    /// This a heading of `level`.
    pub fn with_heading_level(mut self, level: u8) -> Self {
        self.heading_level = Some(level);
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
    /// Which of the node's links the row is, counted in the order they
    /// appear in its text; `None` for the node itself.
    pub link: Option<u16>,
    /// The resolved role (authored, else a default from interactivity).
    pub role: Role,
    /// The resolved accessible label, if any.
    pub label: Option<String>,
    /// What acting on it does, if authored.
    pub hint: Option<String>,
    /// Its value as text, if authored.
    pub value: Option<String>,
    /// How a change to its content is announced.
    pub live: LiveRegion,
    /// Its level, when it is a heading with one.
    pub heading_level: Option<u8>,
    /// Whether this node currently holds focus (derived from the focus slot).
    pub focused: bool,
    /// Whether input does not reach it: it or an ancestor is disabled.
    pub disabled: bool,
    /// The node's live accessibility state (checked / value / range / expanded),
    /// derived from the node's [`SemanticState`] side column. `None` for a node
    /// that carries no live state (a plain container or label).
    pub state: Option<SemanticState>,
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

    #[test]
    fn a_role_is_found_by_its_label() {
        for role in Role::ALL {
            assert_eq!(Role::from_label(role.label()), Some(role));
        }
        assert_eq!(Role::from_label("Button"), None);
    }

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
    fn textfield_role_carries_name() {
        let s = Semantics::role(Role::TextField).with_label("Email");
        assert_eq!(s.role, Role::TextField);
        assert_eq!(s.label.as_deref(), Some("Email"));
    }

    #[test]
    fn tab_and_tablist_roles_carry_names() {
        let list = Semantics::role(Role::TabList);
        assert_eq!(list.role, Role::TabList);
        assert_eq!(list.label, None, "the strip itself needs no name");
        let tab = Semantics::role(Role::Tab).with_label("Details");
        assert_eq!(tab.role, Role::Tab);
        assert_eq!(tab.label.as_deref(), Some("Details"));
    }

    #[test]
    fn navigation_role_needs_no_name() {
        let nav = Semantics::role(Role::Navigation);
        assert_eq!(nav.role, Role::Navigation);
        assert_eq!(nav.label, None, "the stack container itself needs no name");
    }

    #[test]
    fn slider_and_radio_roles_are_distinct() {
        // The two roles added this slice — a slider no longer borrows CheckBox,
        // and a radio option no longer borrows CheckBox.
        let slider = Semantics::role(Role::Slider).with_label("Volume");
        assert_eq!(slider.role, Role::Slider);
        assert_eq!(slider.label.as_deref(), Some("Volume"));
        let radio = Semantics::role(Role::Radio).with_label("Small");
        assert_eq!(radio.role, Role::Radio);
        assert_ne!(Role::Slider, Role::CheckBox);
        assert_ne!(Role::Radio, Role::CheckBox);
    }

    #[test]
    fn semantic_state_default_is_all_none() {
        let s = SemanticState::default();
        assert_eq!(s.checked, None);
        assert_eq!(s.value, None);
        assert_eq!(s.range, None);
        assert_eq!(s.expanded, None);
        assert_eq!(s.selected, None);
    }

    #[test]
    fn semantic_state_checked_sets_only_checked() {
        let s = SemanticState::checked(true);
        assert_eq!(s.checked, Some(true));
        assert_eq!(s.value, None);
        assert_eq!(s.range, None);
        assert_eq!(s.expanded, None);
        assert_eq!(s.selected, None);
        assert_eq!(SemanticState::checked(false).checked, Some(false));
    }

    #[test]
    fn semantic_state_slider_sets_value_and_range() {
        let s = SemanticState::slider(42.0, 0.0, 100.0);
        assert_eq!(s.value, Some(42.0));
        assert_eq!(s.range, Some((0.0, 100.0)));
        assert_eq!(s.checked, None);
        assert_eq!(s.expanded, None);
        assert_eq!(s.selected, None);
    }

    #[test]
    fn semantic_state_with_expanded_composes() {
        let s = SemanticState::default().with_expanded(true);
        assert_eq!(s.expanded, Some(true));
        assert_eq!(s.checked, None);
        // Composable onto a checked state without clobbering it.
        let both = SemanticState::checked(true).with_expanded(false);
        assert_eq!(both.checked, Some(true));
        assert_eq!(both.expanded, Some(false));
    }

    #[test]
    fn semantic_state_with_selected_composes() {
        let s = SemanticState::default().with_selected(true);
        assert_eq!(s.selected, Some(true));
        assert_eq!(s.expanded, None);
        assert_eq!(s.checked, None);
        // A tree row carries expanded (directory) and selected together without
        // either clobbering the other — the treeitem shape.
        let row = SemanticState::default()
            .with_expanded(true)
            .with_selected(false);
        assert_eq!(row.expanded, Some(true));
        assert_eq!(row.selected, Some(false));
    }

    #[test]
    fn tree_and_treeitem_roles_are_distinct() {
        let tree = Semantics::role(Role::Tree).with_label("Files");
        assert_eq!(tree.role, Role::Tree);
        assert_eq!(tree.label.as_deref(), Some("Files"));
        let item = Semantics::role(Role::TreeItem).with_label("main.rs");
        assert_eq!(item.role, Role::TreeItem);
        assert_ne!(Role::Tree, Role::TreeItem);
        assert_ne!(Role::Tree, Role::Region);
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
                    link: None,
                    role: Role::Group,
                    label: None,
                    hint: None,
                    value: None,
                    live: LiveRegion::Off,
                    heading_level: None,
                    focused: false,
                    disabled: false,
                    state: None,
                    bounds: ZERO_RECT,
                    children: vec![1],
                },
                SemanticsNode {
                    id: child_id,
                    link: None,
                    role: Role::Button,
                    label: Some("Add".to_string()),
                    hint: None,
                    value: None,
                    live: LiveRegion::Off,
                    heading_level: None,
                    focused: true,
                    disabled: false,
                    state: None,
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
