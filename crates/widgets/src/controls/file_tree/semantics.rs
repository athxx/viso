//! The file tree's accessibility contract — the [`Role::Tree`] container and the
//! per-row [`Role::TreeItem`] semantics an assistive technology reads.
//!
//! Accessibility is mandatory architecture (AGENTS section 15), so the a11y mapping
//! lives in its own file rather than scattered through the build and reconcile
//! steps. Two concerns:
//!
//! - The **container** is a named [`Role::Tree`] (WAI-ARIA `role=tree`), authored
//!   once on the virtual-list viewport so a screen reader presents the rows as one
//!   navigable hierarchy. Structural — it carries only its role and label.
//! - Each **visible row** is a [`Role::TreeItem`] (`role=treeitem`). Its authored
//!   role is set when the row mounts; its live [`SemanticState`] — `expanded` for a
//!   directory (`aria-expanded`) and `selected` for the current selection
//!   (`aria-selected`) — is written by the reconcile step from the warm
//!   flattened-row model. A file row carries no `expanded` (a file never
//!   discloses); every row carries `selected`.
//!
//! The row state is a field of the warm flattened-row model, not a reactive scalar
//! cell, so the reconcile step writes it **directly** through
//! [`set_semantic_state`](viso_ui::NodeStore::set_semantic_state) rather than the
//! reactive projector path the scalar-cell controls (checkbox, slider) use — the
//! projector exists to drive a node column *from* a reactive cell, and a tree row
//! has no such cell.

use viso_ui::{Role, SemanticState, Semantics};

use super::model::VisibleRow;

/// The named container semantics for a file tree's viewport — a [`Role::Tree`]
/// landmark an assistive technology can jump to and announce by name. `label` is
/// the tree's accessible name (its purpose, e.g. the panel title); an empty name
/// yields an unnamed tree.
pub(super) fn tree_container(label: &str) -> Semantics {
    if label.is_empty() {
        Semantics::role(Role::Tree)
    } else {
        Semantics::role(Role::Tree).with_label(label)
    }
}

/// The authored role for a visible row — a [`Role::TreeItem`] named by its label.
/// Authored once when the row mounts; the live expanded/selected facts are written
/// separately by [`row_state`] as they change.
pub(super) fn tree_item(label: &str) -> Semantics {
    if label.is_empty() {
        Semantics::role(Role::TreeItem)
    } else {
        Semantics::role(Role::TreeItem).with_label(label)
    }
}

/// The live [`SemanticState`] for a visible row: `selected` for every row (a tree
/// row always announces whether it is in the selection), and `expanded` only for a
/// directory row (a file never discloses, so it carries no `aria-expanded`).
pub(super) fn row_state(row: VisibleRow, selected: bool) -> SemanticState {
    let state = SemanticState::default().with_selected(selected);
    if row.is_dir {
        state.with_expanded(row.expanded)
    } else {
        state
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controls::file_tree::model::NodeKey;

    fn dir_row(expanded: bool) -> VisibleRow {
        VisibleRow {
            key: NodeKey(0),
            depth: 0,
            is_dir: true,
            expanded,
        }
    }

    fn file_row() -> VisibleRow {
        VisibleRow {
            key: NodeKey(1),
            depth: 1,
            is_dir: false,
            expanded: false,
        }
    }

    #[test]
    fn container_is_a_named_tree() {
        assert_eq!(tree_container("Files").role, Role::Tree);
        assert_eq!(tree_container("Files").label.as_deref(), Some("Files"));
        assert_eq!(tree_container("").label, None, "an empty name is unnamed");
    }

    #[test]
    fn a_row_is_a_named_treeitem() {
        let item = tree_item("main.rs");
        assert_eq!(item.role, Role::TreeItem);
        assert_eq!(item.label.as_deref(), Some("main.rs"));
    }

    #[test]
    fn a_directory_row_carries_expanded_and_selected() {
        let open = row_state(dir_row(true), true);
        assert_eq!(open.expanded, Some(true), "an open dir announces expanded");
        assert_eq!(open.selected, Some(true));
        let shut = row_state(dir_row(false), false);
        assert_eq!(shut.expanded, Some(false));
        assert_eq!(shut.selected, Some(false));
    }

    #[test]
    fn a_file_row_carries_only_selected() {
        let s = row_state(file_row(), true);
        assert_eq!(
            s.expanded, None,
            "a file never discloses — no aria-expanded"
        );
        assert_eq!(s.selected, Some(true));
    }
}
