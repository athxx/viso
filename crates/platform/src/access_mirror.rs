//! The backend's copy of a published accessibility tree (ADR 0030), for the
//! bridges Viso builds itself out of native objects (iOS, Web).

use std::collections::{HashMap, HashSet};

use accesskit::{Node, TreeUpdate};

/// The tree as of the last update: applies each incremental update and
/// reports which nodes the native side has to patch.
pub(crate) struct TreeMirror {
    nodes: HashMap<u64, Node>,
    root: Option<u64>,
    focus: u64,
}

/// What one update changed in a [`TreeMirror`].
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct MirrorChanges {
    /// Nodes the update carried that are still in the tree.
    pub(crate) changed: Vec<u64>,
    /// Nodes no longer reachable from the root, now dropped.
    pub(crate) removed: Vec<u64>,
    /// The focus moved.
    pub(crate) focus_moved: bool,
}

impl TreeMirror {
    pub(crate) fn new() -> Self {
        Self {
            nodes: HashMap::new(),
            root: None,
            focus: 0,
        }
    }

    /// Fold `update` in, dropping every node it left unreachable.
    pub(crate) fn apply(&mut self, update: TreeUpdate) -> MirrorChanges {
        if let Some(tree) = &update.tree {
            self.root = Some(tree.root.0);
        }
        let focus_moved = self.focus != update.focus.0;
        self.focus = update.focus.0;
        let mut changed = Vec::with_capacity(update.nodes.len());
        for (id, node) in update.nodes {
            changed.push(id.0);
            self.nodes.insert(id.0, node);
        }
        let mut reachable = HashSet::with_capacity(self.nodes.len());
        self.walk(|id, _, _| {
            reachable.insert(id);
        });
        let mut removed: Vec<u64> = self
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

    pub(crate) fn get(&self, id: u64) -> Option<&Node> {
        self.nodes.get(&id)
    }

    #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
    pub(crate) fn root(&self) -> Option<u64> {
        self.root
    }

    #[cfg(any(target_os = "ios", test))]
    pub(crate) fn focus(&self) -> u64 {
        self.focus
    }

    /// Visit every node in document order with its parent's id.
    pub(crate) fn walk(&self, mut visit: impl FnMut(u64, &Node, Option<u64>)) {
        let Some(root) = self.root else { return };
        let mut stack = vec![(root, None)];
        while let Some((id, parent)) = stack.pop() {
            let Some(node) = self.nodes.get(&id) else {
                continue;
            };
            visit(id, node, parent);
            stack.extend(
                node.children()
                    .iter()
                    .rev()
                    .map(|child| (child.0, Some(id))),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(role: accesskit::Role, children: &[u64]) -> accesskit::Node {
        let mut node = accesskit::Node::new(role);
        node.set_children(
            children
                .iter()
                .map(|&id| accesskit::NodeId(id))
                .collect::<Vec<_>>(),
        );
        node
    }

    fn update(
        nodes: Vec<(u64, accesskit::Node)>,
        root: Option<u64>,
        focus: u64,
    ) -> accesskit::TreeUpdate {
        accesskit::TreeUpdate {
            nodes: nodes
                .into_iter()
                .map(|(id, n)| (accesskit::NodeId(id), n))
                .collect(),
            tree: root.map(|id| accesskit::TreeInfo::new(accesskit::NodeId(id))),
            tree_id: accesskit::TreeId::ROOT,
            focus: accesskit::NodeId(focus),
        }
    }

    #[test]
    fn the_mirror_folds_diffs_and_drops_what_they_orphan() {
        use accesskit::Role;
        let mut mirror = TreeMirror::new();
        let first = mirror.apply(update(
            vec![
                (1, node(Role::Window, &[2, 3])),
                (2, node(Role::Button, &[])),
                (3, node(Role::Group, &[4])),
                (4, node(Role::Label, &[])),
            ],
            Some(1),
            1,
        ));
        assert_eq!(first.changed, [1, 2, 3, 4]);
        assert!(first.removed.is_empty());

        let mut order = Vec::new();
        mirror.walk(|id, _, parent| order.push((id, parent)));
        assert_eq!(order, [(1, None), (2, Some(1)), (3, Some(1)), (4, Some(3))]);

        let second = mirror.apply(update(vec![(1, node(Role::Window, &[2]))], None, 2));
        assert_eq!(second.changed, [1]);
        assert_eq!(second.removed, [3, 4], "the group and its label left");
        assert!(second.focus_moved);
        assert_eq!(mirror.focus(), 2);
        assert!(mirror.get(4).is_none());
        assert!(mirror.get(2).is_some(), "an unchanged node stays");
    }
}
