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
use crate::paint::paint_content;
use crate::semantics::{SemanticsNode, SemanticsTree};
use viso_ende::JsonWriter;
use viso_render::{FrameStats, InspectBatch, InspectBatches, LayerClip, Primitive, Quad, Rect};

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

/// The half-open `[start, start + len)` span one node occupies in the paint
/// primitive stream, answering architecture section 62's
/// `NodeId -> paint primitive ranges`. The span is **inclusive of descendants**:
/// it runs from the node's own first primitive through the last primitive of its
/// last painted child (and, for a scroll viewport, through the closing
/// `LayerEnd`). So a child's span nests inside its parent's — the same
/// containment the node tree has — which is what a tool wants when it selects a
/// node to highlight everything it and its subtree drew.
///
/// A node that paints nothing itself and has no painted descendants has
/// `len == 0` (an invisible, childless container).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaintRange {
    /// The node this span belongs to.
    pub id: NodeId,
    /// The index of the node's first primitive in the stream.
    pub start: usize,
    /// The number of primitives in the node's span, its own plus its subtree's
    /// (and, for a scroll viewport, the wrapping `Layer`/`LayerEnd` pair).
    pub len: usize,
}

/// A flat map from each node to its span in the paint primitive stream, in the
/// exact order [`paint_tree`](crate::paint::paint_tree) emits them: pre-order,
/// with overlay roots deferred to the end. Paired with the `Vec<Primitive>`
/// [`paint_ranges`] produced, a tool can attribute any primitive back to the
/// node that drew it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PaintRanges {
    /// The per-node spans, in paint-emission order.
    pub ranges: Vec<PaintRange>,
}

impl PaintRanges {
    /// The number of node spans recorded.
    pub fn len(&self) -> usize {
        self.ranges.len()
    }

    /// Whether no spans were recorded.
    pub fn is_empty(&self) -> bool {
        self.ranges.is_empty()
    }

    /// Find a node's span by its [`NodeId`], if present.
    pub fn get(&self, id: NodeId) -> Option<&PaintRange> {
        self.ranges.iter().find(|r| r.id == id)
    }

    /// The number of primitives the walk emitted — the end of the root span,
    /// which (append semantics aside) equals the produced stream's length. The
    /// root's span is inclusive of every descendant, so this is *not* the sum of
    /// the per-node `len`s (those nest and overlap); it is the root span's reach.
    pub fn total_primitives(&self) -> usize {
        self.ranges
            .iter()
            .map(|r| r.start + r.len)
            .max()
            .unwrap_or(0)
    }

    /// A stable, one-line-per-node text rendering for a golden dump: each line
    /// carries the id and its `start..end` span.
    pub fn dump(&self) -> String {
        use core::fmt::Write as _;

        let mut out = String::new();
        for r in &self.ranges {
            let _ = writeln!(
                out,
                "#{}.{} {}..{}",
                r.id.index(),
                r.id.generation(),
                r.start,
                r.start + r.len,
            );
        }
        out
    }
}

/// Emit the subtree rooted at `root` into `out` — the **same** primitive stream
/// [`paint_tree`](crate::paint::paint_tree) produces — while recording each
/// node's span into a [`PaintRanges`].
///
/// A cold-path introspection surface (architecture section 34/62): it re-walks
/// the tree with the identical pre-order + deferred-overlay + visibility /
/// hidden / scroll-clip rules as `paint_tree`, but wraps each node's emission in
/// a [`PaintRange`] recording. It is kept out of the steady frame path (the
/// frame calls `paint_tree`; a tool or test calls this), so the hot walk carries
/// no range-recording branch. `out` is not cleared — append semantics match
/// `paint_tree`.
pub fn paint_ranges(store: &NodeStore, root: NodeId, out: &mut Vec<Primitive>) -> PaintRanges {
    let mut ranges = PaintRanges::default();
    let mut overlays: Vec<NodeId> = Vec::new();
    paint_subtree_ranges(store, root, out, &mut overlays, &mut ranges);
    let mut i = 0;
    while i < overlays.len() {
        paint_subtree_ranges(store, overlays[i], out, &mut overlays, &mut ranges);
        i += 1;
    }
    ranges
}

