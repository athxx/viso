//! Read-only node-tree introspection for the inspector, Studio, and tests.
//!
//! Architecture section 34/62 ask that a `NodeId`'s tree, layout box, and dirty
//! reasons be inspectable without unsafe memory poking — through the same model
//! the runtime itself uses, so a tool, a headless golden test, and an AI
//! automation all read one contract. [`NodeStore::inspect_tree`] walks the live
//! arena from a root and snapshots each node into a flat, self-contained
//! [`InspectTree`], mirroring the accessibility [`SemanticsTree`](crate::semantics::SemanticsTree)
//! shape: `nodes[0]` is the root, each node names its children by index.
//!
//! This is a cold path (architecture section 7.2) — it allocates a fresh
//! snapshot and is never touched by the steady-state per-node traversal. It only
//! reads the store's existing accessors (`bounds`/`world`/`dirty`/the flag
//! columns/`content_payload`), so building a snapshot changes no node state and
//! leaves the hot-path counters unmoved.

use crate::component::NodeStore;
use crate::content::Content;
use crate::dirty::DirtyClass;
use crate::node::NodeId;
use viso_render::Rect;

/// What a node draws, derived from its content payload rather than stored — the
/// node model keeps no `kind` column, so a plain layout/decoration node reports
/// [`Container`](InspectKind::Container) and a content-bearing node reports the
/// variant of its payload. Enough for a tree readout to label each row without
/// inventing a field the runtime does not keep.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InspectKind {
    /// A node with no drawable content — a layout container or decoration box.
    Container,
    /// A node carrying a shaped glyph run.
    Text,
    /// A node carrying a textured image.
    Image,
    /// A node carrying a vector path.
    Path,
}

impl InspectKind {
    /// The lowercase label used in a tree dump (`container`, `text`, …).
    pub fn label(self) -> &'static str {
        match self {
            InspectKind::Container => "container",
            InspectKind::Text => "text",
            InspectKind::Image => "image",
            InspectKind::Path => "path",
        }
    }

    fn of(content: Option<&Content>) -> Self {
        match content {
            None => InspectKind::Container,
            Some(Content::Text { .. }) => InspectKind::Text,
            Some(Content::Image { .. }) => InspectKind::Image,
            Some(Content::Path { .. }) => InspectKind::Path,
        }
    }
}

/// The interaction/visibility flags a node carries, snapshotted from the store's
/// flag columns. All are read straight from existing accessors; a snapshot never
/// derives new behavior, it only reports what the node already is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct InspectFlags {
    /// Whether the node participates in hit testing.
    pub hittable: bool,
    /// Whether the node (and its subtree) is folded out of layout and paint.
    pub hidden: bool,
    /// Whether the node paints in the deferred top layer.
    pub overlay: bool,
    /// Whether the node can hold focus.
    pub focusable: bool,
    /// Whether the node currently holds focus.
    pub focused: bool,
    /// Whether the node carries authored accessibility semantics.
    pub has_semantics: bool,
}

/// One node's place in the inspected tree: identity, ancestry, what it draws, its
/// layout and world boxes, pending invalidation, and flags. Children are indices
/// into the owning [`InspectTree::nodes`], so the snapshot is self-contained —
/// the same convention [`SemanticsNode`](crate::semantics::SemanticsNode) uses.
#[derive(Debug, Clone, PartialEq)]
pub struct InspectNode {
    /// The node this row snapshots.
    pub id: NodeId,
    /// The parent node, or `None` for the root of the walk.
    pub parent: Option<NodeId>,
    /// What the node draws (derived from its content payload).
    pub kind: InspectKind,
    /// The unscrolled layout box.
    pub bounds: Rect,
    /// The scrolled, world-space box (equals `bounds` with no scrolling ancestor).
    pub world: Rect,
    /// The node's pending invalidation classes — the dirty reasons, readable via
    /// [`DirtyClass::iter_names`] / its `Display`.
    pub dirty: DirtyClass,
    /// The node's interaction/visibility flags.
    pub flags: InspectFlags,
    /// Indices of this node's children within [`InspectTree::nodes`], in tree
    /// order. Indices (not [`NodeId`]s) so a snapshot stands alone.
    pub children: Vec<usize>,
}

