//! Component model and the retained node store: the component/node split, and
//! hot/warm/cold side-storage.
//!
//! A [`Component`] declares its subtree imperatively into a [`BuildCx`]; the
//! tree *is* the [`NodeArena`] — there is no intermediate element layer (a
//! retained tree, not a virtual DOM). Per-node data lives in parallel arrays
//! indexed by [`NodeId::index`], split by traversal frequency: `bounds` and
//! `dirty` are hot (touched every frame), `layout`/`style`/`measured` are warm
//! (read by measure/layout/paint). This is the data-oriented internal that
//! backs the object-oriented external API.

use crate::binding::BindingTable;
use crate::dirty::DirtyClass;
use crate::layout::{self, Align, Axis, Inset, LayoutInput, LayoutTree, Length, Measured, Size};
use crate::node::{NodeArena, NodeId};
use crate::state::StateId;
use crate::style::BoxStyle;
use viso_render::Rect;

/// The application entry into the tree: a component declares its children into
/// the [`BuildCx`]. This slice has no reactive state, so `build` takes `&self`;
/// state and incremental rebuild arrive with the reactive slice.
pub trait Component {
    /// Declare this component's subtree into the build context.
    fn build(&self, cx: &mut BuildCx<'_>);
}

/// A typed handle onto a built node. This slice carries only the [`NodeId`];
/// the typed payload lands with real widgets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Handle {
    id: NodeId,
}

impl Handle {
    /// The node this handle refers to.
    #[inline]
    pub fn id(self) -> NodeId {
        self.id
    }
}

/// Parameters for a Flex container declared via [`BuildCx::flex`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FlexStyle {
    /// Main axis.
    pub axis: Axis,
    /// Gap between adjacent children along the main axis.
    pub gap: f32,
    /// Inner padding on all four edges.
    pub padding: Inset,
    /// Cross-axis alignment of children.
    pub align: Align,
    /// The container's own size request within its parent.
    pub size: Size,
    /// The container's own background/border (transparent = pure layout box).
    pub style: BoxStyle,
}

impl Default for FlexStyle {
    fn default() -> Self {
        FlexStyle {
            axis: Axis::Row,
            gap: 0.0,
            padding: Inset::default(),
            align: Align::Start,
            size: Size::fill(),
            style: BoxStyle::NONE,
        }
    }
}

/// Parameters for a leaf declared via [`BuildCx::leaf`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LeafStyle {
    /// Size request within the parent.
    pub size: Size,
    /// Fill/border/radius.
    pub style: BoxStyle,
}

impl Default for LeafStyle {
    fn default() -> Self {
        LeafStyle {
            size: Size::fixed(0.0, 0.0),
            style: BoxStyle::NONE,
        }
    }
}

/// The retained node store: the arena plus parallel side-storage arrays, all
/// indexed by [`NodeId::index`]. Allocation pushes to every array in lockstep
/// so the index stays aligned; freeing leaves stale values in place and relies
/// on the arena's generation check for liveness.
#[derive(Default)]
pub struct NodeStore {
    /// Identity + ancestry links + free list.
    arena: NodeArena,
    /// Hot: resolved layout box, written by layout, read by paint.
    bounds: Vec<Rect>,
    /// Hot: pending invalidation per node, set by `mark_dirty` and consumed by
    /// the incremental measure/layout/paint passes, cleared at frame end.
    dirty: Vec<DirtyClass>,
    /// Warm: layout parameters (Flex container or leaf size).
    layout: Vec<LayoutInput>,
    /// Warm: paint style.
    style: Vec<BoxStyle>,
    /// Warm: measured natural size cache (recomputed each frame this slice).
    measured: Vec<Measured>,
}

impl NodeStore {
    /// A fresh, empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Drop every node, resetting to empty. Used when a structural rebuild
    /// replaces the whole tree (this slice rebuilds wholesale; targeted
    /// structural diffing lands later).
    pub fn clear(&mut self) {
        self.arena = NodeArena::new();
        self.bounds.clear();
        self.dirty.clear();
        self.layout.clear();
        self.style.clear();
        self.measured.clear();
    }

    /// The arena backing the tree.
    #[inline]
    pub fn arena(&self) -> &NodeArena {
        &self.arena
    }

    /// A node's resolved layout box (valid after a layout pass).
    #[inline]
    pub fn bounds(&self, id: NodeId) -> Rect {
        self.bounds[id.index() as usize]
    }