/// The range-recording twin of `paint::paint_subtree`: it appends the exact same
/// primitives (delegating content emission to the shared `paint_content`) and
/// records this node's descendant-inclusive `[start, start + len)` span.
/// Duplicated rather than threaded through the hot walk so the frame path stays
/// branch-free (architecture section 60 / 7.1).
///
/// The span is recorded in pre-order (parent's `PaintRange` precedes its
/// children's) by reserving the node's slot before descending, then patching its
/// `len` to `out.len() - start` once the subtree and any closing `LayerEnd` have
/// been emitted — so the span reaches through every descendant. A non-live or
/// hidden node paints nothing and records no span, exactly as `paint_subtree`
/// early-returns.
fn paint_subtree_ranges(
    store: &NodeStore,
    root: NodeId,
    out: &mut Vec<Primitive>,
    overlays: &mut Vec<NodeId>,
    ranges: &mut PaintRanges,
) {
    let arena = store.arena();
    if !arena.is_live(root) {
        return;
    }
    if store.hidden(root) {
        return;
    }

    let start = out.len();
    // Reserve this node's slot up front so its span precedes its children's in
    // pre-order; `len` is patched once the whole subtree has been emitted.
    let my_index = ranges.ranges.len();
    ranges.ranges.push(PaintRange {
        id: root,
        start,
        len: 0,
    });

    let world = store.world(root);
    let style = store.style(root);
    if style.is_visible() {
        out.push(Primitive::Quad(Quad {
            rect: world,
            color: style.fill,
            radius: style.radius,
            border: style.border,
        }));
    }

    if let Some(content) = store.content_payload(root) {
        paint_content(content, world, out);
    }

    let scroll_clip = store.is_scroll(root);
    if scroll_clip {
        out.push(Primitive::Layer(LayerClip {
            clip: world,
            opacity: 1.0,
        }));
    }

    let mut child = arena.links(root).and_then(|l| l.first_child);
    while let Some(c) = child {
        if store.is_overlay(c) {
            overlays.push(c);
        } else {
            paint_subtree_ranges(store, c, out, overlays, ranges);
        }
        child = arena.links(c).and_then(|l| l.next_sibling);
    }

    if scroll_clip {
        out.push(Primitive::LayerEnd);
    }

    // Now the subtree (and any closing clip) is fully emitted: the span reaches
    // from this node's first primitive through its last descendant's.
    ranges.ranges[my_index].len = out.len() - start;
}

// --- JSON serialization of the introspection snapshots --------------------
//
// The one canonical machine-readable form of every inspect surface, for Studio
// transport and `viso inspect --json` (architecture section 34's "same
// underlying model"). It reuses `viso_ende::JsonWriter` — the crate already
// depends on `viso-ende` — rather than pulling in a serialization framework
// (no serde). IDs are emitted as numbers, never strings (architecture section
// 16.2 / 29). All of it is cold-path (section 7.2): it walks a snapshot that is
// itself built off `&self` accessors, so nothing here touches a hot path.
//
// The `viso-render` batch snapshot cannot carry its own JSON (`viso-render` may
// not depend on `viso-ende` under the section 10 DAG), so this crate — which
// already sees both its own snapshots and the render batch snapshot's public
// fields — writes the batch JSON over those fields directly.

/// Writes a `NodeId` as `{"index":N,"generation":N}` — the same identity the
/// dumps print as `#index.generation`, in machine-readable form.
fn write_node_id(w: &mut JsonWriter, id: NodeId) {
    w.begin_object();
    w.name("index");
    w.uint(id.index() as u64);
    w.name("generation");
    w.uint(id.generation() as u64);
    w.end_object();
}

/// Writes a `Rect` as `{"x":..,"y":..,"w":..,"h":..}`.
fn write_rect(w: &mut JsonWriter, r: Rect) {
    w.begin_object();
    w.name("x");
    w.number(r.x as f64);
    w.name("y");
    w.number(r.y as f64);
    w.name("w");
    w.number(r.w as f64);
    w.name("h");
    w.number(r.h as f64);
    w.end_object();
}

impl DirtyClass {
    /// Writes the set bits as a JSON array of their names,
    /// `["MEASURE","LAYOUT"]` — the same names [`iter_names`](Self::iter_names)
    /// yields, low bit first. An empty set is `[]`.
    pub fn write_json(&self, w: &mut JsonWriter) {
        w.begin_array();
        for name in self.iter_names() {
            w.string(name);
        }
        w.end_array();
    }
}

