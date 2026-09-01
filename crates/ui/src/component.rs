//! Component model and the retained node store (§9 component/node split,
//! §8.4 hot/warm/cold side-storage).
//!
//! A [`Component`] declares its subtree imperatively into a [`BuildCx`]; the
//! tree *is* the [`NodeArena`] — there is no intermediate element layer (§8.1
//! retained tree, not a virtual DOM). Per-node data lives in parallel arrays
//! indexed by [`NodeId::index`], split by traversal frequency (§8.4): `bounds`
//! and `dirty` are hot (touched every frame), `layout`/`style`/`measured` are
//! warm (read by measure/layout/paint). This is the data-oriented internal
//! that backs the object-oriented external API.

use crate::dirty::DirtyClass;
use crate::layout::{self, Align, Axis, Inset, LayoutInput, LayoutTree, Measured, Size};
use crate::node::{NodeArena, NodeId};
use crate::style::BoxStyle;
use viso_render::Rect;

/// The application entry into the tree: a component declares its children into
/// the [`BuildCx`]. This slice has no reactive state, so `build` takes `&self`;
/// state and incremental rebuild arrive with the reactive slice (§10).
pub trait Component {
    /// Declare this component's subtree into the build context.
    fn build(&self, cx: &mut BuildCx<'_>);
}

/// A typed handle onto a built node (§6.3). This slice carries only the
/// [`NodeId`]; the typed payload lands with real widgets.
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
/// indexed by [`NodeId::index`] (§8.4). Allocation pushes to every array in
/// lockstep so the index stays aligned; freeing leaves stale values in place
/// and relies on the arena's generation check for liveness.
#[derive(Default)]
pub struct NodeStore {
    /// Identity + ancestry links + free list (§16).
    arena: NodeArena,
    /// Hot: resolved layout box, written by layout, read by paint.
    bounds: Vec<Rect>,
    /// Hot: pending invalidation. Stored this slice; propagation lands later.
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
}

/// The measure/layout passes read the store through this bridge, keeping the
/// algorithm decoupled from the concrete array layout (§12.2).
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

/// Build-time context: declares nodes into a [`NodeStore`] while tracking the
/// current parent via a cursor stack (§6.4 phase-specific context). A `flex`
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
