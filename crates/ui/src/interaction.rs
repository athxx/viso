//! The interaction states of a node that a style's `when` reads: hovered (the
//! pointer is over it or a descendant), pressed (the primary button went down
//! on it or a descendant, is not up yet, the pointer is still inside its box
//! and no pan took the contact), focused (it holds focus), focus-visible (it
//! holds focus that the keyboard or an assistive technology moved to it),
//! selected (its tab strip or radio group selects it) and dragging (it won a
//! drag that runs).
//!
//! The states derive from what the router records — the hovered node, the
//! pressed one and which of its ancestors the pointer left, the focus and how
//! it got there, the drags under way — and from the node's semantic state, so
//! a node keeps none of its own. One revision cell, allocated when first
//! asked for, stands for all of them: the frame's settle raises it once after
//! any of that moved, and a reader woken by it asks [`NodeStore::in_state`]
//! for the nodes it styles. A pointer move that changes no hover target
//! raises nothing, and a store nobody asked for the cell raises nothing at
//! all.

use crate::component::NodeStore;
use crate::input::PointerId;
use crate::node::NodeId;
use crate::state::{StateId, StateStore, StateValue};

pub use viso_schema::interaction::Interaction;

/// What the interaction states derive from besides the hovered and focused
/// nodes the store keeps, and the revision cell standing for them.
#[derive(Debug, Default)]
pub(crate) struct InteractionStates {
    /// The node the primary button went down on, until it comes up.
    pressed: Option<NodeId>,
    /// The pointer that pressed it.
    presser: PointerId,
    /// The nodes from the pressed one up whose box the pointer is outside of,
    /// innermost first.
    left: Vec<NodeId>,
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
            self.left.clear();
            self.moved = true;
        }
    }

    pub(crate) fn pressed(&self) -> Option<NodeId> {
        self.pressed
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

    /// Whether `node` is in interaction state `state` now. An inert node is
    /// neither hovered nor pressed.
    pub fn in_state(&self, node: NodeId, state: Interaction) -> bool {
        let focused = self.focused() == Some(node);
        match state {
            Interaction::Hovered => {
                self.hovered().is_some_and(|h| self.within(h, node)) && !self.inert(node)
            }
            Interaction::Pressed => {
                let press = &self.interaction_states;
                press.pressed.is_some_and(|p| self.within(p, node))
                    && !press.left.contains(&node)
                    && !self.inert(node)
            }
            Interaction::Focused => focused,
            Interaction::FocusVisible => focused && self.interaction_states.focus_visible,
            Interaction::Selected => {
                self.semantic_state(node).and_then(|s| s.selected) == Some(true)
            }
            Interaction::Dragging => self.dragging(node),
        }
    }

    /// Records the node the primary button went down on, `None` once it came
    /// up or the press was taken from it.
    pub fn set_pressed(&mut self, node: Option<NodeId>) {
        self.interaction_states.set_pressed(node);
    }

    /// Records that pointer `pointer` pressed `node`.
    pub(crate) fn press(&mut self, node: NodeId, pointer: PointerId) {
        self.set_pressed(Some(node));
        self.interaction_states.presser = pointer;
    }

    /// Ends the press pointer `pointer` made, if it made the one standing.
    pub(crate) fn release_press(&mut self, pointer: PointerId) {
        if self.interaction_states.presser == pointer {
            self.set_pressed(None);
        }
    }

    /// Follows the pointer that pressed to `(x, y)`: the pressed node and its
    /// ancestors whose box it is outside of are not pressed until it is back.
    pub(crate) fn follow_press(&mut self, pointer: PointerId, x: f32, y: f32) {
        let press = &self.interaction_states;
        let Some(pressed) = press.pressed else {
            return;
        };
        if press.presser != pointer {
            return;
        }
        let mut seen = 0;
        let mut same = true;
        let mut at = Some(pressed);
        while let Some(n) = at {
            if self.outside(n, x, y) {
                if press.left.get(seen) != Some(&n) {
                    same = false;
                    break;
                }
                seen += 1;
            }
            at = self.parent(n);
        }
        if same && seen == press.left.len() {
            return;
        }
        let mut left = std::mem::take(&mut self.interaction_states.left);
        left.clear();
        let mut at = Some(pressed);
        while let Some(n) = at {
            if self.outside(n, x, y) {
                left.push(n);
            }
            at = self.parent(n);
        }
        self.interaction_states.left = left;
        self.interaction_states.moved();
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

    /// Whether the screen point `(x, y)` is outside `node`'s box.
    fn outside(&self, node: NodeId, x: f32, y: f32) -> bool {
        let at = self.to_local(node, x, y);
        let b = self.bounds(node);
        !(0.0..=b.w).contains(&at.x) || !(0.0..=b.h).contains(&at.y)
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