    /// A node's paint style.
    #[inline]
    pub fn style(&self, id: NodeId) -> BoxStyle {
        self.style[id.index() as usize]
    }

    /// A node's current pending invalidation set.
    #[inline]
    pub fn dirty(&self, id: NodeId) -> DirtyClass {
        self.dirty[id.index() as usize]
    }

    /// Mark `class` dirty on `id` and propagate each class up the parent chain
    /// to its own boundary.
    ///
    /// The bubbling classes are STRUCTURE and SEMANTICS (up to the root — a
    /// structural or semantic change reaches every ancestor) and MEASURE (up
    /// only while an ancestor's size still depends on its content). Every other
    /// class — STYLE, LAYOUT, TRANSFORM, PAINT, HIT_TEST — stays on the node
    /// itself. In particular a PAINT-only change never touches an ancestor, so
    /// it can never make ancestor layout dirty.
    ///
    /// Propagation walks the existing `parent` links, folding the class into
    /// each ancestor with an idempotent bit-or, so a repeat mark that adds
    /// nothing new stops early. No allocation; worst case is the tree depth.
    pub fn mark_dirty(&mut self, id: NodeId, class: DirtyClass) {
        if !self.arena.is_live(id) {
            return;
        }
        // Everything lands on the node itself.
        self.dirty[id.index() as usize] |= class;

        let bubbling = class & DirtyClass::BUBBLING;
        if bubbling.is_empty() {
            return;
        }
        // Unconditional bubblers reach the root; MEASURE stops at the first
        // ancestor whose own size is fixed on both axes (its box cannot change
        // when a descendant's natural size does).
        let unconditional = bubbling & (DirtyClass::STRUCTURE | DirtyClass::SEMANTICS);
        let mut measure_alive = bubbling.intersects(DirtyClass::MEASURE);

        let mut current = id;
        while let Some(parent) = self.arena.links(current).and_then(|l| l.parent) {
            let mut add = unconditional;
            if measure_alive {
                if fixed_on_both_axes(self.layout[parent.index() as usize]) {
                    // Boundary: this ancestor's size is content-independent, so
                    // MEASURE stops rising here (it is not folded into `parent`).
                    measure_alive = false;
                } else {
                    add |= DirtyClass::MEASURE;
                }
            }
            if add.is_empty() {
                break;
            }
            let slot = &mut self.dirty[parent.index() as usize];
            let before = *slot;
            *slot |= add;
            // If this ancestor already carried everything we add and MEASURE is
            // no longer rising, higher ancestors were reached on a prior mark.
            if *slot == before && !measure_alive {
                break;
            }
            current = parent;
        }
    }

    /// Clear every node's pending invalidation. Run at frame end once the
    /// incremental passes have consumed the dirty set.
    #[inline]
    pub fn clear_dirty(&mut self) {
        self.dirty.fill(DirtyClass::EMPTY);
    }

    /// Whether any node currently carries any dirty class. Lets a frame skip
    /// the incremental passes entirely when nothing changed.
    #[inline]
    pub fn any_dirty(&self) -> bool {
        self.dirty.iter().any(|d| !d.is_empty())
    }

    /// Apply a frame's batch of changed states to the tree: for each changed
    /// [`StateId`], walk its bindings (static then dynamic) and mark the bound
    /// node dirty with the edge's classes. This is the flush phase's core —
    /// one pass over the deduplicated pending set turns many writes into
    /// targeted, layered invalidation. Returns how many bindings were applied
    /// (a steady-state counter; zero means no reactive work this frame).
    ///
    /// `changed` is the drained pending write-set (see
    /// [`StateStore::take_pending`]); `bindings` is the compiled edge table.
    pub fn flush_state_transactions(
        &mut self,
        changed: &[StateId],
        bindings: &BindingTable,
    ) -> u32 {
        let mut applied = 0;
        for &state in changed {
            for edge in bindings.for_state(state) {
                self.mark_dirty(edge.node, edge.class);
                applied += 1;
            }
            for edge in bindings.dynamic_for_state(state) {
                self.mark_dirty(edge.node, edge.class);
                applied += 1;
            }
        }
        applied
    }

