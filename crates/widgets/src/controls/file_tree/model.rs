//! The file-tree data model — a pure, owned tree of nodes plus the pure
//! transforms and the flatten that turn it into the visible-row list the build
//! walk and the reconcile step consume.
//!
//! This module holds **no** `cx`/store types: it is plain data ([`TreeNode`],
//! [`NodeKey`], [`VisibleRow`]) and total functions over it — expand/collapse,
//! selection, the focus-cursor arithmetic, and the prefix flatten — so every
//! structural edit is unit-testable without building a single node (AGENTS
//! section 35). The build walk in [`super::build`] reads the flattened rows to
//! author nodes; the reconcile step in [`super::reconcile`] edits the open and
//! selection sets, reflattens, and drives the virtual list.
//!
//! The model is an **owned tree of `Vec`-linked children**, not a flat
//! `HashMap<Id, Node>`: Viso already owns a generational node arena and forbids a
//! second synthetic-id map on any traversed path (AGENTS sections 8.2, 29, 45). A
//! file tree changes only on discrete expand/collapse/select actions and is
//! walked whole only at flatten time (a cold, discrete-action path), so an owned
//! tree is cache-friendly and hashes nothing on the traversed path.
//!
//! Expansion and selection live in [`NodeKey`]-keyed sets, not in the tree nodes:
//! a key is a stable path identity, so collapsing a folder and reopening it — or
//! reordering the underlying data — keeps a descendant's expanded and selected
//! state (the spec's "stable key preserves expanded state"). The flatten reads
//! the open set to decide which folders to descend into; it never mutates the
//! tree.

use std::collections::HashSet;

/// A stable, `Copy` identity for a tree node — a hash of the node's path. The
/// tree, the open set, and the selection set all reference a node **only** by
/// key, never by position, so a node keeps its expanded and selected state as the
/// tree collapses, reopens, or reorders (the spec's "stable key preserves
/// expanded state"). A small integer, not a string: identity on a traversed path
/// is an ID, never a hashed name re-hashed each frame (AGENTS section 29).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeKey(pub u64);

/// One node of the file tree: a leaf file or a directory with owned children.
///
/// The children are an owned `Vec`, walked in order at flatten time. `label` is
/// cold — the display string, read only when a row is authored, kept off the hot
/// [`VisibleRow`] and looked up by key when a row mounts. `is_dir` decides whether
/// a node can expand: only a directory carries a disclosure arrow and descends on
/// open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeNode {
    /// The node's stable identity (a path hash).
    pub key: NodeKey,
    /// The node's display name (cold — read only when a row is authored).
    pub label: String,
    /// Whether this node is a directory (expandable) rather than a file leaf.
    pub is_dir: bool,
    /// The node's children, in display order. Empty for a file leaf.
    pub children: Vec<TreeNode>,
}

/// One flattened visible row: a node the flatten decided is currently visible,
/// carrying just the hot facts a row build and the reconcile need. `Copy` and
/// heap-free — the label stays in the [`TreeNode`], fetched by `key` only when a
/// row mounts (AGENTS section 8.4). `depth` is the node's nesting level (the root
/// forest is depth 0), used to indent the row; `expanded` is meaningful only for a
/// directory (a file row's disclosure state is `false`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VisibleRow {
    /// The row's node identity.
    pub key: NodeKey,
    /// The node's nesting depth (root forest = 0), driving the indent.
    pub depth: u16,
    /// Whether the node is a directory (carries a disclosure arrow).
    pub is_dir: bool,
    /// Whether the directory is expanded (always `false` for a file).
    pub expanded: bool,
}

impl TreeNode {
    /// A file leaf with the given key and label.
    pub fn file(key: NodeKey, label: impl Into<String>) -> TreeNode {
        TreeNode {
            key,
            label: label.into(),
            is_dir: false,
            children: Vec::new(),
        }
    }

    /// A directory with the given key, label, and children (in display order).
    pub fn dir(key: NodeKey, label: impl Into<String>, children: Vec<TreeNode>) -> TreeNode {
        TreeNode {
            key,
            label: label.into(),
            is_dir: true,
            children,
        }
    }

    /// Find the node with `key` anywhere in this subtree, if present. Walks the
    /// subtree once; cold (a discrete-action lookup, never a per-frame path).
    pub fn find(&self, key: NodeKey) -> Option<&TreeNode> {
        if self.key == key {
            return Some(self);
        }
        self.children.iter().find_map(|c| c.find(key))
    }
}

