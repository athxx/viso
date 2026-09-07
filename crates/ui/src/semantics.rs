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
    /// A two-state toggle: checked or unchecked. Carries a boolean state an
    /// assistive technology announces. The live checked value reaches the
    /// derived tree through the node's [`SemanticState`] side column (projected
    /// from the control's reactive cell in the flush phase), so a snapshot
    /// announces `checked`, not just the role and name.
    CheckBox,
    /// A slider: a continuous value picked from a range. Announces the current
    /// value and its `[min, max]` bounds. The live value reaches the derived
    /// tree through the node's [`SemanticState`] side column (`value` + `range`,
    /// projected from the control's reactive cell in the flush phase); the cold
    /// role carries neither.
    Slider,
    /// One option in a radio group: exactly one of the group is selected.
    /// Announces its selected state as `checked` (WAI-ARIA `role=radio` uses the
    /// checked state for selection). The live selection reaches the derived tree
    /// through the node's [`SemanticState`] side column, projected from the
    /// group's reactive cell in the flush phase.
    Radio,
    /// A static text label.
    Label,
    /// An editable text field: accepts typed characters and IME composition,
    /// carries a caret and an optional selection. The live text value, caret,
    /// and selection are held in the control's own reactive cells, not this cold
    /// role; this slice announces the role and name and proves the editing state
    /// through the control's cells and input tapes (the derive pass has no state
    /// store today — the same treatment as [`CheckBox`](Role::CheckBox)).
    TextField,
    /// One selectable tab in a [`TabList`](Role::TabList): activating it shows
    /// its associated panel and hides the others. Which tab is selected is held
    /// in the control's own reactive cell, not this cold role; this slice
    /// announces the role and name and proves the selection through the control's
    /// cell and input tapes (the derive pass has no state store today — the same
    /// treatment as [`CheckBox`](Role::CheckBox)).
    Tab,
    /// The container of a set of [`Tab`](Role::Tab)s — the tab strip. Groups the
    /// tabs so an assistive technology can present them as one selectable set.
    TabList,
    /// A navigation container holding a stack of pages, of which only the top is
    /// visible. Pushing a page reveals it and hides the one below; popping does
    /// the reverse. Which page is on top is held in the control's own reactive
    /// cell, not this cold role; this slice announces the role and proves the
    /// stack depth through the control's cell and input tapes (the derive pass
    /// has no state store today — the same treatment as
    /// [`CheckBox`](Role::CheckBox)).
    Navigation,
    /// A modal dialog: a window layered over the scene that takes focus and, while
    /// open, confines keyboard navigation to its own subtree (a focus scope) so
    /// the background is inert until it closes. An assistive technology announces
    /// it as a dialog and scopes its reading to the dialog's content. Whether the
    /// dialog is open is held in the control's own reactive cell, not this cold
    /// role; this slice announces the role and proves the open state and focus
    /// trap through the control's cell and input tapes (the derive pass has no
    /// state store today — the same treatment as [`CheckBox`](Role::CheckBox)).
    Dialog,
    /// A polite live region: a transient status message layered over the scene
    /// that announces itself without moving focus (WAI-ARIA `role=status`). The
    /// [`Toast`](../../viso_widgets/index.html) control's content carries this
    /// role. Unlike a [`Dialog`](Role::Dialog) it neither takes focus nor traps
    /// keyboard navigation — an assistive technology reads it when it appears and
    /// the user keeps working. Whether the status is showing is held in the
    /// control's own reactive cell, not this cold role; this slice announces the
    /// role and proves the show/auto-dismiss through the control's cell, input
    /// tapes, and the one-shot timer (the derive pass has no state store today —
    /// the same treatment as [`CheckBox`](Role::CheckBox)).
    Status,
    /// A named landmark region: a perceivable, standalone section of the scene an
    /// assistive technology can navigate to and announce by name (WAI-ARIA
    /// `role=region`). Stronger than [`Group`](Role::Group), which is a mute
    /// structural container, and distinct from [`Navigation`](Role::Navigation),
    /// which is specifically a page stack. The [`Dock`](../../viso_widgets/index.html)
    /// container and each detached floating panel carry this role so a screen
    /// reader can jump between docked areas by name. Purely structural — it carries
    /// no live reactive state, only its role and label.
    Region,
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

/// A node's *live* accessibility state — the reactive facts an assistive
/// technology announces alongside the role and name: whether a checkbox is
/// checked, a slider's current value and range, whether a disclosure is
/// expanded. Distinct from authored [`Semantics`] (cold, static role + label)
/// and derived at flush time.
///
/// This is the projection target that keeps the derive pass single-layer: a
/// control's reactive cell drives a parallel binding that writes this struct
/// into the node's side column during the flush phase (where both stores are
/// live); the derive pass then reads only the node column (`&self`), never the
/// state store, preserving the one-way dependency direction (AGENTS section
/// 3.5). Mirrors the `focused` slot precedent — live state reaches semantics
/// through a node column, not a cross-layer read.
///
/// `Copy` and heap-free: every field is a small scalar, so the side column
/// reuses its slots with no per-frame allocation. Fields are `Option` so a node
/// carries only the facets its role defines (a checkbox sets `checked`; a slider
/// sets `value` + `range`; a plain container sets none and stores `None` in the
/// column).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct SemanticState {
    /// Two-state checked value: a [`CheckBox`](Role::CheckBox) / toggle, or the
    /// selected state of a [`Radio`](Role::Radio) option.
    pub checked: Option<bool>,
    /// A [`Slider`](Role::Slider)'s current value, resolved to the real range
    /// (not the relative 0..1 the control stores internally).
    pub value: Option<f32>,
    /// A [`Slider`](Role::Slider)'s inclusive `(min, max)` bounds.
    pub range: Option<(f32, f32)>,
    /// Whether a disclosure/expandable node is expanded. Reserved for later
    /// wiring (Navigation/Dialog disclosure); carried in the model now so the
    /// column and snapshot shape are stable.
    pub expanded: Option<bool>,
}

impl SemanticState {
    /// A checked/unchecked state (checkbox, toggle, or radio-option selection).
    pub fn checked(checked: bool) -> Self {
        Self {
            checked: Some(checked),
            ..Self::default()
        }
    }

    /// A slider state: a current `value` within inclusive `(min, max)` bounds.
    pub fn slider(value: f32, min: f32, max: f32) -> Self {
        Self {
            value: Some(value),
            range: Some((min, max)),
            ..Self::default()
        }
    }

    /// This with an `expanded` disclosure flag.
    pub fn with_expanded(mut self, expanded: bool) -> Self {
        self.expanded = Some(expanded);
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
    }

    #[test]
    fn semantic_state_checked_sets_only_checked() {
        let s = SemanticState::checked(true);
        assert_eq!(s.checked, Some(true));
        assert_eq!(s.value, None);
        assert_eq!(s.range, None);
        assert_eq!(s.expanded, None);
        assert_eq!(SemanticState::checked(false).checked, Some(false));
    }

    #[test]
    fn semantic_state_slider_sets_value_and_range() {
        let s = SemanticState::slider(42.0, 0.0, 100.0);
        assert_eq!(s.value, Some(42.0));
        assert_eq!(s.range, Some((0.0, 100.0)));
        assert_eq!(s.checked, None);
        assert_eq!(s.expanded, None);
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
                    state: None,
                    bounds: ZERO_RECT,
                    children: vec![1],
                },
                SemanticsNode {
                    id: child_id,
                    role: Role::Button,
                    label: Some("Add".to_string()),
                    focused: true,
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