    /// Allocate a node and push its side-storage in lockstep so array indices
    /// stay aligned with the arena. Newly reused slots overwrite stale values.
    fn alloc(&mut self, input: LayoutInput, style: BoxStyle) -> NodeId {
        let id = self.arena.alloc();
        let i = id.index() as usize;
        // Reused slots already have an array entry; fresh slots extend the tail.
        if i < self.bounds.len() {
            self.bounds[i] = Rect {
                x: 0.0,
                y: 0.0,
                w: 0.0,
                h: 0.0,
            };
            self.dirty[i] = DirtyClass::EMPTY;
            self.layout[i] = input;
            self.style[i] = style;
            self.measured[i] = Measured::default();
        } else {
            debug_assert_eq!(i, self.bounds.len(), "arena index must stay dense");
            self.bounds.push(Rect {
                x: 0.0,
                y: 0.0,
                w: 0.0,
                h: 0.0,
            });
            self.dirty.push(DirtyClass::EMPTY);
            self.layout.push(input);
            self.style.push(style);
            self.measured.push(Measured::default());
        }
        id
    }

    /// Run the bottom-up measure pass then the top-down layout pass over the
    /// subtree at `root`, placing the root into `surface`. `scratch` is a
    /// reusable child-id buffer owned by the caller so frames allocate nothing.
    pub fn layout(&mut self, root: NodeId, surface: Rect, scratch: &mut Vec<u32>) {
        scratch.clear();
        layout::measure(self, root.index(), scratch);
        scratch.clear();
        layout::layout(self, root.index(), surface, scratch);
    }

    /// Re-measure and re-place only the subtrees carrying MEASURE or LAYOUT
    /// invalidation, reusing the last frame's `measured`/`bounds` everywhere
    /// else, and report how many nodes each pass touched.
    ///
    /// A node whose MEASURE rose to it can change its own box, so its redo
    /// starts one level up at its (size-stable) parent; a LAYOUT-only container
    /// keeps its box and re-places its own children. The shallowest such node
    /// subsumes its descendants, so each subtree is recomputed once. `scratch`
    /// (child ids) and `redo_roots` are caller-owned so a redo allocates
    /// nothing. With no MEASURE/LAYOUT dirt the counts are zero and the tree is
    /// untouched.
    pub fn relayout_dirty(
        &mut self,
        root: NodeId,
        surface: Rect,
        scratch: &mut Vec<u32>,
        redo_roots: &mut Vec<NodeId>,
    ) -> (u32, u32) {
        redo_roots.clear();
        if !self.arena.is_live(root) {
            return (0, 0);
        }

        // Roots for the redo: the shallowest nodes whose subtree must be
        // recomputed. A MEASURE change flows through its parent's child
        // distribution, so redo from the parent (clamped to `root`, which owns
        // the whole surface); a LAYOUT-only container redoes itself.
        let relayout = DirtyClass::MEASURE | DirtyClass::LAYOUT;
        for index in 0..self.dirty.len() as u32 {
            let Some(id) = self.arena.live_id(index) else {
                continue;
            };
            if !self.dirty[index as usize].intersects(relayout) {
                continue;
            }
            let redo_from = if self.dirty[index as usize].intersects(DirtyClass::MEASURE) {
                self.arena.links(id).and_then(|l| l.parent).unwrap_or(root)
            } else {
                id
            };
            if !self.is_ancestor_in(redo_roots, redo_from) {
                redo_roots.retain(|&r| !self.is_ancestor(redo_from, r));
                redo_roots.push(redo_from);
            }
        }

        let mut measured = 0;
        let mut laid_out = 0;
        for redo in redo_roots.iter().copied() {
            let bounds = if redo == root {
                surface
            } else {
                self.bounds(redo)
            };
            scratch.clear();
            layout::measure(self, redo.index(), scratch);
            measured += self.subtree_len(redo);
            scratch.clear();
            layout::layout(self, redo.index(), bounds, scratch);
            laid_out += self.subtree_len(redo);
        }
        (measured, laid_out)
    }

