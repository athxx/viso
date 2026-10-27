//! The backend's copy of a published semantics tree, for the bridges Viso
//! builds itself out of native objects (iOS, Web). They fold each
//! [`SemanticsDelta`] in directly.

use std::collections::{HashMap, HashSet};

#[cfg(any(target_os = "ios", test))]
use crate::accessibility::AccessAction;
use crate::accessibility::{AccessNode, SemanticsDelta, SemanticsId};

/// The tree as of the last delta: applies each one and reports which nodes
/// the native side has to patch.
pub(crate) struct TreeMirror {
    nodes: HashMap<SemanticsId, AccessNode>,
    /// Each node's parent, for walking up from an element.
    #[cfg(any(target_os = "ios", test))]
    parents: HashMap<SemanticsId, SemanticsId>,
    root: Option<SemanticsId>,
    focus: SemanticsId,
}

/// What one delta changed in a [`TreeMirror`].
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct MirrorChanges {
    /// Nodes the delta carried that are still in the tree.
    pub(crate) changed: Vec<SemanticsId>,
    /// Nodes no longer reachable from the root, now dropped.
    pub(crate) removed: Vec<SemanticsId>,
    /// The focus moved.
    pub(crate) focus_moved: bool,
}

impl TreeMirror {
    pub(crate) fn new() -> Self {
        Self {
            nodes: HashMap::new(),
            #[cfg(any(target_os = "ios", test))]
            parents: HashMap::new(),
            root: None,
            focus: SemanticsId::default(),
        }
    }

    /// Fold `delta` in, dropping every node it left unreachable.
    pub(crate) fn apply(&mut self, delta: SemanticsDelta) -> MirrorChanges {
        if let Some(root) = delta.root {
            self.root = Some(root);
        }
        let focus_moved = self.focus != delta.focus;
        self.focus = delta.focus;
        let mut changed = Vec::with_capacity(delta.nodes.len());
        for node in delta.nodes {
            changed.push(node.id);
            self.nodes.insert(node.id, node);
        }
        let mut reachable = HashSet::with_capacity(self.nodes.len());
        #[cfg(any(target_os = "ios", test))]
        let mut parents = HashMap::with_capacity(self.nodes.len());
        self.walk(|node, _parent| {
            reachable.insert(node.id);
            #[cfg(any(target_os = "ios", test))]
            if let Some(parent) = _parent {
                parents.insert(node.id, parent);
            }
        });
        #[cfg(any(target_os = "ios", test))]
        {
            self.parents = parents;
        }
        let mut removed: Vec<SemanticsId> = self
            .nodes
            .keys()
            .copied()
            .filter(|id| !reachable.contains(id))
            .collect();
        removed.sort_unstable();
        for id in &removed {
            self.nodes.remove(id);
        }
        changed.retain(|id| reachable.contains(id));
        MirrorChanges {
            changed,
            removed,
            focus_moved,
        }
    }

    pub(crate) fn get(&self, id: SemanticsId) -> Option<&AccessNode> {
        self.nodes.get(&id)
    }

    #[cfg(any(all(target_arch = "wasm32", target_os = "unknown"), test))]
    pub(crate) fn root(&self) -> Option<SemanticsId> {
        self.root
    }

    #[cfg(any(target_os = "ios", test))]
    pub(crate) fn focus(&self) -> SemanticsId {
        self.focus
    }

    /// The nearest of `from` and the nodes about it that offers `action`.
    #[cfg(any(target_os = "ios", test))]
    pub(crate) fn offering(&self, from: SemanticsId, action: AccessAction) -> Option<SemanticsId> {
        let mut at = Some(from);
        while let Some(id) = at {
            if self.nodes.get(&id)?.actions.contains(action) {
                return Some(id);
            }
            at = self.parents.get(&id).copied();
        }
        None
    }