impl InspectFlags {
    fn write_json(&self, w: &mut JsonWriter) {
        w.begin_object();
        w.name("hittable");
        w.bool(self.hittable);
        w.name("hidden");
        w.bool(self.hidden);
        w.name("overlay");
        w.bool(self.overlay);
        w.name("focusable");
        w.bool(self.focusable);
        w.name("focused");
        w.bool(self.focused);
        w.name("has_semantics");
        w.bool(self.has_semantics);
        w.end_object();
    }
}

impl InspectNode {
    fn write_json(&self, w: &mut JsonWriter) {
        w.begin_object();
        w.name("id");
        write_node_id(w, self.id);
        w.name("parent");
        match self.parent {
            Some(p) => write_node_id(w, p),
            None => w.null(),
        }
        w.name("kind");
        w.string(self.kind.label());
        w.name("bounds");
        write_rect(w, self.bounds);
        w.name("world");
        write_rect(w, self.world);
        w.name("dirty");
        self.dirty.write_json(w);
        w.name("flags");
        self.flags.write_json(w);
        w.name("children");
        w.begin_array();
        for &c in &self.children {
            w.uint(c as u64);
        }
        w.end_array();
        w.end_object();
    }
}

impl InspectTree {
    /// Writes the tree as `{"nodes":[…]}`, each node an object with its id,
    /// parent, kind, boxes, dirty reasons, flags, and child indices — the
    /// machine-readable twin of [`dump`](Self::dump).
    pub fn write_json(&self, w: &mut JsonWriter) {
        w.begin_object();
        w.name("nodes");
        w.begin_array();
        for node in &self.nodes {
            node.write_json(w);
        }
        w.end_array();
        w.end_object();
    }
}

impl PaintRange {
    fn write_json(&self, w: &mut JsonWriter) {
        w.begin_object();
        w.name("id");
        write_node_id(w, self.id);
        w.name("start");
        w.uint(self.start as u64);
        w.name("len");
        w.uint(self.len as u64);
        w.end_object();
    }
}

impl PaintRanges {
    /// Writes the spans as `{"total":N,"ranges":[…]}`, where `total` is
    /// [`total_primitives`](Self::total_primitives) and each range carries its
    /// node id, start, and (descendant-inclusive) length.
    pub fn write_json(&self, w: &mut JsonWriter) {
        w.begin_object();
        w.name("total");
        w.uint(self.total_primitives() as u64);
        w.name("ranges");
        w.begin_array();
        for r in &self.ranges {
            r.write_json(w);
        }
        w.end_array();
        w.end_object();
    }
}

/// Writes a `SemanticsNode` object. Optional facts (`label`, per-role `state`)
/// are `null` when absent, so the object shape is stable across nodes.
fn write_semantics_node(w: &mut JsonWriter, node: &SemanticsNode) {
    w.begin_object();
    w.name("id");
    write_node_id(w, node.id);
    w.name("role");
    w.string(node.role.label());
    w.name("label");
    match &node.label {
        Some(l) => w.string(l),
        None => w.null(),
    }
    w.name("focused");
    w.bool(node.focused);
    w.name("state");
    match node.state {
        Some(s) => {
            w.begin_object();
            w.name("checked");
            match s.checked {
                Some(v) => w.bool(v),
                None => w.null(),
            }
            w.name("value");
            match s.value {
                Some(v) => w.number(v as f64),
                None => w.null(),
            }
            w.name("range");
            match s.range {
                Some((min, max)) => {
                    w.begin_array();
                    w.number(min as f64);
                    w.number(max as f64);
                    w.end_array();
                }
                None => w.null(),
            }
            w.name("expanded");
            match s.expanded {
                Some(v) => w.bool(v),
                None => w.null(),
            }
            w.name("selected");
            match s.selected {
                Some(v) => w.bool(v),
                None => w.null(),
            }
            w.end_object();
        }
        None => w.null(),
    }
    w.name("bounds");
    write_rect(w, node.bounds);
    w.name("children");
    w.begin_array();
    for &c in &node.children {
        w.uint(c as u64);
    }
    w.end_array();
    w.end_object();
}

