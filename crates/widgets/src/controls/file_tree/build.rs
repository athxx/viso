//! The build walk — author the file tree's retained node subtree once.
//!
//! A [`FileTree`](super::FileTree) is a named [`Role::Tree`]-to-be container over a
//! **keyed** virtual list: the model's [`flatten`] turns the tree plus the open
//! set into the visible-row list, and each visible row is authored by a keyed
//! row builder the substrate drives as rows recycle. Keying the list by the row's
//! stable [`NodeKey`] (not its scroll index) is what lets an expand or collapse —
//! which changes the row count and shifts every row below the toggle — reuse the
//! surviving rows' host nodes and their state, mounting only the rows that newly
//! entered the window (architecture section 12.4).
//!
//! The walk is cold: it runs once when the tree is authored, and each row body
//! runs only when its row is (re)mounted, never on the per-frame hot path. A row
//! is an indent spacer sized to `depth * indent`, a disclosure glyph for a
//! directory (blank for a file), and the node's label — the label fetched from the
//! warm tree by key at mount time, kept off the hot [`VisibleRow`] (section 8.4).
//!
//! The row builder reads the flattened rows through a **shared** warm cell
//! ([`VisibleRows`]) rather than an owned snapshot: the substrate invokes the
//! keyed `key_of` and the row body live during each reconcile, so a later
//! expand/collapse that reflattens into that same cell and grows/shrinks the item
//! count lets the substrate diff by [`NodeKey`] — mounting the rows that entered,
//! recycling those that left, and reusing every survivor's host. The keyed list,
//! the shared `visible` cell, the shared label index, and the `row_nodes` map this
//! walk fills are the substrate the reconcile step (a later section) drives.

use std::collections::HashMap;
use std::rc::Rc;

use viso_ui::virtual_list::ItemKey;
use viso_ui::{
    Axis, BoxStyle, BuildCx, Component, Inset, LeafStyle, Length, NodeId, Size, VirtualListStyle,
};

use crate::text::label;

use super::model::{NodeKey, TreeNode, VisibleRow};
use super::semantics::{row_state, tree_container, tree_item};
use super::{FileTreeStyle, Labels, RowNodes, VisibleRows};

/// Author the file tree's subtree: a keyed virtual list over the currently
/// visible rows, filling `row_nodes` with each mounted row's host node so the
/// reconcile step (a later section) can find a row by key. Returns the list
/// viewport node so the caller can attach the tree's container semantics to it and
/// so the reconcile step can drive this list's item count.
///
/// `visible` is the flatten of `roots` under `open`, held in a **shared** warm cell
/// the reconcile step rewrites in place on an expand/collapse; the keyed `key_of`
/// and the row body both read that cell live, so a reflatten plus a `set_item_count`
/// is all it takes for the substrate to diff by key and reuse survivors. `labels`
/// maps a node key to its display string, resolved once from the tree so a row body
/// never walks the tree at mount time.
pub fn build_tree(
    cx: &mut BuildCx<'_>,
    visible: &VisibleRows,
    labels: &Labels,
    style: &FileTreeStyle,
    row_nodes: &RowNodes,
) -> NodeId {
    let row_nodes = Rc::clone(row_nodes);
    let style = *style;

    // Key the list by the row's stable NodeKey, so an expand/collapse that shifts
    // every row below the toggle still reuses each surviving row's host node. Both
    // closures read the shared `visible` cell live: the reconcile step reflattens
    // into it, so the substrate sees the new keys and rows on its next pass.
    let key_visible = Rc::clone(visible);
    let key_of = move |index: usize| ItemKey(key_visible.borrow()[index].key.0);

    let row_visible = Rc::clone(visible);
    let labels = Rc::clone(labels);
    let item_count = visible.borrow().len();
    let viewport = cx.virtual_list_keyed(
        VirtualListStyle {
            axis: Axis::Column,
            size: style.size,
            overscan: style.overscan,
            estimated_row: style.row_height,
            style: style.background,
        },
        item_count,
        key_of,
        move |index, cx| {
            let row = row_visible.borrow()[index];
            let node = build_row(cx, row, &labels, &style);
            // Record this row's host by key, so the reconcile step can address it
            // directly rather than searching the arena (section 45).
            row_nodes.borrow_mut().insert(row.key, node);
        },
    );
    // Author the container's Role::Tree so an assistive technology presents the
    // rows as one navigable hierarchy. The viewport carries no live state — the
    // per-row expanded/selected facts live on the rows.
    cx.semantics(viewport, tree_container("")).id()
}