    /// Rebuild the primitive list when any paint-affecting invalidation is
    /// pending, and report how many primitives were emitted (0 when skipped).
    ///
    /// Paint is coarse this slice: a paint-affecting change anywhere re-emits
    /// the whole tree into `out`. Relayout implies repaint (moved boxes must
    /// re-emit their quads); an isolated PAINT or TRANSFORM mark repaints
    /// without any relayout. The renderer still reuses `out` frame to frame.
    pub fn repaint_dirty(&self, root: NodeId, out: &mut Vec<viso_render::Primitive>) -> u32 {
        if !self.arena.is_live(root) {
            return 0;
        }
        let paint_classes =
            DirtyClass::PAINT | DirtyClass::TRANSFORM | DirtyClass::MEASURE | DirtyClass::LAYOUT;
        if !self.dirty.iter().any(|d| d.intersects(paint_classes)) {
            return 0;
        }
        out.clear();
        crate::paint::paint_tree(self, root, out);
        out.len() as u32
    }

    /// Whether any node in `roots` is `node` itself or an ancestor of it.
    fn is_ancestor_in(&self, roots: &[NodeId], node: NodeId) -> bool {
        roots
            .iter()
            .any(|&r| r == node || self.is_ancestor(r, node))
    }

    /// Whether `ancestor` lies strictly above `node` on the parent chain.
    fn is_ancestor(&self, ancestor: NodeId, node: NodeId) -> bool {
        let mut cur = self.arena.links(node).and_then(|l| l.parent);
        while let Some(p) = cur {
            if p == ancestor {
                return true;
            }
            cur = self.arena.links(p).and_then(|l| l.parent);
        }
        false
    }

    /// Count of live nodes in the subtree rooted at `id` (inclusive). Used to
    /// report recompute work; walks ancestry links, no allocation.
    fn subtree_len(&self, id: NodeId) -> u32 {
        let mut count = 1;
        let mut child = self.arena.links(id).and_then(|l| l.first_child);
        while let Some(c) = child {
            count += self.subtree_len(c);
            child = self.arena.links(c).and_then(|l| l.next_sibling);
        }
        count
    }
}

/// How much work each incremental layer did in one frame — the recompute
/// counters an inspector or test reads to confirm only the dirty subtree moved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FrameRecompute {
    /// Nodes whose natural size was re-measured.
    pub measured: u32,
    /// Nodes whose box was re-placed by layout.
    pub laid_out: u32,
    /// Primitives emitted by the paint rebuild (0 when paint was skipped).
    pub painted: u32,
}

impl FrameRecompute {
    /// Whether no layer did any work this frame (a fully idle frame).
    #[inline]
    pub fn is_idle(self) -> bool {
        self.measured == 0 && self.laid_out == 0 && self.painted == 0
    }
}

/// The measure/layout passes read the store through this bridge, keeping the
/// algorithm decoupled from the concrete array layout.
impl LayoutTree for NodeStore {
    #[inline]
    fn input(&self, index: u32) -> LayoutInput {
        self.layout[index as usize]
    }

    fn children(&self, index: u32, out: &mut Vec<u32>) {
        let id = self.arena_id(index);
        let mut child = self.arena.links(id).and_then(|l| l.first_child);
        while let Some(c) = child {
            out.push(c.index());
            child = self.arena.links(c).and_then(|l| l.next_sibling);
        }
    }

    #[inline]
    fn measured(&self, index: u32) -> Measured {
        self.measured[index as usize]
    }

    #[inline]
    fn set_measured(&mut self, index: u32, m: Measured) {
        self.measured[index as usize] = m;
    }

    #[inline]
    fn set_bounds(&mut self, index: u32, r: Rect) {
        self.bounds[index as usize] = r;
    }
}

impl NodeStore {
    /// Reconstruct a live [`NodeId`] from a dense index by reading the arena's
    /// current generation for that slot. Used by the layout bridge, which works
    /// in bare indices for cache density.
    fn arena_id(&self, index: u32) -> NodeId {
        // The layout passes only ever walk live nodes reachable from the root,
        // so the slot is occupied; pair the index with its live generation.
        self.arena.live_id(index).expect("layout walks live nodes")
    }
}

/// Whether a node's own size request is a hard pixel size on both axes. Such a
/// node's box cannot change when a descendant's natural size does, so it is the
/// boundary where a rising MEASURE invalidation stops.
#[inline]
fn fixed_on_both_axes(input: LayoutInput) -> bool {
    let size = input.size();
    matches!(size.width, Length::Fixed(_)) && matches!(size.height, Length::Fixed(_))
}