/// Writes a `SemanticsTree` as `{"nodes":[…]}`, matching the flat
/// child-index shape of the tree and paint-range JSON.
fn write_semantics_tree(w: &mut JsonWriter, tree: &SemanticsTree) {
    w.begin_object();
    w.name("nodes");
    w.begin_array();
    for node in &tree.nodes {
        write_semantics_node(w, node);
    }
    w.end_array();
    w.end_object();
}

/// Writes one render batch over its public fields (the render crate cannot
/// serialize itself; see the module note above).
fn write_batch(w: &mut JsonWriter, b: &InspectBatch) {
    w.begin_object();
    w.name("id");
    w.uint(b.id.0 as u64);
    w.name("pipeline");
    w.string(b.pipeline.label());
    w.name("pipeline_id");
    w.uint(b.pipeline_id.0 as u64);
    w.name("bind_group");
    match b.bind_group {
        Some(bg) => w.uint(bg.0 as u64),
        None => w.null(),
    }
    w.name("range");
    w.begin_array();
    w.uint(b.range.0 as u64);
    w.uint(b.range.1 as u64);
    w.end_array();
    w.name("clip");
    match b.clip {
        Some(c) => write_rect(w, c),
        None => w.null(),
    }
    w.name("offscreen");
    w.bool(b.offscreen);
    w.end_object();
}

/// A single read-only introspection snapshot aggregating every architecture
/// section 62 surface a frame exposes: the node tree, per-node paint spans, the
/// derived semantics tree, the frame's draw batches, and the frame counters.
///
/// It is the one model Studio transport, `viso inspect --json`, and headless
/// golden tests share (architecture section 34). Built by [`snapshot_ui`] off
/// `&self` accessors plus the render crate's own batch/stats snapshot — a cold
/// path (section 7.2) that mutates no state and never runs on the steady frame
/// path. Its fields are public so a consumer can read a surface directly, and
/// [`to_json`](Self::to_json) emits the canonical wire form.
#[derive(Debug, Clone, PartialEq)]
pub struct InspectSnapshot {
    /// The node tree rooted at the inspected root.
    pub tree: InspectTree,
    /// Each node's descendant-inclusive span in the paint primitive stream.
    pub paint_ranges: PaintRanges,
    /// The accessibility tree derived from the same root.
    pub semantics: SemanticsTree,
    /// The frame's draw batches (from the renderer; empty when none supplied).
    pub batches: InspectBatches,
    /// The frame counters (draw calls and geometry units).
    pub stats: FrameStats,
}

impl InspectSnapshot {
    /// Serializes the whole snapshot to the canonical compact JSON:
    /// `{"tree":…,"paint":…,"semantics":…,"batches":[…],"counters":{…}}`. The
    /// `counters` object uses the architecture section 61 names
    /// (`draw_calls`, `instances`, `node_count`, `visible_node_count`).
    pub fn to_json(&self) -> String {
        let mut w = JsonWriter::new();
        w.begin_object();

        w.name("tree");
        self.tree.write_json(&mut w);

        w.name("paint");
        self.paint_ranges.write_json(&mut w);

        w.name("semantics");
        write_semantics_tree(&mut w, &self.semantics);

        w.name("batches");
        w.begin_array();
        for b in &self.batches.batches {
            write_batch(&mut w, b);
        }
        w.end_array();

        w.name("counters");
        w.begin_object();
        w.name("draw_calls");
        w.uint(self.stats.draw_calls as u64);
        w.name("instances");
        w.uint(self.stats.instances as u64);
        w.name("node_count");
        w.uint(self.tree.len() as u64);
        w.name("visible_node_count");
        w.uint(self.visible_node_count() as u64);
        w.end_object();

        w.end_object();
        w.into_string()
    }

    /// The number of nodes in the snapshot not folded out by `hidden`.
    fn visible_node_count(&self) -> usize {
        self.tree.nodes.iter().filter(|n| !n.flags.hidden).count()
    }