    /// Visit every node in document order with its parent's id.
    pub(crate) fn walk(&self, mut visit: impl FnMut(&AccessNode, Option<SemanticsId>)) {
        let Some(root) = self.root else { return };
        let mut stack = vec![(root, None)];
        while let Some((id, parent)) = stack.pop() {
            let Some(node) = self.nodes.get(&id) else {
                continue;
            };
            visit(node, parent);
            stack.extend(node.children.iter().rev().map(|&child| (child, Some(id))));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accessibility::SemanticsRole;

    fn node(id: u64, role: SemanticsRole, children: &[u64]) -> AccessNode {
        AccessNode {
            id: SemanticsId(id),
            role,
            children: children.iter().map(|&c| SemanticsId(c)).collect(),
            ..AccessNode::default()
        }
    }

    fn delta(nodes: Vec<AccessNode>, root: Option<u64>, focus: u64) -> SemanticsDelta {
        SemanticsDelta {
            root: root.map(SemanticsId),
            nodes,
            focus: SemanticsId(focus),
        }
    }

    fn ids(ids: &[u64]) -> Vec<SemanticsId> {
        ids.iter().map(|&id| SemanticsId(id)).collect()
    }

    #[test]
    fn the_mirror_folds_deltas_and_drops_what_they_orphan() {
        use SemanticsRole as R;
        let mut mirror = TreeMirror::new();
        let first = mirror.apply(delta(
            vec![
                node(1, R::Window, &[2, 3]),
                node(2, R::Button, &[]),
                node(3, R::Group, &[4]),
                node(4, R::Label, &[]),
            ],
            Some(1),
            1,
        ));
        assert_eq!(first.changed, ids(&[1, 2, 3, 4]));
        assert!(first.removed.is_empty());
        assert_eq!(mirror.root(), Some(SemanticsId(1)));

        let mut order = Vec::new();
        mirror.walk(|node, parent| order.push((node.id.0, parent.map(|p| p.0))));
        assert_eq!(order, [(1, None), (2, Some(1)), (3, Some(1)), (4, Some(3))]);

        let second = mirror.apply(delta(vec![node(1, R::Window, &[2])], None, 2));
        assert_eq!(second.changed, ids(&[1]));
        assert_eq!(second.removed, ids(&[3, 4]), "the group and its label left");
        assert!(second.focus_moved);
        assert_eq!(mirror.focus(), SemanticsId(2));
        assert!(mirror.get(SemanticsId(4)).is_none());
        assert!(
            mirror.get(SemanticsId(2)).is_some(),
            "an unchanged node stays"
        );
        assert_eq!(
            mirror.root(),
            Some(SemanticsId(1)),
            "an incremental delta keeps the root"
        );
    }

    #[test]
    fn a_changed_node_replaces_its_facts_and_a_moved_child_keeps_its_node() {
        use SemanticsRole as R;
        let mut mirror = TreeMirror::new();
        mirror.apply(delta(
            vec![
                node(1, R::Window, &[2, 3]),
                node(2, R::Group, &[]),
                node(3, R::Slider, &[]),
            ],
            Some(1),
            1,
        ));
        let mut slider = node(3, R::Slider, &[]);
        slider.label = Some("Volume".into());
        let changes = mirror.apply(delta(
            vec![node(1, R::Window, &[2]), node(2, R::Group, &[3]), slider],
            None,
            1,
        ));
        assert!(changes.removed.is_empty(), "the slider only moved");
        assert!(!changes.focus_moved);
        assert_eq!(changes.changed, ids(&[1, 2, 3]));
        assert_eq!(
            mirror.get(SemanticsId(3)).and_then(|n| n.label.as_deref()),
            Some("Volume")
        );
    }

    #[test]
    fn a_walk_up_finds_the_nearest_node_offering_an_action() {
        use crate::accessibility::AccessActions;
        use SemanticsRole as R;
        let mut list = node(2, R::List, &[3]);
        list.actions = AccessActions::NONE.with(AccessAction::ScrollDown);
        let mut mirror = TreeMirror::new();
        mirror.apply(delta(
            vec![
                node(1, R::Window, &[2]),
                list,
                node(3, R::ListItem, &[4]),
                node(4, R::Label, &[]),
            ],
            Some(1),
            1,
        ));
        let down = AccessAction::ScrollDown;
        assert_eq!(mirror.offering(SemanticsId(4), down), Some(SemanticsId(2)));
        assert_eq!(mirror.offering(SemanticsId(2), down), Some(SemanticsId(2)));
        assert_eq!(
            mirror.offering(SemanticsId(4), AccessAction::ScrollUp),
            None
        );
    }
}