/// Build-time context: declares nodes into a [`NodeStore`] while tracking the
/// current parent via a cursor stack (a phase-specific context). A `flex`
/// call pushes its node as the parent for the duration of its child closure,
/// then pops — so the closure's `leaf`/`flex` calls attach beneath it.
pub struct BuildCx<'a> {
    store: &'a mut NodeStore,
    /// Parent cursor stack; the top is the current insertion parent.
    stack: Vec<NodeId>,
    /// The tree root, set by the first declared node.
    root: Option<NodeId>,
}

impl<'a> BuildCx<'a> {
    /// Start a build against `store`. The store should be empty or freshly
    /// [`NodeStore::clear`]ed.
    pub fn new(store: &'a mut NodeStore) -> Self {
        BuildCx {
            store,
            stack: Vec::new(),
            root: None,
        }
    }

    /// The root node declared during the build, if any.
    #[inline]
    pub fn root(&self) -> Option<NodeId> {
        self.root
    }

    /// Declare a Flex container and its children. The `children` closure runs
    /// with this container as the active parent, so nested `flex`/`leaf` calls
    /// attach beneath it.
    pub fn flex(&mut self, style: FlexStyle, children: impl FnOnce(&mut BuildCx<'_>)) -> Handle {
        let input = LayoutInput::Flex {
            axis: style.axis,
            gap: style.gap,
            padding: style.padding,
            align: style.align,
            size: style.size,
        };
        let id = self.push_node(input, style.style);
        self.stack.push(id);
        children(self);
        self.stack.pop();
        Handle { id }
    }

    /// Declare a leaf node.
    pub fn leaf(&mut self, style: LeafStyle) -> Handle {
        let id = self.push_node(LayoutInput::Leaf { size: style.size }, style.style);
        Handle { id }
    }

    /// Allocate a node, attach it under the current parent (or record it as the
    /// root), and return its id.
    fn push_node(&mut self, input: LayoutInput, style: BoxStyle) -> NodeId {
        let id = self.store.alloc(input, style);
        if let Some(&parent) = self.stack.last() {
            self.store.arena.append_child(parent, id);
        } else {
            self.root = Some(id);
        }
        id
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use viso_render::Rgba;

    const RED: Rgba = Rgba {
        r: 1.0,
        g: 0.0,
        b: 0.0,
        a: 1.0,
    };
    const GREEN: Rgba = Rgba {
        r: 0.0,
        g: 1.0,
        b: 0.0,
        a: 1.0,
    };

    fn surface(w: f32, h: f32) -> Rect {
        Rect {
            x: 0.0,
            y: 0.0,
            w,
            h,
        }
    }

    #[test]
    fn alloc_keeps_arrays_index_aligned() {
        let mut store = NodeStore::new();
        let a = store.alloc(
            LayoutInput::Leaf {
                size: Size::fixed(1.0, 1.0),
            },
            BoxStyle::NONE,
        );
        let b = store.alloc(
            LayoutInput::Leaf {
                size: Size::fixed(2.0, 2.0),
            },
            BoxStyle::NONE,
        );
        assert_eq!(a.index(), 0);
        assert_eq!(b.index(), 1);
        // Every parallel array grew in lockstep with the arena.
        assert_eq!(store.bounds.len(), 2);
        assert_eq!(store.layout.len(), 2);
        assert_eq!(store.style.len(), 2);
        assert_eq!(store.measured.len(), 2);
    }

    #[test]
    fn measure_sums_children_plus_gap_and_padding() {
        let mut store = NodeStore::new();
        let root = {
            let mut cx = BuildCx::new(&mut store);
            cx.flex(
                FlexStyle {
                    axis: Axis::Row,
                    gap: 8.0,
                    padding: Inset::all(4.0),
                    ..Default::default()
                },
                |cx| {
                    cx.leaf(LeafStyle {
                        size: Size::fixed(10.0, 20.0),
                        style: BoxStyle::solid(RED),
                    });
                    cx.leaf(LeafStyle {
                        size: Size::fixed(30.0, 12.0),
                        style: BoxStyle::solid(GREEN),
                    });
                },
            );
            cx.root().unwrap()
        };
        let mut scratch = Vec::new();
        layout::measure(&mut store, root.index(), &mut scratch);
        let m = store.measured[root.index() as usize];
        // main = 10 + 30 + gap(8) + padding(4+4) = 56; cross = max(20,12) + 8 = 28.
        assert_eq!(m.w, 56.0);
        assert_eq!(m.h, 28.0);
    }

    #[test]
    fn layout_distributes_fill_by_weight() {
        let mut store = NodeStore::new();
        let (root, fixed, grow1, grow2);
        {
            let mut cx = BuildCx::new(&mut store);
            let mut ids = (None, None, None);
            let r = cx.flex(
                FlexStyle {
                    axis: Axis::Row,
                    gap: 0.0,
                    ..Default::default()
                },
                |cx| {
                    ids.0 = Some(cx.leaf(LeafStyle {
                        size: Size::fixed(40.0, 10.0),
                        ..Default::default()
                    }));
                    ids.1 = Some(cx.leaf(LeafStyle {
                        size: Size {
                            width: crate::layout::Length::Fill { weight: 1.0 },
                            height: crate::layout::Length::Fixed(10.0),
                        },
                        ..Default::default()
                    }));
                    ids.2 = Some(cx.leaf(LeafStyle {
                        size: Size {
                            width: crate::layout::Length::Fill { weight: 3.0 },
                            height: crate::layout::Length::Fixed(10.0),
                        },
                        ..Default::default()
                    }));
                },
            );
            root = r.id();
            fixed = ids.0.unwrap().id();
            grow1 = ids.1.unwrap().id();
            grow2 = ids.2.unwrap().id();
        }
        let mut scratch = Vec::new();
        store.layout(root, surface(140.0, 10.0), &mut scratch);
        // free = 140 - 40 = 100; split 1:3 -> 25 and 75.
        assert_eq!(store.bounds(fixed).w, 40.0);
        assert_eq!(store.bounds(grow1).w, 25.0);
        assert_eq!(store.bounds(grow2).w, 75.0);
        // Placed edge-to-edge from the origin.
        assert_eq!(store.bounds(fixed).x, 0.0);
        assert_eq!(store.bounds(grow1).x, 40.0);
        assert_eq!(store.bounds(grow2).x, 65.0);
    }

    #[test]
    fn align_center_offsets_cross_axis() {
        let mut store = NodeStore::new();
        let (root, child);
        {
            let mut cx = BuildCx::new(&mut store);
            let mut c = None;
            let r = cx.flex(
                FlexStyle {
                    axis: Axis::Row,
                    align: Align::Center,
                    ..Default::default()
                },
                |cx| {
                    c = Some(cx.leaf(LeafStyle {
                        size: Size::fixed(20.0, 40.0),
                        ..Default::default()
                    }));
                },
            );
            root = r.id();
            child = c.unwrap().id();
        }
        let mut scratch = Vec::new();
        store.layout(root, surface(100.0, 100.0), &mut scratch);
        // Cross extent 100, child 40 tall -> centered at y = (100-40)/2 = 30.
        assert_eq!(store.bounds(child).y, 30.0);
        assert_eq!(store.bounds(child).h, 40.0);
    }

    #[test]
    fn nested_flex_lays_out_recursively() {
        let mut store = NodeStore::new();
        let (root, inner, leaf_id);
        {
            let mut cx = BuildCx::new(&mut store);
            let mut inner_h = None;
            let mut leaf_h = None;
            let r = cx.flex(
                FlexStyle {
                    axis: Axis::Row,
                    ..Default::default()
                },
                |cx| {
                    let ih = cx.flex(
                        FlexStyle {
                            axis: Axis::Column,
                            size: Size::fixed(50.0, 80.0),
                            ..Default::default()
                        },
                        |cx| {
                            leaf_h = Some(cx.leaf(LeafStyle {
                                size: Size::fixed(30.0, 20.0),
                                ..Default::default()
                            }));
                        },
                    );
                    inner_h = Some(ih);
                },
            );
            root = r.id();
            inner = inner_h.unwrap().id();
            leaf_id = leaf_h.unwrap().id();
        }
        let mut scratch = Vec::new();
        store.layout(root, surface(200.0, 100.0), &mut scratch);
        // Inner Column is fixed 50x80, placed at origin.
        assert_eq!(store.bounds(inner).w, 50.0);
        assert_eq!(store.bounds(inner).h, 80.0);
        // Its leaf sits at the inner container's origin.
        assert_eq!(store.bounds(leaf_id).x, 0.0);
        assert_eq!(store.bounds(leaf_id).y, 0.0);
        assert_eq!(store.bounds(leaf_id).w, 30.0);
    }
}