/// A flat, tree-shaped inspector view: `nodes[0]` is the root, each node naming
/// its children by index. Flat storage keeps it snapshot-friendly and avoids a
/// per-node heap node, matching [`SemanticsTree`](crate::semantics::SemanticsTree).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct InspectTree {
    /// The nodes in pre-order; `nodes[0]` is the root when non-empty.
    pub nodes: Vec<InspectNode>,
}

impl InspectTree {
    /// The root inspected node, if the tree is non-empty.
    pub fn root(&self) -> Option<&InspectNode> {
        self.nodes.first()
    }

    /// The number of nodes in the snapshot.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Whether the snapshot has no nodes.
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Find a snapshotted node by its [`NodeId`], if present.
    pub fn get(&self, id: NodeId) -> Option<&InspectNode> {
        self.nodes.iter().find(|n| n.id == id)
    }

    /// A stable, indented text rendering of the tree — one line per node, two
    /// spaces of indent per depth — for a golden dump. Each line carries the
    /// id, kind, layout box, and (when non-empty) the dirty reasons, so a
    /// snapshot test reads as a readable tree rather than a debug blob.
    pub fn dump(&self) -> String {
        let mut out = String::new();
        if !self.is_empty() {
            self.dump_into(0, 0, &mut out);
        }
        out
    }

    fn dump_into(&self, index: usize, depth: usize, out: &mut String) {
        use core::fmt::Write as _;

        let node = &self.nodes[index];
        for _ in 0..depth {
            out.push_str("  ");
        }
        let b = node.bounds;
        let _ = write!(
            out,
            "{} #{}.{} [{:.0},{:.0} {:.0}x{:.0}]",
            node.kind.label(),
            node.id.index(),
            node.id.generation(),
            b.x,
            b.y,
            b.w,
            b.h,
        );
        if !node.dirty.is_empty() {
            let _ = write!(out, " dirty={}", node.dirty);
        }
        out.push('\n');
        for &child in &node.children {
            self.dump_into(child, depth + 1, out);
        }
    }
}

impl NodeStore {
    /// Snapshot the live subtree rooted at `root` into an [`InspectTree`].
    ///
    /// A cold-path introspection surface (architecture section 34/62): it walks
    /// ancestry links from `root` in pre-order and records each node's identity,
    /// ancestry, derived kind, layout/world boxes, dirty reasons, and flags. It
    /// reads only existing `&self` accessors, so it mutates no node state and
    /// does not touch the hot per-node traversal. An empty tree comes back for a
    /// non-live `root`.
    pub fn inspect_tree(&self, root: NodeId) -> InspectTree {
        let mut tree = InspectTree::default();
        if self.arena().is_live(root) {
            self.inspect_into(root, &mut tree);
        }
        tree
    }