    /// A stable text rendering of the whole snapshot for a golden dump: the
    /// tree, paint-range, and (via the render crate) batch dumps under labeled
    /// headers, plus the frame counters.
    pub fn dump(&self) -> String {
        use core::fmt::Write as _;

        let mut out = String::new();
        out.push_str("tree:\n");
        out.push_str(&self.tree.dump());
        out.push_str("paint:\n");
        out.push_str(&self.paint_ranges.dump());
        out.push_str("batches:\n");
        out.push_str(&self.batches.dump());
        let _ = writeln!(
            out,
            "counters: draw_calls={} instances={}",
            self.stats.draw_calls, self.stats.instances,
        );
        out
    }
}

/// Builds an [`InspectSnapshot`] for the subtree rooted at `root`.
///
/// A cold-path aggregator (architecture section 34/62): it composes the
/// existing UI-side surfaces — [`inspect_tree`](NodeStore::inspect_tree),
/// [`paint_ranges`], and [`derive_semantics`](NodeStore::derive_semantics) —
/// with the render-side `batches`/`stats` the caller reads from the renderer
/// (this crate never holds a `Renderer`). Pass
/// [`InspectBatches::default()`] and a zeroed [`FrameStats`] when no frame has
/// been encoded yet; the JSON shape stays stable (empty `batches`, zero
/// counters). Introduces no new query entry point — it only combines products
/// of accessors that already exist.
pub fn snapshot_ui(
    store: &NodeStore,
    root: NodeId,
    batches: InspectBatches,
    stats: FrameStats,
) -> InspectSnapshot {
    let tree = store.inspect_tree(root);
    let mut prims = Vec::new();
    let paint_ranges = paint_ranges(store, root, &mut prims);
    let semantics = store.derive_semantics(root);
    InspectSnapshot {
        tree,
        paint_ranges,
        semantics,
        batches,
        stats,
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

    #[test]
    fn child_spans_nest_inside_their_parents_and_reach_the_stream_end() {
        // A root with two content children, each emitting one GlyphRun. The root
        // is an invisible container (BoxStyle::NONE) so it emits nothing itself;
        // its span still reaches through both children (descendant-inclusive).
        let mut store = NodeStore::new();
        let root = leaf(&mut store);
        let a = leaf(&mut store);
        let b = leaf(&mut store);
        store.arena_append_child(root, a);
        store.arena_append_child(root, b);
        store.set_content_payload(a, text_content());
        store.set_content_payload(b, text_content());

        let mut prims = Vec::new();
        let ranges = paint_ranges(&store, root, &mut prims);

        // Two GlyphRun primitives, one per content child.
        assert_eq!(prims.len(), 2);
        assert_eq!(ranges.total_primitives(), prims.len());

        // The root's span covers the whole stream; each child owns exactly one
        // primitive and nests inside the root's span.
        let r = ranges.get(root).expect("root span");
        assert_eq!((r.start, r.len), (0, 2));
        let ra = ranges.get(a).expect("a span");
        let rb = ranges.get(b).expect("b span");
        assert_eq!((ra.start, ra.len), (0, 1));
        assert_eq!((rb.start, rb.len), (1, 1));
        // Containment: each child span sits within the root's.
        for c in [ra, rb] {
            assert!(r.start <= c.start && c.start + c.len <= r.start + r.len);
        }
        // Recorded in pre-order: root, then a, then b.
        assert_eq!(
            ranges.ranges.iter().map(|x| x.id).collect::<Vec<_>>(),
            vec![root, a, b],
        );
    }

    #[test]
    fn paint_ranges_produces_the_same_primitives_as_paint_tree() {
        // The cold range walk must be a faithful twin of the hot paint walk: the
        // stream it appends is byte-for-byte what `paint_tree` produces.
        let mut store = NodeStore::new();
        let root = leaf(&mut store);
        let a = leaf(&mut store);
        let b = leaf(&mut store);
        store.arena_append_child(root, a);
        store.arena_append_child(root, b);
        store.set_content_payload(a, text_content());
        store.set_content_payload(b, text_content());

        let mut from_paint = Vec::new();
        crate::paint::paint_tree(&store, root, &mut from_paint);
        let mut from_ranges = Vec::new();
        let _ = paint_ranges(&store, root, &mut from_ranges);

        assert_eq!(from_paint, from_ranges);
    }

    #[test]
    fn an_invisible_childless_container_records_a_zero_len_span() {
        // A lone invisible leaf (BoxStyle::NONE, no content) paints nothing, so
        // its span is empty — but it is still recorded.
        let mut store = NodeStore::new();
        let root = leaf(&mut store);

        let mut prims = Vec::new();
        let ranges = paint_ranges(&store, root, &mut prims);

        assert!(prims.is_empty());
        assert_eq!(ranges.len(), 1);
        let r = ranges.get(root).expect("root span");
        assert_eq!((r.start, r.len), (0, 0));
    }

    #[test]
    fn paint_ranges_dump_is_stable() {
        let mut store = NodeStore::new();
        let root = leaf(&mut store);
        let child = leaf(&mut store);
        store.arena_append_child(root, child);
        store.set_content_payload(child, text_content());

        let dump = paint_ranges(&store, root, &mut Vec::new()).dump();
        // Root span covers the one child primitive; the child owns it. Pre-order:
        // root line first, then the child.
        let expected = format!(
            "#{}.{} 0..1\n#{}.{} 0..1\n",
            root.index(),
            root.generation(),
            child.index(),
            child.generation(),
        );
        assert_eq!(dump, expected);
    }

    // --- snapshot / JSON ---------------------------------------------------

    use viso_render::FrameStats;

    /// A fixed two-batch frame snapshot (a clipped quad run + an offscreen glyph
    /// run) exercising every batch-JSON branch: label, pipeline id, present and
    /// absent bind group, clip-vs-null, and the offscreen flag.
    fn sample_batches() -> InspectBatches {
        use viso_render::{BatchId, BatchPipeline, BindGroupId, PipelineId};
        InspectBatches {
            batches: vec![
                InspectBatch {
                    id: BatchId(0),
                    pipeline: BatchPipeline::Quad,
                    pipeline_id: PipelineId(7),
                    bind_group: None,
                    range: (0, 3),
                    clip: Some(Rect {
                        x: 1.0,
                        y: 2.0,
                        w: 10.0,
                        h: 20.0,
                    }),
                    offscreen: false,
                },
                InspectBatch {
                    id: BatchId(1),
                    pipeline: BatchPipeline::GlyphRun,
                    pipeline_id: PipelineId(9),
                    bind_group: Some(BindGroupId(4)),
                    range: (3, 12),
                    clip: None,
                    offscreen: true,
                },
            ],
        }
    }

    /// A small fixed tree: a container root with one text child (so the child
    /// paints and carries semantics). Shared by the schema tests.
    fn sample_tree() -> (NodeStore, NodeId) {
        let mut store = NodeStore::new();
        let root = leaf(&mut store);
        let child = leaf(&mut store);
        store.arena_append_child(root, child);
        store.set_content_payload(child, text_content());
        (store, root)
    }

    #[test]
    fn snapshot_aggregates_all_four_surfaces() {
        let (store, root) = sample_tree();
        let stats = FrameStats {
            draw_calls: 2,
            instances: 15,
        };
        let snap = snapshot_ui(&store, root, sample_batches(), stats);

        // Each surface matches what its own accessor produces independently.
        assert_eq!(snap.tree, store.inspect_tree(root));
        assert_eq!(
            snap.paint_ranges,
            paint_ranges(&store, root, &mut Vec::new())
        );
        assert_eq!(snap.semantics, store.derive_semantics(root));
        assert_eq!(snap.batches.len(), 2);
        assert_eq!(snap.stats, stats);
    }

    #[test]
    fn snapshot_json_schema_is_stable() {
        let (store, root) = sample_tree();
        let child = store.inspect_tree(root).nodes[1].id;
        let stats = FrameStats {
            draw_calls: 2,
            instances: 15,
        };
        let json = snapshot_ui(&store, root, sample_batches(), stats).to_json();

        // Exact golden: field names, nesting, id shape (index/generation), and
        // the counter vocabulary are the wire contract Studio/CLI depend on.
        // Golden of the *unsettled* fresh tree: no layout/paint pass has run, so
        // bounds are zero-sized and every node still carries its birth dirty set
        // (the child a full MEASURE|LAYOUT|PAINT|SEMANTICS, the root SEMANTICS
        // from the structural append). Both nodes derive the default `group`
        // role. The point of the test is the *schema* — names, nesting, id shape,
        // counter vocabulary — not any particular settled geometry.
        let expected = format!(
            concat!(
                r#"{{"tree":{{"nodes":["#,
                r#"{{"id":{{"index":{ri},"generation":{rg}}},"parent":null,"kind":"container","#,
                r#""bounds":{{"x":0,"y":0,"w":0,"h":0}},"world":{{"x":0,"y":0,"w":0,"h":0}},"#,
                r#""dirty":["SEMANTICS"],"flags":{{"hittable":true,"hidden":false,"overlay":false,"#,
                r#""focusable":false,"focused":false,"has_semantics":false}},"children":[1]}},"#,
                r#"{{"id":{{"index":{ci},"generation":{cg}}},"parent":{{"index":{ri},"generation":{rg}}},"#,
                r#""kind":"text","bounds":{{"x":0,"y":0,"w":0,"h":0}},"world":{{"x":0,"y":0,"w":0,"h":0}},"#,
                r#""dirty":["MEASURE","LAYOUT","PAINT","SEMANTICS"],"flags":{{"hittable":true,"hidden":false,"overlay":false,"#,
                r#""focusable":false,"focused":false,"has_semantics":false}},"children":[]}}]}},"#,
                r#""paint":{{"total":1,"ranges":["#,
                r#"{{"id":{{"index":{ri},"generation":{rg}}},"start":0,"len":1}},"#,
                r#"{{"id":{{"index":{ci},"generation":{cg}}},"start":0,"len":1}}]}},"#,
                r#""semantics":{{"nodes":["#,
                r#"{{"id":{{"index":{ri},"generation":{rg}}},"role":"group","label":null,"focused":false,"#,
                r#""state":null,"bounds":{{"x":0,"y":0,"w":0,"h":0}},"children":[1]}},"#,
                r#"{{"id":{{"index":{ci},"generation":{cg}}},"role":"group","label":null,"focused":false,"#,
                r#""state":null,"bounds":{{"x":0,"y":0,"w":0,"h":0}},"children":[]}}]}},"#,
                r#""batches":["#,
                r#"{{"id":0,"pipeline":"quad","pipeline_id":7,"bind_group":null,"#,
                r#""range":[0,3],"clip":{{"x":1,"y":2,"w":10,"h":20}},"offscreen":false}},"#,
                r#"{{"id":1,"pipeline":"glyph","pipeline_id":9,"bind_group":4,"#,
                r#""range":[3,12],"clip":null,"offscreen":true}}],"#,
                r#""counters":{{"draw_calls":2,"instances":15,"node_count":2,"visible_node_count":2}}}}"#,
            ),
            ri = root.index(),
            rg = root.generation(),
            ci = child.index(),
            cg = child.generation(),
        );
        assert_eq!(json, expected);
    }

    #[test]
    fn snapshot_dump_is_stable() {
        let (store, root) = sample_tree();
        let snap = snapshot_ui(
            &store,
            root,
            sample_batches(),
            FrameStats {
                draw_calls: 2,
                instances: 15,
            },
        );
        let dump = snap.dump();
        // Sections are labeled and end with the frame counters.
        assert!(dump.starts_with("tree:\n"));
        assert!(dump.contains("\npaint:\n"));
        assert!(dump.contains("\nbatches:\n"));
        assert!(dump.ends_with("counters: draw_calls=2 instances=15\n"));
    }

    #[test]
    fn empty_root_produces_valid_json() {
        // A stale/never-live root: every UI surface is empty and no frame has
        // been encoded, yet the JSON shape is intact — empty arrays, zero
        // counters. This is the headless / no-GPU degradation contract.
        let mut minted = NodeStore::new();
        let dead = leaf(&mut minted);
        let store = NodeStore::new();
        let json = snapshot_ui(
            &store,
            dead,
            InspectBatches::default(),
            FrameStats {
                draw_calls: 0,
                instances: 0,
            },
        )
        .to_json();
        let expected = concat!(
            r#"{"tree":{"nodes":[]},"#,
            r#""paint":{"total":0,"ranges":[]},"#,
            r#""semantics":{"nodes":[]},"#,
            r#""batches":[],"#,
            r#""counters":{"draw_calls":0,"instances":0,"node_count":0,"visible_node_count":0}}"#,
        );
        assert_eq!(json, expected);
    }
}