/// Flatten `roots` into the visible rows, in display (pre-order) order: each root
/// forest node, and — for a directory whose key is in `open` — its children,
/// recursively. A file, or a directory not in `open`, contributes just its own
/// row. `depth` grows by one per level so the build walk can indent each row.
///
/// This is the model's core read: the tree plus the open set define exactly which
/// rows are visible, so expanding or collapsing a folder is "edit `open`, then
/// reflatten". Pure — it reads the tree and the set and allocates one output
/// `Vec`, mutating neither (AGENTS section 10.5).
pub fn flatten(roots: &[TreeNode], open: &HashSet<NodeKey>) -> Vec<VisibleRow> {
    let mut out = Vec::new();
    for root in roots {
        flatten_into(root, 0, open, &mut out);
    }
    out
}

/// Append `node`'s row at `depth`, then — if it is an open directory — its
/// children at `depth + 1`, recursively. The pre-order recursion behind
/// [`flatten`].
fn flatten_into(node: &TreeNode, depth: u16, open: &HashSet<NodeKey>, out: &mut Vec<VisibleRow>) {
    let expanded = node.is_dir && open.contains(&node.key);
    out.push(VisibleRow {
        key: node.key,
        depth,
        is_dir: node.is_dir,
        expanded,
    });
    if expanded {
        for child in &node.children {
            flatten_into(child, depth + 1, open, out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROOT: NodeKey = NodeKey(0);
    const A: NodeKey = NodeKey(1);
    const B: NodeKey = NodeKey(2);
    const A1: NodeKey = NodeKey(11);
    const A2: NodeKey = NodeKey(12);

    /// A two-level fixture: a `root` dir over a `A` dir (children `A1`, `A2`) and
    /// a `B` file.
    fn fixture() -> Vec<TreeNode> {
        vec![TreeNode::dir(
            ROOT,
            "root",
            vec![
                TreeNode::dir(
                    A,
                    "a",
                    vec![TreeNode::file(A1, "a1"), TreeNode::file(A2, "a2")],
                ),
                TreeNode::file(B, "b"),
            ],
        )]
    }

    fn open_of(keys: &[NodeKey]) -> HashSet<NodeKey> {
        keys.iter().copied().collect()
    }

    #[test]
    fn flatten_collapsed_root_yields_only_root() {
        let roots = fixture();
        let rows = flatten(&roots, &open_of(&[]));
        assert_eq!(
            rows.len(),
            1,
            "a collapsed root contributes only its own row"
        );
        assert_eq!(rows[0].key, ROOT);
        assert_eq!(rows[0].depth, 0);
        assert!(rows[0].is_dir);
        assert!(!rows[0].expanded, "root is not in the open set");
    }

    #[test]
    fn flatten_descends_only_open_dirs_in_preorder_with_depth() {
        let roots = fixture();
        // Open root but not A: root, then A (dir, collapsed) and B (file) at depth 1.
        let rows = flatten(&roots, &open_of(&[ROOT]));
        let keys: Vec<_> = rows.iter().map(|r| (r.key, r.depth)).collect();
        assert_eq!(keys, vec![(ROOT, 0), (A, 1), (B, 1)]);
        assert!(!rows[1].expanded, "A is a dir but not open");

        // Open root and A: root, A (expanded), A1, A2 at depth 2, then B.
        let rows = flatten(&roots, &open_of(&[ROOT, A]));
        let keys: Vec<_> = rows.iter().map(|r| (r.key, r.depth)).collect();
        assert_eq!(
            keys,
            vec![(ROOT, 0), (A, 1), (A1, 2), (A2, 2), (B, 1)],
            "pre-order, descending only open dirs, depth per level"
        );
        assert!(rows[1].expanded, "A is open");
    }

    #[test]
    fn toggling_open_set_adds_and_removes_child_rows() {
        let roots = fixture();
        let mut open = open_of(&[ROOT]);
        assert_eq!(flatten(&roots, &open).len(), 3);
        // Expand A -> its two children appear.
        open.insert(A);
        assert_eq!(flatten(&roots, &open).len(), 5);
        // Collapse A -> its children disappear; A itself stays.
        open.remove(&A);
        assert_eq!(flatten(&roots, &open).len(), 3);
    }

    #[test]
    fn open_state_of_descendant_survives_a_parent_collapse() {
        // Stable-key contract: A's membership in `open` is unaffected by whether
        // its ancestor ROOT is open, so reopening ROOT restores A expanded.
        let roots = fixture();
        let open = open_of(&[A]); // A open, ROOT closed
        assert_eq!(
            flatten(&roots, &open).len(),
            1,
            "ROOT closed hides everything below"
        );
        let open = open_of(&[ROOT, A]); // reopen ROOT
        assert_eq!(
            flatten(&roots, &open).len(),
            5,
            "A was still in the open set, so it comes back expanded"
        );
    }

    #[test]
    fn find_locates_nodes_by_key() {
        let roots = fixture();
        assert_eq!(roots[0].find(A2).map(|n| n.label.as_str()), Some("a2"));
        assert_eq!(roots[0].find(NodeKey(999)), None);
    }
}
