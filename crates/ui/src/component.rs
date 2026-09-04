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
use crate::context::EventCx;
use crate::dirty::DirtyClass;
use crate::grid::{GridPlacement, GridStyle, GridTracks, TrackSizing};
use crate::layout::{
    self, Align, Axis, Inset, LayoutInput, LayoutTree, Length, Measured, Size, Vec2,
};
use crate::node::{NodeArena, NodeId};
use crate::reactive::EffectStore;
use crate::semantics::{Role, Semantics, SemanticsNode, SemanticsTree};
use crate::state::{StateId, StateStore, StateValue};
use crate::style::{BoxStyle, StyleId};
use crate::token::Theme;
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

/// Parameters for a scroll viewport declared via [`BuildCx::scroll`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScrollStyle {
    /// The axis the viewport scrolls along.
    pub axis: Axis,
    /// The viewport's own size request within its parent (its visible box).
    pub size: Size,
    /// The viewport's own background/border (transparent = pure clip box).
    pub style: BoxStyle,
}

impl Default for ScrollStyle {
    fn default() -> Self {
        ScrollStyle {
            axis: Axis::Column,
            size: Size::fill(),
            style: BoxStyle::NONE,
        }
    }
}

/// Parameters for a virtual list declared via [`BuildCx::virtual_list`]. The
/// list is a [`ScrollStyle`] viewport whose content is a fixed-extent canvas
/// sized to the whole logical collection, so only a window of rows is ever
/// mounted while the scroll range still spans every item.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VirtualListStyle {
    /// The axis the list scrolls and stacks rows along.
    pub axis: Axis,
    /// The viewport's own size request within its parent (its visible box).
    pub size: Size,
    /// Extra rows mounted on each side of the visible window, so a small scroll
    /// reveals an already-built row rather than stalling on a mount.
    pub overscan: u32,
    /// The initial per-row main extent used to size the canvas and seed the
    /// height model before any row is measured.
    pub estimated_row: f32,
    /// The viewport's own background/border (transparent = pure clip box).
    pub style: BoxStyle,
}

impl Default for VirtualListStyle {
    fn default() -> Self {
        VirtualListStyle {
            axis: Axis::Column,
            size: Size::fill(),
            overscan: 4,
            estimated_row: crate::virtual_list::DEFAULT_ROW_HEIGHT,
            style: BoxStyle::NONE,
        }
    }
}

