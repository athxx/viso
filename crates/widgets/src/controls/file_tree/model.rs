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

    /// The key of the direct parent of `key` in this subtree, or `None` if `key`
    /// is this node itself (a forest root has no parent here) or is absent. Walks
    /// the subtree once; cold (a discrete-action lookup, never a per-frame path) —
    /// keyboard `Left` on a collapsed node uses it to step out to the enclosing
    /// directory.
    fn parent_of(&self, key: NodeKey) -> Option<NodeKey> {
        for child in &self.children {
            if child.key == key {
                return Some(self.key);
            }
            if let Some(found) = child.parent_of(key) {
                return Some(found);
            }
        }
        None
    }
}

/// The key of the direct parent of `key` across the whole forest, or `None` if
/// `key` is a forest root or is absent. Cold (a discrete-action lookup) — the
/// keyboard `Left` step-to-parent reads it.
pub fn parent_of(roots: &[TreeNode], key: NodeKey) -> Option<NodeKey> {
    roots.iter().find_map(|r| r.parent_of(key))
}

/// The outcome of one keyboard navigation step over the flattened rows: either move
/// the focus cursor to a row (`Focus`), or open/close a directory under the cursor
/// (`Expand`/`Collapse`) — a `Right` on a collapsed directory discloses it, a `Left`
/// on an open one closes it, and both otherwise move the cursor. `Copy`: a discrete
/// action, never a per-frame allocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Nav {
    /// Move the focus cursor to this row's key.
    Focus(NodeKey),
    /// Disclose this directory (a `Right` on a collapsed directory).
    Expand(NodeKey),
    /// Close this directory (a `Left` on an open directory).
    Collapse(NodeKey),
}

/// The index of `key` in the flattened rows, or `None` if it is not visible. The
/// flattened list is the navigation domain: because a collapsed subtree contributes
/// no rows, index arithmetic over `rows` naturally skips hidden descendants.
fn row_index(rows: &[VisibleRow], key: NodeKey) -> Option<usize> {
    rows.iter().position(|r| r.key == key)
}

/// Move the focus cursor one visible row toward the start (`Up`) — `None` if there
/// is nowhere to go (already the first row, or `focus` is unset/off-list). Pure
/// index arithmetic over the flattened rows, so it steps over exactly the visible
/// rows, skipping any collapsed subtree.
pub fn focus_up(rows: &[VisibleRow], focus: Option<NodeKey>) -> Option<Nav> {
    let here = focus.and_then(|k| row_index(rows, k));
    match here {
        // No cursor yet: land it on the last row (a first `Up` from nowhere).
        None => rows.last().map(|r| Nav::Focus(r.key)),
        Some(0) => None,
        Some(i) => Some(Nav::Focus(rows[i - 1].key)),
    }
}

/// Move the focus cursor one visible row toward the end (`Down`) — `None` if there
/// is nowhere to go. Mirror of [`focus_up`]; from no cursor it lands on the first row.
pub fn focus_down(rows: &[VisibleRow], focus: Option<NodeKey>) -> Option<Nav> {
    let here = focus.and_then(|k| row_index(rows, k));
    match here {
        None => rows.first().map(|r| Nav::Focus(r.key)),
        Some(i) if i + 1 < rows.len() => Some(Nav::Focus(rows[i + 1].key)),
        Some(_) => None,
    }
}

/// The `Right` step: on a collapsed directory, disclose it ([`Nav::Expand`]); on an
/// already-open directory, move to its first child (the next visible row); on a file
/// (or nowhere), do nothing. `None` when there is nothing to do.
pub fn focus_right(rows: &[VisibleRow], focus: Option<NodeKey>) -> Option<Nav> {
    let i = focus.and_then(|k| row_index(rows, k))?;
    let row = rows[i];
    if row.is_dir && !row.expanded {
        Some(Nav::Expand(row.key))
    } else if row.is_dir && row.expanded {
        // Open directory: its first child is the immediately following visible row.
        rows.get(i + 1).map(|c| Nav::Focus(c.key))
    } else {
        None
    }
}

/// The `Left` step: on an open directory, close it ([`Nav::Collapse`]); otherwise
/// (a file, or a collapsed directory) move to the parent directory's row. `None`
/// when there is nothing to do (a collapsed forest root, or nowhere).
pub fn focus_left(rows: &[VisibleRow], roots: &[TreeNode], focus: Option<NodeKey>) -> Option<Nav> {
    let i = focus.and_then(|k| row_index(rows, k))?;
    let row = rows[i];
    if row.is_dir && row.expanded {
        Some(Nav::Collapse(row.key))
    } else {
        parent_of(roots, row.key).map(Nav::Focus)
    }
}

