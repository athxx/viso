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
use crate::layout::{self, Align, Axis, Inset, LayoutInput, LayoutTree, Length, Measured, Size};
use crate::node::{NodeArena, NodeId};
use crate::semantics::Semantics;
use crate::state::{StateId, StateStore};
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
        self.dirty.clear();
        self.layout.clear();
        self.style.clear();
        self.measured.clear();
        self.hittable.clear();
        self.handlers.clear();
        self.focused = None;
        self.focusable.clear();
        self.key_handlers.clear();
        self.styled.clear();
        self.semantics.clear();
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
    /// Because `bounds` is currently the hittable rect, a MEASURE/LAYOUT change
    /// that re-places a node already updates what it will hit — no extra
    /// recompute pass is needed here. When scroll/clip/transform containers
    /// arrive, a distinct world rect (recomputed on the HIT_TEST/TRANSFORM-dirty
    /// subtree) will diverge from layout `bounds`; that column is deferred until
    /// then rather than added speculatively now.
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
    pub fn layout(&mut self, root: NodeId, surface: Rect, scratch: &mut Vec<u32>) {
        scratch.clear();
        layout::measure(self, root.index(), scratch);
        scratch.clear();
        layout::layout(self, root.index(), surface, scratch);
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
}
