//! Roving groups: a set of focusable nodes that Tab treats as one stop — a
//! radio group's options, a tab strip's tabs, a toolbar's buttons — the
//! arrows moving the focus among them.
//!
//! Tab lands on the group's current member: the one checked or selected,
//! else the one that last held the focus there, else the first; Tab from a
//! member leaves the group. The group's own arrow handling moves the focus
//! within it.

use crate::component::NodeStore;
use crate::node::NodeId;

/// The roving groups, by slot, and the member that last held the focus in
/// each.
#[derive(Debug, Default)]
pub(crate) struct Roving {
    /// `(group, last focused member)`, ascending by the group's slot.
    groups: Vec<(u32, Option<NodeId>)>,
}

impl Roving {
    pub(crate) fn clear(&mut self) {
        self.groups.clear();
    }

    /// Forgets the slot `index`, freed or reused.
    pub(crate) fn forget(&mut self, index: u32) {
        if let Ok(at) = self.groups.binary_search_by_key(&index, |&(g, _)| g) {
            self.groups.remove(at);
        }
    }

    fn find(&self, group: NodeId) -> Option<usize> {
        self.groups
            .binary_search_by_key(&group.index(), |&(g, _)| g)
            .ok()
    }
}

impl NodeStore {
    /// Make node `id` a roving group, Tab stopping once among the focusable
    /// nodes under it, or an ordinary node. A live-guarded write.
    pub fn set_roving(&mut self, id: NodeId, roving: bool) {
        if !self.arena().is_live(id) {
            return;
        }
        let groups = &mut self.roving.groups;
        match (
            groups.binary_search_by_key(&id.index(), |&(g, _)| g),
            roving,
        ) {
            (Err(at), true) => groups.insert(at, (id.index(), None)),
            (Ok(at), false) => {
                groups.remove(at);
            }
            _ => {}
        }
    }

    /// Whether node `id` is a roving group.
    pub fn is_roving(&self, id: NodeId) -> bool {
        self.roving.find(id).is_some()
    }

    /// Notes that `node` holds the focus, in the roving group nearest
    /// above it, if any.
    pub(crate) fn roved(&mut self, node: NodeId) {
        if self.roving.groups.is_empty() {
            return;
        }
        let mut at = self.arena().parent(node);
        while let Some(group) = at {
            if let Some(slot) = self.roving.find(group) {
                self.roving.groups[slot].1 = Some(node);
                return;
            }
            at = self.arena().parent(group);
        }
    }

    /// The member of roving group `group` Tab lands on, of its focusable
    /// `members` in tree order: the checked or selected one, else the one
    /// last focused, else the first.
    pub(crate) fn rover(&self, group: NodeId, members: &[NodeId]) -> Option<NodeId> {
        let chosen = members.iter().copied().find(|&m| {
            self.semantic_state(m)
                .is_some_and(|s| s.checked == Some(true) || s.selected == Some(true))
        });
        let last = self
            .roving
            .find(group)
            .and_then(|at| self.roving.groups[at].1)
            .filter(|m| members.contains(m));
        chosen.or(last).or(members.first().copied())
    }
}

#[cfg(test)]
mod tests {
    use crate::component::{BuildCx, LeafStyle, NodeStore};
    use crate::input::focus_next;
    use crate::layout::Sizing;
    use crate::node::NodeId;
    use crate::semantics::SemanticState;
    use crate::style::BoxStyle;

    fn leaf(cx: &mut BuildCx<'_>) -> NodeId {
        cx.leaf(LeafStyle {
            size: Sizing::fixed(10.0, 10.0),
            style: BoxStyle::NONE,
        })
        .id()
    }

    #[test]
    fn tab_stops_once_in_a_roving_group_on_its_checked_member() {
        let mut store = NodeStore::new();
        let (root, before, group, members, after) = {
            let mut cx = BuildCx::new(&mut store);
            let (mut before, mut group, mut after) = (None, None, None);
            let mut members = Vec::new();
            let root = cx.flex(Default::default(), |cx| {
                before = Some(leaf(cx));
                group = Some(
                    cx.flex(Default::default(), |cx| {
                        members = (0..3).map(|_| leaf(cx)).collect();
                    })
                    .id(),
                );
                after = Some(leaf(cx));
            });
            (
                root.id(),
                before.unwrap(),
                group.unwrap(),
                members,
                after.unwrap(),
            )
        };
        for &n in [before, after].iter().chain(&members) {
            store.set_focusable(n, true);
        }
        store.set_roving(group, true);
        assert!(store.is_roving(group));

        let tab = |store: &mut NodeStore| focus_next(store, root, true);
        assert_eq!(tab(&mut store), Some(before));
        assert_eq!(tab(&mut store), Some(members[0]), "the first, none chosen");
        assert_eq!(tab(&mut store), Some(after), "Tab leaves the group");

        store.set_semantic_state(members[2], SemanticState::checked(true));
        assert_eq!(focus_next(&mut store, root, false), Some(members[2]));
        store.set_semantic_state(members[2], SemanticState::checked(false));
        store.set_focused(Some(members[1]));
        store.set_focused(Some(before));
        assert_eq!(tab(&mut store), Some(members[1]), "the one last focused");

        store.set_roving(group, false);
        assert_eq!(tab(&mut store), Some(members[2]), "an ordinary group again");
    }
}