/// The `Home` step: focus the first visible row. `None` on an empty list.
pub fn focus_home(rows: &[VisibleRow]) -> Option<Nav> {
    rows.first().map(|r| Nav::Focus(r.key))
}

/// The `End` step: focus the last visible row. `None` on an empty list.
pub fn focus_end(rows: &[VisibleRow]) -> Option<Nav> {
    rows.last().map(|r| Nav::Focus(r.key))
}

/// The keys of the contiguous run of visible rows between `anchor` and `focus`
/// inclusive, in visible order — the set a Shift range-select covers. Empty if
/// either endpoint is off the visible list. Pure over the flattened rows, so a
/// range never spans a collapsed (hidden) subtree.
pub fn range_between(rows: &[VisibleRow], anchor: NodeKey, focus: NodeKey) -> Vec<NodeKey> {
    let (Some(a), Some(b)) = (row_index(rows, anchor), row_index(rows, focus)) else {
        return Vec::new();
    };
    let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
    rows[lo..=hi].iter().map(|r| r.key).collect()
}

/// How a selection command combines with the current selection set — the three
/// gestures a file tree offers. `Replace` clears the set to just this key (a plain
/// click, or single-select mode); `Toggle` flips this one key, leaving the rest (a
/// `Ctrl`/`Space` multi-select gesture); `Range` selects the contiguous visible run
/// from the anchor to this key (a `Shift` gesture). `Copy`: a discrete action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectOp {
    /// Clear the selection to just this key (a plain click / single-select).
    Replace,
    /// Flip this one key's membership, keeping the rest (`Ctrl`/`Space`).
    Toggle,
    /// Select the contiguous visible run from the anchor to this key (`Shift`).
    Range,
}