/// Author one row: a horizontal strip of an indent spacer (sized to the row's
/// depth), a disclosure glyph column (an arrow for a directory, blank for a file),
/// and the node's label. Returns the row's host node so the caller can key it.
fn build_row(
    cx: &mut BuildCx<'_>,
    row: VisibleRow,
    labels: &HashMap<NodeKey, String>,
    style: &FileTreeStyle,
) -> NodeId {
    let indent = f32::from(row.depth) * style.indent;
    let text = labels
        .get(&row.key)
        .map(String::as_str)
        .unwrap_or_default()
        .to_owned();
    let glyph = disclosure_glyph(row);
    let text_for_role = text.clone();

    let handle = cx.flex(
        viso_ui::FlexStyle {
            axis: Axis::Row,
            gap: style.gap,
            padding: Inset::default(),
            align: viso_ui::Align::Center,
            size: Size {
                width: Length::fill(),
                height: Length::Fixed(style.row_height),
            },
            style: BoxStyle::NONE,
        },
        |cx| {
            // Indent: a fixed-width spacer per depth level, so a child sits under
            // its parent's label. Zero-width at depth 0 (a root-forest node).
            cx.leaf(LeafStyle {
                size: Size::fixed(indent, style.row_height),
                style: BoxStyle::NONE,
            });
            // Disclosure column: a fixed-width slot holding the arrow glyph for a
            // directory, blank for a file, so labels align across rows regardless
            // of whether a row can expand.
            label(glyph)
                .size(Size::fixed(style.arrow_width, style.row_height))
                .build(cx);
            // The node's display name.
            label(text).build(cx);
        },
    );
    // Author the row as a Role::TreeItem named by its label, and seed its live
    // state so the first accessibility snapshot carries expanded (a directory) and
    // selected without needing an interaction first. A freshly built row is not yet
    // selected; the reconcile step refreshes selected/expanded as they change.
    cx.semantics(handle, tree_item(&text_for_role));
    cx.semantic_state(handle, row_state(row, false)).id()
}

/// The disclosure glyph for a row: a right-pointing arrow for a collapsed
/// directory, a down-pointing arrow for an expanded one, and a blank for a file
/// (which never discloses). The arrow doubles as the click target the command
/// layer wires in a later section.
fn disclosure_glyph(row: VisibleRow) -> &'static str {
    if !row.is_dir {
        ""
    } else if row.expanded {
        "\u{25be}" // ▾ black down-pointing small triangle
    } else {
        "\u{25b8}" // ▸ black right-pointing small triangle
    }
}