/// A node's pointer handler: an `FnMut` driven with the event context when the
/// node is on the dispatch chain of a hit. Boxed because handlers are cold —
/// touched only on an actual hit, never in the per-node hot traversal — so the
/// fat pointer never rides in the hot SoA columns.
pub type PointerHandler = Box<dyn FnMut(&mut EventCx<'_>)>;

/// A node's keyboard/IME handler: an `FnMut` driven with the event context when
/// the node is on the focused node's dispatch chain. Cold like
/// [`PointerHandler`] — touched only when a key or IME event routes to focus,
/// never in the per-node hot traversal — so it lives off the hot SoA columns as
/// an owned box. Parallel to (and independent of) the pointer handler column:
/// pointer events fire only pointer handlers, key/IME events only key handlers.
pub type KeyHandler = Box<dyn FnMut(&mut EventCx<'_>)>;

/// The retained node store: the arena plus parallel side-storage arrays, all
/// indexed by [`NodeId::index`]. Allocation pushes to every array in lockstep
/// so the index stays aligned; freeing leaves stale values in place and relies
/// on the arena's generation check for liveness.
#[derive(Default)]
pub struct NodeStore {
    /// Identity + ancestry links + free list.
    arena: NodeArena,
    /// Hot: resolved layout box, written by layout, read by measure caching.
    /// This is the *unscrolled* layout truth; a scroll offset never rewrites it.
    bounds: Vec<Rect>,
    /// Hot: the scrolled, world-space rect — `bounds` shifted by the accumulated
    /// ancestor scroll. Derived by [`resolve_transforms`](NodeStore::resolve_transforms)
    /// on a TRANSFORM-dirty frame; equals `bounds` for any node with no scrolling
    /// ancestor (the common case). Paint clip and hit testing read this, not
    /// `bounds`.
    world: Vec<Rect>,
    /// Hot: per-node scroll offset, nonzero only on [`LayoutInput::Scroll`]
    /// nodes; default `(0,0)`. A scroll node shifts its content subtree's world
    /// rect by `-scroll` without a relayout (TRANSFORM-class, not LAYOUT).
    scroll: Vec<Vec2>,
    /// Warm: a scroll viewport's content extent (the laid-out size of its content
    /// along each axis), written by the scroll layout arm. `(0,0)` for non-scroll
    /// nodes. Read by the scroll clamp (range = content − viewport, per axis).
    content: Vec<Vec2>,
    /// Warm: the main-axis offset of a positioned row inside an
    /// [`LayoutInput::AbsoluteRows`] canvas. The sentinel `f32::NAN` means "not a
    /// positioned row" — the common case, so an ordinary node reads back `None`.
    /// Written by the virtual-list reconcile step, read by the `AbsoluteRows`
    /// layout arm.
    row_offset: Vec<f32>,
    /// Warm: a grid node's column/row track templates, boxed because only grid
    /// nodes carry them — an ordinary node's entry is `None`. Read by the grid
    /// layout arm through [`LayoutTree::grid_column_tracks`] /
    /// [`grid_row_tracks`](LayoutTree::grid_row_tracks).
    grid_tracks: Vec<Option<Box<GridTracks>>>,
    /// Warm: a grid child's placement/span. `GridPlacement::default()` (auto-flow,
    /// span 1) for every non-explicitly-placed node — the common case.
    grid_placement: Vec<GridPlacement>,
    /// Hot: pending invalidation per node, set by `mark_dirty` and consumed by
    /// the incremental measure/layout/paint passes, cleared at frame end.
    dirty: Vec<DirtyClass>,
    /// Warm: layout parameters (Flex container or leaf size).
    layout: Vec<LayoutInput>,
    /// Warm: paint style.
    style: Vec<BoxStyle>,
    /// Warm: measured natural size cache (recomputed each frame this slice).
    measured: Vec<Measured>,
    /// Hot: whether a node participates in hit testing. Default `true` — a
    /// laid-out node is hittable, including a transparent pure-layout container
    /// (so a point in its padding/gap resolves to it). Flipping it to `false`
    /// makes a node pass-through without a structural change.
    hittable: Vec<bool>,
    /// Cold: per-node pointer handler, index-aligned but mostly `None`. Read
    /// only when a node lands on a hit's dispatch chain, so it lives off the hot
    /// columns as an owned box rather than an inline fat pointer.
    handlers: Vec<Option<PointerHandler>>,
    /// The single focused node, or `None` when nothing holds focus. Keyboard and
    /// IME events route to this node (not hit testing). One slot per store — a
    /// window has one focus — reset to `None` on a structural rebuild.
    focused: Option<NodeId>,
    /// The node currently holding pointer capture, or `None`. While set, the
    /// pointer router redirects Move/Up to this node regardless of what the
    /// pointer is over (drag-to-scroll, thumb drag). One slot per store — a
    /// single pointer this slice — reset to `None` on a structural rebuild.
    capture: Option<NodeId>,
    /// Cold flag column: whether a node can hold focus. Default `false` — unlike
    /// `hittable`, a node opts *in* to focus (only interactive nodes participate
    /// in the focus ring). Maintained index-aligned with the arena.
    focusable: Vec<bool>,
    /// Cold: per-node keyboard/IME handler, index-aligned but mostly `None`.
    /// Parallel to `handlers` but a distinct column: read only when a key/IME
    /// event routes through the focused node's dispatch chain.
    key_handlers: Vec<Option<KeyHandler>>,
    /// Cold: per-node style-token binding, index-aligned but mostly `None`. A
    /// node with a binding derives its warm `style` from theme tokens; a `None`
    /// entry means the node's `style` is a literal that no theme swap touches.
    /// Read only by the STYLE-resolve pass over style-dirty nodes, never in the
    /// hot per-node traversal — so it sits off the warm `style` column.
    styled: Vec<Option<StyleId>>,
    /// Cold: per-node authored accessibility semantics (role + label),
    /// index-aligned but mostly `None`. Holds the only heap data in the store
    /// (the label `String`); read only by the SEMANTICS-derive pass, never in
    /// the hot per-node traversal — so it sits off the hot columns like
    /// `handlers`/`styled`. A `None` entry means a plain layout/decoration node
    /// (the derive pass still gives an interactive node a default role).
    semantics: Vec<Option<Semantics>>,
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
        self.world.clear();
        self.scroll.clear();
        self.content.clear();
        self.row_offset.clear();
        self.grid_tracks.clear();
        self.grid_placement.clear();
        self.dirty.clear();
        self.layout.clear();
        self.style.clear();
        self.measured.clear();
        self.hittable.clear();
        self.handlers.clear();
        self.focusable.clear();
        self.key_handlers.clear();
        self.styled.clear();
        self.semantics.clear();
        self.focused = None;
        self.capture = None;
    }

    /// The arena backing the tree.
    #[inline]
    pub fn arena(&self) -> &NodeArena {
        &self.arena
    }

    /// A node's resolved layout box (valid after a layout pass). This is the
    /// unscrolled layout truth; for the scrolled, clip/hit-test-facing rect use
    /// [`world`](Self::world).
    #[inline]
    pub fn bounds(&self, id: NodeId) -> Rect {
        self.bounds[id.index() as usize]
    }

    /// A node's scrolled, world-space rect (valid after
    /// [`resolve_transforms`](Self::resolve_transforms)). Equals
    /// [`bounds`](Self::bounds) when the node has no scrolling ancestor. Paint
    /// clip and hit testing read this.
    #[inline]
    pub fn world(&self, id: NodeId) -> Rect {
        self.world[id.index() as usize]
    }

    /// A node's scroll offset (nonzero only on scroll viewports).
    #[inline]
    pub fn scroll(&self, id: NodeId) -> Vec2 {
        self.scroll[id.index() as usize]
    }

    /// A scroll viewport's content extent (the laid-out size of its content).
    /// `(0,0)` for non-scroll nodes.
    #[inline]
    pub fn content(&self, id: NodeId) -> Vec2 {
        self.content[id.index() as usize]
    }

    /// Set a node's main-axis offset inside an [`LayoutInput::AbsoluteRows`]
    /// canvas, the position the row is placed at when its parent lays out. The
    /// virtual-list reconcile writes this when it mounts or re-anchors a row.
    #[inline]
    pub fn set_row_offset(&mut self, id: NodeId, offset: f32) {
        self.row_offset[id.index() as usize] = offset;
    }

    /// Clear a node's positioned-row offset back to the "not a row" sentinel, so
    /// it is skipped when an `AbsoluteRows` canvas lays out. Used when a host is
    /// recycled out of the mounted window.
    #[inline]
    pub fn clear_row_offset(&mut self, id: NodeId) {
        self.row_offset[id.index() as usize] = f32::NAN;
    }

    /// Store a grid node's resolved track templates (its warm side payload).
    /// Crate-internal because [`GridTracks`] is an internal warm payload type.
    pub(crate) fn set_grid_tracks(&mut self, id: NodeId, tracks: GridTracks) {
        self.grid_tracks[id.index() as usize] = Some(Box::new(tracks));
    }

    /// Set a grid child's placement/span. Absent = auto-flow with span 1.
    pub fn set_grid_placement(&mut self, id: NodeId, placement: GridPlacement) {
        self.grid_placement[id.index() as usize] = placement;
    }

    /// Test/facade support: allocate a standalone grid node from a [`GridStyle`],
    /// storing its track templates. Does not attach it to a parent.
    pub fn alloc_grid(&mut self, style: GridStyle) -> NodeId {
        let column_count = style.columns.len().max(1) as u16;
        let row_count = style.rows.len() as u16;
        let input = LayoutInput::Grid {
            column_count,
            row_count,
            column_gap: style.column_gap,
            row_gap: style.row_gap,
            padding: style.padding,
            auto_rows: style.auto_rows,
            size: style.size,
        };
        let id = self.alloc(input, style.style);
        self.set_grid_tracks(
            id,
            GridTracks {
                columns: style.columns,
                rows: style.rows,
                auto_rows: style.auto_rows,
            },
        );
        id
    }

    /// Whether a node is a scroll viewport — it clips its content to its box and
    /// carries a scroll offset. Paint and hit testing branch on this to push a
    /// clip and narrow the descent.
    #[inline]
    pub fn is_scroll(&self, id: NodeId) -> bool {
        matches!(self.layout[id.index() as usize], LayoutInput::Scroll { .. })
    }

    /// The axis a scroll viewport scrolls along, or `None` for a non-scroll node.
    /// A viewport scrolls only along its own axis; the router uses this to pick
    /// which ancestor absorbs a scroll delta.
    #[inline]
    pub fn scroll_axis(&self, id: NodeId) -> Option<Axis> {
        match self.layout[id.index() as usize] {
            LayoutInput::Scroll { axis, .. } => Some(axis),
            _ => None,
        }
    }

    /// The scrollable range of a node on `axis`: `max(0, content − viewport)`.
    /// Zero when the content does not exceed the viewport (nothing to scroll).
    #[inline]
    pub fn scroll_range(&self, id: NodeId, axis: Axis) -> f32 {
        let i = id.index() as usize;
        let b = self.bounds[i];
        let viewport = match axis {
            Axis::Row => b.w,
            Axis::Column => b.h,
        };
        (self.content[i].on(axis) - viewport).max(0.0)
    }

    /// A node's laid-out box extent along `axis` (its `bounds` width for Row,
    /// height for Column). The virtual-list reconcile reads the viewport's own
    /// main extent to size the visible window.
    #[inline]
    pub fn bounds_main(&self, id: NodeId, axis: Axis) -> f32 {
        let b = self.bounds[id.index() as usize];
        match axis {
            Axis::Row => b.w,
            Axis::Column => b.h,
        }
    }

    /// A node's measured natural extent along `axis`. The virtual-list
    /// measurement feedback reads a freshly-laid-out row's main extent to fold it
    /// back into the height model.
    #[inline]
    pub fn measured_main(&self, id: NodeId, axis: Axis) -> f32 {
        let m = self.measured[id.index() as usize];
        match axis {
            Axis::Row => m.w,
            Axis::Column => m.h,
        }
    }

    /// Detach `child` from its parent and siblings without freeing it, parking it
    /// as a live orphan the caller can re-attach later. The recycle half of
    /// virtual-list host reuse. Returns `false` (a no-op) for a stale handle or a
    /// child with no parent.
    #[inline]
    pub fn arena_detach(&mut self, child: NodeId) -> bool {
        self.arena.detach_child(child)
    }

    /// Re-attach a parked host as the last child of `parent`, wiring the sibling
    /// chain. The mount half of virtual-list host reuse. Returns `false` if
    /// either node is stale.
    #[inline]
    pub fn arena_append_child(&mut self, parent: NodeId, child: NodeId) -> bool {
        self.arena.append_child(parent, child)
    }

    /// Allocate a fresh host node and append it under `canvas`, returning its id.
    /// Used by the virtual-list reconcile only when the recycle pool is empty
    /// (bounded first-fill growth); steady scroll reuses parked hosts and never
    /// calls this. The host's body is authored separately via a
    /// [`BuildCx::with_parent`]; the host itself is a positioned row placed by the
    /// `AbsoluteRows` canvas at its `row_offset`.
    ///
    /// The host is a `Flex` stacking along the list `axis`: it **fits** its body
    /// on the main axis (so its measured main extent is the real row height the
    /// canvas places and the height model absorbs) and **fills** the cross axis
    /// (so it spans the canvas width for a Column list). It is not a bare `Leaf`,
    /// which would measure to zero and collapse the row.
    pub fn alloc_row_host(&mut self, canvas: NodeId, axis: Axis) -> NodeId {
        let size = match axis {
            Axis::Column => Size {
                width: Length::fill(),
                height: Length::Fit,
            },
            Axis::Row => Size {
                width: Length::Fit,
                height: Length::fill(),
            },
        };
        let host = self.alloc(
            LayoutInput::Flex {
                axis,
                gap: 0.0,
                padding: Inset::default(),
                align: Align::Stretch,
                size,
            },
            BoxStyle::default(),
        );
        self.arena.append_child(canvas, host);
        host
    }

    /// Rewrite an [`LayoutInput::AbsoluteRows`] canvas's own fixed extent along
    /// `axis` to `extent`, keeping the cross axis unchanged. The virtual-list
    /// reconcile calls this when the total logical extent changes so the scroll
    /// viewport above reads the correct scroll range. A no-op for a stale handle
    /// or a non-canvas node.
    pub fn set_absolute_rows_extent(&mut self, canvas: NodeId, axis: Axis, extent: f32) {
        if !self.arena.is_live(canvas) {
            return;
        }
        let i = canvas.index() as usize;
        if let LayoutInput::AbsoluteRows { size, .. } = &mut self.layout[i] {
            match axis {
                Axis::Row => size.width = Length::Fixed(extent),
                Axis::Column => size.height = Length::Fixed(extent),
            }
        }
    }

    /// The node holding pointer capture, if any.
    #[inline]
    pub fn capture(&self) -> Option<NodeId> {
        self.capture
    }

    /// Set (or clear, with `None`) the pointer-capture holder. A live-guarded
    /// write: capturing a stale handle is a no-op, so a released/freed node
    /// never holds capture. Clearing is always honored.
    pub fn set_capture(&mut self, id: Option<NodeId>) {
        match id {
            Some(node) if self.arena.is_live(node) => self.capture = Some(node),
            Some(_) => {}
            None => self.capture = None,
        }
    }

    /// Add `delta` to a scroll viewport's offset, clamped per axis to
    /// `[0, scroll_range]`, and mark the node `TRANSFORM | HIT_TEST | PAINT` so
    /// the next frame re-derives its subtree's world rects and repaints — a
    /// scroll never dirties MEASURE/LAYOUT and never bubbles to ancestors. A
    /// live-guarded write; a stale handle is a no-op. A delta that does not move
    /// the (already-clamped) offset schedules nothing.
    pub fn scroll_by(&mut self, id: NodeId, delta: Vec2) {
        if !self.arena.is_live(id) {
            return;
        }
        let i = id.index() as usize;
        let range_x = self.scroll_range(id, Axis::Row);
        let range_y = self.scroll_range(id, Axis::Column);
        let prev = self.scroll[i];
        let next = Vec2 {
            x: (prev.x + delta.x).clamp(0.0, range_x),
            y: (prev.y + delta.y).clamp(0.0, range_y),
        };
        if next == prev {
            return;
        }
        self.scroll[i] = next;
        self.mark_dirty(
            id,
            DirtyClass::TRANSFORM | DirtyClass::HIT_TEST | DirtyClass::PAINT,
        );
    }

    /// Set a scroll viewport's offset to an absolute `offset`, clamped per axis
    /// to `[0, scroll_range]`, marking the node `TRANSFORM | HIT_TEST | PAINT`
    /// exactly like [`scroll_by`](Self::scroll_by) — the absolute-value sibling of
    /// that delta setter, sharing its clamp, its dirty classes, and its no-bubble
    /// rule.
    ///
    /// A hot reload uses this to restore a surviving viewport's prior offset:
    /// the container may have re-laid-out (so its range shifted), and clamping to
    /// the *new* range keeps the restored offset in bounds. A live-guarded write;
    /// a stale handle is a no-op, and an offset that resolves to the current
    /// (clamped) value schedules nothing. Cold path (reload / programmatic seek).
    pub fn set_scroll(&mut self, id: NodeId, offset: Vec2) {
        if !self.arena.is_live(id) {
            return;
        }
        let i = id.index() as usize;
        let range_x = self.scroll_range(id, Axis::Row);
        let range_y = self.scroll_range(id, Axis::Column);
        let next = Vec2 {
            x: offset.x.clamp(0.0, range_x),
            y: offset.y.clamp(0.0, range_y),
        };
        if next == self.scroll[i] {
            return;
        }
        self.scroll[i] = next;
        self.mark_dirty(
            id,
            DirtyClass::TRANSFORM | DirtyClass::HIT_TEST | DirtyClass::PAINT,
        );
    }

    /// A node's paint style.
    #[inline]
    pub fn style(&self, id: NodeId) -> BoxStyle {
        self.style[id.index() as usize]
    }

    /// A node's style-token binding, if it has one. `None` means the node's
    /// `style` is a literal untouched by theme swaps.
    #[inline]
    pub fn style_id(&self, id: NodeId) -> Option<StyleId> {
        self.styled[id.index() as usize]
    }

    /// Bind a node's `style` to theme tokens, replacing any prior binding. A
    /// live-guarded write, so a stale handle is a no-op.
    ///
    /// This records the binding and marks the node STYLE + PAINT so the next
    /// frame's [`resolve_styles`](Self::resolve_styles) pass folds the tokens'
    /// current values onto the node's `style` and the paint walk re-emits it.
    /// The caller separately binds each of the [`StyleId`]'s tokens' backing
    /// state cells (via the theme) to this node in the [`BindingTable`], so a
    /// later theme swap re-marks the node through the ordinary flush — this
    /// method only establishes the binding and the first resolve.
    pub fn set_style_token(&mut self, id: NodeId, style: StyleId) {
        if !self.arena.is_live(id) {
            return;
        }
        self.styled[id.index() as usize] = Some(style);
        self.mark_dirty(id, DirtyClass::STYLE | DirtyClass::PAINT);
    }

    /// A node's authored semantics, if any. `None` = a plain layout/decoration
    /// node (the derive pass still gives an interactive node a default role).
    #[inline]
    pub fn semantics(&self, id: NodeId) -> Option<&Semantics> {
        self.semantics[id.index() as usize].as_ref()
    }

    /// Set a node's authored semantics, replacing any prior value, and mark it
    /// SEMANTICS-dirty so the next derive re-folds it. A live-guarded write — a
    /// stale handle is a no-op. SEMANTICS bubbles, so ancestors learn their
    /// subtree changed without a separate mark.
    pub fn set_semantics(&mut self, id: NodeId, semantics: Semantics) {
        if !self.arena.is_live(id) {
            return;
        }
        self.semantics[id.index() as usize] = Some(semantics);
        self.mark_dirty(id, DirtyClass::SEMANTICS);
    }

    /// A node's current pending invalidation set.
    #[inline]
    pub fn dirty(&self, id: NodeId) -> DirtyClass {
        self.dirty[id.index() as usize]
    }

    /// Whether a node participates in hit testing (default `true`).
    #[inline]
    pub fn hittable(&self, id: NodeId) -> bool {
        self.hittable[id.index() as usize]
    }

    /// Set whether a node participates in hit testing. A live-guarded write, so
    /// a stale handle is a no-op rather than an out-of-bounds write.
    ///
    /// Hit testing reads a node's [`world`](Self::world) rect, not `bounds`, so a
    /// scroll offset shifts what a node hits without disturbing its layout. That
    /// world rect is re-derived by [`resolve_transforms`](Self::resolve_transforms)
    /// on the TRANSFORM-dirty subtree; flipping the hittable flag here only gates
    /// participation and needs no recompute of its own.
    pub fn set_hittable(&mut self, id: NodeId, hit: bool) {
        if !self.arena.is_live(id) {
            return;
        }
        self.hittable[id.index() as usize] = hit;
    }

    /// Attach a pointer handler to a node, replacing any prior one. A
    /// live-guarded write, so a stale handle is a no-op.
    pub fn set_pointer_handler(&mut self, id: NodeId, handler: PointerHandler) {
        if !self.arena.is_live(id) {
            return;
        }
        self.handlers[id.index() as usize] = Some(handler);
    }

    /// Whether a live node currently carries a pointer handler.
    #[inline]
    pub fn has_handler(&self, id: NodeId) -> bool {
        self.arena.is_live(id) && self.handlers[id.index() as usize].is_some()
    }

    /// Move a node's handler out of the store so the router can call it while
    /// still lending the store's reactive state to an [`EventCx`] — the boxed
    /// closure and the state stores would otherwise alias the same borrow. The
    /// router pairs each `take` with a [`restore_handler`](Self::restore_handler)
    /// once the call returns. `None` for a stale handle or an empty slot.
    #[inline]
    pub fn take_handler(&mut self, id: NodeId) -> Option<PointerHandler> {
        if !self.arena.is_live(id) {
            return None;
        }
        self.handlers[id.index() as usize].take()
    }

    /// Put a handler moved out by [`take_handler`](Self::take_handler) back in
    /// its slot. A live-guarded write; a stale handle drops the handler.
    #[inline]
    pub fn restore_handler(&mut self, id: NodeId, handler: PointerHandler) {
        if !self.arena.is_live(id) {
            return;
        }
        self.handlers[id.index() as usize] = Some(handler);
    }

    /// The currently focused node, or `None` when nothing holds focus.
    #[inline]
    pub fn focused(&self) -> Option<NodeId> {
        self.focused
    }

    /// Set the focus slot. Focusing `Some(id)` is live-guarded — a stale handle
    /// leaves focus unchanged rather than pointing the slot at a dead slot;
    /// `None` always clears. This only moves the slot: dirtying the old and new
    /// nodes for the focus-ring repaint is the router's job (see the input tier).
    pub fn set_focused(&mut self, id: Option<NodeId>) {
        match id {
            Some(node) if !self.arena.is_live(node) => {}
            other => self.focused = other,
        }
    }

    /// Whether a node can hold focus (default `false`).
    #[inline]
    pub fn focusable(&self, id: NodeId) -> bool {
        self.focusable[id.index() as usize]
    }

    /// Set whether a node can hold focus. A live-guarded write, so a stale handle
    /// is a no-op rather than an out-of-bounds write.
    pub fn set_focusable(&mut self, id: NodeId, focusable: bool) {
        if !self.arena.is_live(id) {
            return;
        }
        self.focusable[id.index() as usize] = focusable;
    }

    /// Attach a keyboard/IME handler to a node, replacing any prior one. A
    /// live-guarded write, so a stale handle is a no-op.
    pub fn set_key_handler(&mut self, id: NodeId, handler: KeyHandler) {
        if !self.arena.is_live(id) {
            return;
        }
        self.key_handlers[id.index() as usize] = Some(handler);
    }

    /// Whether a live node currently carries a key handler.
    #[inline]
    pub fn has_key_handler(&self, id: NodeId) -> bool {
        self.arena.is_live(id) && self.key_handlers[id.index() as usize].is_some()
    }

    /// Move a node's key handler out of the store so the router can call it while
    /// still lending the store's reactive state to an [`EventCx`], mirroring
    /// [`take_handler`](Self::take_handler) for the pointer column. The router
    /// pairs each `take` with a
    /// [`restore_key_handler`](Self::restore_key_handler). `None` for a stale
    /// handle or an empty slot.
    #[inline]
    pub fn take_key_handler(&mut self, id: NodeId) -> Option<KeyHandler> {
        if !self.arena.is_live(id) {
            return None;
        }
        self.key_handlers[id.index() as usize].take()
    }

    /// Put a key handler moved out by
    /// [`take_key_handler`](Self::take_key_handler) back in its slot. A
    /// live-guarded write; a stale handle drops the handler.
    #[inline]
    pub fn restore_key_handler(&mut self, id: NodeId, handler: KeyHandler) {
        if !self.arena.is_live(id) {
            return;
        }
        self.key_handlers[id.index() as usize] = Some(handler);
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

    /// Free every **descendant** of `root`, leaving `root` itself live. Each
    /// freed node has its scoped effects cancelled (their cleanups run — the
    /// unmount hook) before its arena slot is released back to the free list.
    /// `root`'s own links are left as-is: it keeps whatever parent it had and
    /// ends with no children.
    ///
    /// This is the clear half of node recycling: a virtual list empties a mounted
    /// host's body so the host can be re-bound to a different item, without
    /// freeing the host or disturbing the tree above it. Freed slots are reused by
    /// the next [`alloc`](Self::alloc), so churn is bounded — no growth.
    ///
    /// `scratch` is a caller-owned stack reused across calls so a recycle allocates
    /// nothing. Returns the number of nodes freed.
    pub fn free_subtree(
        &mut self,
        root: NodeId,
        effects: &mut EffectStore,
        scratch: &mut Vec<NodeId>,
    ) -> u32 {
        if !self.arena.is_live(root) {
            return 0;
        }
        // Detach the body from `root` up front, then free it: collect the child
        // subtree in pre-order onto the stack (children pushed as we pop), and
        // free on the way. Order within a freed set does not matter for the arena
        // (each free is independent), so a simple stack walk suffices; effect
        // cleanup is per-node and order-independent too.
        let base = scratch.len();
        let mut child = self.arena.links(root).and_then(|l| l.first_child);
        while let Some(c) = child {
            let next = self.arena.links(c).and_then(|l| l.next_sibling);
            scratch.push(c);
            child = next;
        }
        // `root` now conceptually has no children; clear its child endpoints.
        if let Some(links) = self.arena.links_mut(root) {
            links.first_child = None;
            links.last_child = None;
        }

        let mut freed = 0;
        while scratch.len() > base {
            let node = scratch.pop().unwrap();
            // Push this node's children before freeing it (we still can read its
            // links while it is live).
            let mut c = self.arena.links(node).and_then(|l| l.first_child);
            while let Some(cc) = c {
                let next = self.arena.links(cc).and_then(|l| l.next_sibling);
                scratch.push(cc);
                c = next;
            }
            effects.cancel_for_node(node);
            if self.arena.free(node) {
                freed += 1;
            }
        }
        scratch.truncate(base);
        freed
    }

    /// Replace `old` — a child of `parent` — with the already-built `new`, the
    /// atomic "change type → rebuild that subtree" step of a directed structural
    /// patch. `old` and its whole subtree are freed (effect cleanups run, unmount
    /// order), then `new` is appended under `parent`. Returns the number of nodes
    /// freed (0 for a stale handle or a non-child `old`, in which case nothing is
    /// mounted either).
    ///
    /// This composes the existing recycle primitives — [`free_subtree`] empties
    /// `old`'s body, then `old` itself is detached, its effects cancelled, and its
    /// slot released, then [`arena_append_child`] mounts `new` — so it introduces
    /// no new mechanism. It does not preserve `old`'s sibling position: `new` lands
    /// last under `parent`, matching how the reconcile authors a fresh subtree.
    /// The hot-reload engine calls this only where a node's *type* changed (so its
    /// runtime state is intentionally lost); an unchanged-type node is patched in
    /// place and never routed here. Cold path (reload / structural edit).
    ///
    /// [`free_subtree`]: Self::free_subtree
    /// [`arena_append_child`]: Self::arena_append_child
    pub fn replace_child(
        &mut self,
        parent: NodeId,
        old: NodeId,
        new: NodeId,
        effects: &mut EffectStore,
        scratch: &mut Vec<NodeId>,
    ) -> u32 {
        // Guard the whole operation up front: a stale `old`, a stale `parent`, or
        // an `old` that is not actually a child of `parent` must leave the tree
        // untouched (no partial free, no orphaned mount).
        if !self.arena.is_live(old) || !self.arena.is_live(parent) {
            return 0;
        }
        if self.arena.links(old).and_then(|l| l.parent) != Some(parent) {
            return 0;
        }
        // Free `old`'s subtree (descendants + their effect cleanups), then unlink,
        // clean up, and free `old` itself. Order matches `free_subtree`'s own
        // per-node teardown (cancel effects before releasing the slot).
        let mut freed = self.free_subtree(old, effects, scratch);
        self.arena.detach_child(old);
        effects.cancel_for_node(old);
        if self.arena.free(old) {
            freed += 1;
        }
        self.arena.append_child(parent, new);
        freed
    }

    /// Derive every node's `world` rect from its `bounds` and the scroll of its
    /// ancestors, in a pre-order walk from `root`. A node's world rect is its
    /// layout box shifted by the accumulated scroll of all scrolling ancestors;
    /// entering a scroll viewport adds that viewport's own `scroll` to the offset
    /// carried into its subtree. Paint clip and hit testing read this.
    ///
    /// Runs only when the tree carries a TRANSFORM-dirty node — a clean frame
    /// skips it entirely (the caller gates on [`any_dirty`](Self::any_dirty) /
    /// the TRANSFORM class). Mirrors [`paint_tree`](crate::paint::paint_tree)'s
    /// recursive pre-order descent; no allocation, worst case the tree depth.
    pub fn resolve_transforms(&mut self, root: NodeId) {
        self.resolve_transforms_from(root, Vec2::ZERO);
    }

    /// Pre-order recursion carrying `offset`, the summed scroll of `root`'s
    /// scrolling ancestors. Writes `world[root] = bounds[root] − offset`, then
    /// folds `root`'s own scroll into the offset handed to its children.
    fn resolve_transforms_from(&mut self, root: NodeId, offset: Vec2) {
        if !self.arena.is_live(root) {
            return;
        }
        let i = root.index() as usize;
        let b = self.bounds[i];
        self.world[i] = Rect {
            x: b.x - offset.x,
            y: b.y - offset.y,
            w: b.w,
            h: b.h,
        };
        // A scroll viewport shifts its own content: fold its scroll into the
        // offset its subtree sees (nonzero only on scroll nodes, so this is an
        // add of zero for ordinary containers).
        let child_offset = Vec2 {
            x: offset.x + self.scroll[i].x,
            y: offset.y + self.scroll[i].y,
        };
        let mut child = self.arena.links(root).and_then(|l| l.first_child);
        while let Some(c) = child {
            self.resolve_transforms_from(c, child_offset);
            child = self.arena.links(c).and_then(|l| l.next_sibling);
        }
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
        let counters = bindings.counters();
        for &state in changed {
            let statics = bindings.for_state(state);
            for edge in statics {
                self.mark_dirty(edge.node, edge.class);
            }
            counters.record_static_eval(statics.len() as u64);
            applied += statics.len() as u32;

            let dynamics = bindings.dynamic_for_state(state);
            for edge in dynamics {
                self.mark_dirty(edge.node, edge.class);
            }
            counters.record_dynamic_eval(dynamics.len() as u64);
            applied += dynamics.len() as u32;
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
            self.world[i] = Rect {
                x: 0.0,
                y: 0.0,
                w: 0.0,
                h: 0.0,
            };
            self.scroll[i] = Vec2::ZERO;
            self.content[i] = Vec2::ZERO;
            self.row_offset[i] = f32::NAN;
            self.grid_tracks[i] = None;
            self.grid_placement[i] = GridPlacement::default();
            self.dirty[i] = DirtyClass::EMPTY;
            self.layout[i] = input;
            self.style[i] = style;
            self.measured[i] = Measured::default();
            self.hittable[i] = true;
            self.handlers[i] = None;
            self.focusable[i] = false;
            self.key_handlers[i] = None;
            self.styled[i] = None;
            self.semantics[i] = None;
        } else {
            debug_assert_eq!(i, self.bounds.len(), "arena index must stay dense");
            self.bounds.push(Rect {
                x: 0.0,
                y: 0.0,
                w: 0.0,
                h: 0.0,
            });
            self.world.push(Rect {
                x: 0.0,
                y: 0.0,
                w: 0.0,
                h: 0.0,
            });
            self.scroll.push(Vec2::ZERO);
            self.content.push(Vec2::ZERO);
            self.row_offset.push(f32::NAN);
            self.grid_tracks.push(None);
            self.grid_placement.push(GridPlacement::default());
            self.dirty.push(DirtyClass::EMPTY);
            self.layout.push(input);
            self.style.push(style);
            self.measured.push(Measured::default());
            self.hittable.push(true);
            self.handlers.push(None);
            self.focusable.push(false);
            self.key_handlers.push(None);
            self.styled.push(None);
            self.semantics.push(None);
        }
        id
    }

    /// Run the bottom-up measure pass then the top-down layout pass over the
    /// subtree at `root`, placing the root into `surface`. `scratch` is a
    /// reusable child-id buffer owned by the caller so frames allocate nothing.
    ///
    /// Layout writes `bounds` (the unscrolled layout truth); this then derives
    /// the `world` rects from it so paint and hit testing — which read `world` —
    /// are consistent immediately after a layout, without a scroll offset (any
    /// existing scroll is re-applied). A later scroll updates `world` on its own
    /// via [`resolve_transforms`](Self::resolve_transforms) without re-laying out.
    pub fn layout(&mut self, root: NodeId, surface: Rect, scratch: &mut Vec<u32>) {
        scratch.clear();
        layout::measure(self, root.index(), scratch);
        scratch.clear();
        layout::layout(self, root.index(), surface, scratch);
        self.resolve_transforms(root);
    }

    /// Re-resolve the warm `style` of every STYLE-dirty node that carries a
    /// style-token binding, folding the tokens' current theme values onto the
    /// node's style, and report how many nodes were re-resolved (0 when no
    /// bound node is style-dirty).
    ///
    /// This is the incremental STYLE layer: it runs after the state flush has
    /// marked STYLE on nodes bound to a swapped theme token (and the setter
    /// marks STYLE on a freshly bound node), before the paint rebuild. A node
    /// whose STYLE is dirty but has no binding is skipped — its `style` is a
    /// literal. Only tokenized fields (`fill`, `radius`) are overwritten from
    /// the token; untokenized fields stay as the node's existing style, so the
    /// fold is idempotent across frames and needs no separate base column.
    /// Allocation-free; touches only the dirty bound nodes.
    pub fn resolve_styles(&mut self, theme: &Theme, states: &StateStore) -> u32 {
        let mut resolved = 0;
        for index in 0..self.dirty.len() {
            if !self.dirty[index].intersects(DirtyClass::STYLE) {
                continue;
            }
            let Some(style_id) = self.styled[index] else {
                continue;
            };
            self.style[index] = style_id.resolve(self.style[index], theme, states);
            resolved += 1;
        }
        resolved
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

    /// Derive the full accessibility tree from the node model, pre-order from
    /// `root`. Reads authored semantics (the cold column) folded with live state
    /// (the focus slot, handler presence, bounds) — the node model is the single
    /// source, nothing is stored twice. Cold: run on a SEMANTICS-dirty frame,
    /// not every frame. Its allocation is the returned tree (a snapshot,
    /// inherently owned). Empty when `root` is a stale handle.
    pub fn derive_semantics(&self, root: NodeId) -> SemanticsTree {
        let mut tree = SemanticsTree::default();
        if self.arena.is_live(root) {
            self.derive_into(root, &mut tree);
        }
        tree
    }

    /// Push `id`'s derived row, recurse its children in sibling order, and
    /// record their indices on the row. Returns `id`'s own index in `tree.nodes`
    /// so the parent can link it. Pre-order, mirroring the paint walk.
    fn derive_into(&self, id: NodeId, tree: &mut SemanticsTree) -> usize {
        let my_index = tree.nodes.len();
        // Resolve role/label: authored wins; else an interactive node (a pointer
        // or key handler present) defaults to Button so an assistive-technology
        // path always exists; else Group.
        let authored = self.semantics(id);
        let interactive = self.has_handler(id) || self.has_key_handler(id);
        let role = match authored.map(|s| s.role) {
            Some(r) => r,
            None if interactive => Role::Button,
            None => Role::Group,
        };
        let label = authored.and_then(|s| s.label.clone());
        let focused = self.focused == Some(id);
        tree.nodes.push(SemanticsNode {
            id,
            role,
            label,
            focused,
            bounds: self.bounds(id),
            children: Vec::new(),
        });
        let mut child = self.arena.links(id).and_then(|l| l.first_child);
        while let Some(c) = child {
            let ci = self.derive_into(c, tree);
            tree.nodes[my_index].children.push(ci);
            child = self.arena.links(c).and_then(|l| l.next_sibling);
        }
        my_index
    }

    /// Re-derive the accessibility tree only when a SEMANTICS invalidation is
    /// pending, returning the new tree (or `None` when SEMANTICS is clean, so a
    /// frame with no semantic change does no work). The whole tree is rebuilt
    /// this slice — SEMANTICS bubbles to the root, so any change reaches `root`;
    /// per-subtree caching is a later refinement once a consumer needs it.
    pub fn derive_semantics_dirty(&self, root: NodeId) -> Option<SemanticsTree> {
        if !self
            .dirty
            .iter()
            .any(|d| d.intersects(DirtyClass::SEMANTICS))
        {
            return None;
        }
        Some(self.derive_semantics(root))
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

    #[inline]
    fn set_content(&mut self, index: u32, content: Vec2) {
        self.content[index as usize] = content;
    }

    #[inline]
    fn row_offset(&self, index: u32) -> Option<f32> {
        let off = self.row_offset[index as usize];
        // The sentinel `f32::NAN` marks a node that is not a positioned row, so a
        // NaN reads back as "no offset" — an ordinary node is skipped by the
        // `AbsoluteRows` layout arm.
        (!off.is_nan()).then_some(off)
    }

    #[inline]
    fn grid_column_tracks(&self, index: u32) -> Option<&[TrackSizing]> {
        self.grid_tracks[index as usize]
            .as_ref()
            .map(|t| t.columns.as_slice())
    }

    #[inline]
    fn grid_row_tracks(&self, index: u32) -> Option<&[TrackSizing]> {
        self.grid_tracks[index as usize]
            .as_ref()
            .map(|t| t.rows.as_slice())
    }

    #[inline]
    fn grid_placement(&self, index: u32) -> GridPlacement {
        self.grid_placement[index as usize]
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
///
/// It optionally also borrows the reactive stores: a cx built with
/// [`BuildCx::with_reactive`] can allocate state cells and wire state→node
/// bindings, so a user application authors its whole interactive scene here. A
/// cx built with [`BuildCx::new`] has no reactive stores — it declares a pure
/// node tree (the shape most tests want) and its `state`/`bind` methods are
/// unreachable by construction.
pub struct BuildCx<'a> {
    store: &'a mut NodeStore,
    /// Reactive state cells, present only for a `with_reactive` cx. `None` for
    /// a `new`-built (node-only) cx.
    states: Option<&'a mut StateStore>,
    /// Compiled state→node edges, present only for a `with_reactive` cx.
    bindings: Option<&'a mut BindingTable>,
    /// Driver-owned virtual-list registry, present only for a `with_reactive`
    /// cx. A `virtual_list` call registers its per-list state here keyed by the
    /// viewport's node index; `None` for a node-only or child-body cx (neither
    /// declares a top-level virtual list).
    lists: Option<&'a mut crate::virtual_list::VirtualLists>,
    /// Parent cursor stack; the top is the current insertion parent.
    stack: Vec<NodeId>,
    /// The placement to apply to the next child authored inside the current
    /// grid closure, then cleared. `None` outside a grid or once consumed.
    pending_placement: Option<GridPlacement>,
    /// The tree root, set by the first declared node.
    root: Option<NodeId>,
}

impl<'a> BuildCx<'a> {
    /// Start a node-only build against `store`. The store should be empty or
    /// freshly [`NodeStore::clear`]ed. The cx has no reactive stores; calling
    /// `state`/`bind` on it panics (a facade-construction invariant — a user
    /// only ever receives a `with_reactive` cx, through `Application::build`).
    pub fn new(store: &'a mut NodeStore) -> Self {
        BuildCx {
            store,
            states: None,
            bindings: None,
            lists: None,
            stack: Vec::new(),
            pending_placement: None,
            root: None,
        }
    }

    /// Start a build that can also author reactive state and bindings. The
    /// facade builds the user application's scene through this constructor so
    /// `state`/`bind` reach the driver-owned stores.
    pub fn with_reactive(
        store: &'a mut NodeStore,
        states: &'a mut StateStore,
        bindings: &'a mut BindingTable,
        lists: &'a mut crate::virtual_list::VirtualLists,
    ) -> Self {
        BuildCx {
            store,
            states: Some(states),
            bindings: Some(bindings),
            lists: Some(lists),
            stack: Vec::new(),
            pending_placement: None,
            root: None,
        }
    }

    /// Start a reactive build whose declarations attach beneath an existing
    /// `parent` node rather than forming a new root. The parent cursor is seeded
    /// with `parent`, so the first `leaf`/`flex`/… lands as its child. Used by the
    /// virtual-list reconcile to author an item body under a recycled host: the
    /// host is already in the tree, and its body nodes hang off it.
    ///
    /// Unlike [`with_reactive`](Self::with_reactive), this does **not** clear the
    /// store — it appends to a live tree. `root()` stays `None` (nothing new is a
    /// root here).
    pub fn with_parent(
        store: &'a mut NodeStore,
        states: &'a mut StateStore,
        bindings: &'a mut BindingTable,
        parent: NodeId,
    ) -> Self {
        BuildCx {
            store,
            states: Some(states),
            bindings: Some(bindings),
            lists: None,
            stack: vec![parent],
            pending_placement: None,
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

    /// Declare a grid container and its children. The `children` closure runs
    /// with this grid as the active parent; each child lands in a cell (auto-flow
    /// unless a preceding [`BuildCx::place`] pinned it). A child inside a cell
    /// still honors its own `Size`: a fill child stretches to the cell, a fit or
    /// fixed child hugs its content — so a nested `flex` composes inside a cell.
    pub fn grid(&mut self, style: GridStyle, children: impl FnOnce(&mut BuildCx<'_>)) -> Handle {
        let column_count = style.columns.len().max(1) as u16;
        let row_count = style.rows.len() as u16;
        let input = LayoutInput::Grid {
            column_count,
            row_count,
            column_gap: style.column_gap,
            row_gap: style.row_gap,
            padding: style.padding,
            auto_rows: style.auto_rows,
            size: style.size,
        };
        let id = self.push_node(input, style.style);
        self.store.set_grid_tracks(
            id,
            GridTracks {
                columns: style.columns,
                rows: style.rows,
                auto_rows: style.auto_rows,
            },
        );
        self.stack.push(id);
        children(self);
        self.stack.pop();
        // A stray placement not consumed by a child does not leak to a sibling.
        self.pending_placement = None;
        Handle { id }
    }

    /// Declare the placement and span of the next child authored inside a grid.
    /// With no `place` call the next child auto-flows with span 1. Ignored for a
    /// child whose parent is not a grid.
    pub fn place(&mut self, placement: GridPlacement) {
        self.pending_placement = Some(placement);
    }

    /// Declare a scroll viewport and its content. Like [`BuildCx::flex`] the
    /// `children` closure runs with the viewport as the active parent; the
    /// viewport lays out a single content child at its natural extent along
    /// `axis` (so overflow becomes scrollable) and clips to its own box. Author a
    /// single container child to hold the scrollable content.
    pub fn scroll(
        &mut self,
        style: ScrollStyle,
        children: impl FnOnce(&mut BuildCx<'_>),
    ) -> Handle {
        let input = LayoutInput::Scroll {
            axis: style.axis,
            size: style.size,
        };
        let id = self.push_node(input, style.style);
        self.stack.push(id);
        children(self);
        self.stack.pop();
        Handle { id }
    }

    /// Declare a virtualized list of `item_count` logical rows that mounts only a
    /// window of visible rows (plus overscan), never the whole collection — the
    /// answer to a 100k-item list that must not build 100k nodes.
    ///
    /// The list is a [`LayoutInput::Scroll`] viewport whose single content child
    /// is a fixed-extent [`LayoutInput::AbsoluteRows`] **canvas**: the canvas is
    /// `Fixed` on the scroll axis at the whole collection's estimated extent
    /// (`estimated_row * item_count`) and `Fixed` on the cross axis, so the
    /// viewport's scroll range spans every item while no per-item node is mounted.
    /// The canvas being fixed on **both** axes is load-bearing — it is the boundary
    /// where a row's `MEASURE` invalidation stops rising, so a row remount never
    /// forces the ancestors above the list to relayout.
    ///
    /// No rows are mounted here at build time. The per-frame virtual-list reconcile
    /// (run before layout) mounts the first window against the real viewport size,
    /// then recycles a handful of hosts on each scroll-boundary crossing. `item`
    /// authors one row's body given its logical index; it is cold, called only when
    /// a row is (re)mounted. Requires a [`BuildCx::with_reactive`] cx — the driver
    /// owns the list registry (see [`BuildCx::state`]).
    pub fn virtual_list(
        &mut self,
        style: VirtualListStyle,
        item_count: usize,
        item: impl FnMut(usize, &mut BuildCx<'_>) + 'static,
    ) -> Handle {
        // The viewport is an ordinary scroll node: it reuses the whole scroll
        // machinery (range, scroll_by, hit-test narrowing, the scroll router)
        // unchanged.
        let viewport = self.push_node(
            LayoutInput::Scroll {
                axis: style.axis,
                size: style.size,
            },
            style.style,
        );

        // The canvas is the viewport's single content child: fixed on the main
        // axis at the full estimated collection extent (so the scroll range is
        // correct), fixed at zero on the cross axis (a pure MEASURE boundary
        // marker — the real cross extent comes from the viewport at layout time).
        let total = style.estimated_row * item_count as f32;
        let canvas_size = match style.axis {
            Axis::Column => Size {
                width: Length::Fixed(0.0),
                height: Length::Fixed(total),
            },
            Axis::Row => Size {
                width: Length::Fixed(total),
                height: Length::Fixed(0.0),
            },
        };
        let canvas = self.store.alloc(
            LayoutInput::AbsoluteRows {
                axis: style.axis,
                size: canvas_size,
            },
            BoxStyle::NONE,
        );
        self.store.arena.append_child(viewport, canvas);

        // Register the per-list state with the driver-owned registry, keyed by the
        // viewport index. `dirty_data` starts set, so the first reconcile mounts.
        let lists = self
            .lists
            .as_mut()
            .expect("virtual_list() requires a with_reactive BuildCx");
        lists.register(
            viewport,
            Box::new(crate::virtual_list::VirtualListState::new(
                item_count,
                style.estimated_row,
                style.overscan as usize,
                style.axis,
                canvas,
                Box::new(item),
            )),
        );

        Handle { id: viewport }
    }

    /// Declare a leaf node.
    pub fn leaf(&mut self, style: LeafStyle) -> Handle {
        let id = self.push_node(LayoutInput::Leaf { size: style.size }, style.style);
        Handle { id }
    }

    /// Attach a pointer handler to an already-declared node and return the
    /// handle so registration chains inline (`cx.on_pointer(cx.leaf(..), |ev| ..)`).
    /// The mirror of how a binding associates state to a node: this associates a
    /// node to an action. The closure is stored cold and driven only when the
    /// node is on a hit's dispatch chain.
    pub fn on_pointer(
        &mut self,
        handle: Handle,
        handler: impl FnMut(&mut EventCx<'_>) + 'static,
    ) -> Handle {
        self.store.set_pointer_handler(handle.id, Box::new(handler));
        handle
    }

    /// Attach authored semantics (role + label) to an already-declared node, for
    /// an accessible tree. Mirrors `on_pointer`: associates a node with its
    /// accessible facts. Returns the handle so authoring chains inline.
    pub fn semantics(&mut self, handle: Handle, semantics: Semantics) -> Handle {
        self.store.set_semantics(handle.id, semantics);
        handle
    }

    /// Allocate a reactive state cell with an initial value, returning its id.
    /// App-scoped: the app stashes the id and reads/writes it from handlers via
    /// an event context. Cold, build-time — not a hot path. Requires a cx built
    /// with [`BuildCx::with_reactive`]; a node-only cx has no state store and
    /// this panics (a facade-construction invariant — a user only ever receives
    /// a reactive cx, through `Application::build`).
    pub fn state(&mut self, initial: StateValue) -> StateId {
        self.states
            .as_mut()
            .expect("state() requires a with_reactive BuildCx")
            .alloc(initial)
    }

    /// Wire a state cell to a node: when the cell changes, the frame's flush
    /// marks `class` on `node`. This is the compiled static binding edge — the
    /// same one the transaction flush reads. Returns the handle so wiring chains
    /// inline. Requires a [`BuildCx::with_reactive`] cx (see [`BuildCx::state`]).
    pub fn bind(&mut self, state: StateId, node: Handle, class: DirtyClass) -> Handle {
        self.bindings
            .as_mut()
            .expect("bind() requires a with_reactive BuildCx")
            .bind(state, node.id, class);
        node
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
        if let Some(p) = self.pending_placement.take() {
            self.store.set_grid_placement(id, p);
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
    fn build_cx_grid_places_an_explicit_child() {
        use crate::grid::{GridPlacement, GridStyle, TrackSizing};
        let mut store = NodeStore::new();
        let (grid, child) = {
            let mut cx = BuildCx::new(&mut store);
            let mut child_id = None;
            let g = cx.grid(
                GridStyle {
                    columns: vec![TrackSizing::Fixed(50.0), TrackSizing::Fixed(50.0)],
                    rows: vec![TrackSizing::Fixed(50.0)],
                    size: Size::fixed(100.0, 50.0),
                    ..Default::default()
                },
                |cx| {
                    cx.place(GridPlacement {
                        column: Some(1),
                        row: Some(0),
                        column_span: 1,
                        row_span: 1,
                    });
                    child_id = Some(
                        cx.leaf(LeafStyle {
                            size: Size::fill(),
                            ..Default::default()
                        })
                        .id(),
                    );
                },
            );
            (g.id(), child_id.unwrap())
        };
        let mut scratch = Vec::new();
        crate::layout::measure(&mut store, grid.index(), &mut scratch);
        crate::layout::layout(&mut store, grid.index(), surface(100.0, 50.0), &mut scratch);
        // Placed in column 1 → x starts at 50.
        assert_eq!(
            store.bounds(child),
            Rect {
                x: 50.0,
                y: 0.0,
                w: 50.0,
                h: 50.0
            }
        );
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
        assert_eq!(store.world.len(), 2);
        assert_eq!(store.scroll.len(), 2);
        assert_eq!(store.content.len(), 2);
        assert_eq!(store.layout.len(), 2);
        assert_eq!(store.style.len(), 2);
        assert_eq!(store.measured.len(), 2);
        assert_eq!(store.focusable.len(), 2);
        assert_eq!(store.key_handlers.len(), 2);
        assert_eq!(store.semantics.len(), 2);
    }

    #[test]
    fn a_fresh_node_has_no_authored_semantics() {
        let mut store = NodeStore::new();
        let id = {
            let mut cx = BuildCx::new(&mut store);
            cx.leaf(LeafStyle::default()).id()
        };
        assert!(
            store.semantics(id).is_none(),
            "a node carries no authored semantics until set"
        );
    }

    #[test]
    fn set_semantics_round_trips_and_bubbles_semantics_dirty() {
        use crate::semantics::{Role, Semantics};
        let mut store = NodeStore::new();
        // A flex parent with one leaf child; set semantics on the leaf.
        let sink: std::rc::Rc<std::cell::RefCell<Vec<NodeId>>> =
            std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let parent = {
            let capture = std::rc::Rc::clone(&sink);
            let mut cx = BuildCx::new(&mut store);
            cx.flex(FlexStyle::default(), |cx| {
                capture
                    .borrow_mut()
                    .push(cx.leaf(LeafStyle::default()).id());
            })
            .id()
        };
        let leaf = sink.borrow()[0];
        store.clear_dirty();

        store.set_semantics(leaf, Semantics::role(Role::Button).with_label("Add"));

        assert_eq!(
            store.semantics(leaf).map(|s| s.role),
            Some(Role::Button),
            "the authored role round-trips"
        );
        assert_eq!(
            store.semantics(leaf).and_then(|s| s.label.as_deref()),
            Some("Add"),
            "the authored label round-trips"
        );
        assert!(
            store.dirty(leaf).intersects(DirtyClass::SEMANTICS),
            "setting semantics marks the node SEMANTICS-dirty"
        );
        assert!(
            store.dirty(parent).intersects(DirtyClass::SEMANTICS),
            "SEMANTICS bubbles to the parent so a subtree change reaches ancestors"
        );
    }

    #[test]
    fn derive_builds_a_flat_pre_order_tree_with_child_indices() {
        use crate::semantics::Role;
        let mut store = NodeStore::new();
        let sink: std::rc::Rc<std::cell::RefCell<Vec<NodeId>>> =
            std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let root = {
            let capture = std::rc::Rc::clone(&sink);
            let mut cx = BuildCx::new(&mut store);
            cx.flex(FlexStyle::default(), |cx| {
                capture
                    .borrow_mut()
                    .push(cx.leaf(LeafStyle::default()).id());
                capture
                    .borrow_mut()
                    .push(cx.leaf(LeafStyle::default()).id());
            })
            .id()
        };
        let (a, b) = {
            let ids = sink.borrow();
            (ids[0], ids[1])
        };

        let tree = store.derive_semantics(root);
        assert_eq!(tree.len(), 3, "root + two leaves");
        let r = tree.root().unwrap();
        assert_eq!(r.id, root);
        assert_eq!(
            r.children,
            vec![1, 2],
            "children recorded by index, in order"
        );
        assert_eq!(tree.nodes[1].id, a);
        assert_eq!(tree.nodes[2].id, b);
        // A plain leaf with no handler and no authored semantics is a Group.
        assert_eq!(tree.nodes[1].role, Role::Group);
    }

    #[test]
    fn an_interactive_node_defaults_to_button() {
        use crate::semantics::Role;
        let mut store = NodeStore::new();
        let (interactive, plain) = {
            let mut cx = BuildCx::new(&mut store);
            let a = cx.leaf(LeafStyle::default());
            cx.on_pointer(a, |_| {});
            let b = cx.leaf(LeafStyle::default());
            (a.id(), b.id())
        };
        // A single-root store: derive from each leaf directly.
        assert_eq!(
            store.derive_semantics(interactive).root().unwrap().role,
            Role::Button,
            "a node with a pointer handler defaults to Button"
        );
        assert_eq!(
            store.derive_semantics(plain).root().unwrap().role,
            Role::Group,
            "a plain leaf defaults to Group"
        );
    }

    #[test]
    fn authored_semantics_win_over_the_interactive_default() {
        use crate::semantics::{Role, Semantics};
        let mut store = NodeStore::new();
        let leaf = {
            let mut cx = BuildCx::new(&mut store);
            cx.leaf(LeafStyle::default()).id()
        };
        store.set_semantics(leaf, Semantics::role(Role::Label).with_label("Hi"));
        let node = store.derive_semantics(leaf);
        let n = node.root().unwrap();
        assert_eq!(n.role, Role::Label);
        assert_eq!(n.label.as_deref(), Some("Hi"));
    }

    #[test]
    fn focus_is_derived_from_the_focus_slot() {
        let mut store = NodeStore::new();
        let sink: std::rc::Rc<std::cell::RefCell<Vec<NodeId>>> =
            std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let root = {
            let capture = std::rc::Rc::clone(&sink);
            let mut cx = BuildCx::new(&mut store);
            cx.flex(FlexStyle::default(), |cx| {
                capture
                    .borrow_mut()
                    .push(cx.leaf(LeafStyle::default()).id());
                capture
                    .borrow_mut()
                    .push(cx.leaf(LeafStyle::default()).id());
            })
            .id()
        };
        let (a, b) = {
            let ids = sink.borrow();
            (ids[0], ids[1])
        };
        store.set_focused(Some(a));
        let tree = store.derive_semantics(root);
        assert!(tree.get(a).unwrap().focused, "the focused node is marked");
        assert!(!tree.get(b).unwrap().focused, "its sibling is not");
    }

    #[test]
    fn derive_dirty_skips_a_clean_frame_and_fires_after_a_change() {
        use crate::semantics::{Role, Semantics};
        let mut store = NodeStore::new();
        let leaf = {
            let mut cx = BuildCx::new(&mut store);
            cx.leaf(LeafStyle::default()).id()
        };
        store.clear_dirty();
        assert!(
            store.derive_semantics_dirty(leaf).is_none(),
            "a frame with nothing SEMANTICS-dirty re-derives nothing"
        );
        store.set_semantics(leaf, Semantics::role(Role::Button));
        assert!(
            store.derive_semantics_dirty(leaf).is_some(),
            "a semantic change triggers a re-derive"
        );
    }

    #[test]
    fn set_semantics_guards_stale_handles() {
        use crate::semantics::{Role, Semantics};
        let mut store = NodeStore::new();
        let id = {
            let mut cx = BuildCx::new(&mut store);
            cx.leaf(LeafStyle::default()).id()
        };
        store.clear();
        // A stale handle after a rebuild is a no-op, not an out-of-bounds write.
        store.set_semantics(id, Semantics::role(Role::Button));
        // Nothing to observe on the dead handle; the guard simply must not panic.
    }

    #[test]
    fn a_fresh_node_is_not_focusable_and_has_no_key_handler() {
        let mut store = NodeStore::new();
        let id = {
            let mut cx = BuildCx::new(&mut store);
            cx.leaf(LeafStyle::default()).id()
        };
        assert!(!store.focusable(id), "focus is opt-in, default false");
        assert!(!store.has_key_handler(id), "no key handler by default");
        assert_eq!(store.focused(), None, "nothing focused in a fresh store");
    }

    #[test]
    fn set_focusable_round_trips() {
        let mut store = NodeStore::new();
        let id = {
            let mut cx = BuildCx::new(&mut store);
            cx.leaf(LeafStyle::default()).id()
        };
        store.set_focusable(id, true);
        assert!(store.focusable(id));
        store.set_focusable(id, false);
        assert!(!store.focusable(id));
    }

    #[test]
    fn set_focused_round_trips_and_guards_stale_handles() {
        let mut store = NodeStore::new();
        let id = {
            let mut cx = BuildCx::new(&mut store);
            cx.leaf(LeafStyle::default()).id()
        };
        store.set_focused(Some(id));
        assert_eq!(store.focused(), Some(id));
        store.set_focused(None);
        assert_eq!(store.focused(), None);

        // A stale handle (bumped generation on a freed slot) never lands in the
        // slot: clearing the tree frees `id`, so focusing it is a no-op.
        store.set_focused(Some(id));
        store.clear();
        assert_eq!(store.focused(), None, "clear() resets the focus slot");
        store.set_focused(Some(id));
        assert_eq!(store.focused(), None, "focusing a dead handle is a no-op");
    }

    #[test]
    fn clear_resets_focus_slot_and_the_two_columns() {
        let mut store = NodeStore::new();
        let id = {
            let mut cx = BuildCx::new(&mut store);
            cx.leaf(LeafStyle::default()).id()
        };
        store.set_focusable(id, true);
        store.set_key_handler(id, Box::new(|_| {}));
        store.set_focused(Some(id));

        store.clear();
        assert_eq!(store.focused(), None);
        assert_eq!(store.focusable.len(), 0);
        assert_eq!(store.key_handlers.len(), 0);
    }

    #[test]
    fn realloc_after_clear_defaults_focusable_and_key_handler() {
        // Set focus + a handler, rebuild wholesale (this slice frees by
        // clearing), then alloc a fresh node into index 0: it comes back with
        // focus off and no handler, proving the alloc path defaults both columns.
        let mut store = NodeStore::new();
        let first = {
            let mut cx = BuildCx::new(&mut store);
            cx.leaf(LeafStyle::default()).id()
        };
        store.set_focusable(first, true);
        store.set_key_handler(first, Box::new(|_| {}));

        store.clear();
        let reused = {
            let mut cx = BuildCx::new(&mut store);
            cx.leaf(LeafStyle::default()).id()
        };
        assert_eq!(reused.index(), 0, "index 0 is allocated again");
        assert!(
            !store.focusable(reused),
            "alloc defaults focusable to false"
        );
        assert!(
            !store.has_key_handler(reused),
            "alloc leaves the key slot empty"
        );
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

    #[test]
    fn on_pointer_registers_cold_handler_and_take_restore_round_trips() {
        use crate::binding::BindingTable;
        use crate::state::{StateStore, StateValue};

        let mut store = NodeStore::new();
        let mut states = StateStore::new();
        let count = states.alloc(StateValue::Int(0));
        let bindings = BindingTable::new();

        let leaf;
        {
            let mut cx = BuildCx::new(&mut store);
            let h = cx.leaf(LeafStyle::default());
            leaf = cx
                .on_pointer(h, move |ev| {
                    let now = match ev.get(count) {
                        Some(StateValue::Int(n)) => n,
                        _ => 0,
                    };
                    ev.set(count, StateValue::Int(now + 1));
                })
                .id();
        }
        assert!(store.has_handler(leaf));

        // The router discipline: move the handler out, drive it with an EventCx
        // borrowing the state stores (no alias with the store), then restore.
        let mut handler = store.take_handler(leaf).expect("handler present");
        assert!(!store.has_handler(leaf), "slot empty while borrowed");
        {
            let mut ev = EventCx::__new(&mut states, &bindings);
            handler(&mut ev);
        }
        store.restore_handler(leaf, handler);
        assert!(store.has_handler(leaf), "handler restored");
        assert_eq!(states.get(count), Some(StateValue::Int(1)));
    }

    #[test]
    fn set_style_token_records_binding_and_marks_style_paint() {
        use crate::state::{StateStore, StateValue};
        use crate::style::StyleId;
        use crate::token::{Theme, TokenInterner, TokenNamespace};

        let mut store = NodeStore::new();
        let mut states = StateStore::new();
        let mut interner = TokenInterner::new();
        let mut theme = Theme::new();
        let bg = interner.intern(TokenNamespace::Color, "bg");
        let cell = states.alloc(StateValue::Color(0.2, 0.4, 0.6, 1.0));
        theme.define(bg, cell);

        let leaf = {
            let mut cx = BuildCx::new(&mut store);
            cx.leaf(LeafStyle::default()).id()
        };
        assert_eq!(store.style_id(leaf), None, "no binding until set");
        store.clear_dirty();

        store.set_style_token(leaf, StyleId::fill(bg));
        assert_eq!(store.style_id(leaf), Some(StyleId::fill(bg)));
        assert!(
            store
                .dirty(leaf)
                .intersects(DirtyClass::STYLE | DirtyClass::PAINT),
            "binding a token marks the node style + paint"
        );

        // The resolve pass folds the token's value onto the node's style.
        let count = store.resolve_styles(&theme, &states);
        assert_eq!(count, 1, "one bound style-dirty node re-resolved");
        assert_eq!(
            store.style(leaf).fill,
            Rgba {
                r: 0.2,
                g: 0.4,
                b: 0.6,
                a: 1.0
            }
        );
    }

    #[test]
    fn resolve_styles_skips_unbound_and_clean_nodes() {
        use crate::state::{StateStore, StateValue};
        use crate::style::StyleId;
        use crate::token::{Theme, TokenInterner, TokenNamespace};

        let mut store = NodeStore::new();
        let mut states = StateStore::new();
        let mut interner = TokenInterner::new();
        let mut theme = Theme::new();
        let bg = interner.intern(TokenNamespace::Color, "bg");
        let cell = states.alloc(StateValue::Color(0.1, 0.1, 0.1, 1.0));
        theme.define(bg, cell);

        let (bound, literal) = {
            let mut cx = BuildCx::new(&mut store);
            let a = cx
                .leaf(LeafStyle {
                    size: Size::fixed(1.0, 1.0),
                    style: BoxStyle::solid(RED),
                })
                .id();
            let b = cx
                .leaf(LeafStyle {
                    size: Size::fixed(1.0, 1.0),
                    style: BoxStyle::solid(GREEN),
                })
                .id();
            (a, b)
        };
        store.set_style_token(bound, StyleId::fill(bg));
        // A literal-styled node is style-dirty but carries no binding: skipped.
        store.mark_dirty(literal, DirtyClass::STYLE);

        let count = store.resolve_styles(&theme, &states);
        assert_eq!(count, 1, "only the bound node re-resolves");
        assert_eq!(
            store.style(bound).fill,
            Rgba {
                r: 0.1,
                g: 0.1,
                b: 0.1,
                a: 1.0
            }
        );
        assert_eq!(store.style(literal).fill, GREEN, "literal style untouched");

        // A clean frame (no STYLE dirt) resolves nothing.
        store.clear_dirty();
        assert_eq!(store.resolve_styles(&theme, &states), 0);
    }

    #[test]
    fn a_theme_swap_reresolves_the_bound_node() {
        use crate::binding::BindingTable;
        use crate::state::{StateStore, StateValue};
        use crate::style::StyleId;
        use crate::token::{Theme, TokenInterner, TokenNamespace};

        let mut store = NodeStore::new();
        let mut states = StateStore::new();
        let mut interner = TokenInterner::new();
        let mut theme = Theme::new();
        let bg = interner.intern(TokenNamespace::Color, "bg");
        let cell = states.alloc(StateValue::Color(0.0, 0.0, 0.0, 1.0));
        theme.define(bg, cell);

        let leaf = {
            let mut cx = BuildCx::new(&mut store);
            cx.leaf(LeafStyle::default()).id()
        };
        store.set_style_token(leaf, StyleId::fill(bg));

        // Bind the token's backing cell to the node so a swap re-marks it, then
        // do the initial resolve and clear the frame's dirt.
        let mut bindings = BindingTable::new();
        bindings.bind(cell, leaf, DirtyClass::STYLE | DirtyClass::PAINT);
        store.resolve_styles(&theme, &states);
        store.clear_dirty();

        // A theme swap is a state write on the token's cell.
        states.set(cell, StateValue::Color(1.0, 1.0, 1.0, 1.0));
        let mut pending = Vec::new();
        states.take_pending(&mut pending);
        store.flush_state_transactions(&pending, &bindings);
        assert!(
            store.dirty(leaf).intersects(DirtyClass::STYLE),
            "the swap re-marked the bound node style"
        );

        let count = store.resolve_styles(&theme, &states);
        assert_eq!(count, 1);
        assert_eq!(
            store.style(leaf).fill,
            Rgba {
                r: 1.0,
                g: 1.0,
                b: 1.0,
                a: 1.0
            }
        );
    }

    #[test]
    fn build_cx_semantics_attaches_authored_facts_and_derives() {
        let mut store = NodeStore::new();
        let sink: std::rc::Rc<std::cell::RefCell<Vec<NodeId>>> =
            std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let root;
        {
            let capture = std::rc::Rc::clone(&sink);
            let mut cx = BuildCx::new(&mut store);
            root = cx
                .flex(FlexStyle::default(), |cx| {
                    let h = cx.leaf(LeafStyle::default());
                    let h = cx.semantics(h, Semantics::role(Role::Button).with_label("Add"));
                    capture.borrow_mut().push(h.id());
                })
                .id();
        }
        let leaf = sink.borrow()[0];

        // The builder wrote the authored column.
        let authored = store.semantics(leaf).expect("authored semantics present");
        assert_eq!(authored.role, Role::Button);
        assert_eq!(authored.label.as_deref(), Some("Add"));

        // And it flows through the derive pass unchanged.
        let tree = store.derive_semantics(root);
        let node = tree.get(leaf).expect("leaf in derived tree");
        assert_eq!(node.role, Role::Button);
        assert_eq!(node.label.as_deref(), Some("Add"));
    }

    #[test]
    fn build_cx_state_allocates_and_reads() {
        let mut store = NodeStore::new();
        let mut states = StateStore::new();
        let mut bindings = BindingTable::new();
        let mut lists = crate::virtual_list::VirtualLists::new();
        let mut cx = BuildCx::with_reactive(&mut store, &mut states, &mut bindings, &mut lists);
        let id = cx.state(StateValue::Int(0));
        assert_eq!(states.get(id), Some(StateValue::Int(0)));
    }

    #[test]
    fn build_cx_bind_wires_a_static_edge() {
        let mut store = NodeStore::new();
        let mut states = StateStore::new();
        let mut bindings = BindingTable::new();
        let mut lists = crate::virtual_list::VirtualLists::new();
        let node;
        let count;
        {
            let mut cx = BuildCx::with_reactive(&mut store, &mut states, &mut bindings, &mut lists);
            count = cx.state(StateValue::Int(0));
            node = cx.leaf(LeafStyle::default());
            let returned = cx.bind(count, node, DirtyClass::PAINT);
            assert_eq!(returned.id(), node.id(), "bind returns the same handle");
        }
        // The bound edge is the one `flush_state_transactions` reads.
        assert!(states.set(count, StateValue::Int(1)));
        let mut changed = Vec::new();
        states.take_pending(&mut changed);
        let applied = store.flush_state_transactions(&changed, &bindings);
        assert_eq!(applied, 1, "the single bound edge applies");
        assert!(store.dirty(node.id()).contains(DirtyClass::PAINT));
        // The flush counted its work on the static path, not the dynamic one.
        let counters = bindings.counters();
        assert_eq!(counters.static_binding_eval(), 1, "one static edge walked");
        assert_eq!(counters.dynamic_binding_eval(), 0, "no dynamic edge walked");
    }

    #[test]
    fn flush_counts_static_and_dynamic_paths_apart() {
        let mut store = NodeStore::new();
        let mut states = StateStore::new();
        let mut bindings = BindingTable::new();
        let mut lists = crate::virtual_list::VirtualLists::new();
        let s;
        let a;
        let b;
        {
            let mut cx = BuildCx::with_reactive(&mut store, &mut states, &mut bindings, &mut lists);
            s = cx.state(StateValue::Int(0));
            a = cx.leaf(LeafStyle::default());
            b = cx.leaf(LeafStyle::default());
            cx.bind(s, a, DirtyClass::PAINT);
        }
        // A dynamic edge on the same state is the explicit fallback path.
        bindings.bind_dynamic(s, b.id(), DirtyClass::LAYOUT);

        assert!(states.set(s, StateValue::Int(1)));
        let mut changed = Vec::new();
        states.take_pending(&mut changed);
        let applied = store.flush_state_transactions(&changed, &bindings);
        assert_eq!(applied, 2, "one static + one dynamic edge apply");
        assert!(store.dirty(a.id()).contains(DirtyClass::PAINT));
        assert!(store.dirty(b.id()).contains(DirtyClass::LAYOUT));

        let counters = bindings.counters();
        assert_eq!(counters.static_binding_eval(), 1, "one static edge walked");
        assert_eq!(
            counters.dynamic_binding_eval(),
            1,
            "one dynamic edge walked"
        );
        assert_eq!(counters.dynamic_subscribe(), 1, "one dynamic subscription");
        assert_eq!(counters.dynamic_fallback_nodes(), 1, "one fallback node");
    }

    #[test]
    #[should_panic]
    fn build_cx_new_has_no_reactive_stores() {
        // A `new`-built cx has no reactive stores; `state()` is a facade-
        // construction invariant violation (a user only ever gets a
        // `with_reactive` cx through `build`), documented and asserted here.
        let mut store = NodeStore::new();
        let mut cx = BuildCx::new(&mut store);
        let _ = cx.state(StateValue::Int(0));
    }

    // ---- scroll ----

    /// A 100×100 vertical viewport over a 100×300 content child (200px range),
    /// laid out and transform-resolved at the origin.
    fn scroll_scene() -> (NodeStore, NodeId, NodeId) {
        let mut store = NodeStore::new();
        let mut content_id = None;
        let root = {
            let mut cx = BuildCx::new(&mut store);
            cx.scroll(
                ScrollStyle {
                    axis: Axis::Column,
                    size: Size::fixed(100.0, 100.0),
                    ..Default::default()
                },
                |cx| {
                    content_id = Some(
                        cx.leaf(LeafStyle {
                            size: Size::fixed(100.0, 300.0),
                            ..Default::default()
                        })
                        .id(),
                    );
                },
            );
            cx.root().unwrap()
        };
        let mut scratch = Vec::new();
        store.layout(root, surface(100.0, 100.0), &mut scratch);
        (store, root, content_id.unwrap())
    }

    #[test]
    fn scroll_range_is_content_minus_viewport() {
        let (store, viewport, _content) = scroll_scene();
        assert_eq!(store.scroll_range(viewport, Axis::Column), 200.0);
        // No horizontal overflow: content and viewport are both 100 wide.
        assert_eq!(store.scroll_range(viewport, Axis::Row), 0.0);
    }

    #[test]
    fn scroll_by_clamps_and_a_stale_delta_is_a_noop() {
        let (mut store, viewport, _content) = scroll_scene();
        store.clear_dirty();
        store.scroll_by(viewport, Vec2 { x: 0.0, y: 250.0 });
        assert_eq!(store.scroll(viewport), Vec2 { x: 0.0, y: 200.0 }, "clamped");
        assert!(store.dirty(viewport).contains(DirtyClass::TRANSFORM));

        // Already at the clamp: another downward delta moves nothing and
        // schedules no work.
        store.clear_dirty();
        store.scroll_by(viewport, Vec2 { x: 0.0, y: 50.0 });
        assert_eq!(store.scroll(viewport), Vec2 { x: 0.0, y: 200.0 });
        assert!(store.dirty(viewport).is_empty(), "no-op schedules nothing");
    }

    #[test]
    fn resolve_transforms_shifts_content_by_scroll() {
        let (mut store, viewport, content) = scroll_scene();
        // Before scrolling, world equals bounds everywhere.
        assert_eq!(store.world(content), store.bounds(content));

        store.scroll_by(viewport, Vec2 { x: 0.0, y: 40.0 });
        store.resolve_transforms(viewport);

        // The viewport itself does not move; its content shifts up by the offset.
        assert_eq!(store.world(viewport), store.bounds(viewport));
        let b = store.bounds(content);
        assert_eq!(
            store.world(content),
            Rect {
                x: b.x,
                y: b.y - 40.0,
                w: b.w,
                h: b.h,
            }
        );
    }

    #[test]
    fn set_scroll_clamps_absolute_and_a_repeat_is_a_noop() {
        let (mut store, viewport, _content) = scroll_scene();
        store.clear_dirty();

        // An absolute offset past the range clamps to the max, like `scroll_by`.
        store.set_scroll(viewport, Vec2 { x: 20.0, y: 250.0 });
        assert_eq!(
            store.scroll(viewport),
            Vec2 { x: 0.0, y: 200.0 },
            "x clamps to 0 (no horizontal range), y clamps to 200"
        );
        assert!(store.dirty(viewport).contains(DirtyClass::TRANSFORM));
        assert!(store.dirty(viewport).contains(DirtyClass::HIT_TEST));
        assert!(store.dirty(viewport).contains(DirtyClass::PAINT));

        // Setting the same (clamped) offset again moves nothing and schedules
        // no work — the reload restore of an unchanged offset is free.
        store.clear_dirty();
        store.set_scroll(viewport, Vec2 { x: 0.0, y: 200.0 });
        assert!(store.dirty(viewport).is_empty(), "no-op schedules nothing");

        // A lower absolute offset moves the viewport back up and re-dirties it.
        store.set_scroll(viewport, Vec2 { x: 0.0, y: 50.0 });
        assert_eq!(store.scroll(viewport), Vec2 { x: 0.0, y: 50.0 });
        assert!(store.dirty(viewport).contains(DirtyClass::TRANSFORM));
    }

    // ---- replace_child ----

    #[test]
    fn replace_child_frees_old_subtree_and_mounts_new() {
        let mut store = NodeStore::new();
        let sink: std::rc::Rc<std::cell::RefCell<Vec<NodeId>>> =
            std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        // parent { old { grandchild } }
        let parent = {
            let capture = std::rc::Rc::clone(&sink);
            let mut cx = BuildCx::new(&mut store);
            cx.flex(FlexStyle::default(), |cx| {
                cx.flex(FlexStyle::default(), |cx| {
                    capture
                        .borrow_mut()
                        .push(cx.leaf(LeafStyle::default()).id());
                });
            })
            .id()
        };
        let old = store
            .arena()
            .links(parent)
            .and_then(|l| l.first_child)
            .unwrap();
        let grandchild = sink.borrow()[0];

        // A freshly built replacement, parked as an orphan until it is mounted.
        let new = {
            let mut cx = BuildCx::new(&mut store);
            cx.leaf(LeafStyle::default()).id()
        };

        let mut effects = EffectStore::new();
        let mut scratch = Vec::new();
        let freed = store.replace_child(parent, old, new, &mut effects, &mut scratch);

        assert_eq!(freed, 2, "old and its grandchild are freed");
        assert!(!store.arena().is_live(old), "old is gone");
        assert!(!store.arena().is_live(grandchild), "old's subtree is gone");
        assert!(store.arena().is_live(new), "the replacement stays live");
        assert_eq!(
            store.arena().links(parent).and_then(|l| l.first_child),
            Some(new),
            "new is mounted under parent",
        );
    }

    #[test]
    fn replace_child_rejects_a_non_child_and_touches_nothing() {
        let mut store = NodeStore::new();
        // Two unrelated roots; `stray` is not a child of `parent`.
        let parent = {
            let mut cx = BuildCx::new(&mut store);
            cx.flex(FlexStyle::default(), |_| {}).id()
        };
        let stray = {
            let mut cx = BuildCx::new(&mut store);
            cx.leaf(LeafStyle::default()).id()
        };
        let new = {
            let mut cx = BuildCx::new(&mut store);
            cx.leaf(LeafStyle::default()).id()
        };

        let mut effects = EffectStore::new();
        let mut scratch = Vec::new();
        let freed = store.replace_child(parent, stray, new, &mut effects, &mut scratch);

        assert_eq!(freed, 0, "a non-child old frees nothing");
        assert!(
            store.arena().is_live(stray),
            "the wrongly-named node survives"
        );
        assert!(store.arena().is_live(new), "the replacement is not mounted");
        assert!(
            store
                .arena()
                .links(parent)
                .and_then(|l| l.first_child)
                .is_none(),
            "parent is left untouched",
        );
    }
}