    /// Push `id`, then recurse into its children, wiring child indices — the same
    /// flat-with-indices build the semantics derive uses.
    fn inspect_into(&self, id: NodeId, tree: &mut InspectTree) -> usize {
        let my_index = tree.nodes.len();
        let parent = self.arena().links(id).and_then(|l| l.parent);
        tree.nodes.push(InspectNode {
            id,
            parent,
            kind: InspectKind::of(self.content_payload(id)),
            bounds: self.bounds(id),
            world: self.world(id),
            dirty: self.dirty(id),
            flags: InspectFlags {
                hittable: self.hittable(id),
                hidden: self.hidden(id),
                overlay: self.is_overlay(id),
                focusable: self.focusable(id),
                focused: self.focused() == Some(id),
                has_semantics: self.semantics(id).is_some(),
            },
            children: Vec::new(),
        });
        let mut child = self.arena().links(id).and_then(|l| l.first_child);
        while let Some(c) = child {
            let ci = self.inspect_into(c, tree);
            tree.nodes[my_index].children.push(ci);
            child = self.arena().links(c).and_then(|l| l.next_sibling);
        }
        my_index
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::Size;
    use viso_render::Rgba;

    /// A leaf carrying a shaped (here empty) glyph run, so its kind resolves to
    /// `Text` and setting it marks MEASURE | LAYOUT | PAINT | SEMANTICS.
    fn text_content() -> Content {
        Content::Text {
            glyphs: Vec::new(),
            atlas: viso_render::TextureId(1),
            color_glyphs: Vec::new(),
            color_atlas: None,
            color: Rgba::TRANSPARENT,
            natural: crate::layout::Vec2 { x: 0.0, y: 0.0 },
            baseline: 0.0,
            shaped_at_width: None,
            soft_wrap: false,
        }
    }

    fn leaf(store: &mut NodeStore) -> NodeId {
        store.alloc_leaf(Size::fixed(1.0, 1.0))
    }

    #[test]
    fn snapshots_tree_shape_and_ancestry() {
        let mut store = NodeStore::new();
        let root = leaf(&mut store);
        let a = leaf(&mut store);
        let b = leaf(&mut store);
        store.arena_append_child(root, a);
        store.arena_append_child(root, b);

        let tree = store.inspect_tree(root);
        assert_eq!(tree.len(), 3);
        let r = tree.root().expect("non-empty");
        assert_eq!(r.id, root);
        assert_eq!(r.parent, None);
        assert_eq!(r.children.len(), 2);
        // Children are recorded in tree order, by index into the flat vec.
        assert_eq!(tree.nodes[r.children[0]].id, a);
        assert_eq!(tree.nodes[r.children[1]].id, b);
        assert_eq!(tree.nodes[r.children[0]].parent, Some(root));
        assert_eq!(tree.get(b).expect("present").id, b);
    }

    #[test]
    fn kind_is_derived_from_content_payload() {
        let mut store = NodeStore::new();
        let root = leaf(&mut store);
        let labeled = leaf(&mut store);
        store.arena_append_child(root, labeled);
        store.set_content_payload(labeled, text_content());

        let tree = store.inspect_tree(root);
        assert_eq!(tree.get(root).unwrap().kind, InspectKind::Container);
        assert_eq!(tree.get(labeled).unwrap().kind, InspectKind::Text);
    }

    #[test]
    fn dirty_reasons_are_snapshotted_readably() {
        let mut store = NodeStore::new();
        let root = leaf(&mut store);
        let labeled = leaf(&mut store);
        store.arena_append_child(root, labeled);
        // Setting text content marks MEASURE | LAYOUT | PAINT | SEMANTICS on the
        // leaf, and MEASURE | SEMANTICS bubble up to the root.
        store.set_content_payload(labeled, text_content());

        let tree = store.inspect_tree(root);
        let leaf_dirty = tree.get(labeled).unwrap().dirty;
        assert!(leaf_dirty.contains(DirtyClass::MEASURE));
        assert!(leaf_dirty.contains(DirtyClass::LAYOUT));
        assert!(leaf_dirty.contains(DirtyClass::PAINT));
        assert!(leaf_dirty.contains(DirtyClass::SEMANTICS));
        // The readable reasons render low-bit-first through the DirtyClass surface.
        assert_eq!(
            leaf_dirty.to_string(),
            "MEASURE | LAYOUT | PAINT | SEMANTICS"
        );
        // SEMANTICS is an unconditional bubbler, so it reaches the root; PAINT is
        // local and never rises, and MEASURE stops at the fixed-size root box.
        let root_dirty = tree.root().unwrap().dirty;
        assert!(root_dirty.contains(DirtyClass::SEMANTICS));
        assert!(!root_dirty.contains(DirtyClass::PAINT));
    }

    #[test]
    fn a_non_live_root_yields_an_empty_tree() {
        // An id minted in one store names nothing in a fresh one — inspecting it
        // there is a non-live root and snapshots to an empty tree.
        let mut minted = NodeStore::new();
        let id = leaf(&mut minted);
        let empty = NodeStore::new();
        assert!(!empty.arena().is_live(id));
        assert!(empty.inspect_tree(id).is_empty());
    }

    #[test]
    fn dump_is_indented_and_carries_reasons() {
        let mut store = NodeStore::new();
        let root = leaf(&mut store);
        let child = leaf(&mut store);
        store.arena_append_child(root, child);
        store.set_content_payload(child, text_content());

        let dump = store.inspect_tree(root).dump();
        // Root line first, child indented two spaces under it; the text child
        // carries its dirty reasons, the container root only the bubbled subset.
        let expected = format!(
            "container #{}.{} [0,0 0x0] dirty=SEMANTICS\n  \
             text #{}.{} [0,0 0x0] dirty=MEASURE | LAYOUT | PAINT | SEMANTICS\n",
            root.index(),
            root.generation(),
            child.index(),
            child.generation(),
        );
        assert_eq!(dump, expected);
    }
}
