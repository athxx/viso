//! The interaction states of a node that a style's `when` reads: hovered (the
//! pointer is over it or a descendant), pressed (the primary button went down
//! on it or a descendant and is not up yet), focused (it holds focus) and
//! focus-visible (it holds focus that the keyboard or an assistive technology
//! moved to it).
//!
//! The states derive from what the router records — the hovered node, the
//! pressed one, the focus and how it got there — so a node keeps none of its
//! own. One revision cell, allocated when first asked for, stands for all of
//! them: the frame's settle raises it once after any of that moved, and a
//! reader woken by it asks [`NodeStore::in_state`] for the nodes it styles. A
//! pointer move that changes no hover target raises nothing, and a store
//! nobody asked for the cell raises nothing at all.

use crate::component::NodeStore;
use crate::node::NodeId;
use crate::state::{StateId, StateStore, StateValue};

/// One interaction state of a node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Interaction {
    /// The pointer is over the node or one of its descendants.
    Hovered = 0,
    /// The primary button went down on the node or a descendant and has not
    /// come up.
    Pressed = 1,
    /// The node holds focus.
    Focused = 2,
    /// The node holds focus the keyboard or an assistive technology moved to
    /// it.
    FocusVisible = 3,
}

impl Interaction {
    /// Every state, by tag.
    pub const ALL: [Interaction; 4] = [
        Interaction::Hovered,
        Interaction::Pressed,
        Interaction::Focused,
        Interaction::FocusVisible,
    ];

    /// The state with tag `tag`.
    pub fn from_tag(tag: u8) -> Option<Interaction> {
        Interaction::ALL.get(usize::from(tag)).copied()
    }

    /// Its tag.
    pub fn tag(self) -> u8 {
        self as u8
    }
}

/// What the interaction states derive from besides the hovered and focused
/// nodes the store keeps, and the revision cell standing for them.
#[derive(Debug, Default)]
pub(crate) struct InteractionStates {
    /// The node the primary button went down on, until it comes up.
    pressed: Option<NodeId>,
    /// Whether the focus came from the keyboard or an assistive technology.
    focus_visible: bool,
    /// Whether something the states derive from moved since the last settle.
    moved: bool,
    cell: Option<StateId>,
}

impl InteractionStates {
    pub(crate) fn clear(&mut self) {
        *self = InteractionStates::default();
    }

    /// Records that something the states derive from moved.
    pub(crate) fn moved(&mut self) {
        self.moved = true;
    }

    pub(crate) fn set_pressed(&mut self, node: Option<NodeId>) {
        if self.pressed != node {
            self.pressed = node;
            self.moved = true;
        }
    }

    pub(crate) fn set_focus_visible(&mut self, visible: bool) {
        if self.focus_visible != visible {
            self.focus_visible = visible;
            self.moved = true;
        }
    }
}

impl NodeStore {
    /// The revision cell raised whenever an interaction state of any node may
    /// have changed, allocated in `states` on first ask.
    pub fn interaction_cell(&mut self, states: &mut StateStore) -> StateId {
        if let Some(cell) = self.interaction_states.cell {
            return cell;
        }
        let cell = states.alloc(StateValue::Int(0));
        self.interaction_states.cell = Some(cell);
        cell
    }

    /// Whether `node` is in interaction state `state` now.
    pub fn in_state(&self, node: NodeId, state: Interaction) -> bool {
        let focused = self.focused() == Some(node);
        match state {
            Interaction::Hovered => self.hovered().is_some_and(|h| self.within(h, node)),
            Interaction::Pressed => self
                .interaction_states
                .pressed
                .is_some_and(|p| self.within(p, node)),
            Interaction::Focused => focused,
            Interaction::FocusVisible => focused && self.interaction_states.focus_visible,
        }
    }

    /// Records the node the primary button went down on, `None` once it came
    /// up.
    pub fn set_pressed(&mut self, node: Option<NodeId>) {
        self.interaction_states.set_pressed(node);
    }

    /// Records whether the focus came from the keyboard or an assistive
    /// technology rather than a pointer.
    pub fn set_focus_visible(&mut self, visible: bool) {
        self.interaction_states.set_focus_visible(visible);
    }

    /// Raises the revision cell once something the states derive from moved
    /// since the last call.
    pub fn sync_interactions(&mut self, states: &mut StateStore) {
        let interaction = &mut self.interaction_states;
        if !std::mem::take(&mut interaction.moved) {
            return;
        }
        if let Some(cell) = interaction.cell {
            let revision = match states.get(cell) {
                Some(StateValue::Int(n)) => n.wrapping_add(1),
                _ => 0,
            };
            states.set(cell, StateValue::Int(revision));
        }
    }

    /// Whether `node` is `ancestor` or lies under it.
    fn within(&self, node: NodeId, ancestor: NodeId) -> bool {
        let mut at = Some(node);
        while let Some(n) = at {
            if n == ancestor {
                return true;
            }
            at = self.parent(n);
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::component::{BuildCx, LeafStyle};

    fn tree() -> (NodeStore, NodeId, NodeId, NodeId) {
        let mut store = NodeStore::new();
        let [root, a, b] = {
            let mut cx = BuildCx::new(&mut store);
            [(); 3].map(|()| cx.leaf(LeafStyle::default()).id())
        };
        store.arena_append_child(root, a);
        store.arena_append_child(root, b);
        (store, root, a, b)
    }

    #[test]
    fn a_node_is_hovered_and_pressed_through_its_descendants() {
        let (mut store, root, a, b) = tree();
        store.set_hovered(Some(a));
        store.set_pressed(Some(a));
        assert!(store.in_state(root, Interaction::Hovered));
        assert!(store.in_state(a, Interaction::Pressed));
        assert!(!store.in_state(b, Interaction::Hovered));
        store.set_hovered(Some(b));
        store.set_pressed(None);
        assert!(
            store.in_state(root, Interaction::Hovered),
            "b is the root's too"
        );
        assert!(!store.in_state(a, Interaction::Hovered));
        assert!(!store.in_state(a, Interaction::Pressed));
    }

    #[test]
    fn focus_is_visible_only_when_the_keyboard_moved_it() {
        let (mut store, _, a, _) = tree();
        store.set_focused(Some(a));
        store.set_focus_visible(false);
        assert!(store.in_state(a, Interaction::Focused));
        assert!(!store.in_state(a, Interaction::FocusVisible));
        store.set_focus_visible(true);
        assert!(store.in_state(a, Interaction::FocusVisible));
    }

    #[test]
    fn the_cell_rises_once_per_settle_after_a_move_and_never_without_one() {
        let (mut store, _, a, b) = tree();
        let mut states = StateStore::new();
        let cell = store.interaction_cell(&mut states);
        store.sync_interactions(&mut states);
        assert!(!states.has_pending(), "nothing moved");
        store.set_hovered(Some(a));
        store.set_hovered(Some(b));
        store.set_pressed(Some(b));
        store.sync_interactions(&mut states);
        assert_eq!(states.get(cell), Some(StateValue::Int(1)));
        store.set_hovered(Some(b));
        store.sync_interactions(&mut states);
        assert_eq!(
            states.get(cell),
            Some(StateValue::Int(1)),
            "the same target"
        );
    }
}