/// Build the key-to-label index by walking the tree once. The label is cold — read
/// only when a row mounts — so it lives in the tree, not the hot [`VisibleRow`];
/// this pre-resolves it into a flat map the row builder can index without a tree
/// walk per mount. Resolved once when the control is authored and shared with the
/// row builder through a [`Labels`] cell.
pub(super) fn label_index(roots: &[TreeNode]) -> HashMap<NodeKey, String> {
    fn walk(node: &TreeNode, out: &mut HashMap<NodeKey, String>) {
        out.insert(node.key, node.label.clone());
        for child in &node.children {
            walk(child, out);
        }
    }
    let mut out = HashMap::new();
    for root in roots {
        walk(root, &mut out);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controls::file_tree::model::TreeNode;
    use std::cell::RefCell;
    use std::collections::HashSet;
    use viso_ui::{
        BindingTable, NodeStore, SemanticProjector, StateStore, TextEdits, VirtualLists,
    };

    const ROOT: NodeKey = NodeKey(0);
    const A: NodeKey = NodeKey(1);
    const B: NodeKey = NodeKey(2);
    const A1: NodeKey = NodeKey(11);

    fn fixture() -> Vec<TreeNode> {
        vec![TreeNode::dir(
            ROOT,
            "root",
            vec![
                TreeNode::dir(A, "a", vec![TreeNode::file(A1, "a1")]),
                TreeNode::file(B, "b"),
            ],
        )]
    }

    /// The reactive stores a keyed virtual-list build writes into, kept together so
    /// a test can build the tree and inspect the registered list state.
    struct Reactive {
        store: NodeStore,
        states: StateStore,
        bindings: BindingTable,
        lists: VirtualLists,
        text_edits: TextEdits,
        projectors: SemanticProjector,
    }

    impl Reactive {
        fn new() -> Self {
            Reactive {
                store: NodeStore::new(),
                states: StateStore::new(),
                bindings: BindingTable::new(),
                lists: VirtualLists::new(),
                text_edits: TextEdits::new(),
                projectors: SemanticProjector::new(),
            }
        }

        fn build(
            &mut self,
            roots: &[TreeNode],
            open: &HashSet<NodeKey>,
            row_nodes: &RowNodes,
        ) -> NodeId {
            let visible: VisibleRows =
                Rc::new(RefCell::new(super::super::model::flatten(roots, open)));
            let labels: Labels = Rc::new(label_index(roots));
            let mut cx = BuildCx::with_reactive(
                &mut self.store,
                &mut self.states,
                &mut self.bindings,
                &mut self.lists,
                &mut self.text_edits,
                &mut self.projectors,
            );
            let style = FileTreeStyle::default();
            let node = build_tree(&mut cx, &visible, &labels, &style, row_nodes);
            let _ = cx.root();
            node
        }
    }

    /// A FileTree builds to a keyed virtual-list viewport whose item count is the
    /// flattened visible-row count, and it mounts no rows at build time (the
    /// substrate mounts the first window on the first reconcile).
    #[test]
    fn build_maps_to_a_keyed_virtual_list_over_the_visible_rows() {
        let roots = fixture();
        let open: HashSet<NodeKey> = [ROOT, A].into_iter().collect();
        let row_nodes: RowNodes = Rc::new(RefCell::new(HashMap::new()));
        let mut rx = Reactive::new();
        let viewport = rx.build(&roots, &open, &row_nodes);

        assert!(
            rx.store.is_scroll(viewport),
            "a FileTree maps to a scroll viewport (the keyed virtual list)"
        );
        let state = rx
            .lists
            .get(viewport)
            .expect("the tree registers per-list state on its viewport");
        // ROOT, A (expanded), A1, B => 4 visible rows; the canvas extent is seeded
        // to row_height * item_count, so the extent reflects the flattened count.
        let row_h = FileTreeStyle::default().row_height;
        assert_eq!(
            state.total_extent(),
            row_h * 4.0,
            "the list extent is row_height * the flattened visible-row count"
        );
        assert_eq!(
            state.mounted_count(),
            0,
            "no rows mount until the first reconcile"
        );
    }

    /// A collapsed root yields a single-item list, confirming the flatten drives
    /// the list length: closing everything shows only the root row.
    #[test]
    fn build_item_count_follows_the_open_set() {
        let roots = fixture();
        let open: HashSet<NodeKey> = HashSet::new(); // nothing open
        let row_nodes: RowNodes = Rc::new(RefCell::new(HashMap::new()));
        let mut rx = Reactive::new();
        let viewport = rx.build(&roots, &open, &row_nodes);

        let state = rx.lists.get(viewport).expect("registered state");
        let row_h = FileTreeStyle::default().row_height;
        assert_eq!(
            state.total_extent(),
            row_h,
            "a fully collapsed tree is a one-row list (just the root)"
        );
    }
}