/// Apply `op` to the selection set for the row `key`, over the visible `rows` and the
/// current `anchor`, returning the new anchor. Pure over the warm selection state (the
/// reconcile step owns the actual `HashSet`), so the gesture-to-set mapping is
/// unit-testable without a store:
///
/// - `Replace` clears `sel` to just `key` and anchors there;
/// - `Toggle` flips `key` (anchoring there), leaving the rest;
/// - `Range` selects the contiguous visible run from the anchor (or `key` itself if
///   there is none) to `key` without moving the anchor — so a run of `Shift`+arrow
///   grows a single range from one fixed end.
pub fn apply_select(
    sel: &mut HashSet<NodeKey>,
    rows: &[VisibleRow],
    anchor: Option<NodeKey>,
    key: NodeKey,
    op: SelectOp,
) -> Option<NodeKey> {
    match op {
        SelectOp::Replace => {
            sel.clear();
            sel.insert(key);
            Some(key)
        }
        SelectOp::Toggle => {
            if !sel.remove(&key) {
                sel.insert(key);
            }
            Some(key)
        }
        SelectOp::Range => {
            let from = anchor.unwrap_or(key);
            sel.clear();
            for k in range_between(rows, from, key) {
                sel.insert(k);
            }
            // A range never moves the anchor: successive Shift steps grow one range.
            Some(from)
        }
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

    #[test]
    fn parent_of_reads_the_forest() {
        let roots = fixture();
        assert_eq!(parent_of(&roots, A), Some(ROOT), "A's parent is ROOT");
        assert_eq!(parent_of(&roots, A1), Some(A), "a1's parent is A");
        assert_eq!(parent_of(&roots, ROOT), None, "a forest root has no parent");
        assert_eq!(
            parent_of(&roots, NodeKey(999)),
            None,
            "absent key: no parent"
        );
    }

    #[test]
    fn up_down_step_over_visible_rows_skipping_collapsed_subtrees() {
        let roots = fixture();
        // ROOT open, A collapsed: visible = [ROOT, A, B]. A's children are hidden,
        // so Down from A lands on B, never on a1 (index arithmetic skips them).
        let rows = flatten(&roots, &open_of(&[ROOT]));
        assert_eq!(focus_down(&rows, Some(ROOT)), Some(Nav::Focus(A)));
        assert_eq!(
            focus_down(&rows, Some(A)),
            Some(Nav::Focus(B)),
            "A collapsed: Down skips its hidden children"
        );
        assert_eq!(focus_down(&rows, Some(B)), None, "no row past the last");
        assert_eq!(focus_up(&rows, Some(B)), Some(Nav::Focus(A)));
        assert_eq!(focus_up(&rows, Some(ROOT)), None, "no row before the first");

        // From no cursor, Down lands on the first row and Up on the last.
        assert_eq!(focus_down(&rows, None), Some(Nav::Focus(ROOT)));
        assert_eq!(focus_up(&rows, None), Some(Nav::Focus(B)));
    }

    #[test]
    fn right_expands_a_collapsed_dir_then_descends_to_first_child() {
        let roots = fixture();
        // A collapsed: Right discloses it rather than moving.
        let rows = flatten(&roots, &open_of(&[ROOT]));
        assert_eq!(focus_right(&rows, Some(A)), Some(Nav::Expand(A)));
        // A open: Right moves to its first child a1 (the next visible row).
        let rows = flatten(&roots, &open_of(&[ROOT, A]));
        assert_eq!(focus_right(&rows, Some(A)), Some(Nav::Focus(A1)));
        // On a file, Right does nothing.
        assert_eq!(focus_right(&rows, Some(B)), None);
    }

    #[test]
    fn left_collapses_an_open_dir_else_steps_to_parent() {
        let roots = fixture();
        // A open: Left closes it.
        let rows = flatten(&roots, &open_of(&[ROOT, A]));
        assert_eq!(focus_left(&rows, &roots, Some(A)), Some(Nav::Collapse(A)));
        // On a1 (a file under A): Left steps out to the parent A.
        assert_eq!(focus_left(&rows, &roots, Some(A1)), Some(Nav::Focus(A)));
        // A collapsed: Left on A steps to its parent ROOT.
        let rows = flatten(&roots, &open_of(&[ROOT]));
        assert_eq!(focus_left(&rows, &roots, Some(A)), Some(Nav::Focus(ROOT)));
        // ROOT is open here: Left on an open directory closes it.
        assert_eq!(
            focus_left(&rows, &roots, Some(ROOT)),
            Some(Nav::Collapse(ROOT))
        );
        // On a collapsed forest root: nowhere to go (no parent, not open to close).
        let rows = flatten(&roots, &open_of(&[]));
        assert_eq!(focus_left(&rows, &roots, Some(ROOT)), None);
    }

    #[test]
    fn home_end_jump_to_the_visible_extremes() {
        let roots = fixture();
        let rows = flatten(&roots, &open_of(&[ROOT, A]));
        assert_eq!(focus_home(&rows), Some(Nav::Focus(ROOT)));
        assert_eq!(focus_end(&rows), Some(Nav::Focus(B)), "B is the last row");
    }

    #[test]
    fn range_between_covers_the_contiguous_visible_run_either_direction() {
        let roots = fixture();
        // visible = [ROOT, A, a1, a2, B].
        let rows = flatten(&roots, &open_of(&[ROOT, A]));
        assert_eq!(range_between(&rows, A, A2), vec![A, A1, A2]);
        // Order-independent: anchor after focus yields the same run.
        assert_eq!(range_between(&rows, A2, A), vec![A, A1, A2]);
        // A single-row range is just that row.
        assert_eq!(range_between(&rows, B, B), vec![B]);
        // An off-list endpoint yields an empty range.
        assert!(range_between(&rows, A, NodeKey(999)).is_empty());
    }

    #[test]
    fn apply_select_replace_toggle_and_range_map_the_three_gestures() {
        let roots = fixture();
        let rows = flatten(&roots, &open_of(&[ROOT, A])); // [ROOT, A, a1, a2, B]
        let mut sel = HashSet::new();

        // Replace: clears to just the clicked key, anchors there.
        let anchor = apply_select(&mut sel, &rows, None, A1, SelectOp::Replace);
        assert_eq!(anchor, Some(A1));
        assert_eq!(sel, open_of(&[A1]));

        // Toggle: adds a second key, keeps the first, re-anchors on the toggled key.
        let anchor = apply_select(&mut sel, &rows, anchor, B, SelectOp::Toggle);
        assert_eq!(anchor, Some(B));
        assert_eq!(sel, open_of(&[A1, B]));
        // Toggling B again removes it.
        apply_select(&mut sel, &rows, anchor, B, SelectOp::Toggle);
        assert_eq!(sel, open_of(&[A1]));

        // Range: from the anchor A1 to a2 covers the contiguous run, anchor unmoved.
        let anchor = apply_select(&mut sel, &rows, Some(A1), A2, SelectOp::Range);
        assert_eq!(anchor, Some(A1), "a range never moves the anchor");
        assert_eq!(sel, open_of(&[A1, A2]));
        // Growing the range the other way from the same anchor replaces, not unions.
        apply_select(&mut sel, &rows, anchor, ROOT, SelectOp::Range);
        assert_eq!(
            sel,
            open_of(&[ROOT, A, A1]),
            "range from anchor A1 up to ROOT covers ROOT, A, a1"
        );

        // Range with no anchor falls back to a single-key selection.
        let mut sel = HashSet::new();
        apply_select(&mut sel, &rows, None, B, SelectOp::Range);
        assert_eq!(sel, open_of(&[B]));
    }
}
