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

use crate::layout::OnAxis;
use core::time::Duration;
use std::any::Any;
use std::rc::Rc;
use std::sync::Arc;

use crate::animation::{
    AnimationEnd, AnimationEndHandler, AnimationId, EndedAnimation, Keyframes, LookTransition,
    LookValue, NodeAnimation, Parts, ReducedMotion, Run, Timing, TrackRun, TranslateAnim,
};
use crate::asset::{
    Asset, AssetDraw, AssetFit, AssetOrigin, AssetPayload, AssetSource, AssetTable,
};
use crate::binding::{BindingTable, Template};
use crate::check_mark::CheckMark;
use crate::content::{BrushImage, Content, TextRequest};
use crate::context::EventCx;
use crate::dirty::{DirtyCause, DirtyClass, DirtyMark};
use crate::grid::{GridPlacement, GridStyle, GridTracks, TrackSizing};
use crate::input::{PointerContacts, PointerId};
use crate::layout::{
    self, AbsoluteInsets, Align, Alignment2D, Axis, BoxRules, Inset, Justify, LayoutInput,
    LayoutTree, Length, Measured, Placement, Sizing, StackPlacement, Vec2,
};
use crate::node::{NodeArena, NodeId, NodeIndex};
use crate::reactive::{ComputeCx, EffectStore, SemanticProjector};
use crate::semantics::{Role, SemanticState, Semantics, SemanticsNode, SemanticsTree};
use crate::shader::ShaderSlots;
use crate::state::{StateId, StateKey, StateStore, StateValue};
use crate::style::{BoxStyle, InteractionStyle, StyleId};
use crate::task::{Continuation, TaskFuture, TaskId, TaskOps};
use crate::text_style::WidthUse;
use crate::timer::TimerRequest;
use crate::token::Theme;
use crate::window::{ChromeContext, WindowOpenRequest};
use viso_render::asset::{ResourceId, ResourceRevision};
use viso_render::scene3d::SceneModel;
use viso_render::{
    Affine2, Border, BrushProgram, PathCmd, Rect, Rgba, SceneSlot, Stroke, TextureId,
};
use viso_runtime::{LoopWaker, TaskSet};

/// The application entry into the tree: a component declares its children into
/// the [`BuildCx`]. `build` takes `&self`: a component holds no reactive state
/// itself; its cells live in the build context (`cx.state`).
pub trait Component {
    /// Declare this component's subtree into the build context.
    fn build(&self, cx: &mut BuildCx<'_>);
}

/// A typed handle onto a built node. It carries only the [`NodeId`].
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

    /// The handle of node `id`, declared already.
    #[inline]
    pub(crate) fn of(id: NodeId) -> Handle {
        Handle { id }
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
    /// Main-axis placement of the packed child group. Only has a visible effect
    /// when no child is `Fill` (a Fill child consumes the main slack, leaving
    /// nothing to distribute).
    pub justify: Justify,
    /// The container's own size request within its parent.
    pub size: Sizing,
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
            justify: Justify::Start,
            size: Sizing::fill(),
            style: BoxStyle::NONE,
        }
    }
}

/// Parameters for a stack declared via [`BuildCx::stack`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StackStyle {
    /// Where a child sits in the content box unless it aligns itself.
    pub align: Alignment2D,
    /// Inner padding on all four edges.
    pub padding: Inset,
    /// The container's own size request within its parent.
    pub size: Sizing,
    /// The container's own background/border.
    pub style: BoxStyle,
}

impl Default for StackStyle {
    fn default() -> Self {
        StackStyle {
            align: Alignment2D::TopStart,
            padding: Inset::default(),
            size: Sizing {
                width: Length::Fit,
                height: Length::Fit,
            },
            style: BoxStyle::NONE,
        }
    }
}

/// Parameters for an absolute container declared via [`BuildCx::absolute`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AbsoluteStyle {
    /// Inner padding on all four edges.
    pub padding: Inset,
    /// The container's own size request within its parent.
    pub size: Sizing,
    /// The container's own background/border.
    pub style: BoxStyle,
}

impl Default for AbsoluteStyle {
    fn default() -> Self {
        AbsoluteStyle {
            padding: Inset::default(),
            size: Sizing::fill(),
            style: BoxStyle::NONE,
        }
    }
}

/// Parameters for a leaf declared via [`BuildCx::leaf`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LeafStyle {
    /// Sizing request within the parent.
    pub size: Sizing,
    /// Fill/border/radius.
    pub style: BoxStyle,
}

impl Default for LeafStyle {
    fn default() -> Self {
        LeafStyle {
            size: Sizing::fixed(0.0, 0.0),
            style: BoxStyle::NONE,
        }
    }
}

/// Parameters for a scroll viewport declared via [`BuildCx::scroll`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScrollStyle {
    /// The axis the viewport scrolls along.
    pub axis: Axis,
    /// Whether it scrolls across its axis too: its content then takes its
    /// natural extent both ways, and it has a bar for each.
    pub across: bool,
    /// The viewport's own size request within its parent (its visible box).
    pub size: Sizing,
    /// The viewport's own background/border (transparent = pure clip box).
    pub style: BoxStyle,
    /// Whether it shows scroll bars.
    pub bars: crate::scroll_bar::ScrollBars,
    /// Whether it opens at the end of its content and follows the end as
    /// the content grows, while it stands there (a log, a chat).
    pub tail: bool,
    /// Whether a column refreshes when pulled down past its start (see
    /// [`refresh`](crate::refresh)).
    pub refresh: bool,
    /// How it comes to rest on its content's children, if it does (see
    /// [`snap`](crate::snap)).
    pub snap: Option<crate::snap::ScrollSnap>,
    /// The page dots over its end edge across, a snapping viewport's (see
    /// [`page_dots`](crate::page_dots)).
    pub dots: Option<crate::page_dots::PageDotsStyle>,
}

impl Default for ScrollStyle {
    fn default() -> Self {
        ScrollStyle {
            axis: Axis::Column,
            across: false,
            size: Sizing::fill(),
            style: BoxStyle::NONE,
            bars: crate::scroll_bar::ScrollBars::Auto,
            tail: false,
            refresh: false,
            snap: None,
            dots: None,
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
    /// Whether it scrolls across its axis too, its rows as long across as
    /// [`NodeStore::set_list_across`] says (a table's columns).
    pub across: bool,
    /// The viewport's own size request within its parent (its visible box).
    pub size: Sizing,
    /// Extra rows mounted on each side of the visible window, so a small scroll
    /// reveals an already-built row rather than stalling on a mount.
    pub overscan: u32,
    /// The initial per-row main extent used to size the canvas and seed the
    /// height model before any row is measured.
    pub estimated_row: f32,
    /// The viewport's own background/border (transparent = pure clip box).
    pub style: BoxStyle,
    /// Whether it shows scroll bars.
    pub bars: crate::scroll_bar::ScrollBars,
    /// Whether it opens at its last row and follows the end as rows are
    /// added, while it stands there (a log, a chat).
    pub tail: bool,
    /// Whether a column refreshes when pulled down past its start (see
    /// [`refresh`](crate::refresh)).
    pub refresh: bool,
    /// How it comes to rest on its rows, if it does (see
    /// [`snap`](crate::snap)).
    pub snap: Option<crate::snap::ScrollSnap>,
    /// Whether each row is exactly as long as the viewport: a pager's pages.
    pub pages: bool,
    /// The page dots over its end edge across, a snapping list's.
    pub dots: Option<crate::page_dots::PageDotsStyle>,
    /// How it lays its items in lanes across its axis, if it does: a grid
    /// or a masonry (see [`virtual_grid`](crate::virtual_grid)).
    pub grid: Option<crate::virtual_grid::ListGrid>,
}

impl Default for VirtualListStyle {
    fn default() -> Self {
        VirtualListStyle {
            axis: Axis::Column,
            across: false,
            size: Sizing::fill(),
            overscan: 4,
            estimated_row: crate::virtual_list::DEFAULT_ROW_HEIGHT,
            style: BoxStyle::NONE,
            bars: crate::scroll_bar::ScrollBars::Auto,
            tail: false,
            refresh: false,
            snap: None,
            pages: false,
            dots: None,
            grid: None,
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

/// Applies one method call to every index-aligned column of [`NodeStore`] in
/// lockstep, so a column added to the store is added in one place.
macro_rules! node_columns {
    ($store:ident . $op:ident ( $($arg:expr),* )) => {
        $store.bounds.$op($($arg),*);
        $store.reach.$op($($arg),*);
        $store.rules_at.$op($($arg),*);
        $store.transform_root.$op($($arg),*);
        $store.scroll.$op($($arg),*);
        $store.translate.$op($($arg),*);
        $store.content.$op($($arg),*);
        $store.row_offset.$op($($arg),*);
        $store.grid_tracks.$op($($arg),*);
        $store.placed_at.$op($($arg),*);
        $store.dirty.$op($($arg),*);
        $store.layout.$op($($arg),*);
        $store.style.$op($($arg),*);
        $store.measured.$op($($arg),*);
        $store.hittable.$op($($arg),*);
        $store.hidden.$op($($arg),*);
        $store.clips.$op($($arg),*);
        $store.overlay.$op($($arg),*);
        $store.chrome.$op($($arg),*);
        $store.opacity.$op($($arg),*);
        $store.handlers.$op($($arg),*);
        $store.focusable.$op($($arg),*);
        $store.disabled.$op($($arg),*);
        $store.key_handlers.$op($($arg),*);
        $store.styled.$op($($arg),*);
        $store.interaction.$op($($arg),*);
        $store.semantics.$op($($arg),*);
        $store.semantic_state.$op($($arg),*);
        $store.content_payload.$op($($arg),*);
        $store.check_marks.$op($($arg),*);
        $store.text_request.$op($($arg),*);
        $store.owner.$op($($arg),*);
    };
}

/// A node with no [`BoxRules`].
const NO_RULES: u32 = u32::MAX;

/// A node with no [`Placement`].
const NO_PLACEMENT: u32 = u32::MAX;

/// The placement of `placement`'s kind that says nothing.
fn unplaced(placement: Placement) -> Placement {
    match placement {
        Placement::Grid(_) => Placement::Grid(GridPlacement::default()),
        Placement::Stack(_) => Placement::Stack(StackPlacement::default()),
        Placement::Absolute(_) => Placement::Absolute(AbsoluteInsets::NONE),
    }
}

/// Whether `part` adds nothing to a reach `own` already covers.
fn within(part: Rect, own: Rect) -> bool {
    part.is_empty()
        || (part.x >= own.x
            && part.y >= own.y
            && part.max_x() <= own.max_x()
            && part.max_y() <= own.max_y())
}

/// A node no template instance built.
pub(crate) const NO_OWNER: u32 = u32::MAX;
/// Tags an owner whose node sits in the instance's block.
pub(crate) const OWNER_KEPT: u32 = 1 << 31;

/// An open instance scope's node cursor.
#[derive(Debug, Clone, Copy)]
struct NodeScope {
    owner: u32,
    /// The next slot of the scope's block; `None` for a scope without one.
    next: Option<u32>,
    base: u32,
    end: u32,
    /// Where the scope's ordinary nodes begin in `scope_log`.
    log: u32,
}

/// Slots a growing store adds at once when it runs out: every column grows to
/// the same capacity in the same step, so a burst of creations reallocates
/// each column O(log n) times, together.
const GROW_MIN: usize = 64;

/// A scroll a [`NodeStore::request_scroll`] waits for the next layout to
/// apply.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ScrollRequest {
    /// Set the viewport's offset.
    To(Vec2),
    /// Move the viewport's offset.
    By(Vec2),
    /// Scroll the viewports enclosing the node the least that shows it.
    Reveal,
}

/// How a programmatic scroll moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ScrollBehavior {
    /// At once.
    #[default]
    Instant,
    /// Gliding there (see [`NodeStore::glide_to`]).
    Smooth,
}

/// The nodes an asset node shows in place of its picture.
#[derive(Debug, Clone, Copy)]
struct AssetFallbacks {
    node: NodeId,
    loading: Option<NodeId>,
    failed: Option<NodeId>,
}

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
    /// Hot: where a node's subtree is seen and hit — its box joined with each
    /// shown, in-place child's reach as that child's translate places it — in
    /// layout space before the node's own translate. A scroll viewport, which
    /// clips its content, and a hidden node reach only their box; an overlay
    /// child paints in the top layer and is left out. Kept by layout, by
    /// translates and by overlay flips; hit testing prunes on it and a fade
    /// composites over it.
    reach: Vec<Rect>,
    /// Hot: each node's slot in `box_rules`, [`NO_RULES`] for a node with
    /// none — almost every node, so layout reads one word to know.
    rules_at: Vec<u32>,
    /// Cold: the [`BoxRules`] of the nodes that have them, each with the
    /// parent content box it was last laid in; `free_rules` lists the slots a
    /// dropped one left.
    box_rules: Vec<(BoxRules, Vec2)>,
    free_rules: Vec<u32>,
    /// How many nodes have rules: while none does, layout reads no slot.
    ruled: u32,
    /// Warm: whether a node moves its own box without a relayout — set by its
    /// first translate and kept, so it stays a transform root while it
    /// lives. Scroll viewports move their content and need no flag.
    transform_root: Vec<bool>,
    /// Hot: per-node scroll offset, nonzero only on [`LayoutInput::Scroll`]
    /// nodes; default `(0,0)`. A scroll node shifts its content subtree's world
    /// rect by `-scroll` without a relayout (TRANSFORM-class, not LAYOUT).
    scroll: Vec<Vec2>,
    /// Hot: per-node world-space translate, nonzero only on nodes being moved by
    /// a transform-only animation or an edge-anchored slide (a sheet drawer);
    /// default `(0,0)`. Unlike `scroll` — which shifts only the *content subtree*
    /// and is clamped to the scrollable range — a translate shifts the *node
    /// itself* (its own world rect *and* its subtree) and is never clamped: a
    /// slide-in starts fully off-screen. TRANSFORM-class, not LAYOUT: moving a
    /// node never re-measures or re-lays it out. Set through
    /// [`set_translate`](Self::set_translate) / [`translate_by`](Self::translate_by).
    translate: Vec<Vec2>,
    /// Hot: how each node scales and rotates about its transform origin after
    /// layout, [`Turn::NONE`](crate::transform::Turn::NONE) on almost every
    /// node. TRANSFORM-class like `translate`.
    turn: Vec<crate::transform::Turn>,
    /// A turned node's box moved in a layout: its pivot did, so the transform
    /// roots need recomputing.
    turn_moved: bool,
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
    /// Warm: the lane across its [`LayoutInput::AbsoluteRows`] canvas a
    /// positioned row sits on, sized only as far as the last node given one;
    /// a node past it, or holding [`FULL_LANE`](crate::virtual_grid::FULL_LANE),
    /// sits across them all.
    row_lane: Vec<u16>,
    /// Warm: a grid node's column/row track templates, boxed because only grid
    /// nodes carry them — an ordinary node's entry is `None`. Read by the grid
    /// layout arm through [`LayoutTree::grid_column_tracks`] /
    /// [`grid_row_tracks`](LayoutTree::grid_row_tracks).
    grid_tracks: Vec<Option<Box<GridTracks>>>,
    /// Warm: each node's slot in `placements`, [`NO_PLACEMENT`] for a node
    /// that says nothing of where it sits in its parent — almost every one.
    placed_at: Vec<u32>,
    /// Cold: the placements of the grid, stack and absolute children that
    /// have one; `free_placements` lists the slots a dropped one left.
    placements: Vec<Placement>,
    free_placements: Vec<u32>,
    /// Hot: pending invalidation per node, set by `mark_dirty` and consumed by
    /// the incremental measure/layout/paint passes, cleared at frame end.
    dirty: Vec<DirtyClass>,
    /// The per-class worklists over `dirty`: what each pass visits.
    dirty_lists: crate::dirty::DirtyLists,
    /// Scratch for [`relayout_dirty`](Self::relayout_dirty): the candidate
    /// redo roots with their depths, and per slot whether it is an accepted
    /// root. Both are empty between calls.
    redo_candidates: Vec<(u32, NodeId)>,
    redo_flag: Vec<bool>,
    /// Warm: layout parameters (Flex container or leaf size).
    layout: Vec<LayoutInput>,
    /// Warm: paint style.
    style: Vec<BoxStyle>,
    /// Warm: measured natural size cache (recomputed each frame).
    measured: Vec<Measured>,
    /// Hot: whether a node participates in hit testing. Default `true` — a
    /// laid-out node is hittable, including a transparent pure-layout container
    /// (so a point in its padding/gap resolves to it). Flipping it to `false`
    /// makes a node pass-through without a structural change.
    hittable: Vec<bool>,
    /// Hot: whether a node (and its subtree) is folded out of layout and paint.
    /// Default `false` — unlike `hittable`, a node opts *in* to hiding (the
    /// hide/show family of controls flips it on demand). A hidden node measures
    /// to zero (contributing nothing to a parent's main-axis sum), lays out to a
    /// zero rect, and emits no paint primitive for itself or any descendant — all
    /// without a structural rebuild. Shared substrate for Tabs and the wider
    /// hide/show family (NavigationStack/Modal/Popup/Sheet).
    hidden: Vec<bool>,
    /// Hot: whether a node clips its children to its box (its `clip`), which
    /// paint, hit testing and the reach read.
    clips: Vec<bool>,
    /// Hot: whether a node is an overlay — painted in a deferred top layer, after
    /// every non-overlay node, regardless of its position in the tree. Default
    /// `false` — a node opts *in* to the top layer (the overlay family:
    /// Popup/Modal/Sheet/Toast). Unlike `hidden` it does not touch layout: an
    /// overlay still lays out in place (a popup is anchored to its parent's box),
    /// so flipping it marks PAINT only, not LAYOUT. Paint collects overlay subtrees
    /// during the main walk and emits them last, so top-layer content draws over
    /// the rest of the scene without a z-index sort.
    overlay: Vec<bool>,
    /// Hot: whether a node is its scroll viewport's chrome (a scroll bar
    /// track): laid out along the viewport's edge, not scrolled with its
    /// content. Default `false`.
    chrome: Vec<bool>,
    /// Hot: each node's opacity, applied to it and its subtree as one layer.
    /// Default `1.0`; `0.0` paints nothing.
    opacity: Vec<f32>,
    /// Cold: per-node pointer handler, index-aligned but mostly `None`. Read
    /// only when a node lands on a hit's dispatch chain, so it lives off the hot
    /// columns as an owned box rather than an inline fat pointer.
    handlers: Vec<Option<PointerHandler>>,
    /// The single focused node, or `None` when nothing holds focus. Keyboard and
    /// IME events route to this node (not hit testing). One slot per store — a
    /// window has one focus — reset to `None` on a structural rebuild.
    focused: Option<NodeId>,
    /// Per-pointer capture and gesture state. While a pointer is captured the
    /// router redirects its samples to the captor regardless of what the
    /// pointer is over (a thumb drag, a finger that landed on a control).
    /// Cleared on a structural rebuild.
    contacts: PointerContacts,
    /// The subtree focus traversal is confined to, or `None` for the whole tree.
    /// When set (an open modal/dialog traps the ring), [`focus_next`] starts its
    /// pre-order from this node instead of the passed root, so Tab cycles only
    /// within the scoped subtree. One slot per store — a single active trap —
    /// reset to `None` on a structural rebuild. Mirrors [`focused`]: a
    /// single retained slot, zero steady-state cost, not a per-Tab recomputation.
    ///
    /// [`focus_next`]: crate::input::focus_next
    /// [`focused`]: Self::focused
    focus_scope: Option<NodeId>,
    /// The node the pointer is currently over, or `None` when the pointer is over
    /// no interactive node (or has left the window). The router diffs this slot
    /// against each pointer-move hit to synthesize per-node enter/leave; it is a
    /// pure move-time projection of the hit target, never a hot per-node column.
    /// One slot per store — a single pointer — reset to `None` on a
    /// structural rebuild. Mirrors [`capture`]/[`focused`]: a single retained
    /// slot, zero steady-state cost when the hover target does not change.
    ///
    /// [`capture`]: Self::capture
    /// [`focused`]: Self::focused
    hovered: Option<NodeId>,
    /// The interaction states a style reads, for the nodes asked for them.
    pub(crate) interaction_states: crate::interaction::InteractionStates,
    /// Cold flag column: whether a node can hold focus. Default `false` — unlike
    /// `hittable`, a node opts *in* to focus (only interactive nodes participate
    /// in the focus ring). Maintained index-aligned with the arena.
    focusable: Vec<bool>,
    /// Cold flag column: whether a node turned its own input off (its
    /// `enabled` is `false`), which makes it and its subtree inert.
    disabled: Vec<bool>,
    /// How many nodes are disabled, so a tree with none answers
    /// [`inert`](Self::inert) without walking an ancestry.
    disabled_count: u32,
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
    /// Warm: per-node interaction-state box selection, index-aligned but mostly
    /// `None`. An interactive node (a button, a toggle) keeps its resting /
    /// hover / pressed boxes plus the cells driving them here; a `None` entry
    /// means the node paints its literal `style` unconditionally. Read only by
    /// the STYLE-resolve pass over style-dirty nodes (via
    /// [`resolve_interaction_styles`](Self::resolve_interaction_styles)), which
    /// selects the winning box into the warm `style` column — never in the hot
    /// per-node traversal, so it sits off the `style` column like `styled`.
    /// Distinct from `styled`: token folding derives field values from theme
    /// tokens, this picks one whole box from a fixed set by interaction state.
    interaction: Vec<Option<InteractionStyle>>,
    /// Cold: per-node authored accessibility semantics (role + label),
    /// index-aligned but mostly `None`. Holds the only heap data in the store
    /// (the label `String`); read only by the SEMANTICS-derive pass, never in
    /// the hot per-node traversal — so it sits off the hot columns like
    /// `handlers`/`styled`. A `None` entry means a plain layout/decoration node
    /// (the derive pass still gives an interactive node a default role).
    semantics: Vec<Option<Semantics>>,
    /// Cold: per-node *live* accessibility state (checked / value / range /
    /// expanded), index-aligned but mostly `None`. Unlike authored `semantics`,
    /// this is reactive: a control registers a parallel projection binding whose
    /// flush writes the current cell value here (via
    /// [`set_semantic_state`](Self::set_semantic_state)), and the SEMANTICS-derive
    /// pass reads it (`&self`) — so live state reaches the derived tree through a
    /// node column, never a cross-layer StateStore read,
    /// mirroring the `focused` slot. Heap-free ([`SemanticState`] is `Copy`), so
    /// the column reuses its slots with no per-frame allocation.
    semantic_state: Vec<Option<SemanticState>>,
    /// Cold: per-node drawable content (text/image/path) plus its measured
    /// intrinsic size, index-aligned but mostly `None` — only content-bearing
    /// nodes carry it, boxed off the hot columns like `semantics`. The measure
    /// pass reads its `natural()` for a `Fit` axis; paint lowers it to a
    /// glyph-run/image/path primitive. Named `content_payload` to avoid the
    /// scroll `content` extent column above.
    content_payload: Vec<Option<Box<Content>>>,
    /// Cold: what each check draws beside its caption (its text content),
    /// index-aligned and `None` for every node but a check.
    check_marks: Vec<Option<(CheckMark, f32)>>,
    /// The brush programs `Content::Shader` nodes paint with, by slot.
    shaders: ShaderSlots,
    scenes: crate::scenes::SceneSlots,
    /// The scene commits being marked, kept for its capacity.
    scratch_scene_marks: Vec<(NodeId, u64)>,
    /// The assets `Content::Asset` nodes show, by resource.
    assets: AssetTable,
    /// Cold: the nodes shown in place of an asset node's picture while it
    /// loads and once it fails, by the asset node.
    asset_fallbacks: Vec<AssetFallbacks>,
    /// Cold: the asset nodes that hold an animation still, on its first frame.
    still_assets: Vec<NodeId>,
    /// Cold: the video nodes — their sources, what the app asks of each and
    /// what its player reports.
    videos: crate::video::VideoTable,
    /// Reused to repaint the nodes showing an asset.
    scratch_asset_nodes: Vec<NodeId>,
    /// Cold: per-node unshaped text request, index-aligned but mostly `None`.
    /// `viso-ui` cannot shape text (no font stack), so a text node records what
    /// it wants drawn here and an upper tier that owns text preparation drains
    /// these, shapes each, and writes the result back into `content_payload`.
    /// Boxed off the hot columns like `semantics`; a cold `String` in sparse
    /// side storage, never touched by the hot per-node traversal.
    text_request: Vec<Option<Box<TextRequest>>>,
    /// Cold, transient handoff buffer (not index-aligned): transform animations
    /// a handler requested this frame, sitting here between the router (which
    /// drains them off the [`EventCx`](crate::context::EventCx) after a dispatch
    /// and enqueues them via [`queue_animation`](Self::queue_animation)) and the
    /// driver (which drains this via [`take_animation_requests`](Self::take_animation_requests)
    /// into its live registry, before ticking). Empty and allocation-free in the
    /// overwhelmingly common frame that starts no animation; the router holds the
    /// store, not the driver's registry, so the animation cannot be applied in
    /// place — the same deferral the text-request queue uses.
    animation_requests: Vec<TranslateAnim>,
    /// Cold, not index-aligned: the look properties moving toward a new value,
    /// at most one per node and property. Empty while nothing moves, so a
    /// settled frame never walks it.
    transitions: Vec<LookTransition>,
    /// Cold: the animations nodes run by request, at most one per node and
    /// property, and the ones ended since the last delivery.
    pub(crate) animations: Vec<NodeAnimation>,
    animation_ends: Vec<EndedAnimation>,
    /// The last [`AnimationId`] given out.
    last_animation: u64,
    /// Whether motion is reduced: what moves on a reducible timing lands at
    /// once, and a programmatic scroll jumps rather than glides.
    reduced_motion: bool,
    /// Cold: the pages leaving their stacks, which take no pointer
    /// ([`set_page`](Self::set_page)).
    pub(crate) leaving: Vec<NodeId>,
    /// Cold: the paged stacks and the page each shows.
    pub(crate) paged: crate::navigation::Paged,
    /// Cold, transient handoff buffer (not index-aligned): one-shot timer arms a
    /// handler requested this frame, sitting here between the router (which drains
    /// them off the [`EventCx`](crate::context::EventCx) after a dispatch and
    /// enqueues them via [`queue_timer`](Self::queue_timer)) and the driver (which
    /// drains this via [`take_timer_requests`](Self::take_timer_requests) and arms
    /// each on its live [`TimerRegistry`](crate::timer::TimerRegistry), against the
    /// frame's `now`). Empty and allocation-free in the overwhelmingly common
    /// frame that arms no timer; the same deferral the animation queue uses.
    timer_requests: Vec<TimerRequest>,
    /// Cold, transient handoff buffer (not index-aligned): window-open requests a
    /// handler produced this frame, sitting here between the router (which drains
    /// them off the [`EventCx`](crate::context::EventCx) after a dispatch and
    /// enqueues them via [`queue_window_open`](Self::queue_window_open)) and the
    /// facade (which drains this via [`take_window_opens`](Self::take_window_opens)
    /// to create the OS window and build its store). Empty and allocation-free in
    /// the overwhelmingly common frame that opens no window; the same deferral the
    /// animation and timer queues use.
    window_opens: Vec<WindowOpenRequest>,
    /// Cold, transient handoff buffer (not index-aligned): raw ids of windows a
    /// handler asked to close this frame, sitting here between the router and the
    /// facade (which drains this via [`take_window_closes`](Self::take_window_closes)
    /// and asks the platform to close each). Empty and allocation-free in the
    /// overwhelmingly common frame that closes no window.
    window_closes: Vec<u32>,
    /// Cold, transient handoffs and side tables of the clipboard: what a
    /// handler copied and the pastes it asked for this frame, until the
    /// facade hands them to the platform, and the nodes that take pastes.
    pub(crate) clipboard: crate::clipboard::Clipboard,
    /// Cold, transient handoff buffer (not index-aligned): text leaves the
    /// layout pass assigned a width their run was not shaped/wrapped at, sitting
    /// here between layout (which records `(id, width)` via
    /// [`request_text_reflow`](LayoutTree::request_text_reflow) as it places each
    /// wrap-eligible Flex child) and the facade (which owns the font stack and
    /// drains this via [`take_text_reflows`](Self::take_text_reflows), reshapes
    /// each at its assigned width, and writes the wrapped run back). `viso-ui`
    /// cannot shape text, so the reflow cannot happen in place — the same
    /// deferral the text-request queue uses. Width is physical px, already
    /// quantized to an integer by the recorder so drag-resize does not reshape
    /// every sub-pixel frame. Empty and allocation-free in the overwhelmingly
    /// common frame with no wrap-eligible text whose width changed.
    text_reflows: Vec<(NodeId, f32)>,
    /// Cold, transient handoff buffer (not index-aligned): edit intents a
    /// pointer handler recorded (click-to-place, drag-select), sitting here
    /// between the pointer router, which holds the store but not the
    /// driver-owned [`TextEdits`](crate::text_edit::TextEdits) registry, and
    /// [`reconcile`](crate::text_edit::reconcile), which drains them onto each
    /// node's buffer. Empty and allocation-free in a frame with no text click.
    edit_requests: Vec<(NodeId, crate::text_edit::EditIntent)>,
    /// Cold, transient handoff buffer (not index-aligned): the text each text
    /// field is seeded with from the state it shows, sitting here between the
    /// view runtime, which holds the store but not the
    /// [`TextEdits`](crate::text_edit::TextEdits) registry, and
    /// [`reconcile`](crate::text_edit::reconcile), which seeds each node's
    /// buffer. Empty in a frame whose shown states did not change.
    text_seeds: Vec<(NodeId, crate::text_edit::FieldSeed)>,
    /// Cold, transient (not index-aligned): the scrolls waiting for the next
    /// layout to know the ranges they clamp to and the rects they reveal — the
    /// offsets a hot reload carries onto freshly built viewports and the
    /// scrolls a view's actions ask for — in the order they were asked.
    scroll_requests: Vec<(NodeId, ScrollRequest, ScrollBehavior)>,
    /// Cold retained slot (not index-aligned): the nodes a caption widget
    /// declared as draggable self-drawn-caption regions, in declaration order. A
    /// self-drawn-chrome window's caption registers its blank band here at build
    /// through [`register_draggable`](BuildCx::register_draggable); the facade
    /// reads each node's post-layout `world` box, converts to logical points, and
    /// hands the set to the platform's window-drag back-channel. Empty for every
    /// window without a self-drawn caption (the common case) and reset on a
    /// structural rebuild, like [`focused`](Self::focused). A tiny list (one band
    /// per caption), so a `Vec<NodeId>` beats a per-node flag column that the
    /// facade would have to sweep the whole arena to read.
    draggable_regions: Vec<NodeId>,
    /// Cold: the UI tasks handlers spawned, each owned by the node whose
    /// handler spawned it. Empty in the common tree, and then no free or frame
    /// touches it.
    tasks: TaskSet<Option<Continuation>, NodeId>,
    /// Cold: the session's service registry, type-erased because this tier
    /// cannot name it, lent to each dispatch's [`EventCx`].
    services: Option<Rc<dyn Any>>,
    /// Cold: the structure hooks of the mounted views: one per view with
    /// control-flow regions, and one per view whose nodes show reactive values.
    structure_hooks: crate::structure::StructureHooks,
    /// Cold: the nodes reporting their scroll or visible rows after layout.
    notices: crate::notice::Notices,
    /// The nodes that take drags and the drags under way.
    pub(crate) drags: crate::drag::Drags,
    /// The nodes that take long presses and the holds under way.
    pub(crate) holds: crate::long_press::Holds,
    /// The nodes that take pinches and the pinches under way.
    pub(crate) pinches: crate::pinch::Pinches,
    /// Cold: the nodes that hear the wheel over them.
    pub(crate) wheels: crate::wheel::Wheels,
    /// Cold: the viewers and how each one's content shows.
    pub(crate) viewers: crate::viewer::Viewers,
    /// Cold: the swipe rows, and what their gestures settled.
    pub(crate) row_gestures: crate::row_gesture::RowGestures,
    /// The window's sheets ([`crate::sheet`]).
    pub(crate) sheets: crate::sheet::Sheets,
    pub(crate) drag_drops: crate::drag_drop::DragDrops,
    /// Cold: the nodes folded to their first child.
    pub(crate) folds: crate::fold::Folds,
    /// Cold: the text fields and the marks they draw.
    pub(crate) fields: crate::field_marks::Fields,
    /// Cold: the nodes that take text themselves ([`crate::text_entry`]).
    pub(crate) text_entries: crate::text_entry::TextEntries,
    /// Cold: each pointer's last press, which a press soon after counts on.
    pub(crate) clicks: crate::clicks::Clicks,
    /// Cold: the cursors nodes name.
    pub(crate) cursors: crate::cursor::Cursors,
    /// Cold: the texts that hold links, and what is done to them.
    pub(crate) links: crate::links::Links,
    /// Cold: the nodes laid out over the whole surface.
    pub(crate) layers: crate::layer::Layers,
    /// Cold: the node shown full screen, and the window's own state.
    pub(crate) fullscreen: crate::fullscreen::Fullscreen,
    /// Cold: the node the mouse's pointer is locked to, and the platform's
    /// own lock state.
    pub(crate) pointer_lock: crate::pointer_lock::PointerLock,
    /// The anchored layers (cold): each one's anchor and where it went.
    pub(crate) anchors: crate::anchored::Anchors,
    /// The wakes whose time came, waiting to reach their nodes.
    pub(crate) wakes: Vec<(NodeId, u32)>,
    /// Cold: the nodes that cast shadows.
    pub(crate) shadows: crate::shadow::Shadows,
    /// Cold: the nodes whose boxes are glass.
    pub(crate) backdrops: crate::backdrop::Backdrops,
    /// Cold: the nodes a press ripples, the ripples showing and the haptic
    /// the frame plays.
    pub(crate) feedback: crate::feedback::Feedback,
    /// Cold: the nodes' access keys and the window's menu bar.
    pub(crate) access: crate::access::AccessKeys,
    /// The viewports coasting after a flick, and how they coast.
    pub(crate) motions: crate::scroll_motion::ScrollMotions,
    /// Cold: the viewports' scroll bars and how they look.
    pub(crate) bars: crate::scroll_bar::Bars,
    /// Cold: the focus ring, how it looks, and the nodes that wear none.
    pub(crate) ring: crate::focus_ring::FocusRing,
    /// Cold: the roving groups Tab stops at once.
    pub(crate) roving: crate::roving::Roving,
    /// Cold: the viewports that refresh when pulled, and how they show it.
    pub(crate) refreshers: crate::refresh::Refreshers,
    /// Cold: the spinners that spin.
    pub(crate) spinners: crate::spinner::Spinners,
    /// Cold: the shimmers that move, and their clock.
    pub(crate) shimmers: crate::shimmer::Shimmers,
    /// Cold: the progress indicators that move.
    pub(crate) progresses: crate::progress::Progresses,
    pub(crate) sliders: crate::slider::Sliders,
    /// Cold: the nodes pinned at their viewport's start edge.
    pub(crate) stickies: crate::sticky::Stickies,
    /// Cold: the viewports that snap to their items.
    pub(crate) snaps: crate::snap::Snaps,
    /// Cold: the nodes that show a descendant as active.
    actives: crate::semantics::Sparse<NodeId>,
    /// Cold: the form controls a value is checked of.
    validities: crate::semantics::Sparse<crate::semantics::Validity>,
    /// Cold: the tables, rows and cells that say where they stand.
    tables: crate::semantics::Sparse<crate::semantics::TableFacts>,
    /// Cold: the parts of the controls each node roots, by slot, which the
    /// node owns: they go when it is freed, and their handles go stale.
    control_parts: Vec<(u32, Rc<dyn Any>)>,
    /// Where the focus was before it last moved, until the move is told.
    focus_from: Option<Option<NodeId>>,
    /// Cold: the nodes whose lengths read the environment (`px`, `sp`, `em`),
    /// re-folded into `layout` when it changes. Empty in a tree of plain `dp`
    /// and `%` lengths.
    lengths: crate::length::LengthBindings,
    /// Cold: the effects builds registered since the last flush, which the
    /// flush moves into its [`EffectStore`] and runs for the first time.
    pending_effects: crate::reactive::PendingEffects,
    /// Cold, transient: the semantic-state projections a build without the
    /// projector registry registered, waiting for the flush to adopt them.
    pending_projections: crate::reactive::PendingProjections,
    /// Cold: what runs when the app goes to the background or the window
    /// closes. Empty in a tree that persists nothing.
    suspend_hooks: crate::suspend::SuspendHooks,
    /// Cold: the clients the frame loop advances, each owned by a node.
    /// Empty in a window that runs no game.
    frame_clients: crate::frame_client::FrameClients,
    /// What the last paint left: its transform roots, where each
    /// node's primitives sit, what the last incremental paint rewrote.
    painting: crate::paint::PaintSlot,
    /// The slots of the nodes whose box moved since the last paint.
    moved: Vec<u32>,
    /// The next frame re-emits the whole tree: the device scale moved every
    /// snapped displacement, a transform root, a fade or an overlay started or
    /// stopped, or so many boxes moved that a splice would not pay.
    repaint_pending: bool,
    /// Warm: the template instance each node belongs to, tagged
    /// [`OWNER_KEPT`] when its slot is part of the instance's block;
    /// [`NO_OWNER`] for a node no instance built.
    owner: Vec<u32>,
    /// The open instance scopes, innermost last.
    scopes: Vec<NodeScope>,
    /// The ordinary nodes the open scopes allocated.
    scope_log: Vec<NodeId>,
    /// The instance of each owned node freed since the binding table last
    /// collected, one entry per node.
    freed_owners: Vec<u32>,
    /// The grid layouts' working buffers, one set per grid nesting level
    /// seen, reused frame after frame.
    grid_scratch: Vec<crate::layout::GridScratch>,
    /// Cold: the marks recorded while an inspector watches the store; `None`
    /// while none does, and then a mark records nothing.
    #[cfg(feature = "inspect")]
    journal: Option<Box<crate::dirty::DirtyJournal>>,
}

impl NodeStore {
    /// A fresh, empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// A store that holds at most `capacity` live nodes and allocates every
    /// column once, up front: creation never reallocates, and exceeding the
    /// budget is [`ArenaFull`](crate::node::ArenaFull) rather than growth. The
    /// embedded profiles build their stores this way.
    pub fn with_fixed_capacity(capacity: u32) -> Self {
        let mut store = NodeStore {
            arena: NodeArena::with_fixed_capacity(capacity),
            ..Self::default()
        };
        let n = capacity as usize;
        node_columns!(store.reserve_exact(n));
        store.dirty_lists.reserve_slots(n);
        store.redo_flag.reserve_exact(n);
        store.painting.get_mut().index.reserve_slots(n);
        store
    }

    /// Reserve room for `additional` more slots in the arena and every column,
    /// together. A builder that knows its node count (a template instantiation,
    /// an AOT package load) calls this once before creating them.
    pub fn reserve_nodes(&mut self, additional: usize) {
        if self.arena.fixed_capacity().is_some() {
            return;
        }
        self.arena.reserve(additional);
        node_columns!(self.reserve(additional));
    }

    /// Whether another node fits: always for a growing store, while slots
    /// remain for a fixed-capacity one.
    #[inline]
    pub fn has_room(&self) -> bool {
        self.arena.has_room()
    }

    /// Drop every node, resetting to empty. Used when a structural rebuild
    /// replaces the whole tree (the rebuild is wholesale).
    pub fn clear(&mut self) {
        self.arena.clear();
        node_columns!(self.clear());
        self.row_lane.clear();
        self.disabled_count = 0;
        self.box_rules.clear();
        self.free_rules.clear();
        self.ruled = 0;
        self.placements.clear();
        self.free_placements.clear();
        self.dirty_lists.reset();
        self.shaders.clear();
        self.assets.clear();
        self.asset_fallbacks.clear();
        self.still_assets.clear();
        self.videos.clear();
        self.animation_requests.clear();
        self.leaving.clear();
        self.transitions.clear();
        self.animations.clear();
        self.animation_ends.clear();
        self.timer_requests.clear();
        self.window_opens.clear();
        self.window_closes.clear();
        self.clipboard.clear();
        self.text_reflows.clear();
        self.edit_requests.clear();
        self.text_seeds.clear();
        self.scroll_requests.clear();
        let painting = self.painting.get_mut();
        painting.transforms.clear();
        painting.index.clear();
        self.focused = None;
        self.focus_from = None;
        self.contacts.clear();
        self.hovered = None;
        self.interaction_states.clear();
        self.drags.clear();
        self.holds.clear();
        self.pinches.clear();
        self.wheels.clear();
        self.viewers.clear();
        self.row_gestures.clear();
        self.sheets.clear();
        self.drag_drops.clear();
        self.folds.clear();
        self.paged.clear();
        self.fields.clear();
        self.text_entries.clear();
        self.clicks.clear();
        self.cursors.clear();
        self.links.clear();
        self.layers.clear();
        self.fullscreen.clear();
        self.pointer_lock.clear();
        self.anchors.clear();
        self.wakes.clear();
        self.shadows.clear();
        self.backdrops.clear();
        self.feedback.clear();
        self.ring.clear();
        self.access.clear();
        self.roving.clear();
        self.motions.clear();
        self.bars.clear();
        self.refreshers.clear();
        self.spinners.clear();
        self.shimmers.clear();
        self.progresses.clear();
        self.sliders.clear();
        self.stickies.clear();
        self.snaps.clear();
        self.actives.clear();
        self.validities.clear();
        self.tables.clear();
        self.control_parts.clear();
        self.focus_scope = None;
        self.draggable_regions.clear();
        self.tasks.clear();
        self.structure_hooks.clear();
        self.notices.clear();
        self.lengths.clear();
        self.pending_effects.clear();
        self.pending_projections.clear();
        self.suspend_hooks.clear();
        self.frame_clients.clear();
        self.scopes.clear();
        self.scope_log.clear();
        self.freed_owners.clear();
    }

    /// Registers an effect scoped to `node` whose dependencies `when` declares
    /// (see [`EffectStore::alloc_when`]). The next flush adopts it and runs it
    /// for the first time, after the commit that mounted `node`; freeing `node`
    /// cancels it, running its cleanup.
    pub fn add_effect(
        &mut self,
        node: NodeId,
        when: impl FnMut(&mut crate::reactive::ComputeCx<'_>) -> bool + 'static,
        body: impl FnMut(&mut crate::reactive::EffectCx<'_>) -> Option<crate::reactive::Cleanup>
        + 'static,
    ) {
        self.pending_effects.push(node, when, body);
    }

    /// Whether an effect registered since the last flush waits to run.
    pub fn has_pending_effects(&self) -> bool {
        !self.pending_effects.is_empty()
    }

    pub(crate) fn pending_effects_mut(&mut self) -> &mut crate::reactive::PendingEffects {
        &mut self.pending_effects
    }

    /// Whether a semantic-state projection registered since the last flush
    /// waits to run.
    pub fn has_pending_projections(&self) -> bool {
        !self.pending_projections.is_empty()
    }

    pub(crate) fn pending_projections_mut(&mut self) -> &mut crate::reactive::PendingProjections {
        &mut self.pending_projections
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

    /// A node's on-screen rect: its [`bounds`](Self::bounds) displaced by its
    /// own translate and its ancestors' translates and scrolls, the displacement
    /// snapped to device pixels (see [`transform`](crate::transform)); the box
    /// around where it lands when it or an ancestor is turned. Equals `bounds`
    /// for a node nothing moves. Derived on demand by walking the ancestors —
    /// paint and hit testing carry it down their walks instead.
    pub fn world(&self, id: NodeId) -> Rect {
        let bounds = self.bounds[id.index() as usize];
        let at = self.world_transform(id);
        if at.is_translation() {
            let t = at.translation();
            bounds.translate(self.snap_shift(Vec2 { x: t.x, y: t.y }))
        } else {
            bounds.transform_bounds(&at)
        }
    }

    /// [`world`](Self::world) before it is snapped to the device pixels.
    fn unsnapped_world(&self, id: NodeId) -> Rect {
        self.bounds[id.index() as usize].transform_bounds(&self.world_transform(id))
    }

    /// Where a point of the screen falls in `id`'s box: from its top-left
    /// corner, along its own axes as layout laid them, before any turn.
    pub fn to_local(&self, id: NodeId, x: f32, y: f32) -> Vec2 {
        let bounds = self.bounds[id.index() as usize];
        let at = self.world_transform(id);
        let p = if at.is_translation() {
            let t = self.snap_shift(at.translation());
            Vec2 {
                x: x - t.x,
                y: y - t.y,
            }
        } else {
            at.inverse().transform_point(Vec2 { x, y })
        };
        Vec2 {
            x: p.x - bounds.x,
            y: p.y - bounds.y,
        }
    }

    /// Where `id`'s subtree is seen and hit, before `id`'s own translate: its
    /// box joined with what its shown children reach where they sit. Equals
    /// `bounds` for a node whose children stay inside it, and for a scroll
    /// viewport, which clips them.
    #[inline]
    pub fn reach(&self, id: NodeId) -> Rect {
        self.reach[id.index() as usize]
    }

    /// The part of `id`'s on-screen rect no enclosing scroll viewport clips
    /// away: where it is seen and can be hit. Empty when it is scrolled out of
    /// view or folded away; a fully transparent node still holds its place. Walks the ancestors — a cold query for assistive
    /// technology and scenario runs.
    pub fn seen(&self, id: NodeId) -> Rect {
        if !self.arena.is_live(id) {
            return Rect::ZERO;
        }
        let mut seen = self.world(id);
        // The top layer paints past every scroll clip above it.
        let mut clipped = true;
        let mut at = id;
        loop {
            let i = at.index() as usize;
            if self.concealed(i) {
                return Rect::ZERO;
            }
            clipped &= !self.overlay[i];
            let Some(parent) = self.arena.parent(at) else {
                break;
            };
            if clipped && self.clips_children(parent) {
                seen = seen.intersect(self.world(parent));
            }
            at = parent;
        }
        seen
    }

    /// What slot `i` adds to its parent's reach: its reach where its translate
    /// and turn place it, a device pixel wider for the snapping of a moved
    /// subtree.
    pub(crate) fn reach_in_parent(&self, i: usize) -> Rect {
        let reach = self.reach[i];
        let translate = self.translate[i];
        if reach.is_empty() {
            return reach;
        }
        if self.turn[i].turns()
            && let Some(id) = self.arena.live_id(i as u32)
        {
            return reach
                .transform_bounds(&self.self_transform(id))
                .inflate(1.0 / self.device_scale());
        }
        if translate == Vec2::ZERO {
            return reach;
        }
        reach
            .translate(translate)
            .inflate(1.0 / self.device_scale())
    }

    /// `id`'s reach from its box and its children's current reaches.
    fn joined_reach(&self, id: NodeId) -> Rect {
        let i = id.index() as usize;
        let own = self.bounds[i];
        if self.concealed(i)
            || self.clips[i]
            || matches!(self.layout[i], LayoutInput::Scroll { .. })
        {
            return own;
        }
        let mut reach = own;
        let mut child = self.arena.first_child(id);
        while let Some(c) = child {
            let ci = c.index() as usize;
            if !self.concealed(ci) && !self.overlay[ci] {
                reach = reach.accumulate(self.reach_in_parent(ci));
            }
            child = self.arena.next_sibling(c);
        }
        reach
    }

    /// Record `id`'s reach, reporting whether it changed. A fade composites
    /// over its reach, so a fading node whose reach moved repaints its layer.
    fn set_reach(&mut self, id: NodeId, reach: Rect) -> bool {
        let i = id.index() as usize;
        if self.reach[i] == reach {
            return false;
        }
        self.reach[i] = reach;
        if self.opacity[i] < 1.0 {
            self.note_moved(i as u32);
        }
        true
    }

    /// Note that slot `index` paints over a new box: the next paint re-emits
    /// it, or the whole tree once so many moved that a splice would not pay.
    fn note_moved(&mut self, index: u32) {
        if self.moved.len() < self.bounds.len() / 2 + 16 {
            self.moved.push(index);
        } else {
            self.repaint_pending = true;
        }
    }

    /// Re-join the reach of every node in `id`'s subtree, children first.
    pub(crate) fn rejoin_subtree(&mut self, id: NodeId) {
        if !self.concealed(id.index() as usize) {
            let mut child = self.arena.first_child(id);
            while let Some(c) = child {
                self.rejoin_subtree(c);
                child = self.arena.next_sibling(c);
            }
        }
        let reach = self.joined_reach(id);
        self.set_reach(id, reach);
    }

    /// Carry a change of `id`'s part in its parent's reach — `was` before —
    /// up the ancestry, stopping at the first ancestor it leaves as it was: one
    /// whose box held the part before and after, or whose re-joined reach did
    /// not move.
    pub(crate) fn lift_reach(&mut self, mut id: NodeId, mut was: Rect) {
        while let Some(parent) = self.arena.parent(id) {
            let now = self.reach_in_parent(id.index() as usize);
            let own = self.bounds[parent.index() as usize];
            if within(was, own) && within(now, own) {
                return;
            }
            let before = self.reach_in_parent(parent.index() as usize);
            let reach = self.joined_reach(parent);
            if !self.set_reach(parent, reach) {
                return;
            }
            (id, was) = (parent, before);
        }
    }

    /// Re-join the reach of `id`'s parent, whose children's parts changed in a
    /// way no before/after pair describes, and carry it up.
    fn rejoin_above(&mut self, id: NodeId) {
        let Some(parent) = self.arena.parent(id) else {
            return;
        };
        let before = self.reach_in_parent(parent.index() as usize);
        let reach = self.joined_reach(parent);
        if self.set_reach(parent, reach) {
            self.lift_reach(parent, before);
        }
    }

    /// The top-layer roots the last paint drew, in the order it drew them.
    pub(crate) fn top_layer(&self) -> impl DoubleEndedIterator<Item = NodeId> + '_ {
        self.painting
            .get()
            .transforms
            .overlays
            .iter()
            .map(|&(node, _)| node)
    }

    /// The unsnapped displacement `id`'s ancestors apply to it: the sum of their
    /// translates and negated scrolls (a viewport's chrome does not scroll).
    pub fn shift_above(&self, id: NodeId) -> Vec2 {
        let mut shift = Vec2::ZERO;
        let mut from = id.node_index();
        let mut at = self.arena.parent_of(from);
        while let Some(i) = at.get_some() {
            // Grouped as `transform_above` composes, so the two agree to the bit
            // while nothing is turned.
            let scrolled = self.scroll[i.slot()] + self.pull_shift_at(i.slot() as u32);
            if scrolled == Vec2::ZERO || !self.chrome[from.slot()] {
                shift += self.translate[i.slot()] - scrolled;
            } else {
                shift += self.translate[i.slot()];
            }
            from = at;
            at = self.arena.parent_of(at);
        }
        shift
    }

    /// The displacement a node applies to itself and its subtree: its
    /// translate.
    #[inline]
    pub fn self_shift(&self, id: NodeId) -> Vec2 {
        self.translate[id.index() as usize]
    }

    /// The affine a node applies to itself and its subtree, from its layout
    /// space: its turn about its origin, then its translate.
    pub fn self_transform(&self, id: NodeId) -> Affine2 {
        let i = id.index() as usize;
        let t = self.translate[i];
        let turn = self.turn[i];
        if !turn.turns() {
            return Affine2::translate(t.x, t.y);
        }
        let b = self.bounds[i];
        let (fx, fy) = turn.origin.fractions();
        let fx = if self.direction_at(id.index()) == crate::adaptive::LayoutDirection::Rtl {
            1.0 - fx
        } else {
            fx
        };
        let (ox, oy) = (b.x + fx * b.w, b.y + fy * b.h);
        Affine2::translate(-ox, -oy)
            .then(&Affine2::scale(turn.scale, turn.scale))
            .then(&Affine2::from_rotation(turn.rotation))
            .then(&Affine2::translate(ox + t.x, oy + t.y))
    }

    /// What a node applies to its child `child`: its own transform, then, for
    /// a scroll viewport's content, its scroll.
    fn child_transform(&self, id: NodeId, child: NodeId) -> Affine2 {
        let own = self.self_transform(id);
        if self.is_scroll(id) && !self.is_chrome(child) {
            self.content_transform(id).then(&own)
        } else {
            own
        }
    }

    /// The affine `id`'s ancestors apply to it, unsnapped: from its layout
    /// space to the screen.
    pub fn transform_above(&self, id: NodeId) -> Affine2 {
        let mut above = Affine2::IDENTITY;
        let mut child = id;
        let mut at = self.arena.parent(id);
        while let Some(parent) = at {
            above = above.then(&self.child_transform(parent, child));
            child = parent;
            at = self.arena.parent(parent);
        }
        above
    }

    /// The affine from `id`'s layout space to the screen, its own transform
    /// included, unsnapped.
    pub fn world_transform(&self, id: NodeId) -> Affine2 {
        self.self_transform(id).then(&self.transform_above(id))
    }

    /// A node's turn: its scale and rotation about its transform origin.
    #[inline]
    pub fn turn(&self, id: NodeId) -> crate::transform::Turn {
        self.turn[id.index() as usize]
    }

    /// Turns a node: scales and rotates it and its subtree about its origin
    /// after layout, marking it `TRANSFORM` — plus `PAINT` the first time,
    /// when it becomes a transform root. Never MEASURE or LAYOUT. Ends a
    /// scale or rotation in flight. A live-guarded write; an unchanged turn
    /// schedules nothing.
    pub fn set_turn(&mut self, id: NodeId, turn: crate::transform::Turn) {
        if self.arena.is_live(id) {
            self.settle(id, &LookValue::Scale(turn.scale));
            self.settle(id, &LookValue::Rotation(turn.rotation));
            self.write_turn(id, |_| turn);
        }
    }

    /// Scales a node about its origin (see [`set_turn`](Self::set_turn)).
    pub fn set_scale(&mut self, id: NodeId, scale: f32) {
        if self.arena.is_live(id) {
            self.settle(id, &LookValue::Scale(scale));
            self.write_turn(id, |turn| crate::transform::Turn { scale, ..turn });
        }
    }

    /// Rotates a node clockwise by `radians` about its origin (see
    /// [`set_turn`](Self::set_turn)).
    pub fn set_rotation(&mut self, id: NodeId, radians: f32) {
        if self.arena.is_live(id) {
            self.settle(id, &LookValue::Rotation(radians));
            self.write_turn(id, |turn| crate::transform::Turn {
                rotation: radians,
                ..turn
            });
        }
    }

    /// Where in its box a node scales and rotates about (see
    /// [`set_turn`](Self::set_turn)).
    pub fn set_transform_origin(&mut self, id: NodeId, origin: crate::layout::Alignment2D) {
        if self.arena.is_live(id) {
            self.write_turn(id, |turn| crate::transform::Turn { origin, ..turn });
        }
    }

    /// Writes the turn `edit` makes of node `id`'s, leaving any move in
    /// flight. `id` is live.
    fn write_turn(
        &mut self,
        id: NodeId,
        edit: impl FnOnce(crate::transform::Turn) -> crate::transform::Turn,
    ) {
        let i = id.index() as usize;
        let mut model = self.turn[i];
        if let LookValue::Scale(scale) = self.shown(id, &LookValue::Scale(0.0)) {
            model.scale = scale;
        }
        if let LookValue::Rotation(rotation) = self.shown(id, &LookValue::Rotation(0.0)) {
            model.rotation = rotation;
        }
        let model = edit(model);
        let mut turn = model;
        if self.hold(id, LookValue::Scale(model.scale)) {
            turn.scale = self.turn[i].scale;
        }
        if self.hold(id, LookValue::Rotation(model.rotation)) {
            turn.rotation = self.turn[i].rotation;
        }
        self.show_turn(id, turn);
    }

    /// Shows `turn` as node `id`'s. `id` is live.
    fn show_turn(&mut self, id: NodeId, turn: crate::transform::Turn) {
        let i = id.index() as usize;
        if self.turn[i] == turn {
            return;
        }
        let was = self.reach_in_parent(i);
        self.turn[i] = turn;
        self.lift_reach(id, was);
        self.mark_moved(id);
    }

    /// Whether the transform roots need recomputing: a scroll, translate or
    /// turn is pending, or a turned node's box moved.
    pub fn transforms_pending(&self) -> bool {
        self.turn_moved || self.any_dirty_class(DirtyClass::TRANSFORM)
    }

    /// The displacement a scroll viewport adds for its content: its negated
    /// scroll, and a bounced pull past its end.
    #[inline]
    pub fn content_shift(&self, id: NodeId) -> Vec2 {
        -self.scroll[id.index() as usize] - self.pull_shift_at(id.index())
    }

    /// What a scroll viewport applies to its content: its
    /// [`content_shift`](Self::content_shift), then a stretched pull's
    /// stretch along its axis from the end it is pulled at.
    pub fn content_transform(&self, id: NodeId) -> Affine2 {
        let s = self.content_shift(id);
        let shift = Affine2::translate(s.x, s.y);
        let Some((axis, factor, pivot)) = self.pull_stretch(id) else {
            return shift;
        };
        let ((sx, sy), (px, py)) = match axis {
            Axis::Row => ((factor, 1.0), (pivot, 0.0)),
            Axis::Column => ((1.0, factor), (0.0, pivot)),
        };
        shift
            .then(&Affine2::translate(-px, -py))
            .then(&Affine2::scale(sx, sy))
            .then(&Affine2::translate(px, py))
    }

    /// `raw` rounded to the device pixel grid.
    #[inline]
    pub fn snap_shift(&self, raw: Vec2) -> Vec2 {
        let scale = self.device_scale();
        Vec2 {
            x: (raw.x * scale).round() / scale,
            y: (raw.y * scale).round() / scale,
        }
    }

    /// Device pixels per logical point — the length environment's scale
    /// factor; displacements snap to this grid.
    #[inline]
    pub fn device_scale(&self) -> f32 {
        let scale = self.length_env().scale_factor;
        if scale > 0.0 { scale } else { 1.0 }
    }

    /// Re-emit the whole tree the next frame, whatever is dirty.
    pub fn request_repaint(&mut self) {
        self.repaint_pending = true;
    }

    /// Whether a node moves its own box without a relayout (it has been
    /// translated or turned). Scroll viewports move their content instead: see
    /// [`is_scroll`](Self::is_scroll).
    #[inline]
    pub fn is_transform_root(&self, id: NodeId) -> bool {
        self.transform_root[id.index() as usize]
    }

    /// The transform roots of the last paint and their displacements.
    #[inline]
    pub fn transforms(&self) -> &crate::transform::TransformTable {
        &self.painting.get().transforms
    }

    /// Recompute the transform roots' places after scrolls, translates and
    /// turns, and report whether any moved. One compose per root: no node is
    /// visited.
    pub fn update_transforms(&mut self) -> bool {
        self.turn_moved = false;
        let mut painting = self.painting.lend();
        let moved = painting.transforms.update(self);
        self.painting.give_back(painting);
        moved
    }

    /// A node's scroll offset (nonzero only on scroll viewports).
    #[inline]
    pub fn scroll(&self, id: NodeId) -> Vec2 {
        self.scroll[id.index() as usize]
    }

    /// A node's world-space translate (nonzero only on nodes being slid/animated).
    /// See [`set_translate`](Self::set_translate).
    #[inline]
    pub fn translate(&self, id: NodeId) -> Vec2 {
        self.translate[id.index() as usize]
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

    /// Whether node `id` is a row an `AbsoluteRows` canvas positions: a
    /// virtual list's row host.
    #[inline]
    pub fn is_positioned_row(&self, id: NodeId) -> bool {
        !self.row_offset[id.index() as usize].is_nan()
    }

    /// Clear a node's positioned-row offset back to the "not a row" sentinel, so
    /// it is skipped when an `AbsoluteRows` canvas lays out. Used when a host is
    /// recycled out of the mounted window.
    #[inline]
    pub fn clear_row_offset(&mut self, id: NodeId) {
        self.row_offset[id.index() as usize] = f32::NAN;
        self.set_row_lane(id, crate::virtual_grid::FULL_LANE);
    }

    /// Set the lane across its [`LayoutInput::AbsoluteRows`] canvas node `id`
    /// sits on, [`FULL_LANE`](crate::virtual_grid::FULL_LANE) for across them
    /// all.
    pub fn set_row_lane(&mut self, id: NodeId, lane: u16) {
        let i = id.index() as usize;
        if i >= self.row_lane.len() {
            if lane == crate::virtual_grid::FULL_LANE {
                return;
            }
            self.row_lane.resize(i + 1, crate::virtual_grid::FULL_LANE);
        }
        self.row_lane[i] = lane;
    }

    /// The lane across its canvas node `id` sits on.
    #[inline]
    pub fn row_lane(&self, id: NodeId) -> u16 {
        LayoutTree::row_lane(self, id.index())
    }

    /// Grid `id`'s track payload, if it has one.
    pub(crate) fn grid_tracks_mut(&mut self, id: NodeId) -> Option<&mut GridTracks> {
        self.grid_tracks[id.index() as usize].as_deref_mut()
    }

    /// Grid `id`'s track payload slot.
    pub(crate) fn grid_tracks_slot(&mut self, id: NodeId) -> &mut Option<Box<GridTracks>> {
        &mut self.grid_tracks[id.index() as usize]
    }

    /// Store a grid node's track templates (its warm side payload): a
    /// [`TrackSizing::Length`] track folds against the environment now, and
    /// again at the next layout once the node's font size is known, and
    /// whenever what it reads moves.
    pub(crate) fn set_grid_tracks(
        &mut self,
        id: NodeId,
        columns: Vec<TrackSizing>,
        rows: Vec<TrackSizing>,
    ) {
        let i = id.index() as usize;
        let gap_pct = self.grid_tracks[i].as_ref().map_or([0.0; 2], |t| t.gap_pct);
        let authored = columns
            .iter()
            .chain(&rows)
            .any(|t| matches!(t, TrackSizing::Length(_)));
        let tracks = if authored {
            let env = self.length_env();
            let font = env.root_font_size();
            let fold = |tracks: &[TrackSizing]| -> Vec<TrackSizing> {
                tracks
                    .iter()
                    .map(|&t| crate::length::fold_track(t, &env, font).unwrap_or(t))
                    .collect()
            };
            GridTracks {
                columns: fold(&columns),
                rows: fold(&rows),
                authored: Some(Box::new([columns, rows])),
                gap_pct,
            }
        } else {
            GridTracks {
                columns,
                rows,
                authored: None,
                gap_pct,
            }
        };
        let deps = tracks
            .authored
            .as_ref()
            .map_or(crate::length::LengthDeps::EMPTY, |a| {
                crate::length::tracks_deps(&a[..])
            });
        self.grid_tracks[i] = Some(Box::new(tracks));
        self.bind_grid_tracks(id, deps);
    }

    /// Rewrite a grid child's placement and span in place, marking its
    /// parent `MEASURE | LAYOUT | PAINT`: the cells and the tracks they size
    /// move. Absent = auto-flow with span 1. A no-op for a stale handle or an
    /// unchanged placement.
    pub fn set_grid_placement(&mut self, id: NodeId, placement: GridPlacement) {
        self.set_placement(id, Placement::Grid(placement));
    }

    /// Rewrite where a child sits in its parent, marking what that moves: a
    /// grid cell its grid `MEASURE | LAYOUT | PAINT`; a stack alignment or
    /// absolute insets the parent `LAYOUT | PAINT`, its size following no
    /// child's place; a stack layer its stack `PAINT | HIT_TEST` and the
    /// next paint whole, the children painting in a new order. A default
    /// placement drops it. A no-op for a stale handle or an unchanged one.
    pub fn set_placement(&mut self, id: NodeId, placement: Placement) {
        if !self.arena.is_live(id) {
            return;
        }
        let i = id.index() as usize;
        let was = match self.placed_at[i] {
            NO_PLACEMENT => None,
            slot => Some(self.placements[slot as usize]),
        };
        let before = was.unwrap_or_else(|| unplaced(placement));
        if before == placement {
            return;
        }
        if placement.is_default() {
            self.drop_placement(i);
        } else {
            self.keep_placement(i, placement);
        }
        let Some(parent) = self.arena.parent(id) else {
            return;
        };
        let class = match (before, placement) {
            (Placement::Stack(a), Placement::Stack(b)) => {
                let mut class = DirtyClass::EMPTY;
                if a.align != b.align {
                    class |= DirtyClass::LAYOUT | DirtyClass::PAINT;
                }
                if a.layer != b.layer {
                    class |= DirtyClass::PAINT | DirtyClass::HIT_TEST;
                    self.request_repaint();
                }
                class
            }
            (Placement::Absolute(_), Placement::Absolute(_)) => {
                DirtyClass::LAYOUT | DirtyClass::PAINT
            }
            _ => {
                self.request_repaint();
                DirtyClass::MEASURE | DirtyClass::LAYOUT | DirtyClass::PAINT | DirtyClass::HIT_TEST
            }
        };
        self.mark_dirty(parent, class);
    }

    /// Where `id` sits in its parent, or `None` when it says nothing of it.
    pub fn placement(&self, id: NodeId) -> Option<Placement> {
        if !self.arena.is_live(id) {
            return None;
        }
        LayoutTree::placement(self, id.index()).copied()
    }

    /// Keep `placement` in slot `i`'s placement, taking a free one if it has
    /// none.
    fn keep_placement(&mut self, i: usize, placement: Placement) {
        match self.placed_at[i] {
            NO_PLACEMENT => {
                let slot = match self.free_placements.pop() {
                    Some(slot) => {
                        self.placements[slot as usize] = placement;
                        slot
                    }
                    None => {
                        self.placements.push(placement);
                        (self.placements.len() - 1) as u32
                    }
                };
                self.placed_at[i] = slot;
            }
            slot => self.placements[slot as usize] = placement,
        }
    }

    /// Free slot `i`'s placement, if it has one.
    fn drop_placement(&mut self, i: usize) {
        let slot = core::mem::replace(&mut self.placed_at[i], NO_PLACEMENT);
        if slot != NO_PLACEMENT {
            self.free_placements.push(slot);
        }
    }

    /// The layer stack child `id` paints in; `0` for any other node.
    #[inline]
    pub(crate) fn stack_layer(&self, id: NodeId) -> i32 {
        let layer = match LayoutTree::placement(self, id.index()) {
            Some(Placement::Stack(placed)) => placed.layer,
            _ => 0,
        };
        // A pinned node rides above the siblings of its layer, and a lifted
        // row above them all.
        layer
            .saturating_add(i32::from(self.is_pinned(id)))
            .saturating_add(2 * i32::from(self.is_lifted(id)))
    }

    /// Whether `id`'s children paint by layer: it is a stack one of whose
    /// children is off layer 0, or a child of it is pinned.
    #[inline]
    pub(crate) fn is_layered(&self, id: NodeId) -> bool {
        (matches!(self.layout[id.index() as usize], LayoutInput::Stack { .. })
            && self.arena.children(id).any(|c| self.stack_layer(c) != 0))
            || self.raises_children(id)
            || self.lifts_child(id)
    }

    /// Rewrite a Grid container's tracks, gaps, cell alignment, subgrid axes
    /// and adaptive columns from `style` in place, marking it `MEASURE |
    /// LAYOUT | PAINT`, or `LAYOUT | PAINT` when only the cell alignment
    /// moved. Its size, padding and box style stay. A no-op for a stale
    /// handle, an unchanged grid or a node that is not a Grid.
    pub fn set_grid_template(&mut self, id: NodeId, style: GridStyle) {
        if !self.arena.is_live(id) {
            return;
        }
        let i = id.index() as usize;
        let LayoutInput::Grid {
            column_count,
            row_count,
            column_gap,
            row_gap,
            auto_rows,
            align_items,
            subgrid_columns,
            subgrid_rows,
            adaptive_columns,
            ..
        } = &mut self.layout[i]
        else {
            return;
        };
        let same_tracks = self.grid_tracks[i].as_deref().is_some_and(|t| {
            t.authored_columns() == style.columns && t.authored_rows() == style.rows
        });
        let measures = !same_tracks
            || (*column_gap, *row_gap, *auto_rows)
                != (style.column_gap, style.row_gap, style.auto_rows)
            || (*subgrid_columns, *subgrid_rows) != (style.subgrid_columns, style.subgrid_rows)
            || *adaptive_columns != style.adaptive_columns;
        let aligns = *align_items != style.align_items;
        if !measures && !aligns {
            return;
        }
        *column_count = style.columns.len().max(1) as u16;
        *row_count = style.rows.len() as u16;
        (*column_gap, *row_gap, *auto_rows) = (style.column_gap, style.row_gap, style.auto_rows);
        (*subgrid_columns, *subgrid_rows) = (style.subgrid_columns, style.subgrid_rows);
        *adaptive_columns = style.adaptive_columns;
        *align_items = style.align_items;
        if !same_tracks {
            self.set_grid_tracks(id, style.columns, style.rows);
        }
        let class = if measures {
            DirtyClass::MEASURE | DirtyClass::LAYOUT | DirtyClass::PAINT
        } else {
            DirtyClass::LAYOUT | DirtyClass::PAINT
        };
        self.mark_dirty(id, class);
    }

    /// A Grid container's template as [`set_grid_template`](Self::set_grid_template)
    /// takes it: its tracks, gaps, cell alignment, subgrid axes and adaptive
    /// columns; `None` for a stale handle or a node that is not a Grid.
    pub fn grid_template(&self, id: NodeId) -> Option<GridStyle> {
        if !self.arena.is_live(id) {
            return None;
        }
        let i = id.index() as usize;
        let LayoutInput::Grid {
            column_gap,
            row_gap,
            auto_rows,
            align_items,
            subgrid_columns,
            subgrid_rows,
            adaptive_columns,
            ..
        } = self.layout[i]
        else {
            return None;
        };
        let tracks = self.grid_tracks[i].as_deref();
        Some(GridStyle {
            columns: tracks
                .map(|t| t.authored_columns().to_vec())
                .unwrap_or_default(),
            rows: tracks
                .map(|t| t.authored_rows().to_vec())
                .unwrap_or_default(),
            auto_rows,
            column_gap,
            row_gap,
            align_items,
            subgrid_columns,
            subgrid_rows,
            adaptive_columns,
            ..GridStyle::default()
        })
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
            align_items: style.align_items,
            subgrid_columns: style.subgrid_columns,
            subgrid_rows: style.subgrid_rows,
            adaptive_columns: style.adaptive_columns,
            size: style.size,
        };
        let id = self.alloc(input, style.style);
        self.set_grid_tracks(id, style.columns, style.rows);
        id
    }

    /// Allocate a bare leaf node of a given size, unattached. A leaf measures to
    /// its `Sizing` request against any content payload attached with
    /// [`Self::set_content_payload`] (a `Fit` axis resolves to the content's
    /// natural extent). Used by the grid arm's cell-alignment tests, which seat a
    /// content-bearing leaf in a cell and read back its baseline.
    #[cfg(test)]
    pub(crate) fn alloc_leaf(&mut self, size: Sizing) -> NodeId {
        self.alloc(LayoutInput::Leaf { size }, BoxStyle::NONE)
    }

    /// Whether a node is a scroll viewport — it clips its content to its box and
    /// carries a scroll offset. Paint and hit testing branch on this to push a
    /// clip and narrow the descent.
    #[inline]
    pub fn is_scroll(&self, id: NodeId) -> bool {
        matches!(self.layout[id.index() as usize], LayoutInput::Scroll { .. })
    }

    /// Whether a node was asked to clip its children to its box (its
    /// `clip`), default `false`.
    #[inline]
    pub fn clips(&self, id: NodeId) -> bool {
        self.clips[id.index() as usize]
    }

    /// Whether a node's children are seen and hit only inside its box,
    /// rounded by its corners: it clips, or it is a scroll viewport.
    #[inline]
    pub fn clips_children(&self, id: NodeId) -> bool {
        self.clips(id) || self.is_scroll(id)
    }

    /// Clip a node's children to its box, rounded by its corners, or stop.
    /// Its reach shrinks to its box, or grows back to what its children
    /// reach, and the stream is painted whole once. Marks PAINT and HIT_TEST.
    /// A live-guarded write.
    pub fn set_clip(&mut self, id: NodeId, clip: bool) {
        if !self.arena.is_live(id) {
            return;
        }
        let i = id.index() as usize;
        if self.clips[i] == clip {
            return;
        }
        self.clips[i] = clip;
        let before = self.reach_in_parent(i);
        let reach = self.joined_reach(id);
        if self.set_reach(id, reach) {
            self.lift_reach(id, before);
        }
        self.repaint_pending = true;
        self.mark_dirty(id, DirtyClass::PAINT | DirtyClass::HIT_TEST);
    }

    /// Whether a node is a two-dimensional track grid — it sizes its children
    /// into the cells of a column/row template rather than stacking them along a
    /// single axis. Introspection/tests branch on this to distinguish a grid
    /// container from a flex box.
    #[inline]
    pub fn is_grid(&self, id: NodeId) -> bool {
        matches!(self.layout[id.index() as usize], LayoutInput::Grid { .. })
    }

    /// The axis a scroll viewport scrolls along, or `None` for a non-scroll node:
    /// the axis its content stacks on, and the one its snapping, tail, refresh
    /// and stickies work along. See [`scrolls_on`](Self::scrolls_on) for
    /// whether it scrolls a way.
    #[inline]
    pub fn scroll_axis(&self, id: NodeId) -> Option<Axis> {
        match self.layout[id.index() as usize] {
            LayoutInput::Scroll { axis, .. } => Some(axis),
            _ => None,
        }
    }

    /// Whether node `id` is a scroll viewport scrolling along `axis`: its own
    /// axis, or across it too.
    #[inline]
    pub fn scrolls_on(&self, id: NodeId, axis: Axis) -> bool {
        match self.layout[id.index() as usize] {
            LayoutInput::Scroll {
                axis: along,
                across,
                ..
            } => along == axis || across,
            _ => false,
        }
    }

    /// The least and greatest offset a scroll viewport takes on `axis`:
    /// `[0, range]`, or `[−range, 0]` along a row read right to left, whose
    /// content starts at its right edge and scrolls toward its left.
    #[inline]
    pub fn scroll_span(&self, id: NodeId, axis: Axis) -> (f32, f32) {
        let range = self.scroll_range(id, axis);
        if axis == Axis::Row
            && self.scrolls_on(id, Axis::Row)
            && self.direction_at(id.index()) == crate::adaptive::LayoutDirection::Rtl
        {
            (-range, 0.0)
        } else if axis == Axis::Column {
            // A refresh holds a gap open before the content.
            (-self.refreshers.held(id), range)
        } else {
            (0.0, range)
        }
    }

    /// The scrollable range of a node on `axis`: `max(0, content − viewport)`,
    /// the viewport less the room its bars keep. Zero when the content does
    /// not exceed the viewport (nothing to scroll).
    #[inline]
    pub fn scroll_range(&self, id: NodeId, axis: Axis) -> f32 {
        let i = id.index() as usize;
        let b = self.bounds[i];
        let gutter = self.scroll_gutter(id);
        let viewport = match axis {
            Axis::Row => b.w - gutter.x,
            Axis::Column => b.h - gutter.y,
        };
        (self.content[i].on(axis) - viewport).max(0.0)
    }

    /// The room viewport `id` keeps for classic bars: `x` beside a vertical
    /// one, `y` under a horizontal one.
    #[inline]
    pub(crate) fn scroll_gutter(&self, id: NodeId) -> Vec2 {
        match self.layout[id.index() as usize] {
            LayoutInput::Scroll { gutter, .. } => gutter,
            _ => Vec2::ZERO,
        }
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

    /// Repaint `parent`'s children whole: the order they paint in changed.
    pub(crate) fn restack_children(&mut self, parent: NodeId) {
        self.arena.note_relinked(parent.node_index(), None);
        self.mark_dirty(parent, DirtyClass::PAINT | DirtyClass::HIT_TEST);
    }

    /// Links the orphan `child` under `parent` immediately before `before`, or
    /// last when `before` is `None`, marking `parent` for re-measure. Returns
    /// `false` (a no-op) under the conditions of [`NodeArena::insert_before`].
    pub fn arena_insert_before(
        &mut self,
        parent: NodeId,
        child: NodeId,
        before: Option<NodeId>,
    ) -> bool {
        let linked = self.arena.insert_before(parent, child, before);
        if linked {
            self.mark_dirty(
                parent,
                DirtyClass::MEASURE | DirtyClass::LAYOUT | DirtyClass::PAINT,
            );
        }
        linked
    }

    pub(crate) fn structure_hooks(&self) -> &crate::structure::StructureHooks {
        &self.structure_hooks
    }

    pub(crate) fn frame_clients(&self) -> &crate::frame_client::FrameClients {
        &self.frame_clients
    }

    pub(crate) fn frame_clients_mut(&mut self) -> &mut crate::frame_client::FrameClients {
        &mut self.frame_clients
    }

    /// The arena alongside the frame clients, to drop those whose owner died.
    pub(crate) fn arena_and_frame_clients(
        &mut self,
    ) -> (&NodeArena, &mut crate::frame_client::FrameClients) {
        (&self.arena, &mut self.frame_clients)
    }

    pub(crate) fn suspend_hooks(&self) -> &crate::suspend::SuspendHooks {
        &self.suspend_hooks
    }

    pub(crate) fn suspend_hooks_mut(&mut self) -> &mut crate::suspend::SuspendHooks {
        &mut self.suspend_hooks
    }

    pub(crate) fn structure_hooks_mut(&mut self) -> &mut crate::structure::StructureHooks {
        &mut self.structure_hooks
    }

    pub(crate) fn notices(&self) -> &crate::notice::Notices {
        &self.notices
    }

    pub(crate) fn notices_mut(&mut self) -> &mut crate::notice::Notices {
        &mut self.notices
    }

    pub(crate) fn lengths(&self) -> &crate::length::LengthBindings {
        &self.lengths
    }

    pub(crate) fn lengths_mut(&mut self) -> &mut crate::length::LengthBindings {
        &mut self.lengths
    }

    /// The layout input of live node `id`.
    pub(crate) fn layout_input_mut(&mut self, id: NodeId) -> &mut LayoutInput {
        &mut self.layout[id.index() as usize]
    }

    /// The parent of `child` in the arena, or `None` for a root or a stale handle.
    /// A read-only ancestry query the sibling of [`arena_detach`](Self::arena_detach)
    /// / [`arena_append_child`](Self::arena_append_child) needs: to re-home a node
    /// under the same parent another node already lives under, a caller resolves that
    /// parent from the sibling. A discrete-action lookup off the compact link store,
    /// never a per-frame path.
    #[inline]
    pub fn parent(&self, child: NodeId) -> Option<NodeId> {
        self.arena.parent(child)
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
            Axis::Column => Sizing {
                width: Length::fill(),
                height: Length::Fit,
            },
            Axis::Row => Sizing {
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
                justify: Justify::Start,
                size,
            },
            BoxStyle::default(),
        );
        self.arena.append_child(canvas, host);
        self.set_semantics(host, Semantics::role(Role::ListItem));
        host
    }

    /// Allocate a fixed-size floating host under an [`LayoutInput::AbsoluteRows`]
    /// `canvas` and return `(host, mount)`: `host` is the positioned outer node the
    /// canvas places (give it a `row_offset` for its Y and a `set_translate` for its
    /// X), and `mount` is the inner exactly-`size` box a floating panel attaches
    /// under. The sibling of [`alloc_row_host`](Self::alloc_row_host) for a canvas
    /// whose children carry their own rectangle rather than fitting content.
    ///
    /// Two nodes are needed because an `AbsoluteRows` canvas *cross-forces* every row
    /// to its own full cross extent (a virtual list's rows are full-width by design):
    /// a lone host sized `Fixed(w) × Fixed(h)` would keep its main extent but have its
    /// cross extent overwritten to the canvas width, so a single node cannot pin both
    /// `w` and `h`. Instead the outer `host` **fits** its body on the canvas main axis
    /// (so its measured main extent is exactly `h`, which the canvas places at the
    /// `row_offset`) and lets the cross be forced; its `Align::Start` keeps the body
    /// at the cross origin rather than stretching it; and the inner `mount`, sized
    /// exactly `size`, holds the true `w × h` rectangle at that origin. The panel keeps
    /// its identity across a redock; [`set_fixed_size`](Self::set_fixed_size) re-sizes
    /// the `mount` when the float rect changes.
    pub fn alloc_fixed_host(&mut self, canvas: NodeId, size: Sizing) -> (NodeId, NodeId) {
        // The outer host fits its body on the AbsoluteRows main axis (so its measured
        // main extent is the float's main extent the canvas places) and pins the body
        // to the cross origin with Align::Start, since the canvas forces the cross.
        let host = self.alloc(
            LayoutInput::Flex {
                axis: Axis::Column,
                gap: 0.0,
                padding: Inset::default(),
                align: Align::Start,
                justify: Justify::Start,
                size: Sizing {
                    width: Length::Fit,
                    height: Length::Fit,
                },
            },
            BoxStyle::default(),
        );
        // The inner mount is the exact w × h rectangle the panel lives in.
        let mount = self.alloc(
            LayoutInput::Flex {
                axis: Axis::Column,
                gap: 0.0,
                padding: Inset::default(),
                align: Align::Stretch,
                justify: Justify::Start,
                size,
            },
            BoxStyle::default(),
        );
        self.arena.append_child(canvas, host);
        self.arena.append_child(host, mount);
        (host, mount)
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
            let length = match axis {
                Axis::Row => &mut size.width,
                Axis::Column => &mut size.height,
            };
            if *length == Length::Fixed(extent) {
                return;
            }
            *length = Length::Fixed(extent);
            self.request_moved(canvas);
        }
    }

    /// Make the rows of virtual list `viewport` `extent` long across its
    /// axis — a table's columns side by side: a list built to scroll
    /// [`across`](VirtualListStyle::across) scrolls to show them, and with
    /// less than it shows they still fill it. A stale handle or a node that
    /// is not a list is a no-op.
    pub fn set_list_across(&mut self, viewport: NodeId, extent: f32) {
        if !self.arena.is_live(viewport) {
            return;
        }
        let Some(axis) = self.scroll_axis(viewport) else {
            return;
        };
        let canvas = self.arena.children(viewport).find(|c| {
            matches!(
                self.layout[c.index() as usize],
                LayoutInput::AbsoluteRows { .. }
            )
        });
        if let Some(canvas) = canvas {
            self.set_absolute_rows_extent(canvas, crate::layout::cross_of(axis), extent.max(0.0));
        }
    }

    /// Lay the rows of [`LayoutInput::AbsoluteRows`] canvas `canvas` on
    /// `lanes` lanes `cross_gap` apart, re-placing them.
    pub fn set_absolute_rows_lanes(&mut self, canvas: NodeId, lanes: u16, cross_gap: f32) {
        if !self.arena.is_live(canvas) {
            return;
        }
        let i = canvas.index() as usize;
        if let LayoutInput::AbsoluteRows {
            lanes: l,
            cross_gap: g,
            ..
        } = &mut self.layout[i]
            && (*l, *g) != (lanes, cross_gap)
        {
            (*l, *g) = (lanes, cross_gap);
            self.mark_dirty(canvas, DirtyClass::LAYOUT | DirtyClass::PAINT);
        }
    }

    /// Rewrite a flex child's own main-axis [`Length`] to `Fill { weight }` in
    /// place, marking it `LAYOUT | PAINT` and its parent `LAYOUT`, so the parent
    /// re-runs its leftover-space distribution and the pane repaints at its new
    /// size. The general form of
    /// [`set_absolute_rows_extent`](Self::set_absolute_rows_extent): a reconcile
    /// step holding `&mut NodeStore` calls this to turn a committed drag fraction
    /// into live geometry, the same handler-writes-intent / reconcile-mutates-store
    /// split the virtual list uses.
    ///
    /// Only the weight changes, never the child's measured extent, so nothing
    /// re-measures above the parent. A no-op for a stale
    /// handle, or for a node whose main-axis length is not already `Fill` (a fixed
    /// or fit pane has no weight to rewrite).
    pub fn set_flex_child_weight(&mut self, child: NodeId, axis: Axis, weight: f32) {
        if !self.arena.is_live(child) {
            return;
        }
        let i = child.index() as usize;
        let size = self.layout[i].size_mut();
        let slot = match axis {
            Axis::Row => &mut size.width,
            Axis::Column => &mut size.height,
        };
        if let Length::Fill { weight: w } = slot {
            *w = weight;
            self.mark_dirty(child, DirtyClass::LAYOUT | DirtyClass::PAINT);
            self.request_moved(child);
        }
    }

    /// Rewrite a node's own [`Sizing`] in place, marking it `MEASURE | LAYOUT |
    /// PAINT`: the size it requests changed, so its parent re-lays it out at the
    /// new extent (MEASURE rises until an ancestor fixed on both axes). The same-class in-place `LayoutInput`
    /// size rewrite as [`set_flex_child_weight`](Self::set_flex_child_weight) and
    /// [`set_absolute_rows_extent`](Self::set_absolute_rows_extent), but general over
    /// both axes and any length kind — a reconcile step holding `&mut NodeStore` calls
    /// this to size a host it owns (a floating dock panel's fixed-size host) to a
    /// runtime rectangle without rebuilding the subtree. A no-op for a stale handle
    /// or an unchanged size.
    pub fn set_fixed_size(&mut self, id: NodeId, size: Sizing) {
        if !self.arena.is_live(id) {
            return;
        }
        let slot = self.layout[id.index() as usize].size_mut();
        if *slot == size {
            return;
        }
        *slot = size;
        self.mark_dirty(
            id,
            DirtyClass::MEASURE | DirtyClass::LAYOUT | DirtyClass::PAINT,
        );
        self.request_moved(id);
    }

    /// Note that the size node `id` requests of its parent changed: the parent
    /// re-places its children, so the box it gives `id` follows the request.
    /// A re-measure alone moves no box whose request does not follow its
    /// content (see [`relayout_dirty`](Self::relayout_dirty)). A stack or an
    /// absolute container places each child alone, so its re-measure does.
    pub(crate) fn request_moved(&mut self, id: NodeId) {
        if let Some(parent) = self.arena.parent(id)
            && !matches!(
                self.layout[parent.index() as usize],
                LayoutInput::Stack { .. } | LayoutInput::Absolute { .. }
            )
        {
            self.mark_dirty(parent, DirtyClass::LAYOUT);
        }
    }

    /// The size node `id` requests of its parent, or `None` for a stale handle.
    pub fn size_request(&self, id: NodeId) -> Option<Sizing> {
        self.arena
            .is_live(id)
            .then(|| self.layout[id.index() as usize].size())
    }

    /// A Flex container's axis, main-axis placement and cross-axis
    /// alignment; `None` for any other node.
    pub fn flex_arrangement(&self, id: NodeId) -> Option<(Axis, Justify, Align)> {
        if !self.arena.is_live(id) {
            return None;
        }
        match self.layout[id.index() as usize] {
            LayoutInput::Flex {
                axis,
                justify,
                align,
                ..
            } => Some((axis, justify, align)),
            _ => None,
        }
    }

    /// Rewrite a Flex container's axis, main-axis placement and cross-axis
    /// alignment in place, marking it `LAYOUT | PAINT`, and `MEASURE` too when
    /// its axis turns. A no-op for a stale handle, an unchanged arrangement or
    /// a node that is not a Flex container.
    pub fn set_flex_arrangement(&mut self, id: NodeId, to: (Axis, Justify, Align)) {
        if !self.arena.is_live(id) {
            return;
        }
        let LayoutInput::Flex {
            axis,
            justify,
            align,
            ..
        } = &mut self.layout[id.index() as usize]
        else {
            return;
        };
        let measures = *axis != to.0;
        if (*axis, *justify, *align) == to {
            return;
        }
        (*axis, *justify, *align) = to;
        let class = if measures {
            DirtyClass::MEASURE | DirtyClass::LAYOUT | DirtyClass::PAINT
        } else {
            DirtyClass::LAYOUT | DirtyClass::PAINT
        };
        self.mark_dirty(id, class);
    }

    /// Rewrite a Flex container's main-axis gap, or both of a Grid's gaps, in
    /// place, marking it `MEASURE | LAYOUT | PAINT`. A no-op for a stale handle,
    /// an unchanged gap or a node with no gap.
    pub fn set_gap(&mut self, id: NodeId, value: f32) {
        if !self.arena.is_live(id) {
            return;
        }
        match &mut self.layout[id.index() as usize] {
            LayoutInput::Flex { gap, .. } => {
                if *gap == value {
                    return;
                }
                *gap = value;
            }
            LayoutInput::Grid {
                column_gap,
                row_gap,
                ..
            } => {
                if *column_gap == value && *row_gap == value {
                    return;
                }
                *column_gap = value;
                *row_gap = value;
            }
            _ => return,
        }
        self.mark_dirty(
            id,
            DirtyClass::MEASURE | DirtyClass::LAYOUT | DirtyClass::PAINT,
        );
    }

    /// Rewrite where a stack puts the children that do not align themselves,
    /// marking it `LAYOUT | PAINT`. A no-op for a stale handle, an unchanged
    /// alignment or a node that is not a stack.
    pub fn set_stack_align(&mut self, id: NodeId, to: Alignment2D) {
        if !self.arena.is_live(id) {
            return;
        }
        let LayoutInput::Stack { align, .. } = &mut self.layout[id.index() as usize] else {
            return;
        };
        if *align == to {
            return;
        }
        *align = to;
        self.mark_dirty(id, DirtyClass::LAYOUT | DirtyClass::PAINT);
    }

    /// Drop `id`'s placement, marking what that moves as
    /// [`set_placement`](Self::set_placement) does.
    pub fn clear_placement(&mut self, id: NodeId) {
        let Some(placement) = self.placement(id) else {
            return;
        };
        self.set_placement(id, unplaced(placement));
    }

    /// Rewrite a container's padding in place, marking it
    /// `MEASURE | LAYOUT | PAINT` (padding is part of its natural size). The
    /// facade keeps a full-screen window's content clear of system UI this way,
    /// following each safe-area or keyboard change without a rebuild. A no-op
    /// for a stale handle or a node with no padding.
    pub fn set_padding(&mut self, id: NodeId, inset: Inset) {
        if !self.arena.is_live(id) {
            return;
        }
        match &mut self.layout[id.index() as usize] {
            LayoutInput::Flex { padding, .. }
            | LayoutInput::Grid { padding, .. }
            | LayoutInput::Stack { padding, .. }
            | LayoutInput::Absolute { padding, .. } => {
                if *padding == inset {
                    return;
                }
                *padding = inset;
            }
            _ => return,
        }
        self.mark_dirty(
            id,
            DirtyClass::MEASURE | DirtyClass::LAYOUT | DirtyClass::PAINT,
        );
    }

    /// Bound `id`'s box beyond its size request, and give it a margin and a
    /// padding percentage, all resolved against its parent's content box at
    /// layout; [`BoxRules::NONE`] drops them. A change marks MEASURE, LAYOUT
    /// and PAINT. A live-guarded write; a stale handle is a no-op.
    pub fn set_box_rules(&mut self, id: NodeId, rules: BoxRules) {
        if !self.arena.is_live(id) || self.box_rules(id) == rules {
            return;
        }
        let i = id.index() as usize;
        if rules == BoxRules::NONE {
            self.drop_rules(i);
        } else if self.rules_at[i] != NO_RULES {
            self.box_rules[self.rules_at[i] as usize].0 = rules;
        } else {
            let slot = match self.free_rules.pop() {
                Some(slot) => {
                    self.box_rules[slot as usize] = (rules, layout::INDEFINITE);
                    slot
                }
                None => {
                    self.box_rules.push((rules, layout::INDEFINITE));
                    (self.box_rules.len() - 1) as u32
                }
            };
            self.rules_at[i] = slot;
            self.ruled += 1;
        }
        self.mark_dirty(
            id,
            DirtyClass::MEASURE | DirtyClass::LAYOUT | DirtyClass::PAINT,
        );
    }

    /// `id`'s box rules; [`BoxRules::NONE`] for a node with none.
    pub fn box_rules(&self, id: NodeId) -> BoxRules {
        match self.rules_at.get(id.index() as usize) {
            Some(&slot) if slot != NO_RULES && self.arena.is_live(id) => {
                self.box_rules[slot as usize].0
            }
            _ => BoxRules::NONE,
        }
    }

    /// Free slot `i`'s box rules, if it has any.
    fn drop_rules(&mut self, i: usize) {
        let slot = core::mem::replace(&mut self.rules_at[i], NO_RULES);
        if slot != NO_RULES {
            self.free_rules.push(slot);
            self.ruled -= 1;
        }
    }

    /// A node holding pointer capture, if any pointer is captured.
    #[inline]
    pub fn capture(&self) -> Option<NodeId> {
        self.contacts.any_captor()
    }

    /// The node `pointer` is captured to, if any.
    #[inline]
    pub fn capture_of(&self, pointer: PointerId) -> Option<NodeId> {
        self.contacts.captor(pointer)
    }

    /// Capture `pointer` to a node (or release it with `None`), returning the
    /// node that held it before. A live-guarded write: capturing a stale handle
    /// is a no-op, so a released/freed node never holds capture. Clearing is
    /// always honored.
    pub fn set_capture(&mut self, pointer: PointerId, id: Option<NodeId>) -> Option<NodeId> {
        match id {
            Some(node) if !self.arena.is_live(node) => self.contacts.captor(pointer),
            _ => {
                if pointer == PointerId::MOUSE {
                    self.hold_cursor(id.is_some());
                }
                self.contacts.set_captor(pointer, id, false)
            }
        }
    }

    /// Per-pointer routing state.
    #[inline]
    pub(crate) fn contacts(&self) -> &PointerContacts {
        &self.contacts
    }

    #[inline]
    pub(crate) fn contacts_mut(&mut self) -> &mut PointerContacts {
        &mut self.contacts
    }

    /// Register a node as a draggable self-drawn-caption region. A live-guarded,
    /// deduplicating append: a stale or already-registered handle is a no-op, so a
    /// rebuild that re-declares the caption never doubles the list (the list is
    /// cleared on the wholesale rebuild anyway, but the guard keeps a repeated
    /// call within one build idempotent). The facade reads
    /// [`draggable_regions`](Self::draggable_regions) after layout.
    pub fn push_draggable_region(&mut self, id: NodeId) {
        if !self.arena.is_live(id) || self.draggable_regions.contains(&id) {
            return;
        }
        self.draggable_regions.push(id);
    }

    /// The nodes a caption widget declared as draggable regions, in declaration
    /// order. Empty for every window without a self-drawn caption. The facade
    /// reads each node's post-layout [`world`](Self::world) box to push the
    /// window's drag regions to the platform.
    #[inline]
    pub fn draggable_regions(&self) -> &[NodeId] {
        &self.draggable_regions
    }

    /// The node the pointer is currently over, if any. The router reads this to
    /// diff against each move-time hit target and synthesize per-node
    /// enter/leave; it is never a hot per-node column.
    #[inline]
    pub fn hovered(&self) -> Option<NodeId> {
        self.hovered
    }

    /// Set (or clear, with `None`) the hovered node. A live-guarded write:
    /// pointing hover at a stale handle is a no-op, so a released/freed node
    /// never appears hovered. Clearing is always honored. The router calls this
    /// once per move, after dispatching the enter/leave pair, to commit the new
    /// hover target — mirroring [`set_capture`].
    ///
    /// [`set_capture`]: Self::set_capture
    pub fn set_hovered(&mut self, id: Option<NodeId>) {
        let next = match id {
            Some(node) if self.arena.is_live(node) => Some(node),
            Some(_) => return,
            None => None,
        };
        if self.hovered != next {
            self.interaction_states.moved();
        }
        self.hovered = next;
    }

    /// Add `delta` to a scroll viewport's offset, clamped per axis to its
    /// [`scroll_span`](Self::scroll_span), and mark the node `TRANSFORM` only: its content is a
    /// transform root, so the next frame moves the retained draws and hit
    /// testing reads the new offset directly — a scroll never dirties MEASURE,
    /// LAYOUT, PAINT or HIT_TEST and never bubbles to ancestors. A live-guarded
    /// write; a stale handle is a no-op. A delta that does not move the
    /// (already-clamped) offset schedules nothing.
    pub fn scroll_by(&mut self, id: NodeId, delta: Vec2) {
        if !self.arena.is_live(id) {
            return;
        }
        let i = id.index() as usize;
        let ((x0, x1), (y0, y1)) = (
            self.scroll_span(id, Axis::Row),
            self.scroll_span(id, Axis::Column),
        );
        let prev = self.scroll[i];
        let next = Vec2 {
            x: (prev.x + delta.x).clamp(x0, x1),
            y: (prev.y + delta.y).clamp(y0, y1),
        };
        if next == prev {
            return;
        }
        self.scroll[i] = next;
        self.mark_dirty(id, DirtyClass::TRANSFORM);
    }

    /// Set a scroll viewport's offset to an absolute `offset`, clamped per axis
    /// to its [`scroll_span`](Self::scroll_span), marking the node `TRANSFORM` exactly like
    /// [`scroll_by`](Self::scroll_by) — the absolute-value sibling of
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
        let ((x0, x1), (y0, y1)) = (
            self.scroll_span(id, Axis::Row),
            self.scroll_span(id, Axis::Column),
        );
        let next = Vec2 {
            x: offset.x.clamp(x0, x1),
            y: offset.y.clamp(y0, y1),
        };
        if next == self.scroll[i] {
            return;
        }
        self.scroll[i] = next;
        self.mark_dirty(id, DirtyClass::TRANSFORM);
    }

    /// Scroll a viewport `distance` from its start along `axis` (leftward on a
    /// row read right to left), unclamped: the virtual-list reconcile seeks a
    /// row on a canvas whose extent it has just set, before the layout that
    /// measures it. Marks `TRANSFORM` like [`set_scroll`](Self::set_scroll).
    pub(crate) fn seek_scroll(&mut self, id: NodeId, axis: Axis, distance: f32) {
        if !self.arena.is_live(id) {
            return;
        }
        let i = id.index() as usize;
        let along = if axis == Axis::Row
            && self.direction_at(id.index()) == crate::adaptive::LayoutDirection::Rtl
        {
            -distance
        } else {
            distance
        };
        let mut next = self.scroll[i];
        match axis {
            Axis::Row => next.x = along,
            Axis::Column => next.y = along,
        }
        if next == self.scroll[i] {
            return;
        }
        self.scroll[i] = next;
        self.mark_dirty(id, DirtyClass::TRANSFORM);
    }

    /// Restore a scroll viewport's offset once the next layout has measured its
    /// content: a freshly built viewport has no scroll range yet, so the offset
    /// waits and is then applied through [`set_scroll`](Self::set_scroll),
    /// clamped to the range that layout found. A hot reload carries a kept
    /// viewport's offset onto the node that rebuilt it this way. Cold path.
    pub fn restore_scroll(&mut self, id: NodeId, offset: Vec2) {
        self.request_scroll(id, ScrollRequest::To(offset));
    }

    /// Scrolls once the next layout has placed the tree: a viewport to an
    /// offset or by a delta, clamped to the range layout finds, or the
    /// enclosing viewports the least that shows a node. A view's
    /// `scroll_to`, `scroll_by` and `scroll_into_view` reach a node the same
    /// action mounts this way. A stale handle is a no-op. Cold path.
    pub fn request_scroll(&mut self, id: NodeId, request: ScrollRequest) {
        self.request_scroll_with(id, request, ScrollBehavior::Instant);
    }

    /// [`request_scroll`](Self::request_scroll), at once or gliding: a
    /// smooth move by a delta adds it to where a glide under way is headed.
    pub fn request_scroll_with(
        &mut self,
        id: NodeId,
        request: ScrollRequest,
        behavior: ScrollBehavior,
    ) {
        if self.arena.is_live(id) {
            // The asked scroll takes over from a coast.
            match request {
                ScrollRequest::Reveal => self.stop_coasts_over(id),
                _ if behavior == ScrollBehavior::Instant => self.stop_coast(id),
                _ => {}
            }
            self.scroll_requests.push((id, request, behavior));
        }
    }

    /// Apply every waiting [`request_scroll`](Self::request_scroll), in the
    /// order asked, after a layout pass.
    fn settle_scroll_requests(&mut self) {
        if self.scroll_requests.is_empty() {
            return;
        }
        for (id, request, behavior) in std::mem::take(&mut self.scroll_requests) {
            let smooth = behavior == ScrollBehavior::Smooth;
            match request {
                ScrollRequest::To(offset) if smooth => self.glide_to(id, offset),
                ScrollRequest::To(offset) => self.set_scroll(id, offset),
                ScrollRequest::By(delta) if smooth => {
                    let to = self.scroll_target(id);
                    self.glide_to(
                        id,
                        Vec2 {
                            x: to.x + delta.x,
                            y: to.y + delta.y,
                        },
                    );
                }
                ScrollRequest::By(delta) => self.scroll_by(id, delta),
                ScrollRequest::Reveal => self.reveal(id, smooth),
            }
        }
    }

    /// Scrolls each viewport enclosing `id`, innermost first, the least that
    /// brings `id`'s on-screen rect inside it, at once or gliding: a rect
    /// larger than the viewport shows its leading edge. Each viewport is
    /// figured where the ones inside it are headed.
    fn reveal(&mut self, id: NodeId, smooth: bool) {
        if !self.arena.is_live(id) {
            return;
        }
        let rect = self.unsnapped_world(id);
        // How far the ones inside move the node on screen.
        let mut moved = Vec2::ZERO;
        let mut at = self.arena.parent(id);
        while let Some(viewport) = at {
            at = self.arena.parent(viewport);
            if self.scroll_axis(viewport).is_none() {
                continue;
            }
            let (now, from) = (self.scroll(viewport), self.scroll_target(viewport));
            // What it shows: its box less the room its bars keep.
            let mut seen = self.unsnapped_world(viewport);
            let gutter = self.scroll_gutter(viewport);
            if self.direction_at(viewport.index()) == crate::adaptive::LayoutDirection::Rtl {
                seen.x += gutter.x;
            }
            seen.w -= gutter.x;
            seen.h -= gutter.y;
            let x = rect.x + moved.x - (from.x - now.x);
            let y = rect.y + moved.y - (from.y - now.y);
            let fit = |start: f32, len: f32, view: f32, view_len: f32| {
                if start < view {
                    start - view
                } else if start + len > view + view_len {
                    (start + len - (view + view_len)).min(start - view)
                } else {
                    0.0
                }
            };
            let mut to = from;
            if self.scrolls_on(viewport, Axis::Row) {
                let (least, most) = self.scroll_span(viewport, Axis::Row);
                to.x = (from.x + fit(x, rect.w, seen.x, seen.w)).clamp(least, most);
            }
            if self.scrolls_on(viewport, Axis::Column) {
                let (least, most) = self.scroll_span(viewport, Axis::Column);
                to.y = (from.y + fit(y, rect.h, seen.y, seen.h)).clamp(least, most);
            }
            moved.x -= to.x - now.x;
            moved.y -= to.y - now.y;
            if to == from {
                continue;
            }
            if smooth {
                self.glide_to(viewport, to);
            } else {
                self.set_scroll(viewport, to);
            }
        }
    }

    /// Set a node's world-space translate to an absolute `offset`, moving the
    /// node itself (and its subtree) without a relayout, and mark it `TRANSFORM`
    /// — plus `PAINT` the first time, when it becomes a transform root. A
    /// translate never dirties MEASURE/LAYOUT and never bubbles to ancestors.
    ///
    /// Unlike [`set_scroll`](Self::set_scroll) this is **not clamped**: a
    /// transform-only slide starts a node fully off-screen (a translate of one
    /// full extent past the surface edge) and animates it to `(0,0)`, so the
    /// setter must accept arbitrarily large or negative offsets. A live-guarded
    /// write; a stale handle is a no-op, and an offset equal to the current value
    /// schedules nothing.
    pub fn set_translate(&mut self, id: NodeId, offset: Vec2) {
        if !self.arena.is_live(id) || self.hold(id, LookValue::Translate(offset)) {
            return;
        }
        self.show_translate(id, offset);
    }

    /// Shows `offset` as node `id`'s translate, past any pin of it. `id` is
    /// live.
    fn show_translate(&mut self, id: NodeId, offset: Vec2) {
        let offset = offset + self.pin_of(id);
        self.put_translate(id, offset);
    }

    /// Writes `offset` as node `id`'s whole translate. `id` is live.
    pub(crate) fn put_translate(&mut self, id: NodeId, offset: Vec2) {
        let i = id.index() as usize;
        if offset == self.translate[i] {
            return;
        }
        let was = self.reach_in_parent(i);
        self.translate[i] = offset;
        self.lift_reach(id, was);
        self.mark_moved(id);
    }

    /// Add `delta` to a node's world-space translate — the delta sibling of
    /// [`set_translate`](Self::set_translate), sharing its unclamped write, dirty
    /// classes, and no-bubble rule. A live-guarded write; a stale handle is a
    /// no-op, and a zero delta schedules nothing.
    pub fn translate_by(&mut self, id: NodeId, delta: Vec2) {
        if !self.arena.is_live(id) {
            return;
        }
        let LookValue::Translate(now) = self.shown(id, &LookValue::Translate(delta)) else {
            return;
        };
        let next = Vec2 {
            x: now.x + delta.x,
            y: now.y + delta.y,
        };
        self.set_translate(id, next);
    }

    /// Mark a translated node: `TRANSFORM` once it is a transform root; the
    /// first translate makes it one, which the paint that records it needs.
    fn mark_moved(&mut self, id: NodeId) {
        let i = id.index() as usize;
        if self.transform_root[i] {
            self.mark_dirty(id, DirtyClass::TRANSFORM);
        } else {
            self.transform_root[i] = true;
            self.mark_dirty(id, DirtyClass::TRANSFORM | DirtyClass::PAINT);
            self.repaint_pending = true;
        }
    }

    /// A node's layout parameters: its kind, its size request and what it
    /// arranges its children by. Cold path, for an inspector.
    pub fn layout_input(&self, id: NodeId) -> LayoutInput {
        self.layout[id.index() as usize]
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

    /// A node's interaction-state box selection, if it has one. `None` means the
    /// node paints its literal `style` unconditionally.
    #[inline]
    pub fn interaction_style(&self, id: NodeId) -> Option<InteractionStyle> {
        self.interaction[id.index() as usize]
    }

    /// Bind a node's `style` to an interaction-state box selection, replacing
    /// any prior selection. A live-guarded write, so a stale handle is a no-op.
    ///
    /// This records the variants + driving cells and marks the node STYLE +
    /// PAINT so the next frame's
    /// [`resolve_interaction_styles`](Self::resolve_interaction_styles) pass
    /// selects the current winning box into the node's warm `style` and the
    /// paint walk re-emits it. The caller separately binds each driving cell to
    /// this node STYLE (so a press/hover flip re-marks the node through the
    /// ordinary flush); this method only establishes the selection and the
    /// first resolve.
    pub fn set_interaction_style(&mut self, id: NodeId, interaction: InteractionStyle) {
        if !self.arena.is_live(id) {
            return;
        }
        self.interaction[id.index() as usize] = Some(interaction);
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

    /// The descendant node `id` shows as active while it keeps the focus
    /// itself, if it is still in the tree.
    pub fn active_descendant(&self, id: NodeId) -> Option<NodeId> {
        self.actives
            .get(id.index())
            .filter(|&a| self.arena.is_live(a))
    }

    /// Show `active`, a descendant of `id`, as the one active in `id` while
    /// `id` keeps the focus — a combo box's highlighted option — or none, and
    /// mark `id` SEMANTICS-dirty. A stale handle is a no-op.
    pub fn set_active_descendant(&mut self, id: NodeId, active: Option<NodeId>) {
        if self.arena.is_live(id) && self.actives.set(id.index(), active) {
            self.mark_dirty(id, DirtyClass::SEMANTICS);
        }
    }

    /// What checking form control `id`'s value says of it; the default for a
    /// node nothing is checked of. An error node gone reads as none.
    pub fn validity(&self, id: NodeId) -> crate::semantics::Validity {
        let mut validity = self.validities.get(id.index()).unwrap_or_default();
        validity.error = validity.error.filter(|&e| self.arena.is_live(e));
        validity
    }

    /// Say what checking form control `id`'s value says of it, marking it
    /// SEMANTICS-dirty when that changed. A stale handle is a no-op.
    pub fn set_validity(&mut self, id: NodeId, validity: crate::semantics::Validity) {
        let value = (validity != crate::semantics::Validity::default()).then_some(validity);
        if self.arena.is_live(id) && self.validities.set(id.index(), value) {
            self.mark_dirty(id, DirtyClass::SEMANTICS);
        }
    }

    /// Where table, row or cell `id` stands: a table's rows and columns, a
    /// row's or cell's place, a column header's sort; the default for a node
    /// that says none.
    pub fn table_facts(&self, id: NodeId) -> crate::semantics::TableFacts {
        self.tables.get(id.index()).unwrap_or_default()
    }

    /// Say where table, row or cell `id` stands, marking it SEMANTICS-dirty
    /// when that changed. A stale handle is a no-op.
    pub fn set_table_facts(&mut self, id: NodeId, facts: crate::semantics::TableFacts) {
        let value = (facts != crate::semantics::TableFacts::default()).then_some(facts);
        if self.arena.is_live(id) && self.tables.set(id.index(), value) {
            self.mark_dirty(id, DirtyClass::SEMANTICS);
        }
    }

    /// A node's live accessibility state (checked / value / range / expanded),
    /// if any. `None` = a node that carries no reactive semantic state (a plain
    /// container, a label, or a control before its projection binding has run).
    #[inline]
    pub fn semantic_state(&self, id: NodeId) -> Option<SemanticState> {
        self.semantic_state[id.index() as usize]
    }

    /// Set a node's live accessibility state, replacing any prior value, and mark
    /// it SEMANTICS-dirty so the next derive re-folds it (an accessibility-state
    /// change invalidates SEMANTICS). A live-guarded write —
    /// a stale handle is a no-op. SEMANTICS bubbles, so ancestors learn their
    /// subtree changed without a separate mark.
    ///
    /// This is the write end of the reactive-state projection: a control's cell
    /// drives a parallel binding whose flush computes the new [`SemanticState`]
    /// and calls this (in the flush phase, where the state store is live), so the
    /// `&self` derive pass never reads the state store itself. A check's mark
    /// follows the state it carries: a chevron whether it is expanded, a box
    /// or a switch whether it is checked.
    pub fn set_semantic_state(&mut self, id: NodeId, state: SemanticState) {
        if !self.arena.is_live(id) {
            return;
        }
        let slot = &mut self.semantic_state[id.index() as usize];
        if slot.and_then(|s| s.selected) != state.selected {
            self.interaction_states.moved();
        }
        *slot = Some(state);
        self.mark_dirty(id, DirtyClass::SEMANTICS);
        if let Some(mark) = self.check_mark(id) {
            let follows = match mark.shape {
                crate::check_mark::CheckShape::Chevron => state.expanded,
                crate::check_mark::CheckShape::Radio => state.checked.or(state.selected),
                crate::check_mark::CheckShape::Plate => None,
                crate::check_mark::CheckShape::Chip { .. }
                | crate::check_mark::CheckShape::Segment => state.checked.or(state.selected),
                _ => state.checked,
            };
            if let Some(checked) = follows {
                self.set_check_mark(id, Some(CheckMark { checked, ..mark }));
            }
        }
    }

    /// Node `id`'s page dots, to change in place; the caller marks them.
    pub(crate) fn page_dots_mut(&mut self, id: NodeId) -> Option<&mut crate::page_dots::PageDots> {
        if !self.arena.is_live(id) {
            return None;
        }
        match self.content_payload[id.index() as usize].as_deref_mut() {
            Some(Content::PageDots(dots)) => Some(dots),
            _ => None,
        }
    }

    /// Node `id`'s progress indicator, to change in place; the caller marks
    /// it.
    pub(crate) fn progress_mut(&mut self, id: NodeId) -> Option<&mut crate::progress::Progress> {
        if !self.arena.is_live(id) {
            return None;
        }
        match self.content_payload[id.index() as usize].as_deref_mut() {
            Some(Content::Progress(progress)) => Some(progress),
            _ => None,
        }
    }

    /// Node `id`'s content, to change in place; the caller marks it.
    pub(crate) fn content_payload_mut(&mut self, id: NodeId) -> Option<&mut Content> {
        self.content_payload[id.index() as usize].as_deref_mut()
    }

    /// Node `id`'s spinner, to change in place; the caller marks it.
    pub(crate) fn spinner_mut(&mut self, id: NodeId) -> Option<&mut crate::spinner::Spinner> {
        if !self.arena.is_live(id) {
            return None;
        }
        match self.content_payload[id.index() as usize].as_deref_mut() {
            Some(Content::Spinner(spinner)) => Some(spinner),
            _ => None,
        }
    }

    /// What a check draws beside its caption, `None` for a node that is no
    /// check.
    #[inline]
    pub fn check_mark(&self, id: NodeId) -> Option<CheckMark> {
        self.check_marks[id.index() as usize].map(|(mark, _)| mark)
    }

    /// How far a check's mark shows moved toward checked, `0` to `1`: its
    /// state at rest, between them while it moves.
    pub(crate) fn check_progress(&self, id: NodeId) -> f32 {
        self.check_marks[id.index() as usize].map_or(0.0, |(_, shown)| shown)
    }

    /// Makes a node a check drawing `mark` beside its caption, or no check.
    /// A new state moves the mark there over its shape's timing (at once
    /// while motion is reduced), repainting it each frame; a check's first
    /// state, a new shape, or becoming or ceasing to be a check shows at once
    /// and re-measures it as well. A step of a chip's motion re-measures it
    /// too, its tick taking room. A live-guarded write — a stale handle is a
    /// no-op.
    pub fn set_check_mark(&mut self, id: NodeId, mark: Option<CheckMark>) {
        if !self.arena.is_live(id) {
            return;
        }
        let i = id.index() as usize;
        let prior = self.check_marks[i];
        if prior.map(|(m, _)| m) == mark {
            return;
        }
        let reshaped = prior.map(|(m, _)| m.shape) != mark.map(|m| m.shape);
        match (prior, mark) {
            (Some((_, shown)), Some(mark)) if !reshaped => {
                self.check_marks[i] = Some((mark, shown));
                self.move_look(id, LookValue::Check(mark.rest()), mark.timing(), None);
            }
            _ => {
                self.settle(id, &LookValue::Check(0.0));
                self.check_marks[i] = mark.map(|m| (m, m.rest()));
                // A new shape may add or drop the cross a chip's "Remove"
                // action stands for.
                self.mark_dirty(
                    id,
                    DirtyClass::PAINT
                        | DirtyClass::MEASURE
                        | DirtyClass::LAYOUT
                        | DirtyClass::SEMANTICS,
                );
            }
        }
    }

    /// A node's drawable content (text/image/path), if any. `None` = a node that
    /// draws only its background quad (the common case).
    #[inline]
    pub fn content_payload(&self, id: NodeId) -> Option<&Content> {
        self.content_payload[id.index() as usize].as_deref()
    }

    /// Set a node's drawable content, replacing any prior value, and mark it
    /// MEASURE | LAYOUT | PAINT dirty: content carries an intrinsic size, so a
    /// changed payload can resize a `Fit` node and must re-measure, re-lay-out,
    /// and repaint. A live-guarded write — a stale handle is a no-op.
    ///
    /// Text content additionally marks SEMANTICS
    /// (`text content -> MEASURE + LAYOUT + PAINT + SEMANTICS`). A text node's
    /// visible run is its accessible name when no separate label is authored, so
    /// replacing that run in place (a reactive text change re-shaped by the upper
    /// tier) must re-derive the accessibility tree. Image and path content carry
    /// no intrinsic accessible name, so they stay MEASURE | LAYOUT | PAINT — an
    /// image's alt text lives in authored [`Semantics`], not in its pixels.
    pub fn set_content_payload(&mut self, id: NodeId, content: Content) {
        if !self.arena.is_live(id) {
            return;
        }
        let mut classes = DirtyClass::MEASURE | DirtyClass::LAYOUT | DirtyClass::PAINT;
        if matches!(content, Content::Text { .. }) {
            classes |= DirtyClass::SEMANTICS;
        }
        let mut content = content;
        self.arriving(id, &mut content);
        self.leave_content(id);
        self.content_payload[id.index() as usize] = Some(Box::new(content));
        self.mark_dirty(id, classes);
    }

    /// Paint `id`'s box with `program` and `params`, its parameters encoded as
    /// one of the program's instances, sampling the images `images`, one per
    /// texture parameter in order; the program takes a slot in
    /// [`shaders`](Self::shaders), or keeps the one it has. Each image draws
    /// whatever of it the [`assets`](Self::assets) table holds: until all are
    /// resident the node draws nothing. A node already painting with the same
    /// program marks PAINT only — a program has no intrinsic size, so only a
    /// node taking it from other content re-measures. A live-guarded write —
    /// a stale handle is a no-op.
    pub fn set_shader(
        &mut self,
        id: NodeId,
        program: &Arc<BrushProgram>,
        params: Arc<[u8]>,
        images: &[ResourceId],
    ) {
        if !self.arena.is_live(id) {
            return;
        }
        for &resource in images {
            self.assets.hold(resource, id);
        }
        let images = images
            .iter()
            .map(|&resource| BrushImage {
                resource,
                texture: self.raster(resource),
            })
            .collect();
        let slot = self.shaders.take(program);
        let same = matches!(
            self.content_payload(id),
            Some(Content::Shader { program, .. }) if *program == slot
        );
        self.leave_content(id);
        let classes = if same {
            DirtyClass::PAINT
        } else {
            DirtyClass::MEASURE | DirtyClass::LAYOUT | DirtyClass::PAINT
        };
        self.content_payload[id.index() as usize] = Some(Box::new(Content::Shader {
            program: slot,
            params,
            images,
        }));
        self.mark_dirty(id, classes);
    }

    /// The texture `resource`'s raster is resident in, if it is.
    fn raster(&self, resource: ResourceId) -> Option<TextureId> {
        match self.assets.resolve(resource).0 {
            AssetDraw::Raster(texture) => Some(texture),
            AssetDraw::Nothing | AssetDraw::Failed | AssetDraw::Vector(_) => None,
        }
    }

    /// Replace the parameters of `id`'s shader, marking PAINT only. A no-op
    /// for a stale handle or a node not painting with a program.
    pub fn set_shader_params(&mut self, id: NodeId, params: Arc<[u8]>) {
        if !self.arena.is_live(id) {
            return;
        }
        let Some(content) = self.content_payload[id.index() as usize].as_deref_mut() else {
            return;
        };
        let Content::Shader {
            params: current, ..
        } = content
        else {
            return;
        };
        *current = params;
        self.mark_dirty(id, DirtyClass::PAINT);
    }

    /// The brush programs this store's nodes paint with, by slot.
    pub fn shaders(&self) -> &ShaderSlots {
        &self.shaders
    }

    /// Show `resource` on `id`, placed by `fit` and colored by `tint`, drawing
    /// whatever of it the [`assets`](Self::assets) table holds (nothing, until
    /// it is decoded; a raster's size but no pixels, until it is resident). A
    /// node already showing the resource at the same natural size marks PAINT
    /// only. A live-guarded write — a stale handle is a no-op.
    pub fn set_asset(
        &mut self,
        id: NodeId,
        resource: ResourceId,
        fit: AssetFit,
        tint: Option<Rgba>,
    ) {
        self.show_asset(id, resource, fit, tint, false);
    }

    /// [`set_asset`](Self::set_asset), its raster drawn flipped left to
    /// right when `mirrored`.
    fn show_asset(
        &mut self,
        id: NodeId,
        resource: ResourceId,
        fit: AssetFit,
        tint: Option<Rgba>,
        mirrored: bool,
    ) {
        if !self.arena.is_live(id) {
            return;
        }
        let (draw, [w, h]) = self.assets.resolve(resource);
        let natural = Vec2::new(w, h);
        let content = Content::Asset {
            resource,
            draw,
            natural,
            fit,
            tint,
            mirrored,
        };
        if self.content_payload[id.index() as usize].as_deref() == Some(&content) {
            return;
        }
        self.assets.hold(resource, id);
        self.leave_content(id);
        let slot = &mut self.content_payload[id.index() as usize];
        let same_size = matches!(
            slot.as_deref(),
            Some(Content::Asset { natural: n, .. }) if *n == natural
        );
        let shown = match &content {
            Content::Asset { draw, .. } if !self.asset_fallbacks.is_empty() => Some(draw.clone()),
            _ => None,
        };
        *slot = Some(Box::new(content));
        let classes = if same_size {
            DirtyClass::PAINT
        } else {
            DirtyClass::MEASURE | DirtyClass::LAYOUT | DirtyClass::PAINT
        };
        self.mark_dirty(id, classes);
        if let Some(draw) = shown {
            self.show_fallbacks(id, &draw);
        }
    }

    /// The assets this store's nodes show.
    pub fn assets(&self) -> &AssetTable {
        &self.assets
    }

    /// The assets this store's nodes show, to offer encoded bytes to.
    pub fn assets_mut(&mut self) -> &mut AssetTable {
        &mut self.assets
    }

    /// Stores `payload` as `resource`'s at `revision` and rewrites the nodes
    /// showing it; a payload older than the table's is ignored. A vector draws
    /// at once; a raster keeps drawing its resident pixels until
    /// [`attach_texture`](Self::attach_texture) gives the new ones.
    pub fn provide_asset(
        &mut self,
        resource: ResourceId,
        revision: ResourceRevision,
        payload: AssetPayload,
    ) {
        if self.assets.provide(resource, revision, payload) {
            self.refresh_asset(resource);
        }
    }

    /// Records `texture`, uploaded from `resource`'s raster at `revision`, and
    /// rewrites the nodes showing it. Returns the texture for the caller to
    /// release once no frame in flight draws it: the one replaced, or `texture`
    /// itself when the table has moved past `revision`.
    pub fn attach_texture(
        &mut self,
        resource: ResourceId,
        revision: ResourceRevision,
        texture: TextureId,
    ) -> Option<TextureId> {
        match self.assets.attach(resource, revision, texture) {
            Ok(old) => {
                self.refresh_asset(resource);
                old
            }
            Err(unused) => Some(unused),
        }
    }

    /// Every live node showing `resource`, or a brush sampling it. Cold: a
    /// walk of the content column.
    pub fn asset_nodes(&self, resource: ResourceId) -> impl Iterator<Item = NodeId> + '_ {
        self.content_payload
            .iter()
            .enumerate()
            .filter_map(move |(i, content)| match content.as_deref() {
                Some(Content::Asset { resource: r, .. }) if *r == resource => {
                    self.arena.live_id(i as u32)
                }
                Some(Content::Shader { images, .. })
                    if images.iter().any(|image| image.resource == resource) =>
                {
                    self.arena.live_id(i as u32)
                }
                _ => None,
            })
    }

    /// Rewrites every live node showing `resource` to what the table now
    /// resolves it to: PAINT when its natural size holds, MEASURE | LAYOUT |
    /// PAINT when it moves; a brush sampling it takes its texture, PAINT.
    /// Cold: a walk of the content column, once per new payload.
    fn refresh_asset(&mut self, resource: ResourceId) {
        let (draw, [w, h]) = self.assets.resolve(resource);
        let natural = Vec2::new(w, h);
        let raster = match draw {
            AssetDraw::Raster(texture) => Some(texture),
            AssetDraw::Nothing | AssetDraw::Failed | AssetDraw::Vector(_) => None,
        };
        for i in 0..self.content_payload.len() {
            if let Some(Content::Shader { images, .. }) = self.content_payload[i].as_deref_mut() {
                let mut moved = false;
                for image in images.iter_mut().filter(|image| image.resource == resource) {
                    moved |= image.texture != raster;
                    image.texture = raster;
                }
                if moved && let Some(id) = self.arena.live_id(i as u32) {
                    self.mark_dirty(id, DirtyClass::PAINT);
                }
                continue;
            }
            let Some(Content::Asset {
                resource: r,
                draw: d,
                natural: n,
                ..
            }) = self.content_payload[i].as_deref_mut()
            else {
                continue;
            };
            if *r != resource || (*d == draw && *n == natural) {
                continue;
            }
            let classes = if *n == natural {
                DirtyClass::PAINT
            } else {
                DirtyClass::MEASURE | DirtyClass::LAYOUT | DirtyClass::PAINT
            };
            *d = draw.clone();
            *n = natural;
            if let Some(id) = self.arena.live_id(i as u32) {
                self.mark_dirty(id, classes);
                self.show_fallbacks(id, &draw);
            }
        }
    }

    /// Shows `loading` in place of asset node `id`'s picture while it loads
    /// and `failed` once it fails, hiding each otherwise. A live-guarded
    /// write — a stale handle is a no-op.
    pub fn set_asset_fallbacks(
        &mut self,
        id: NodeId,
        loading: Option<NodeId>,
        failed: Option<NodeId>,
    ) {
        if !self.arena.is_live(id) {
            return;
        }
        let arena = &self.arena;
        self.asset_fallbacks
            .retain(|f| f.node != id && arena.is_live(f.node));
        self.asset_fallbacks.push(AssetFallbacks {
            node: id,
            loading,
            failed,
        });
        let draw = match self.content_payload(id) {
            Some(Content::Asset { draw, .. }) => draw.clone(),
            _ => AssetDraw::Nothing,
        };
        self.show_fallbacks(id, &draw);
    }

    /// Shows asset node `id`'s fallback for what it now draws, hiding the
    /// other.
    fn show_fallbacks(&mut self, id: NodeId, draw: &AssetDraw) {
        let Some(f) = self.asset_fallbacks.iter().find(|f| f.node == id) else {
            return;
        };
        let (loading, failed) = (f.loading, f.failed);
        if let Some(node) = loading {
            self.set_hidden(node, *draw != AssetDraw::Nothing);
        }
        if let Some(node) = failed {
            self.set_hidden(node, *draw != AssetDraw::Failed);
        }
    }

    /// Marks PAINT on every node showing `resource`, a brush sampling it
    /// included: its texture's texels changed where they lie.
    pub fn repaint_asset(&mut self, resource: ResourceId) {
        let mut nodes = std::mem::take(&mut self.scratch_asset_nodes);
        nodes.clear();
        nodes.extend_from_slice(self.assets.nodes(resource));
        for &node in &nodes {
            self.mark_dirty(node, DirtyClass::PAINT);
        }
        self.scratch_asset_nodes = nodes;
    }

    /// Shows on `id` the video `source` plays, placed into its box by `fit`
    /// and multiplied by `tint`: its pictures once its player hands them
    /// over, nothing (its stand-ins) before. A node already playing the same
    /// source keeps playing it where it is; another source starts over,
    /// paused. A live-guarded write — a stale handle is a no-op.
    pub fn set_video(
        &mut self,
        id: NodeId,
        source: &crate::video::VideoSource,
        fit: AssetFit,
        tint: Option<Rgba>,
    ) {
        if !self.arena.is_live(id) {
            return;
        }
        // The entry is kept out while its content is rewritten, which would
        // let go of it.
        let kept = if self.videos.get(id).is_some_and(|v| v.source.same(source)) {
            self.videos.forget(id)
        } else {
            None
        };
        let (resource, mirrored) = match &kept {
            Some(entry) => (entry.resource, entry.mirrored()),
            None => (self.videos.show(id, source).0, false),
        };
        self.assets.host(resource);
        self.show_asset(id, resource, fit, tint, mirrored);
        if let Some(entry) = kept {
            self.videos.restore(entry);
        }
    }

    /// Asks video node `id`'s picture to show mirrored, or not; `None`
    /// mirrors a camera facing the user. A node showing no video ignores it.
    pub fn set_video_mirror(&mut self, id: NodeId, mirror: Option<bool>) {
        if let Some(video) = self.videos.get_mut(id)
            && video.mirror != mirror
        {
            video.mirror = mirror;
            self.mirror_video(id);
        }
    }

    /// Draws video node `id`'s picture mirrored as its entry now says: the
    /// mirror asked of it, or the facing its camera reported. PAINT where
    /// that changed.
    pub fn mirror_video(&mut self, id: NodeId) {
        let Some(mirror) = self.videos.get(id).map(|v| v.mirrored()) else {
            return;
        };
        if let Some(Content::Asset { mirrored, .. }) =
            self.content_payload[id.index() as usize].as_deref_mut()
            && *mirrored != mirror
        {
            *mirrored = mirror;
            self.mark_dirty(id, DirtyClass::PAINT);
        }
    }

    /// The video nodes, with their requests and reports.
    pub fn videos(&self) -> &crate::video::VideoTable {
        &self.videos
    }

    /// The video nodes, for the facade to take their requests and report
    /// what their players do; a report changes what a watcher of
    /// [`NoticeKinds::VIDEO`](crate::notice::NoticeKinds::VIDEO) hears next.
    pub fn videos_mut(&mut self) -> &mut crate::video::VideoTable {
        &mut self.videos
    }

    /// What video node `id`'s player reports; `None` for a node showing no
    /// video.
    pub fn video_status(&self, id: NodeId) -> Option<crate::video::VideoStatus> {
        self.videos.get(id).map(|v| v.status)
    }

    /// What is asked of video node `id`; `None` for a node showing no video.
    pub fn playback(&self, id: NodeId) -> Option<crate::video::Playback> {
        self.videos.get(id).map(|v| v.playback)
    }

    /// Asks video node `id` to play as `playback` says. A node showing no
    /// video ignores it.
    pub fn set_playback(&mut self, id: NodeId, playback: crate::video::Playback) {
        if let Some(video) = self.videos.get_mut(id) {
            video.playback = playback;
        }
    }

    /// Asks video node `id` to play as `playback` says when its build asks
    /// for something other than it did last; what was asked since —
    /// a pause from a handler, its end — stands otherwise.
    pub fn declare_playback(&mut self, id: NodeId, playback: crate::video::Playback) {
        if let Some(video) = self.videos.get_mut(id)
            && video.declared != Some(playback)
        {
            video.declared = Some(playback);
            video.playback = playback;
        }
    }

    /// Asks video node `id` to play, or to pause.
    pub fn set_video_playing(&mut self, id: NodeId, playing: bool) {
        if let Some(video) = self.videos.get_mut(id) {
            video.playback.playing = playing;
        }
    }

    /// Asks video node `id` to move to `time`; the picture there shows even
    /// while it is paused.
    pub fn seek_video(&mut self, id: NodeId, time: std::time::Duration) {
        if let Some(video) = self.videos.get_mut(id) {
            video.seek = Some(time);
        }
    }

    /// Records the natural size of the video shown as `resource` and lays
    /// out again the nodes showing it when it moved.
    pub fn size_video(&mut self, resource: ResourceId, width: u32, height: u32) {
        if self
            .assets
            .size_hosted(resource, [width as f32, height as f32])
        {
            self.refresh_asset(resource);
        }
    }

    /// Shows `texture`, of `texels`, as the picture of the video shown as
    /// `resource`, rewriting the nodes showing it. Returns the texture for
    /// the caller to release once no frame in flight draws it: the one
    /// replaced, or `texture` itself when `resource` is no video's. A new
    /// picture written into the texture shown already needs only
    /// [`repaint_asset`](Self::repaint_asset).
    pub fn show_video_frame(
        &mut self,
        resource: ResourceId,
        texture: TextureId,
        texels: (u32, u32),
    ) -> Option<TextureId> {
        let old = self.assets.attach_hosted(resource, texture, texels);
        if old != Some(texture) {
            self.refresh_asset(resource);
        }
        old
    }

    /// Records that the video shown as `resource` cannot play: its nodes show
    /// their error stand-ins. Returns its texture for the caller to release.
    pub fn fail_video(&mut self, resource: ResourceId) -> Option<TextureId> {
        let old = self.assets.fail_hosted(resource);
        self.refresh_asset(resource);
        old
    }

    /// Forgets the video shown as `resource` once no node plays it, returning
    /// its texture for the caller to release.
    pub fn forget_video(&mut self, resource: ResourceId) -> Option<TextureId> {
        if self.videos.of(resource).is_some() {
            return None;
        }
        self.assets.forget_hosted(resource)
    }

    /// Holds the animation asset node `id` shows still, on its first frame,
    /// or lets it play (the default). An animation several nodes show plays
    /// while any of them on screen lets it. A live-guarded write — a stale
    /// handle is a no-op.
    pub fn set_asset_still(&mut self, id: NodeId, still: bool) {
        if !self.arena.is_live(id) {
            return;
        }
        let arena = &self.arena;
        self.still_assets.retain(|&n| n != id && arena.is_live(n));
        if still {
            self.still_assets.push(id);
        }
    }

    /// Whether asset node `id` lets an animation it shows play.
    pub fn plays_asset(&self, id: NodeId) -> bool {
        !self.still_assets.contains(&id)
    }

    /// Whether any of node `id`'s box shows inside `viewport`, a rect in
    /// world space: it and its ancestors are shown — not hidden, folded or
    /// paged away, nor fully transparent — and no ancestor that clips cuts it
    /// away. Cold: a walk up its ancestors.
    pub fn on_screen(&self, id: NodeId, viewport: Rect) -> bool {
        if !self.arena.is_live(id) {
            return false;
        }
        let mut rect = self.world(id).intersect(viewport);
        let mut at = Some(id);
        while let Some(node) = at {
            if self.concealed(node.index() as usize) || self.opacity(node) <= 0.0 {
                return false;
            }
            if node != id && self.clips(node) {
                rect = rect.intersect(self.world(node));
            }
            if rect.w <= 0.0 || rect.h <= 0.0 {
                return false;
            }
            at = self.arena.parent(node);
        }
        true
    }

    /// Records that `resource`'s bytes at `revision` could not be read or did
    /// not decode, and rewrites the nodes showing it; ignored when the table
    /// holds newer bytes or a payload as new.
    pub fn fail_asset(&mut self, resource: ResourceId, revision: ResourceRevision) {
        if self.assets.fail(resource, revision) {
            self.refresh_asset(resource);
        }
    }

    /// Lets go of the program or scene slot node `index` paints with and of
    /// the assets it shows, if it does: its content is about to change.
    fn leave_content(&mut self, node: NodeId) {
        match self.content_payload[node.index() as usize].as_deref() {
            Some(Content::Shader {
                program, images, ..
            }) => {
                self.shaders.leave(*program);
                for image in images.iter() {
                    self.assets.release(image.resource, node);
                }
            }
            Some(Content::Scene { slot, .. }) => {
                for model in self.scenes.leave(*slot) {
                    self.assets.release(model, node);
                }
            }
            Some(Content::Asset { resource, .. }) => {
                let resource = *resource;
                self.assets.release(resource, node);
                if self
                    .videos
                    .get(node)
                    .is_some_and(|v| v.resource == resource)
                {
                    self.videos.forget(node);
                }
            }
            _ => {}
        }
    }

    /// Shows a 3D scene on `id`: the node takes a slot in
    /// [`scenes`](Self::scenes), or keeps the one it has, and returns it.
    /// Fill the slot's frame and [`commit_scene`](Self::commit_scene) it to
    /// show it. `None` for a stale handle.
    pub fn set_scene(&mut self, id: NodeId) -> Option<SceneSlot> {
        if !self.arena.is_live(id) {
            return None;
        }
        if let Some(Content::Scene { slot, .. }) = self.content_payload(id) {
            return Some(*slot);
        }
        self.leave_content(id);
        let slot = self.scenes.take(id);
        self.content_payload[id.index() as usize] =
            Some(Box::new(Content::Scene { slot, revision: 0 }));
        self.mark_dirty(
            id,
            DirtyClass::MEASURE | DirtyClass::LAYOUT | DirtyClass::PAINT,
        );
        Some(slot)
    }

    /// glTF model `asset` resident in this store's window, for the filler of
    /// `slot` to draw with
    /// [`SceneFrame::push_model`](viso_render::scene3d::SceneFrame::push_model).
    /// The slot's node holds the asset while it shows the scene: the first
    /// call offers its bytes to the [`assets`](Self::assets) table, which the
    /// facade decodes off the frame path and uploads; `None` until then, and
    /// for a stale slot or a model that does not decode. Once it is resident
    /// the slot is [`reset`](crate::scenes::SceneSlots::reset), for its filler
    /// to draw it afresh.
    pub fn scene_model(&mut self, slot: SceneSlot, asset: &Asset) -> Option<Arc<SceneModel>> {
        let (node, held) = self.scenes.hold_model(slot, asset.id)?;
        if !held {
            self.offer_asset(asset);
            self.assets.hold(asset.id, node);
        }
        self.assets.model(asset.id).cloned()
    }

    /// Lets the node of `slot` stop holding model `id`, which
    /// [`scene_model`](Self::scene_model) had it hold: the asset table may
    /// sweep the model once no node holds it. A filler drawing a changing
    /// set of models — tiles coming into view and leaving it — lets go of
    /// each it no longer draws.
    pub fn release_scene_model(&mut self, slot: SceneSlot, id: ResourceId) {
        if let Some(node) = self.scenes.release_model(slot, id) {
            self.assets.release(id, node);
        }
    }

    /// Records `model`, uploaded from `resource`'s model at `revision`, and
    /// resets the scenes holding it so their fillers draw it. The model it
    /// replaces is retired for the facade to release
    /// ([`AssetTable::take_retired_models`]); `model` comes back when the
    /// table has moved past `revision`.
    pub fn attach_model(
        &mut self,
        resource: ResourceId,
        revision: ResourceRevision,
        model: SceneModel,
    ) -> Result<(), SceneModel> {
        self.assets.attach_model(resource, revision, model)?;
        for i in 0..self.assets.nodes(resource).len() {
            let node = self.assets.nodes(resource)[i];
            if let Some(Content::Scene { slot, .. }) = self.content_payload(node) {
                let slot = *slot;
                self.scenes.reset(slot);
                self.mark_dirty(node, DirtyClass::PAINT);
            }
        }
        Ok(())
    }

    /// Offers `asset`'s bytes to the asset table, or where to read them from,
    /// unless the table knows it already.
    pub(crate) fn offer_asset(&mut self, asset: &Asset) {
        if self.assets.knows(asset.id) {
            return;
        }
        match &asset.source {
            AssetSource::Bytes(bytes) => {
                self.assets
                    .offer(asset.id, ResourceRevision::default(), bytes.clone());
            }
            AssetSource::File(path) => {
                let origin = AssetOrigin::File(Arc::clone(path));
                self.assets.locate(asset.id, origin);
            }
            AssetSource::Url(url) => {
                let origin = AssetOrigin::Url(Arc::clone(url));
                self.assets.locate(asset.id, origin);
            }
        }
    }

    /// The 3D scenes this store's nodes show, by slot.
    pub fn scenes(&self) -> &crate::scenes::SceneSlots {
        &self.scenes
    }

    /// The 3D scenes this store's nodes show, to fill, or for the facade to
    /// sync into a renderer.
    pub fn scenes_mut(&mut self) -> &mut crate::scenes::SceneSlots {
        &mut self.scenes
    }

    /// Hands the frame of `slot` over as its next revision and marks its
    /// node `PAINT`.
    pub fn commit_scene(&mut self, slot: SceneSlot) {
        self.scenes.commit(slot);
        self.mark_committed_scenes();
    }

    /// Writes each committed scene's revision into its node's content and
    /// marks the node `PAINT`.
    pub(crate) fn mark_committed_scenes(&mut self) {
        let mut committed = std::mem::take(&mut self.scratch_scene_marks);
        committed.clear();
        committed.extend(self.scenes.take_committed());
        for &(node, revision) in &committed {
            if !self.arena.is_live(node) {
                continue;
            }
            if let Some(Content::Scene { revision: r, .. }) =
                self.content_payload[node.index() as usize].as_deref_mut()
            {
                *r = revision;
                self.mark_dirty(node, DirtyClass::PAINT);
            }
        }
        self.scratch_scene_marks = committed;
    }

    /// The arena alongside the scenes and the frame clients, for the clients
    /// to fill the scenes under their owners.
    pub(crate) fn scenes_and_frame_clients(
        &mut self,
    ) -> (
        &NodeArena,
        &mut crate::scenes::SceneSlots,
        &crate::frame_client::FrameClients,
    ) {
        (&self.arena, &mut self.scenes, &self.frame_clients)
    }

    /// Frees node `node`'s slot, letting go of the program it paints with, its
    /// event handlers and the parts of the controls it roots. A node an instance built is reported to the
    /// binding table, and a slot of an instance's block stays with the block.
    fn free_node(&mut self, node: NodeId) -> bool {
        if !self.arena.is_live(node) {
            return false;
        }
        let index = node.index() as usize;
        self.leave_content(node);
        self.content_payload[index] = None;
        // The handlers and the controls' parts go with the node: a handle to a
        // control it rooted goes stale now.
        self.handlers[index] = None;
        self.key_handlers[index] = None;
        if !self.control_parts.is_empty() {
            self.control_parts
                .retain(|(slot, _)| *slot as usize != index);
        }
        let owner = self.owner[index];
        if owner == NO_OWNER {
            return self.arena.free(node);
        }
        self.freed_owners.push(owner & !OWNER_KEPT);
        if owner & OWNER_KEPT != 0 {
            return self.arena.free_kept(node);
        }
        self.owner[index] = NO_OWNER;
        self.arena.free(node)
    }

    /// Gives node `owner` the parts of a control it roots, kept until it is
    /// freed. A live-guarded write.
    pub(crate) fn own_control_parts(&mut self, owner: NodeId, parts: Rc<dyn Any>) {
        if self.arena.is_live(owner) {
            self.control_parts.push((owner.index(), parts));
        }
    }

    /// Write back a run reshaped at a layout-assigned width, marking
    /// MEASURE | LAYOUT | PAINT but **not** SEMANTICS. Used only by the facade's
    /// reflow drain: a width-only reshape wraps the same text to a new height, so
    /// it must re-measure/re-lay-out/repaint, but the accessible name is
    /// unchanged — dirtying the semantics tree on every resize frame would be
    /// spurious. The text-*content*
    /// change path ([`Self::set_content_payload`]) still marks SEMANTICS; this is
    /// the reflow-only sibling. A live-guarded write — a stale handle is a no-op.
    pub fn set_reflowed_content(&mut self, id: NodeId, mut content: Content) {
        if !self.arena.is_live(id) {
            return;
        }
        self.arriving(id, &mut content);
        self.leave_content(id);
        self.content_payload[id.index() as usize] = Some(Box::new(content));
        self.mark_dirty(
            id,
            DirtyClass::MEASURE | DirtyClass::LAYOUT | DirtyClass::PAINT,
        );
    }

    /// A node's pending unshaped text request, if any. Set by authoring, read
    /// and cleared by the upper tier that shapes text (see
    /// [`Self::take_text_requests`]).
    #[inline]
    pub fn text_request(&self, id: NodeId) -> Option<&TextRequest> {
        self.text_request[id.index() as usize].as_deref()
    }

    /// Record an unshaped text request on a node, replacing any prior request.
    /// `viso-ui` cannot shape it; the request waits in a cold side column until
    /// an upper tier drains it via [`Self::take_text_requests`], shapes it, and
    /// writes the shaped [`Content`] back with [`Self::set_content_payload`]. A
    /// live-guarded write — a stale handle is a no-op.
    pub fn set_text_request(&mut self, id: NodeId, request: TextRequest) {
        if !self.arena.is_live(id) {
            return;
        }
        if request.spans.iter().any(|s| s.style.link.is_some()) || !self.links(id).is_empty() {
            let text = request.text.as_str();
            let (targets, labels) = request
                .spans
                .iter()
                .filter_map(|s| {
                    let label = text.get(s.range.clone()).unwrap_or_default();
                    Some((s.style.link.clone()?, Box::from(label)))
                })
                .unzip();
            self.set_links(id, targets, labels);
        }
        self.text_request[id.index() as usize] = Some(Box::new(request));
    }

    /// Show `request` on text node `id` in place of the text it shows: it is
    /// shaped again before the next layout, and a label's accessible name
    /// follows it. A stale handle is a no-op.
    pub fn set_text(&mut self, id: NodeId, request: TextRequest) {
        if !self.arena.is_live(id) {
            return;
        }
        if let Some(s) = self.semantics(id)
            && s.role == Role::Label
            && s.label.as_deref() != Some(request.text.as_str())
        {
            let named = s.clone().with_label(request.text.clone());
            self.set_semantics(id, named);
        }
        self.set_text_request(id, request);
    }

    /// Move every pending text request out into `out` as `(NodeId, request)`
    /// pairs, clearing the column. The shaping tier calls this, shapes each
    /// request, and writes the result back with [`Self::set_content_payload`] —
    /// splitting the read (here) from the write avoids borrowing the store for
    /// the shape step. `out` is cleared first; only live nodes are yielded.
    /// This is a cold path — it runs only when text is (re)declared, not per
    /// frame — so moving the boxed requests out is fine.
    pub fn take_text_requests(&mut self, out: &mut Vec<(NodeId, Box<TextRequest>)>) {
        out.clear();
        for (i, slot) in self.text_request.iter_mut().enumerate() {
            if let Some(req) = slot.take()
                && let Some(id) = self.arena.live_id(i as u32)
            {
                out.push((id, req));
            }
        }
    }

    /// Move every queued text reflow out into `out` as `(NodeId, width)` pairs,
    /// clearing the buffer. The facade calls this after a layout pass, reshapes
    /// each run at its assigned width, and writes the wrapped run back with
    /// [`Self::set_reflowed_content`]. `out` is cleared first. Like
    /// [`take_animation_requests`](Self::take_animation_requests) the queue is
    /// not index-aligned (each entry names its own node), so this is a flat move
    /// with no per-node scan — empty and allocation-free in the common frame
    /// with no width-changed wrap-eligible text. The recorder already
    /// live-guards each id, so no re-validation is needed here.
    pub fn take_text_reflows(&mut self, out: &mut Vec<(NodeId, f32)>) {
        out.clear();
        out.append(&mut self.text_reflows);
    }

    /// Whether a layout queued a text reflow not yet taken.
    #[inline]
    pub fn has_text_reflows(&self) -> bool {
        !self.text_reflows.is_empty()
    }

    /// Whether a node declared text not yet shaped. Scans the column: for
    /// the cold paths that wait on text, never a frame.
    pub fn has_text_requests(&self) -> bool {
        self.text_request.iter().any(Option::is_some)
    }

    /// Enqueue an edit intent a pointer handler recorded for `node`. It waits
    /// here until [`reconcile`](crate::text_edit::reconcile) moves it onto the
    /// node's edit buffer.
    #[inline]
    pub fn queue_edit(&mut self, node: NodeId, intent: crate::text_edit::EditIntent) {
        self.edit_requests.push((node, intent));
    }

    /// Whether any pointer-recorded edit intent is waiting for reconcile.
    #[inline]
    pub fn has_edit_requests(&self) -> bool {
        !self.edit_requests.is_empty()
    }

    /// Move every queued edit intent out into `out` (cleared first).
    pub fn take_edit_requests(&mut self, out: &mut Vec<(NodeId, crate::text_edit::EditIntent)>) {
        out.clear();
        out.append(&mut self.edit_requests);
    }

    /// Seed text field `node` with `seed`: its declared style and look, and
    /// its text when a value drives it and the field does not already hold it. It waits here until
    /// [`reconcile`](crate::text_edit::reconcile) seeds the node's edit buffer,
    /// before that frame's edits apply. A seed reports no change: it is the
    /// state the field shows, not an edit. A stale handle is a no-op.
    pub fn seed_text(&mut self, node: NodeId, seed: crate::text_edit::FieldSeed) {
        if self.arena.is_live(node) {
            self.text_seeds.push((node, seed));
        }
    }

    /// Take the seeds queued for `node`, the latest of them: what a carried
    /// edit buffer takes its style from instead of reconcile seeding it.
    pub fn take_text_seed(&mut self, node: NodeId) -> Option<crate::text_edit::FieldSeed> {
        let mut latest = None;
        self.text_seeds.retain(|(seeded, seed)| {
            if *seeded == node {
                latest = Some(seed.clone());
            }
            *seeded != node
        });
        latest
    }

    /// Whether any text field seed is waiting for reconcile.
    #[inline]
    pub fn has_text_seeds(&self) -> bool {
        !self.text_seeds.is_empty()
    }

    /// Move every queued text field seed out into `out` (cleared first).
    pub fn take_text_seeds(&mut self, out: &mut Vec<(NodeId, crate::text_edit::FieldSeed)>) {
        out.clear();
        out.append(&mut self.text_seeds);
    }

    /// Enqueue a transform animation the router drained off an
    /// [`EventCx`](crate::context::EventCx) this frame. It sits in the store's
    /// transient handoff buffer until the driver drains it with
    /// [`take_animation_requests`](Self::take_animation_requests) into its live
    /// registry (the store holds no registry of its own — the driver owns that).
    /// A cold path: it runs only when a handler starts a slide, not per frame.
    #[inline]
    pub fn queue_animation(&mut self, anim: TranslateAnim) {
        self.animation_requests.push(anim);
    }

    /// Move every queued animation out into `out`, clearing the buffer. The
    /// driver calls this each frame and hands the drained slides to its
    /// [`AnimationRegistry`](crate::animation::AnimationRegistry) before ticking.
    /// `out` is cleared first. Unlike [`take_text_requests`](Self::take_text_requests)
    /// the queue is not index-aligned (an animation names its own node), so this
    /// is a flat move with no per-node scan — empty and allocation-free in the
    /// common frame that queued nothing.
    pub fn take_animation_requests(&mut self, out: &mut Vec<TranslateAnim>) {
        out.clear();
        out.append(&mut self.animation_requests);
    }

    /// Enqueue a one-shot timer arm the router drained off an
    /// [`EventCx`](crate::context::EventCx) this frame. It sits in the store's
    /// transient handoff buffer until the driver drains it with
    /// [`take_timer_requests`](Self::take_timer_requests) and arms it on its live
    /// [`TimerRegistry`](crate::timer::TimerRegistry) (the store holds no registry
    /// of its own — the driver owns that). A cold path: it runs only when a
    /// handler arms a timer (a toast's show), not per frame.
    #[inline]
    pub fn queue_timer(&mut self, req: TimerRequest) {
        self.timer_requests.push(req);
    }

    /// Move every queued timer arm out into `out`, clearing the buffer. The
    /// driver calls this each frame and arms the drained requests on its
    /// [`TimerRegistry`](crate::timer::TimerRegistry) against the frame's `now`.
    /// `out` is cleared first; the queue is not index-aligned (a request names its
    /// own node), so this is a flat move with no per-node scan — empty and
    /// allocation-free in the common frame that armed nothing.
    pub fn take_timer_requests(&mut self, out: &mut Vec<TimerRequest>) {
        out.clear();
        out.append(&mut self.timer_requests);
    }

    /// Point this store's tasks at the loop `waker` and lend `services` to
    /// every dispatch. The driver calls it once, before any handler spawns; a
    /// store that already holds tasks keeps its waker.
    #[doc(hidden)]
    pub fn __install_task_host(&mut self, waker: LoopWaker, services: Option<Rc<dyn Any>>) {
        if self.tasks.is_empty() {
            self.tasks = TaskSet::new(waker);
        }
        self.services = services;
    }

    /// The service registry lent to dispatches, type-erased.
    #[doc(hidden)]
    pub fn __services(&self) -> Option<&dyn Any> {
        self.services.as_deref()
    }

    /// Apply the task spawns and cancellations `node`'s handler recorded. A
    /// spawn for a node freed since is dropped unpolled.
    pub fn apply_task_ops(&mut self, node: NodeId, ops: TaskOps) {
        for (id, owner, future) in ops.spawns {
            let owner = owner.unwrap_or(node);
            if self.arena.is_live(owner) {
                self.tasks.spawn(id, owner, future);
            }
        }
        for id in ops.cancels {
            self.tasks.cancel(id);
        }
    }

    /// Apply task spawns that each name their owner, as effect bodies record
    /// them; one whose owner is freed is dropped unpolled.
    pub(crate) fn apply_owned_task_ops(&mut self, ops: TaskOps) {
        for (id, owner, future) in ops.spawns {
            if let Some(owner) = owner.filter(|&owner| self.arena.is_live(owner)) {
                self.tasks.spawn(id, owner, future);
            }
        }
        for id in ops.cancels {
            self.tasks.cancel(id);
        }
    }

    /// Run `future` as a task owned by `owner`, first polled at the next
    /// [`poll_tasks`](Self::poll_tasks); `None`, dropping it, when `owner` is
    /// freed.
    pub fn spawn_task(&mut self, owner: NodeId, future: TaskFuture) -> Option<TaskId> {
        if !self.arena.is_live(owner) {
            return None;
        }
        let id = TaskId::fresh();
        self.tasks.spawn(id, owner, future);
        Some(id)
    }

    /// Drop task `id`; a finished or unknown id is a no-op.
    pub fn cancel_task(&mut self, id: TaskId) {
        self.tasks.cancel(id);
    }

    /// Whether a task is due a poll: spawned or woken since the last
    /// [`poll_tasks`](Self::poll_tasks).
    #[inline]
    pub fn tasks_woken(&self) -> bool {
        self.tasks.is_woken()
    }

    /// Poll the due tasks and push each finished one's continuation onto
    /// `out` (`None` for a task that writes nothing back).
    pub fn poll_tasks(&mut self, out: &mut Vec<Option<Continuation>>) {
        self.tasks.poll(out);
    }

    /// The number of live tasks.
    pub fn task_count(&self) -> usize {
        self.tasks.len()
    }

    /// Drop every task `node` owns.
    fn cancel_tasks_for(&mut self, node: NodeId) {
        if !self.tasks.is_empty() {
            self.tasks.cancel_where(|owner| owner == node);
        }
    }

    /// Enqueue a window-open request the router drained off an
    /// [`EventCx`](crate::context::EventCx) this frame. It sits in the store's
    /// transient handoff buffer until the facade drains it with
    /// [`take_window_opens`](Self::take_window_opens) — where it holds a
    /// scheduling context — to create the OS window and build its tree. A cold
    /// path: it runs only when a handler opens a window, not per frame.
    #[inline]
    pub fn queue_window_open(&mut self, req: WindowOpenRequest) {
        self.window_opens.push(req);
    }

    /// Move every queued window-open request out into `out`, clearing the buffer.
    /// The facade calls this each frame and opens a window per request. `out` is
    /// cleared first; the queue is not index-aligned, so this is a flat move with
    /// no per-node scan — empty and allocation-free in the common frame that
    /// opened no window.
    pub fn take_window_opens(&mut self, out: &mut Vec<WindowOpenRequest>) {
        out.clear();
        out.append(&mut self.window_opens);
    }

    /// Enqueue a window-close request (a raw window id) the router drained off an
    /// [`EventCx`](crate::context::EventCx) this frame. It sits in the store's
    /// transient handoff buffer until the facade drains it with
    /// [`take_window_closes`](Self::take_window_closes) and asks the platform to
    /// close that window. A cold path: it runs only when a handler closes a
    /// window, not per frame.
    #[inline]
    pub fn queue_window_close(&mut self, window: u32) {
        self.window_closes.push(window);
    }

    /// Move every queued window-close id out into `out`, clearing the buffer. The
    /// facade calls this each frame and closes a window per id. `out` is cleared
    /// first; empty and allocation-free in the common frame that closed no window.
    pub fn take_window_closes(&mut self, out: &mut Vec<u32>) {
        out.clear();
        out.append(&mut self.window_closes);
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
    /// scroll offset shifts what a node hits without disturbing its layout;
    /// flipping the hittable flag here only gates participation and needs no
    /// recompute of its own.
    pub fn set_hittable(&mut self, id: NodeId, hit: bool) {
        if !self.arena.is_live(id) {
            return;
        }
        self.hittable[id.index() as usize] = hit;
    }

    /// Whether a node is folded out of layout and paint (default `false`):
    /// hidden itself, or a child past the first of a folded node.
    #[inline]
    pub fn hidden(&self, id: NodeId) -> bool {
        self.concealed(id.index() as usize)
    }

    /// Whether node `id` shows: live, and neither it nor an ancestor hidden
    /// or fully transparent. What runs only to be seen — a spinner, a
    /// progress indicator's sweep — stops beating frames while it does not.
    pub fn shows(&self, id: NodeId) -> bool {
        if !self.arena.is_live(id) {
            return false;
        }
        let mut at = Some(id);
        while let Some(node) = at {
            if self.hidden(node) || self.opacity(node) <= 0.0 {
                return false;
            }
            at = self.arena.parent(node);
        }
        true
    }

    /// Whether the slot `i` is out of layout and paint.
    #[inline]
    fn concealed(&self, i: usize) -> bool {
        let index = crate::node::NodeIndex::new(i as u32);
        self.hidden[i] || self.folded_away(index) || self.paged_away(index)
    }

    /// Set whether a node (and its subtree) is folded out of layout, paint and
    /// the semantics tree. A live-guarded write, so a stale handle is a no-op.
    ///
    /// Hiding a node changes its measured size (a hidden subtree measures to
    /// zero), what paints and what assistive technology reads, so a change
    /// marks the node MEASURE | LAYOUT | PAINT | SEMANTICS dirty and its parent's
    /// children changed; the parent re-places its children and repaints. Flipping the flag needs no structural
    /// rebuild — every panel is built once and shown or hidden on demand.
    pub fn set_hidden(&mut self, id: NodeId, hidden: bool) {
        if !self.arena.is_live(id) {
            return;
        }
        if self.hidden[id.index() as usize] == hidden {
            return;
        }
        self.hidden[id.index() as usize] = hidden;
        self.shown_changed(id);
    }

    /// Node `id` entered or left layout, paint and the semantics tree: its
    /// parent re-places its children and repaints.
    pub(crate) fn shown_changed(&mut self, id: NodeId) {
        self.relinked(id);
        self.mark_dirty(
            id,
            DirtyClass::MEASURE | DirtyClass::LAYOUT | DirtyClass::PAINT | DirtyClass::SEMANTICS,
        );
    }

    /// Note that `id` enters or leaves its parent's paint.
    fn relinked(&mut self, id: NodeId) {
        match self.arena.parent(id) {
            Some(parent) => self.arena.note_relinked(parent.node_index(), Some(id)),
            None => self.repaint_pending = true,
        }
    }

    /// A node's opacity (default `1.0`).
    #[inline]
    pub fn opacity(&self, id: NodeId) -> f32 {
        self.opacity[id.index() as usize]
    }

    /// Set the opacity a node and its subtree paint at, clamped to `[0, 1]`
    /// (a non-finite value is opaque). A live-guarded write that marks PAINT
    /// only when the value changes: opacity never moves a box.
    pub fn set_opacity(&mut self, id: NodeId, opacity: f32) {
        if !self.arena.is_live(id) {
            return;
        }
        self.settle(id, &LookValue::Opacity(opacity));
        self.write_opacity(id, opacity);
    }

    fn write_opacity(&mut self, id: NodeId, opacity: f32) {
        if !self.hold(id, LookValue::Opacity(opacity)) {
            self.show_opacity(id, opacity);
        }
    }

    fn show_opacity(&mut self, id: NodeId, opacity: f32) {
        let opacity = if opacity.is_finite() {
            opacity.clamp(0.0, 1.0)
        } else {
            1.0
        };
        let slot = &mut self.opacity[id.index() as usize];
        if *slot != opacity {
            // A node that keeps fading through its layer only re-tints it; one
            // that starts or stops fading paints another stream, and one that
            // shows or hides enters or leaves its parent's.
            let layered = |o: f32| o > 0.0 && o < 1.0;
            let shown = (*slot > 0.0) != (opacity > 0.0);
            if layered(*slot) != layered(opacity) {
                self.repaint_pending = true;
            }
            *slot = opacity;
            if shown {
                self.relinked(id);
            }
            self.mark_dirty(id, DirtyClass::PAINT);
        }
    }

    /// Set the color a node's box fills with, keeping its radius and border. A
    /// live-guarded write that marks PAINT only when the color changes.
    pub fn set_fill(&mut self, id: NodeId, fill: Rgba) {
        if !self.arena.is_live(id) {
            return;
        }
        self.settle(id, &LookValue::Fill(fill));
        self.write_fill(id, fill);
    }

    fn write_fill(&mut self, id: NodeId, fill: Rgba) {
        let style = &mut self.style[id.index() as usize];
        if style.fill != fill {
            style.fill = fill;
            self.mark_dirty(id, DirtyClass::PAINT);
        }
    }

    /// Round a node's box corners by `radius` dp and `pct` of the shorter
    /// side of the box it paints, each never negative. A live-guarded write
    /// that marks PAINT only when the radius changes.
    pub(crate) fn write_corner_radius(&mut self, id: NodeId, radius: f32, pct: f32) {
        if !self.arena.is_live(id) {
            return;
        }
        let style = &mut self.style[id.index() as usize];
        let (radius, pct) = (radius.max(0.0), pct.max(0.0));
        if style.radius != radius || style.radius_pct != pct {
            style.radius = radius;
            style.radius_pct = pct;
            self.mark_dirty(id, DirtyClass::PAINT);
        }
    }

    /// Stroke a node's box with `border` inside its edge, or with none for
    /// [`Border::NONE`]. A live-guarded write that marks PAINT only when the
    /// border changes.
    pub fn set_border(&mut self, id: NodeId, border: Border) {
        if !self.arena.is_live(id) {
            return;
        }
        let style = &mut self.style[id.index() as usize];
        if style.border != border {
            style.border = border;
            self.mark_dirty(id, DirtyClass::PAINT);
        }
    }

    /// The value of the look property `of` names that a node shows now; `of`
    /// itself for one it shows none of (a text color with no text, a size
    /// that is not fixed), which then shows at once.
    pub(crate) fn shown(&self, id: NodeId, of: &LookValue) -> LookValue {
        if let Some(running) = self.animations.iter().find(|a| a.motion.moves(id, of)) {
            return running.model;
        }
        let i = id.index() as usize;
        match *of {
            LookValue::Fill(_) => LookValue::Fill(self.style[i].fill),
            LookValue::Opacity(_) => LookValue::Opacity(self.opacity[i]),
            LookValue::Radius { .. } => LookValue::Radius {
                dp: self.style[i].radius,
                pct: self.style[i].radius_pct,
            },
            LookValue::TextColor(_) => match self.content_payload[i].as_deref() {
                Some(Content::Text { color, .. }) => LookValue::TextColor(*color),
                _ => *of,
            },
            LookValue::Translate(_) => LookValue::Translate(self.translate[i] - self.pin_of(id)),
            LookValue::Width(_) => match self.layout[i].size().width {
                Length::Fixed(v) => LookValue::Width(v),
                _ => *of,
            },
            LookValue::Height(_) => match self.layout[i].size().height {
                Length::Fixed(v) => LookValue::Height(v),
                _ => *of,
            },
            LookValue::Scale(_) => LookValue::Scale(self.turn[i].scale),
            LookValue::Rotation(_) => LookValue::Rotation(self.turn[i].rotation),
            LookValue::Check(_) => match self.check_marks[i] {
                Some((_, shown)) => LookValue::Check(shown),
                None => *of,
            },
            LookValue::Thumb(_) => match self.content_payload[i].as_deref() {
                Some(Content::Segments(segments)) => LookValue::Thumb(segments.shown()),
                _ => *of,
            },
        }
    }

    fn write_look(&mut self, id: NodeId, value: LookValue) {
        match value {
            LookValue::Fill(fill) => self.write_fill(id, fill),
            LookValue::Opacity(opacity) => self.write_opacity(id, opacity),
            LookValue::Radius { dp, pct } => self.write_corner_radius(id, dp, pct),
            LookValue::TextColor(ink) => {
                if let Some(Content::Text { color, .. }) =
                    self.content_payload[id.index() as usize].as_deref_mut()
                    && *color != ink
                {
                    *color = ink;
                    self.mark_dirty(id, DirtyClass::PAINT);
                }
            }
            LookValue::Translate(offset) => self.set_translate(id, offset),
            LookValue::Thumb(shown) => self.slide_thumb(id, shown),
            LookValue::Check(progress) => {
                if let Some((mark, shown)) = &mut self.check_marks[id.index() as usize]
                    && *shown != progress
                {
                    *shown = progress;
                    let dirty = if mark.moves_caption() {
                        DirtyClass::PAINT | DirtyClass::MEASURE | DirtyClass::LAYOUT
                    } else {
                        DirtyClass::PAINT
                    };
                    self.mark_dirty(id, dirty);
                }
            }
            LookValue::Scale(scale) => {
                self.write_turn(id, |turn| crate::transform::Turn { scale, ..turn })
            }
            LookValue::Rotation(rotation) => {
                self.write_turn(id, |turn| crate::transform::Turn { rotation, ..turn })
            }
            LookValue::Width(v) | LookValue::Height(v) => {
                let width = matches!(value, LookValue::Width(_));
                let size = self.layout_input_mut(id).size_mut();
                let slot = if width {
                    &mut size.width
                } else {
                    &mut size.height
                };
                if *slot != Length::Fixed(v) {
                    *slot = Length::Fixed(v);
                    self.request_moved(id);
                    self.mark_dirty(
                        id,
                        DirtyClass::MEASURE | DirtyClass::LAYOUT | DirtyClass::PAINT,
                    );
                }
            }
        }
    }

    /// Whether the property `of` names is moving on `id`.
    #[inline]
    pub(crate) fn is_moving(&self, id: NodeId, of: &LookValue) -> bool {
        !self.transitions.is_empty() && self.transitions.iter().any(|t| t.moves(id, of))
    }

    /// A text content arriving while its color moves keeps the color shown
    /// and turns the move toward the color it brings.
    fn arriving(&mut self, id: NodeId, content: &mut Content) {
        if let Content::Text { color, .. } = content
            && self.is_moving(id, &LookValue::TextColor(*color))
        {
            let target = LookValue::TextColor(*color);
            if let LookValue::TextColor(shown) = self.shown(id, &target) {
                self.present(id, target);
                *color = shown;
            }
        }
    }

    /// Drops the transition moving the property `of` names on `id`, if any.
    pub(crate) fn settle(&mut self, id: NodeId, of: &LookValue) {
        if let Some(at) = self.transitions.iter().position(|t| t.moves(id, of)) {
            self.transitions.swap_remove(at);
        }
    }

    /// Moves a node's look property to `to` over `timing`, starting from the
    /// value it shows now: one already moving retargets from where it is, a
    /// spring carrying on at the velocity it had. An instant timing, or a
    /// target the node already shows with nothing in flight, writes at once.
    /// Each frame's [`tick_transitions`](Self::tick_transitions) writes the
    /// value it shows, marking PAINT. A stale handle is a no-op.
    pub fn transition(&mut self, id: NodeId, to: LookValue, timing: Timing) {
        if self.arena.is_live(id) {
            self.move_look(id, to, timing, None);
        }
    }

    /// [`transition`](Self::transition) on a live node, a move from rest
    /// beginning at `velocity` when one is given.
    fn move_look(&mut self, id: NodeId, to: LookValue, timing: Timing, velocity: Option<Parts>) {
        let from = self.shown(id, &to);
        let moving = self.transitions.iter().position(|t| t.moves(id, &to));
        if self.lands(&timing) || (moving.is_none() && velocity.is_none() && from == to) {
            self.settle(id, &to);
            self.write_look(id, to);
            return;
        }
        let transition = LookTransition::new(id, from, to, timing);
        match moving {
            Some(at) if self.transitions[at].to == to => {}
            Some(at) => {
                let carried = self.transitions[at].velocity();
                self.transitions[at] = transition.carrying(carried);
            }
            None => self
                .transitions
                .push(transition.carrying(velocity.unwrap_or_default())),
        }
    }

    /// Shows `value` on a node as a view's first delivery does: at once, unless
    /// the property is moving, when the move heads to `value` instead (keeping
    /// its clock when it already does). A stale handle is a no-op.
    pub fn present(&mut self, id: NodeId, value: LookValue) {
        if !self.arena.is_live(id) {
            return;
        }
        match self.transitions.iter().position(|t| t.moves(id, &value)) {
            None => self.write_look(id, value),
            Some(at) => {
                let timing = self.transitions[at].timing;
                self.transition(id, value, timing);
            }
        }
    }

    /// Moves the transitions in flight on `id` out into `out`, leaving the node
    /// at the value it shows.
    pub fn lift_transitions(&mut self, id: NodeId, out: &mut Vec<LookTransition>) {
        let mut i = self.transitions.len();
        while i > 0 {
            i -= 1;
            if self.transitions[i].node == id {
                out.push(self.transitions.swap_remove(i));
            }
        }
    }

    /// Resumes on `id` a transition lifted from the node it replaces: the
    /// property shows where the move was and carries on toward the value `id`
    /// shows now, on its clock when that is still its target, else from where
    /// it was over its timing. A stale handle is a no-op.
    pub fn resume_transition(&mut self, id: NodeId, mut moving: LookTransition) {
        if !self.arena.is_live(id) {
            return;
        }
        let target = self.shown(id, &moving.to);
        let (at, done) = moving.at();
        self.settle(id, &target);
        self.write_look(id, at);
        if done {
            self.write_look(id, target);
        } else if moving.to == target {
            moving.node = id;
            self.transitions.push(moving);
        } else {
            self.move_look(id, target, moving.timing, Some(moving.velocity()));
        }
    }

    /// Reduce motion, or stop: the platform's setting, or the environment's
    /// in its place. Reduced, a move on a timing that reduces lands at once
    /// — the ones under way land now, and an animation that would run ends
    /// finished — and a programmatic scroll jumps where it would glide; what
    /// a finger or a flick moves still moves.
    pub fn set_reduced_motion(&mut self, on: bool) {
        if self.reduced_motion == on {
            return;
        }
        self.reduced_motion = on;
        if !on {
            return;
        }
        let mut i = self.transitions.len();
        while i > 0 {
            i -= 1;
            if self.transitions[i].timing.reduced == ReducedMotion::Instant {
                let landed = self.transitions.swap_remove(i);
                if self.arena.is_live(landed.node) {
                    self.write_look(landed.node, landed.to);
                }
            }
        }
        let mut i = self.animations.len();
        while i > 0 {
            i -= 1;
            if self.animations[i].reduced == ReducedMotion::Instant {
                let ended = self.animations.swap_remove(i);
                self.end_animation(ended, true);
            }
        }
        let glides: Vec<(NodeId, Vec2)> = self
            .motions
            .glides
            .drain(..)
            .map(|g| (g.viewport, g.target))
            .collect();
        for (id, to) in glides {
            self.set_scroll(id, to);
        }
    }

    /// Whether motion is reduced.
    #[inline]
    pub fn reduced_motion(&self) -> bool {
        self.reduced_motion
    }

    /// Whether a move that is `reduced` so lands at once now.
    #[inline]
    fn reduces(&self, reduced: ReducedMotion) -> bool {
        self.reduced_motion && reduced == ReducedMotion::Instant
    }

    /// Whether a move on `timing` lands the moment it starts.
    #[inline]
    pub(crate) fn lands(&self, timing: &Timing) -> bool {
        timing.is_instant() || self.reduces(timing.reduced)
    }

    /// Whether any look property is moving: the frame loop keeps beating
    /// while one is.
    #[inline]
    pub fn is_transitioning(&self) -> bool {
        !self.transitions.is_empty()
            || !self.animations.is_empty()
            || self.ring.appearing()
            || !self.animation_ends.is_empty()
    }

    /// Advances every moving look property by `delta`, writing the value each
    /// node shows; one that arrives is written at its target and dropped, as is
    /// one whose node is gone. A flat pass with no allocation.
    pub fn tick_transitions(&mut self, delta: Duration) {
        self.tick_ring(delta);
        let mut i = self.transitions.len();
        while i > 0 {
            i -= 1;
            let node = self.transitions[i].node;
            if !self.arena.is_live(node) {
                self.transitions.swap_remove(i);
                continue;
            }
            let (value, done) = self.transitions[i].advance(delta);
            self.write_look(node, value);
            if done {
                self.transitions.swap_remove(i);
            }
        }
        let mut i = self.animations.len();
        while i > 0 {
            i -= 1;
            let node = self.animations[i].motion.node();
            if !self.arena.is_live(node) {
                let gone = self.animations.swap_remove(i);
                self.end_animation(gone, false);
                continue;
            }
            let (value, done) = self.animations[i].motion.advance(delta);
            if done {
                let ran = self.animations.swap_remove(i);
                self.end_animation(ran, true);
            } else {
                self.show_look(node, value);
            }
        }
    }

    /// Runs an animation on node `id`: the value it presents of the property
    /// `to` names moves there from the value it presents now over `timing`,
    /// then the property's model value shows again — what the node is told to
    /// show meanwhile is that model value. A newer animation of the same
    /// property ends one running, unfinished, a spring carrying on at the
    /// velocity it had. `on_end` is called with how it ended after the layout
    /// of the frame it ends in, and the node's
    /// [`NoticeKinds::ANIMATION_END`](crate::NoticeKinds::ANIMATION_END)
    /// watcher hears it. `None` for a stale handle or a property other than
    /// translate, scale, rotation and opacity, which would lay out.
    pub fn animate(
        &mut self,
        id: NodeId,
        to: LookValue,
        timing: Timing,
        on_end: Option<AnimationEndHandler>,
    ) -> Option<AnimationId> {
        let instant = self.lands(&timing);
        self.run_animation(
            id,
            to,
            (instant, timing.reduced),
            on_end,
            |presented, carried| {
                Some(Run::Move(
                    LookTransition::new(id, presented, to, timing).carrying(carried),
                ))
            },
        )
    }

    /// Runs the animation `track` makes on node `id`, as
    /// [`animate`](Self::animate) runs one toward a target: the value it
    /// presents of its stops' property moves through them, cycle by cycle,
    /// from the value it presents now, then the model value shows again. Its
    /// end names its last stop's value as the target. A stop of another
    /// property than its first's, or with an `at` that is not finite, is
    /// left out; `None` without any, or as `animate` gives it.
    pub fn play(
        &mut self,
        id: NodeId,
        track: Keyframes,
        on_end: Option<AnimationEndHandler>,
    ) -> Option<AnimationId> {
        let property = track.stops.first()?.value;
        let instant = track.is_instant() || self.reduces(track.reduced);
        let reduced = track.reduced;
        self.run_animation(id, property, (instant, reduced), on_end, |presented, _| {
            TrackRun::new(id, presented, track).map(Run::Track)
        })
    }

    /// Starts on live node `id` the run `make` makes of the property
    /// `property` names, given the value presented and the velocity of the
    /// animation it replaces; it ends at once when `instant`, and is
    /// `reduced` so while motion is reduced.
    pub(crate) fn run_animation(
        &mut self,
        id: NodeId,
        property: LookValue,
        (instant, reduced): (bool, ReducedMotion),
        on_end: Option<AnimationEndHandler>,
        make: impl FnOnce(LookValue, Parts) -> Option<Run>,
    ) -> Option<AnimationId> {
        let animates = matches!(
            property,
            LookValue::Translate(_)
                | LookValue::Scale(_)
                | LookValue::Rotation(_)
                | LookValue::Opacity(_)
        );
        if !self.arena.is_live(id) || !animates {
            return None;
        }
        let model = self.shown(id, &property);
        let presented = self.presented(id, &property);
        let prior = self
            .animations
            .iter()
            .position(|a| a.motion.moves(id, &property));
        let carried = prior.map_or_else(Parts::default, |at| self.animations[at].motion.velocity());
        let motion = make(presented, carried)?;
        if let Some(at) = prior {
            let prior = self.animations.swap_remove(at);
            self.queue_end(prior, false);
        }
        self.last_animation += 1;
        let animation = AnimationId(self.last_animation);
        let running = NodeAnimation {
            id: animation,
            motion,
            model,
            on_end,
            reduced,
            quiet: false,
            hides: None,
        };
        if instant {
            self.end_animation(running, true);
        } else {
            self.animations.push(running);
        }
        Some(animation)
    }

    /// Ends animation `animation` unfinished, showing its property's model
    /// value; one that ended already is left be.
    pub fn cancel_animation(&mut self, animation: AnimationId) {
        self.stop_animation(animation, false);
    }

    /// Ends animation `animation` as finished, at once.
    pub fn finish_animation(&mut self, animation: AnimationId) {
        self.stop_animation(animation, true);
    }

    /// Whether animation `animation` runs.
    pub fn is_animating(&self, animation: AnimationId) -> bool {
        self.animations.iter().any(|a| a.id == animation)
    }

    fn stop_animation(&mut self, animation: AnimationId, finished: bool) {
        if let Some(at) = self.animations.iter().position(|a| a.id == animation) {
            let stopped = self.animations.swap_remove(at);
            self.end_animation(stopped, finished);
        }
    }

    /// Shows the model value `ended` held and queues its end.
    pub(crate) fn end_animation(&mut self, ended: NodeAnimation, finished: bool) {
        let node = ended.motion.node();
        if self.arena.is_live(node) {
            self.show_look(node, ended.model);
        }
        if finished
            && let Some(page) = ended.hides
            && self.leaving.contains(&page)
        {
            self.leaving.retain(|&p| p != page);
            if self.arena.is_live(page) {
                self.shown_changed(page);
            }
        }
        self.queue_end(ended, finished);
    }

    fn queue_end(&mut self, ended: NodeAnimation, finished: bool) {
        if ended.quiet {
            return;
        }
        let end = AnimationEnd {
            animation: ended.id,
            target: ended.motion.target(),
            finished,
        };
        self.animation_ends
            .push((ended.motion.node(), end, ended.on_end));
    }

    /// The animations ended since the last call, oldest first.
    pub(crate) fn take_animation_ends(&mut self) -> Vec<EndedAnimation> {
        std::mem::take(&mut self.animation_ends)
    }

    /// Whether an animation of node `id` holds the property `value` names:
    /// the value lands in its model rather than on the node.
    fn hold(&mut self, id: NodeId, value: LookValue) -> bool {
        if self.animations.is_empty() {
            return false;
        }
        match self
            .animations
            .iter_mut()
            .find(|a| a.motion.moves(id, &value))
        {
            Some(running) => {
                running.model = value;
                true
            }
            None => false,
        }
    }

    /// The value node `id` presents of the property `of` names, of those an
    /// animation moves.
    pub(crate) fn presented(&self, id: NodeId, of: &LookValue) -> LookValue {
        let i = id.index() as usize;
        match *of {
            LookValue::Translate(_) => LookValue::Translate(self.translate[i] - self.pin_of(id)),
            LookValue::Scale(_) => LookValue::Scale(self.turn[i].scale),
            LookValue::Rotation(_) => LookValue::Rotation(self.turn[i].rotation),
            LookValue::Opacity(_) => LookValue::Opacity(self.opacity[i]),
            _ => *of,
        }
    }

    /// Shows `value` on node `id`, past any animation's hold: the presented
    /// values an animation moves. `id` is live.
    pub(crate) fn show_look(&mut self, id: NodeId, value: LookValue) {
        let turn = self.turn[id.index() as usize];
        match value {
            LookValue::Translate(offset) => self.show_translate(id, offset),
            LookValue::Opacity(opacity) => self.show_opacity(id, opacity),
            LookValue::Scale(scale) => self.show_turn(id, crate::transform::Turn { scale, ..turn }),
            LookValue::Rotation(rotation) => {
                self.show_turn(id, crate::transform::Turn { rotation, ..turn })
            }
            _ => self.write_look(id, value),
        }
    }

    /// Whether a node is an overlay — painted in the deferred top layer, over
    /// every non-overlay node regardless of tree position (default `false`).
    #[inline]
    pub fn is_overlay(&self, id: NodeId) -> bool {
        self.overlay[id.index() as usize]
    }

    /// Set whether a node (and its subtree) paints in the top layer. A
    /// live-guarded write, so a stale handle is a no-op.
    ///
    /// Unlike [`set_hidden`](Self::set_hidden), an overlay still lays out in place
    /// — it only changes paint *order*, not measure or geometry (a popup is
    /// anchored to its parent's box and simply draws on top). So this marks PAINT
    /// only, never LAYOUT: flipping the flag re-emits the primitive stream in the
    /// new order without re-measuring anything.
    pub fn set_overlay(&mut self, id: NodeId, overlay: bool) {
        if !self.arena.is_live(id) {
            return;
        }
        self.overlay[id.index() as usize] = overlay;
        self.rejoin_above(id);
        self.mark_dirty(id, DirtyClass::PAINT);
        self.repaint_pending = true;
    }

    /// Whether a node is its scroll viewport's chrome: a scroll bar track,
    /// laid out along the viewport's edge and not scrolled with its content.
    #[inline]
    pub fn is_chrome(&self, id: NodeId) -> bool {
        self.chrome[id.index() as usize]
    }

    /// Mark a scroll viewport's child its chrome. Its viewport lays it out
    /// and paints it anew.
    pub(crate) fn set_chrome(&mut self, id: NodeId, chrome: bool) {
        if !self.arena.is_live(id) {
            return;
        }
        self.chrome[id.index() as usize] = chrome;
        if let Some(parent) = self.parent(id) {
            self.mark_dirty(parent, DirtyClass::LAYOUT | DirtyClass::PAINT);
        }
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

    /// Where the focus was and where it is, once since the focus last moved
    /// and then `None`: what [`KeyRouter::route_focus`](crate::KeyRouter::route_focus)
    /// tells the nodes. A move back to where it was is none.
    pub fn take_focus_move(&mut self) -> Option<(Option<NodeId>, Option<NodeId>)> {
        let from = self.focus_from.take()?;
        (from != self.focused).then_some((from, self.focused))
    }

    /// Set the focus slot. Focusing `Some(id)` is live-guarded — a stale handle
    /// leaves focus unchanged rather than pointing the slot at a dead slot;
    /// `None` always clears. This only moves the slot: dirtying the old and new
    /// nodes for the focus-ring repaint is the router's job (see the input tier).
    pub fn set_focused(&mut self, id: Option<NodeId>) {
        match id {
            Some(node) if !self.arena.is_live(node) => {}
            other => {
                let moved = self.focused != other;
                if moved {
                    self.interaction_states.moved();
                    self.focus_from.get_or_insert(self.focused);
                }
                self.focused = other;
                if moved {
                    self.ring_landed();
                    if let Some(node) = other {
                        self.roved(node);
                    }
                    self.swipe_focus(other);
                    self.reorder_focus(other);
                    self.drag_focus(other);
                }
            }
        }
    }

    /// The subtree focus traversal is confined to, or `None` for the whole tree.
    /// While `Some`, [`focus_next`](crate::input::focus_next) cycles Tab only
    /// within this node's subtree — the focus trap an open modal installs.
    #[inline]
    pub fn focus_scope(&self) -> Option<NodeId> {
        self.focus_scope
    }

    /// Set (or clear, with `None`) the focus scope. Scoping to `Some(id)` is
    /// live-guarded — a stale handle leaves the scope unchanged rather than
    /// trapping focus in a dead subtree; `None` always clears. Only moves the
    /// slot; it does not itself move focus (a control pairs it with a
    /// `request_focus` into the scope when opening).
    pub fn set_focus_scope(&mut self, id: Option<NodeId>) {
        match id {
            Some(node) if !self.arena.is_live(node) => {}
            other => self.focus_scope = other,
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

    /// Whether a node takes input as far as its own `enabled` goes (default
    /// `true`); [`inert`](Self::inert) says whether an ancestor turned it off.
    #[inline]
    pub fn enabled(&self, id: NodeId) -> bool {
        !self.disabled[id.index() as usize]
    }

    /// Whether input does not reach `id`: it or an ancestor is disabled. An
    /// inert node takes no pointer or key event, no focus and no hover or
    /// press state, and reads as disabled. Free while no node is disabled,
    /// otherwise a walk up the ancestry.
    pub fn inert(&self, id: NodeId) -> bool {
        if self.disabled_count == 0 {
            return false;
        }
        let mut at = Some(id);
        while let Some(node) = at {
            if self.disabled[node.index() as usize] {
                return true;
            }
            at = self.arena.parent(node);
        }
        false
    }

    /// Turn input to a node and its subtree on or off. Disabling takes the
    /// focus and the press from a node it makes inert. Marks SEMANTICS, and
    /// moves the interaction states a style reads. A live-guarded write.
    pub fn set_enabled(&mut self, id: NodeId, enabled: bool) {
        if !self.arena.is_live(id) {
            return;
        }
        let at = id.index() as usize;
        if self.disabled[at] != enabled {
            return;
        }
        self.disabled[at] = !enabled;
        if enabled {
            self.disabled_count -= 1;
        } else {
            self.disabled_count += 1;
        }
        self.interaction_states.moved();
        self.mark_dirty(id, DirtyClass::SEMANTICS);
        if enabled {
            return;
        }
        if let Some(focused) = self.focused
            && self.inert(focused)
        {
            self.mark_dirty(focused, DirtyClass::PAINT | DirtyClass::SEMANTICS);
            self.set_focused(None);
        }
        if self
            .interaction_states
            .pressed()
            .is_some_and(|p| self.inert(p))
        {
            self.set_pressed(None);
        }
    }

    /// Attach a keyboard/IME handler to a node, replacing any prior one. A
    /// live-guarded write, so a stale handle is a no-op.
    pub fn set_key_handler(&mut self, id: NodeId, handler: KeyHandler) {
        if !self.arena.is_live(id) {
            return;
        }
        self.key_handlers[id.index() as usize] = Some(handler);
    }

    /// Detach a node's pointer and key handlers, as a reload does before it
    /// attaches the recompiled ones to a retained node. A live-guarded write.
    pub fn clear_event_handlers(&mut self, id: NodeId) {
        if !self.arena.is_live(id) {
            return;
        }
        self.handlers[id.index() as usize] = None;
        self.key_handlers[id.index() as usize] = None;
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
        #[cfg(feature = "inspect")]
        if let Some(journal) = &mut self.journal {
            journal.record(id, class);
        }
        // Everything lands on the node itself.
        self.dirty[id.index() as usize] |= class;
        self.dirty_lists.note(id.index(), class);

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
        while let Some(parent) = self.arena.parent(current) {
            let mut add = unconditional;
            if measure_alive {
                if measures_alone(self.layout[parent.index() as usize]) {
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
            self.dirty_lists.note(parent.index(), add);
            // If this ancestor already carried everything we add and MEASURE is
            // no longer rising, higher ancestors were reached on a prior mark.
            if *slot == before && !measure_alive {
                break;
            }
            current = parent;
        }
    }

    /// Clear every node's pending invalidation, and the boxes moved and the
    /// children changed with it. Run at frame end once the incremental passes
    /// have consumed the dirty set; costs the nodes marked this frame, not the
    /// store.
    #[inline]
    pub fn clear_dirty(&mut self) {
        self.dirty_lists.clear(&mut self.dirty);
        self.clear_paint_logs();
    }

    /// Whether any node currently carries any dirty class. Lets a frame skip
    /// the incremental passes entirely when nothing changed; O(1) on an idle
    /// frame.
    #[inline]
    pub fn any_dirty(&self) -> bool {
        self.dirty_lists
            .marked()
            .iter()
            .any(|&i| !self.dirty[i as usize].is_empty())
    }

    /// How many nodes carry a dirty class: what the frame's changes
    /// invalidated, until [`clear_dirty`](Self::clear_dirty).
    pub fn dirty_count(&self) -> usize {
        self.dirty_lists
            .marked()
            .iter()
            .filter(|&&i| !self.dirty[i as usize].is_empty())
            .count()
    }

    /// The per-class worklists: which nodes each pass visits this frame.
    #[inline]
    pub fn dirty_lists(&self) -> &crate::dirty::DirtyLists {
        &self.dirty_lists
    }

    /// Whether any node carries a dirty class intersecting `class`. Lets a frame
    /// gate a specific pass precisely — the facade recomputes the transform
    /// roots only when a `TRANSFORM`-dirty node exists, so a pure paint-only
    /// frame (a color change, no motion) never touches them.
    #[inline]
    pub fn any_dirty_class(&self, class: DirtyClass) -> bool {
        self.dirty_lists.any(class, &self.dirty)
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
        let mut child = self.arena.first_child(root);
        while let Some(c) = child {
            let next = self.arena.next_sibling(c);
            scratch.push(c);
            child = next;
        }
        // `root` now conceptually has no children; clear its child endpoints.
        self.arena.forget_children(root);

        let mut freed = 0;
        while scratch.len() > base {
            let node = scratch.pop().unwrap();
            // Push this node's children before freeing it (we still can read its
            // links while it is live).
            let mut c = self.arena.first_child(node);
            while let Some(cc) = c {
                let next = self.arena.next_sibling(cc);
                scratch.push(cc);
                c = next;
            }
            effects.cancel_for_node(node);
            self.cancel_tasks_for(node);
            if self.free_node(node) {
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
        if self.arena.parent(old) != Some(parent) {
            return 0;
        }
        // Free `old`'s subtree (descendants + their effect cleanups), then unlink,
        // clean up, and free `old` itself. Order matches `free_subtree`'s own
        // per-node teardown (cancel effects before releasing the slot).
        let mut freed = self.free_subtree(old, effects, scratch);
        self.arena.detach_child(old);
        effects.cancel_for_node(old);
        self.cancel_tasks_for(old);
        if self.free_node(old) {
            freed += 1;
        }
        self.arena.append_child(parent, new);
        freed
    }

    /// Free `root` and its whole subtree, unlinking it from its parent (which is
    /// marked for re-measure) and cancelling each freed node's effects and tasks.
    /// Returns the number of nodes freed, 0 for a stale `root`.
    ///
    /// The unmount of a control-flow region's fragment. `scratch` is the same
    /// reusable stack [`free_subtree`](Self::free_subtree) takes.
    pub fn free_tree(
        &mut self,
        root: NodeId,
        effects: &mut EffectStore,
        scratch: &mut Vec<NodeId>,
    ) -> u32 {
        if !self.arena.is_live(root) {
            return 0;
        }
        let mut freed = self.free_subtree(root, effects, scratch);
        if let Some(parent) = self.parent(root) {
            self.arena.detach_child(root);
            self.mark_dirty(
                parent,
                DirtyClass::MEASURE | DirtyClass::LAYOUT | DirtyClass::PAINT,
            );
        }
        effects.cancel_for_node(root);
        self.cancel_tasks_for(root);
        if self.free_node(root) {
            freed += 1;
        }
        freed
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
            let cause = self.set_dirty_cause(DirtyCause::State(state));
            let statics = bindings.targets(state, |target, class| match target {
                Ok(slot) => {
                    if let Some(id) = self.arena.live_id(slot) {
                        self.mark_dirty(id, class);
                    }
                }
                Err(id) => self.mark_dirty(id, class),
            });
            counters.record_static_eval(u64::from(statics));
            applied += statics;

            let dynamics = bindings.dynamic_for_state(state);
            for edge in dynamics {
                self.mark_dirty(edge.node, edge.class);
            }
            counters.record_dynamic_eval(dynamics.len() as u64);
            applied += dynamics.len() as u32;
            self.set_dirty_cause(cause);
        }
        applied
    }

    /// Names what the marks from now on are for, in the journal an
    /// inspector watches; the cause it replaces. Without the `inspect`
    /// feature, or while no inspector watches, it records nothing.
    #[inline]
    pub fn set_dirty_cause(&mut self, cause: DirtyCause) -> DirtyCause {
        #[cfg(feature = "inspect")]
        if let Some(journal) = &mut self.journal {
            return core::mem::replace(&mut journal.cause, cause);
        }
        let _ = cause;
        DirtyCause::Other
    }

    /// Starts or stops recording each direct mark and its cause, for an
    /// inspector. Cold path; without the `inspect` feature it does nothing.
    pub fn journal_dirty(&mut self, on: bool) {
        #[cfg(feature = "inspect")]
        match (on, self.journal.is_some()) {
            (true, false) => self.journal = Some(Box::default()),
            (false, true) => self.journal = None,
            _ => {}
        }
        let _ = on;
    }

    /// Whether marks are being recorded.
    pub fn journals_dirty(&self) -> bool {
        #[cfg(feature = "inspect")]
        return self.journal.is_some();
        #[cfg(not(feature = "inspect"))]
        false
    }

    /// Moves the marks recorded since the last drain onto `out`; how many
    /// the bound dropped meanwhile.
    pub fn take_dirty_marks(&mut self, out: &mut Vec<DirtyMark>) -> u32 {
        #[cfg(feature = "inspect")]
        if let Some(journal) = &mut self.journal {
            out.append(&mut journal.marks);
            return core::mem::take(&mut journal.dropped);
        }
        let _ = out;
        0
    }

    /// Allocate a node and reset its slot in every column. Inside an open
    /// instance scope the node takes the scope's next block slot, or is an
    /// ordinary node the scope records; a reused slot is overwritten in place,
    /// and a fresh one extends every column by one, after growing them
    /// together when they are full.
    pub(crate) fn alloc(&mut self, input: LayoutInput, style: BoxStyle) -> NodeId {
        let (id, owner) = match self.scopes.last_mut() {
            Some(scope) => match scope.next {
                Some(next) if next < scope.end => {
                    scope.next = Some(next + 1);
                    (self.arena.alloc_at(next), scope.owner | OWNER_KEPT)
                }
                _ => {
                    let owner = scope.owner;
                    let id = self.arena.alloc();
                    self.scope_log.push(id);
                    (id, owner)
                }
            },
            None => (self.arena.alloc(), NO_OWNER),
        };
        let i = id.index() as usize;
        if i >= self.bounds.len() {
            debug_assert_eq!(i, self.bounds.len(), "arena index must stay dense");
            self.grow_columns(i + 1);
        }
        self.bounds[i] = Rect::ZERO;
        self.reach[i] = Rect::ZERO;
        self.drop_rules(i);
        self.transform_root[i] = false;
        self.scroll[i] = Vec2::ZERO;
        self.translate[i] = Vec2::ZERO;
        self.turn[i] = crate::transform::Turn::NONE;
        self.content[i] = Vec2::ZERO;
        self.row_offset[i] = f32::NAN;
        if let Some(lane) = self.row_lane.get_mut(i) {
            *lane = crate::virtual_grid::FULL_LANE;
        }
        self.grid_tracks[i] = None;
        self.drop_placement(i);
        self.dirty[i] = DirtyClass::EMPTY;
        self.layout[i] = input;
        self.style[i] = style;
        self.measured[i] = Measured::default();
        self.hittable[i] = true;
        self.hidden[i] = false;
        self.folds.forget(i as u32);
        self.paged.forget(i as u32);
        self.fields.forget(i as u32);
        self.text_entries.forget(i as u32);
        self.clicks.forget(i as u32);
        self.cursors.forget(i as u32);
        self.links.forget(i as u32);
        self.layers.forget(i as u32);
        self.anchors.forget(i as u32);
        self.shadows.forget(i as u32);
        self.backdrops.forget(i as u32);
        self.feedback.forget(i as u32);
        self.ring.forget(i as u32);
        self.access.forget(i as u32);
        self.roving.forget(i as u32);
        self.bars.forget(i as u32);
        self.refreshers.forget(i as u32);
        self.spinners.forget(i as u32);
        self.shimmers.forget(i as u32);
        self.progresses.forget(i as u32);
        self.sliders.forget(i as u32);
        self.stickies.forget(i as u32);
        self.snaps.forget(i as u32);
        self.actives.forget(i as u32);
        self.validities.forget(i as u32);
        self.tables.forget(i as u32);
        self.clips[i] = false;
        self.overlay[i] = false;
        self.chrome[i] = false;
        self.opacity[i] = 1.0;
        self.handlers[i] = None;
        self.focusable[i] = false;
        if std::mem::take(&mut self.disabled[i]) {
            self.disabled_count -= 1;
        }
        self.key_handlers[i] = None;
        self.styled[i] = None;
        self.interaction[i] = None;
        self.semantics[i] = None;
        self.semantic_state[i] = None;
        self.content_payload[i] = None;
        self.check_marks[i] = None;
        self.text_request[i] = None;
        self.owner[i] = owner;
        id
    }

    /// Extend every column to `len` slots, growing them together when they
    /// are full. The new slots hold placeholders until a node takes them.
    fn grow_columns(&mut self, len: usize) {
        let more = len - self.bounds.len();
        if self.bounds.len() + more > self.bounds.capacity()
            && self.arena.fixed_capacity().is_none()
        {
            let grow = self.bounds.len().max(GROW_MIN).max(more);
            node_columns!(self.reserve_exact(grow));
        }
        for _ in 0..more {
            self.bounds.push(Rect::ZERO);
            self.reach.push(Rect::ZERO);
            self.rules_at.push(NO_RULES);
            self.transform_root.push(false);
            self.scroll.push(Vec2::ZERO);
            self.translate.push(Vec2::ZERO);
            self.turn.push(crate::transform::Turn::NONE);
            self.content.push(Vec2::ZERO);
            self.row_offset.push(f32::NAN);
            self.grid_tracks.push(None);
            self.placed_at.push(NO_PLACEMENT);
            self.dirty.push(DirtyClass::EMPTY);
            self.layout.push(LayoutInput::Leaf {
                size: Sizing {
                    width: Length::Fixed(0.0),
                    height: Length::Fixed(0.0),
                },
            });
            self.style.push(BoxStyle::NONE);
            self.measured.push(Measured::default());
            self.hittable.push(true);
            self.hidden.push(false);
            self.clips.push(false);
            self.overlay.push(false);
            self.chrome.push(false);
            self.opacity.push(1.0);
            self.handlers.push(None);
            self.focusable.push(false);
            self.disabled.push(false);
            self.key_handlers.push(None);
            self.styled.push(None);
            self.interaction.push(None);
            self.semantics.push(None);
            self.semantic_state.push(None);
            self.content_payload.push(None);
            self.check_marks.push(None);
            self.text_request.push(None);
            self.owner.push(NO_OWNER);
        }
    }

    /// Open an instance scope for instance `owner`: the nodes built
    /// until it closes fill a block of `len` slots, or, with `None`, are
    /// ordinary nodes the scope lists. A fixed-capacity store with no room
    /// for the block opens a scope without one.
    pub(crate) fn open_scope(&mut self, owner: u32, len: Option<u32>) {
        let block = len.and_then(|len| {
            let base = self.arena.reserve_block(len)?;
            if self.bounds.len() < self.arena.slot_count() {
                self.grow_columns(self.arena.slot_count());
            }
            Some((base, len))
        });
        let (next, base, end) = match block {
            Some((base, len)) => (Some(base), base, base + len),
            None => (None, 0, 0),
        };
        self.scopes.push(NodeScope {
            owner,
            next,
            base,
            end,
            log: self.scope_log.len() as u32,
        });
    }

    /// Close the innermost instance scope and report its nodes. Its logged
    /// nodes stay listed until [`drop_scope_log`](Self::drop_scope_log).
    pub(crate) fn close_scope(&mut self) -> crate::state::ScopeSlots {
        let scope = self.scopes.pop().expect("an open instance scope");
        crate::state::ScopeSlots {
            base: scope.base,
            len: scope.end - scope.base,
            filled: scope.next.map_or(0, |next| next - scope.base),
            log: scope.log,
        }
    }

    /// The ordinary nodes of a closed scope, from its `log` position.
    pub(crate) fn scope_log(&self, log: u32) -> &[NodeId] {
        &self.scope_log[log as usize..]
    }

    /// Forget the ordinary nodes logged from `log` on.
    pub(crate) fn drop_scope_log(&mut self, log: u32) {
        self.scope_log.truncate(log as usize);
    }

    /// Re-tag nodes `nodes` of instance `owner` as its block, kept until
    /// [`release_block`](Self::release_block).
    pub(crate) fn keep_block(&mut self, owner: u32, base: u32, len: u32) {
        for slot in &mut self.owner[base as usize..(base + len) as usize] {
            *slot = owner | OWNER_KEPT;
        }
    }

    /// Give back the block of `len` slots at `base`, whose nodes are all
    /// freed, for the next block of the same length.
    pub(crate) fn release_block(&mut self, base: u32, len: u32) {
        for slot in &mut self.owner[base as usize..(base + len) as usize] {
            *slot = NO_OWNER;
        }
        self.arena.release_block(base, len);
    }

    /// The instance of each owned node freed since the last call.
    pub(crate) fn take_freed_owners(&mut self, out: &mut Vec<u32>) {
        out.append(&mut self.freed_owners);
    }

    /// The template instance node `id` belongs to, if one built it.
    #[inline]
    pub fn instance_of(&self, id: NodeId) -> Option<u32> {
        if !self.arena.is_live(id) {
            return None;
        }
        let owner = self.owner[id.index() as usize];
        (owner != NO_OWNER).then_some(owner & !OWNER_KEPT)
    }

    /// Run the bottom-up measure pass then the top-down layout pass over the
    /// subtree at `root`, placing the root into `surface`. `scratch` is a
    /// reusable child-id buffer owned by the caller so frames allocate nothing.
    ///
    /// Layout writes `bounds` (the unscrolled layout truth); the on-screen
    /// [`world`](Self::world) rects follow from it and the current scrolls and
    /// translates, so a later scroll moves them without re-laying out.
    pub fn layout(&mut self, root: NodeId, surface: Rect, scratch: &mut Vec<u32>) {
        self.fold_lengths();
        scratch.clear();
        layout::measure(self, root.index(), scratch);
        scratch.clear();
        layout::layout(self, root.index(), surface, scratch);
        let was = self.reach_in_parent(root.index() as usize);
        self.rejoin_subtree(root);
        self.lift_reach(root, was);
        self.lay_layers(root, surface, scratch);
        self.follow_tails();
        self.settle_scroll_requests();
        self.fit_viewers();
        self.fit_swipes();
        self.fit_reorders();
        self.fit_sheets();
        self.fit_drag_drops();
        self.sync_refreshers();
        self.sync_stickies();
        self.sync_snaps();
        self.fly_heroes();
        if self.sync_scroll_bars() {
            let mut redo = Vec::new();
            self.relayout_dirty(root, surface, scratch, &mut redo);
        }
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
        for &index in self.dirty_lists.of(DirtyClass::STYLE) {
            let index = index as usize;
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

    /// Re-select the warm `style` of every STYLE-dirty node that carries an
    /// interaction-state box selection, from that selection's variants and the
    /// current cell values, and report how many nodes were re-selected (0 when
    /// no interactive node is style-dirty).
    ///
    /// This is the interaction-state STYLE layer, a sibling of
    /// [`resolve_styles`](Self::resolve_styles): it runs after the state flush
    /// has marked STYLE on nodes whose pressed/hover cell flipped (and the
    /// setter marks STYLE on a freshly bound node), before the paint rebuild. A
    /// node whose STYLE is dirty but carries no selection is skipped — its
    /// `style` is a literal (or a token fold). Theme-free: the selection reads
    /// only the reactive cells, so it needs no [`Theme`], which is why it can run
    /// in the live frame loop where `resolve_styles` (theme-dependent) does not
    /// yet. The chosen box is one of the stored variants, so the selection is
    /// idempotent across frames and needs no separate base column.
    /// Allocation-free; touches only the dirty selecting nodes.
    pub fn resolve_interaction_styles(&mut self, states: &StateStore) -> u32 {
        self.resolve_interaction_styles_since(states, 0).0
    }

    /// [`resolve_interaction_styles`](Self::resolve_interaction_styles) over
    /// the STYLE worklist from entry `from` on: the nodes that gained STYLE
    /// since a resolve that returned `from` as its cursor. Returns how many it
    /// re-selected and the cursor for the next call. The worklist only grows
    /// within a frame, so a frame that resolves early and again before paint
    /// visits each node once.
    pub fn resolve_interaction_styles_since(
        &mut self,
        states: &StateStore,
        from: usize,
    ) -> (u32, usize) {
        let list = self.dirty_lists.of(DirtyClass::STYLE);
        let mut resolved = 0;
        for &index in list.get(from..).unwrap_or_default() {
            let index = index as usize;
            if !self.dirty[index].intersects(DirtyClass::STYLE) {
                continue;
            }
            let Some(interaction) = self.interaction[index] else {
                continue;
            };
            self.style[index] = interaction.resolve(states);
            resolved += 1;
        }
        (resolved, list.len())
    }

    /// Re-measure and re-place only the subtrees carrying MEASURE or LAYOUT
    /// invalidation, reusing the last frame's `measured`/`bounds` everywhere
    /// else, and report how many nodes each pass touched.
    ///
    /// A node whose MEASURE rose to it can change its own box, so its redo
    /// starts one level up at its (size-stable) parent — unless it is placed
    /// alone: a flex child of a box fixed on both axes whose children and
    /// requests did not change, its own main-axis request fixed or a fill
    /// share, keeps its main-axis run and moves no sibling, so it is redone by
    /// itself over its new cross extent. A LAYOUT-only container keeps its box
    /// and re-places its own children. The shallowest such node subsumes its
    /// descendants, so each subtree is recomputed once. `scratch`
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
        self.fold_lengths();

        // Roots for the redo: the shallowest nodes whose subtree must be
        // recomputed. A MEASURE change flows through its parent's child
        // distribution, so redo from the parent (clamped to `root`, which owns
        // the whole surface); a LAYOUT-only container redoes itself. The
        // candidates come from the MEASURE and LAYOUT worklists, shallowest
        // first, and one under an accepted root is subsumed by it — O(marked ×
        // depth), nothing on a frame with no layout dirt.
        let mut candidates = core::mem::take(&mut self.redo_candidates);
        for class in [DirtyClass::MEASURE, DirtyClass::LAYOUT] {
            for &index in self.dirty_lists.of(class) {
                let d = self.dirty[index as usize];
                // A node carrying both enters once, as MEASURE.
                if !d.intersects(class)
                    || (class == DirtyClass::LAYOUT && d.intersects(DirtyClass::MEASURE))
                {
                    continue;
                }
                let Some(id) = self.arena.live_id(index) else {
                    continue;
                };
                // A positioned row of an absolute-rows canvas moves no sibling,
                // whatever its size: it is redone alone.
                let redo_from = if class == DirtyClass::MEASURE
                    && self.row_canvas(id).is_none()
                    && !self.placed_alone(id, root)
                {
                    self.arena.parent(id).unwrap_or(root)
                } else {
                    id
                };
                candidates.push((self.depth_of(redo_from), redo_from));
            }
        }
        candidates.sort_unstable_by_key(|&(depth, id)| (depth, id.index()));
        if self.redo_flag.len() < self.arena.slot_count() {
            self.redo_flag.resize(self.arena.slot_count(), false);
        }
        for &(_, candidate) in &candidates {
            if !self.under_flagged(candidate) {
                self.redo_flag[candidate.index() as usize] = true;
                redo_roots.push(candidate);
            }
        }
        for r in redo_roots.iter() {
            self.redo_flag[r.index() as usize] = false;
        }
        candidates.clear();
        self.redo_candidates = candidates;

        let mut measured = 0;
        let mut laid_out = 0;
        for redo in redo_roots.iter().copied() {
            if redo != root && self.is_layer(redo) {
                self.lay_layer(redo, surface, scratch);
                measured += self.subtree_len(redo);
                laid_out += self.subtree_len(redo);
                continue;
            }
            scratch.clear();
            layout::measure(self, redo.index(), scratch);
            measured += self.subtree_len(redo);
            let bounds = if redo == root {
                layout::root_box(self, redo.index(), surface)
            } else if let Some((canvas, axis)) = self.row_canvas(redo) {
                layout::row_box(
                    self,
                    canvas.index(),
                    redo.index(),
                    self.bounds(canvas),
                    axis,
                )
                .unwrap_or_else(|| self.bounds(redo))
            } else if self.dirty[redo.index() as usize].intersects(DirtyClass::MEASURE)
                && let Some(parent) = self.arena.parent(redo)
            {
                let basis = self.basis_of(parent, root);
                let (laid, kept) = (self.bounds(parent), self.bounds(redo));
                let within = self.kept_within(parent.index());
                layout::placed_child_box(
                    self,
                    parent.index(),
                    redo.index(),
                    laid,
                    basis,
                    within,
                    kept,
                )
                .unwrap_or(kept)
            } else {
                self.bounds(redo)
            };
            scratch.clear();
            let basis = self.basis_of(redo, root);
            let within = if redo == root {
                Vec2 {
                    x: surface.w,
                    y: surface.h,
                }
            } else {
                self.kept_within(redo.index())
            };
            layout::layout_on(self, redo.index(), bounds, basis, within, scratch);
            let was = self.reach_in_parent(redo.index() as usize);
            self.rejoin_subtree(redo);
            self.lift_reach(redo, was);
            laid_out += self.subtree_len(redo);
        }
        // A layout of the whole tree skipped its layers, and a new surface
        // moves them; a relayout may have moved an anchor or resized an
        // anchored layer.
        if redo_roots.contains(&root) || self.layer_surface() != Some(surface) {
            self.lay_layers(root, surface, scratch);
        } else {
            self.follow_anchors();
        }
        // A tailing viewport follows its content's end, and a waiting scroll
        // applies, once layout knows the range.
        self.follow_tails();
        self.settle_scroll_requests();
        // A viewer keeps its content within the bounds its boxes now set.
        self.fit_viewers();
        // A swipe row stays open as far as its actions now reach.
        self.fit_swipes();
        self.fit_reorders();
        self.fit_sheets();
        self.fit_drag_drops();
        // A refresher starts or ends and its indicator follows its pull.
        self.sync_refreshers();
        self.sync_stickies();
        self.sync_snaps();
        self.fly_heroes();
        // The bars follow their viewports; a thumb whose length changed lays
        // out again, alone in its track.
        if self.sync_scroll_bars() {
            let (m, l) = self.relayout_dirty(root, surface, scratch, redo_roots);
            return (measured + m, laid_out + l);
        }
        (measured, laid_out)
    }

    /// Whether re-measured node `id` is placed alone: its parent, not
    /// re-placing its children this frame, keeps its box whatever `id`
    /// measures to and moves no sibling with it, and `id` is shown. So is
    /// every child of an absolute container, and of a stack fixed on both
    /// axes; a child of a flex container fixed on both axes requesting a
    /// fixed main extent or a fill share.
    fn placed_alone(&self, id: NodeId, root: NodeId) -> bool {
        let Some(parent) = self.arena.parent(id) else {
            return false;
        };
        if id == root || self.concealed(id.index() as usize) {
            return false;
        }
        if self.redone_alone(id) {
            return true;
        }
        let p = parent.index() as usize;
        let alone = match self.layout[p] {
            LayoutInput::Flex { axis, .. } => {
                fixed_on_both_axes(self.layout[p])
                    && matches!(
                        self.layout[id.index() as usize].size().on(axis),
                        Length::Fixed(_) | Length::Fill { .. }
                    )
            }
            LayoutInput::Stack { .. } => fixed_on_both_axes(self.layout[p]),
            LayoutInput::Absolute { .. } => true,
            _ => false,
        };
        alone
            && !self.dirty[p].intersects(DirtyClass::MEASURE | DirtyClass::LAYOUT)
            && !self.arena.is_relinked(parent.node_index())
    }

    /// The absolute-rows canvas `id` is a positioned row of, and its axis.
    fn row_canvas(&self, id: NodeId) -> Option<(NodeId, Axis)> {
        if self.row_offset[id.index() as usize].is_nan() {
            return None;
        }
        let parent = self.arena.parent(id)?;
        match self.layout[parent.index() as usize] {
            LayoutInput::AbsoluteRows { axis, .. } => Some((parent, axis)),
            _ => None,
        }
    }

    /// How many parent links lie between `id` and the top of its tree.
    pub(crate) fn depth_of(&self, id: NodeId) -> u32 {
        let mut depth = 0;
        let mut at = id.node_index();
        loop {
            let parent = self.arena.parent_of(at);
            if parent.is_none() {
                return depth;
            }
            depth += 1;
            at = parent;
        }
    }

    /// Whether `id` or one of its ancestors is an accepted redo root.
    fn under_flagged(&self, id: NodeId) -> bool {
        let mut at = id.node_index();
        while !at.is_none() {
            if self.redo_flag[at.slot()] {
                return true;
            }
            // A layout from above a layer skips it.
            if self.redone_alone_at(at) {
                return false;
            }
            at = self.arena.parent_of(at);
        }
        false
    }

    /// The percent basis node `id`'s box gives its children, as a layout from
    /// `root` down reaches it.
    fn basis_of(&self, id: NodeId, root: NodeId) -> layout::Basis {
        match self.parent(id) {
            Some(parent) if id != root => {
                let (above, child) = (
                    self.layout[parent.index() as usize],
                    self.layout[id.index() as usize],
                );
                let basis = self.basis_of(parent, root);
                match above {
                    LayoutInput::Absolute { .. } => {
                        basis.placed(child.size(), layout::pinned(self, id.index()))
                    }
                    _ => basis.child(above, child),
                }
            }
            _ => layout::Basis::DEFINITE,
        }
    }

    /// Bring the primitive list up to date when any paint-affecting
    /// invalidation is pending.
    ///
    /// The nodes whose look or box changed re-emit their own primitives, the
    /// nodes whose children changed diff them against the stream, and the
    /// result is spliced into `out`. When every rewritten primitive keeps its
    /// place and kind the frame is [`Painted::Patched`], the positions at
    /// [`patched`](Self::patched); otherwise [`Painted::Spliced`], the runs
    /// at [`splices`](Self::splices). A frame where a transform root, a fade
    /// or an overlay starts or stops, or a transform root or an overlay enters
    /// or leaves, re-emits the whole tree. A `TRANSFORM` mark alone does
    /// nothing here: its roots move through
    /// [`update_transforms`](Self::update_transforms) and the renderer shifts
    /// their retained draws.
    pub fn repaint_dirty(
        &mut self,
        root: NodeId,
        out: &mut Vec<viso_render::Primitive>,
    ) -> Painted {
        if !self.arena.is_live(root) {
            return Painted::Nothing;
        }
        let paint_classes = DirtyClass::PAINT | DirtyClass::MEASURE | DirtyClass::LAYOUT;
        if !self.repaint_pending
            && !self.any_dirty_class(paint_classes)
            && self.moved.is_empty()
            && self.arena.relinks().is_empty()
        {
            return Painted::Nothing;
        }
        if !self.repaint_pending
            && let Some(spliced) = self.splice(root, out)
        {
            self.clear_paint_logs();
            return match spliced {
                crate::paint::Spliced::Patched(n) => Painted::Patched(n),
                crate::paint::Spliced::Spliced(n) => Painted::Spliced(n),
            };
        }
        Painted::Whole(self.repaint(root, out))
    }

    /// Splice the changes since the last paint into `out`.
    fn splice(
        &mut self,
        root: NodeId,
        out: &mut Vec<viso_render::Primitive>,
    ) -> Option<crate::paint::Spliced> {
        let mut painting = self.painting.lend();
        let p = &mut *painting;
        let painted = self
            .dirty_lists
            .of(DirtyClass::PAINT)
            .iter()
            .copied()
            .filter(|&i| self.dirty[i as usize].contains(DirtyClass::PAINT));
        let spliced = crate::paint::splice_walk(
            self,
            root,
            &mut p.transforms,
            &mut p.index,
            painted.chain(self.moved.iter().copied()),
            self.arena.relinks(),
            out,
            &mut p.patched,
            &mut p.splices,
        );
        self.painting.give_back(painting);
        spliced
    }

    /// Forget what changed since the last paint: the next one starts from it.
    fn clear_paint_logs(&mut self) {
        self.moved.clear();
        self.arena.clear_relinks();
    }

    /// The stream positions the last [`Painted::Patched`] rewrote, ascending.
    pub fn patched(&self) -> &[u32] {
        &self.painting.get().patched
    }

    /// The runs of the stream the last [`Painted::Spliced`] replaced,
    /// ascending, in the new stream's indices.
    pub fn splices(&self) -> &[viso_render::Splice] {
        &self.painting.get().splices
    }

    /// Re-emit the whole tree at `root` into `out` and record its transform
    /// roots, whatever is dirty. Reports how many primitives were emitted.
    pub fn repaint(&mut self, root: NodeId, out: &mut Vec<viso_render::Primitive>) -> u32 {
        self.repaint_pending = false;
        self.clear_paint_logs();
        out.clear();
        if !self.arena.is_live(root) {
            let painting = self.painting.get_mut();
            painting.transforms.clear();
            painting.index.clear();
            return 0;
        }
        let mut painting = self.painting.lend();
        let p = &mut *painting;
        crate::paint::paint_indexed(self, root, out, &mut p.transforms, &mut p.index);
        self.painting.give_back(painting);
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
            self.derive_into(root, self.inert(root), &mut tree);
        }
        tree
    }

    /// Push `id`'s derived row, recurse its children in sibling order, and
    /// record their indices on the row. Returns `id`'s own index in `tree.nodes`
    /// so the parent can link it. Pre-order, mirroring the paint walk.
    /// `inert` is whether an ancestor is disabled.
    fn derive_into(&self, id: NodeId, inert: bool, tree: &mut SemanticsTree) -> usize {
        let disabled = inert || self.disabled[id.index() as usize];
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
        let links = self.links(id).len() as u16;
        // A text of links holds the focus on one of them.
        let focused = self.focused == Some(id) && links == 0;
        tree.nodes.push(SemanticsNode {
            id,
            link: None,
            role,
            label,
            hint: authored.and_then(|s| s.hint.clone()),
            value: authored.and_then(|s| s.value.clone()),
            live: authored.map(|s| s.live).unwrap_or_default(),
            heading_level: authored.and_then(|s| s.heading_level),
            focused,
            disabled,
            // Live state comes from the node's own side column (`&self`), never
            // a cross-layer state-store read: a control's projection binding
            // wrote it in the flush phase.
            state: self.semantic_state(id),
            active: self.active_descendant(id),
            validity: self.validity(id),
            table: self.table_facts(id),
            bounds: self.bounds(id),
            children: Vec::new(),
        });
        for c in self.arena.children(id) {
            // A subtree authored hidden is shown but not read; one folded out
            // of layout is neither.
            if !self.concealed(c.index() as usize) && !self.semantics(c).is_some_and(|s| s.hidden) {
                let ci = self.derive_into(c, disabled, tree);
                tree.nodes[my_index].children.push(ci);
            }
        }
        let on = self.focused_link(id);
        let origin = self.bounds(id);
        for link in 0..links {
            let ci = tree.nodes.len();
            let bounds = self
                .link_boxes(id, link)
                .reduce(Rect::union)
                .map_or(origin, |seen| {
                    let world = self.world(id);
                    Rect {
                        x: origin.x + seen.x - world.x,
                        y: origin.y + seen.y - world.y,
                        ..seen
                    }
                });
            tree.nodes.push(SemanticsNode {
                id,
                link: Some(link),
                role: Role::Link,
                label: self.link_label(id, link).map(str::to_owned),
                hint: None,
                value: None,
                live: crate::semantics::LiveRegion::Off,
                heading_level: None,
                focused: on == Some(link),
                disabled,
                state: None,
                active: None,
                validity: crate::semantics::Validity::default(),
                table: crate::semantics::TableFacts::default(),
                bounds,
                children: Vec::new(),
            });
            tree.nodes[my_index].children.push(ci);
        }
        my_index
    }

    /// Re-derive the accessibility tree only when a SEMANTICS invalidation is
    /// pending, returning the new tree (or `None` when SEMANTICS is clean, so a
    /// frame with no semantic change does no work). The whole tree is rebuilt —
    /// SEMANTICS bubbles to the root, so any change reaches `root`; there is no
    /// per-subtree caching.
    pub fn derive_semantics_dirty(&self, root: NodeId) -> Option<SemanticsTree> {
        if !self.any_dirty_class(DirtyClass::SEMANTICS) {
            return None;
        }
        Some(self.derive_semantics(root))
    }

    /// Count of live nodes in the subtree rooted at `id` (inclusive). Used to
    /// report recompute work; walks ancestry links, no allocation.
    pub(crate) fn subtree_len(&self, id: NodeId) -> u32 {
        let mut count = 1;
        for c in self.arena.children(id) {
            count += self.subtree_len(c);
        }
        count
    }
}

/// What [`NodeStore::repaint_dirty`] did to the primitive list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Painted {
    /// Nothing paint-affecting was pending: the list is as it was.
    Nothing,
    /// The whole tree was re-emitted: this many primitives.
    Whole(u32),
    /// This many primitives were rewritten in place, at
    /// [`NodeStore::patched`].
    Patched(u32),
    /// Runs of the list were replaced, at [`NodeStore::splices`]: this many
    /// primitives went in.
    Spliced(u32),
}

impl Painted {
    /// The primitives emitted or rewritten.
    pub fn count(self) -> u32 {
        match self {
            Painted::Nothing => 0,
            Painted::Whole(n) | Painted::Patched(n) | Painted::Spliced(n) => n,
        }
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
    /// Primitives the paint emitted, or rewrote in place when only paint
    /// changed (0 when paint was skipped).
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
    fn rtl(&self, index: u32) -> bool {
        self.direction_at(index) == crate::adaptive::LayoutDirection::Rtl
    }

    #[inline]
    fn chrome(&self, index: u32) -> bool {
        self.chrome[index as usize]
    }

    #[inline]
    fn input(&self, index: u32) -> LayoutInput {
        self.layout[index as usize]
    }

    #[inline]
    fn children(&self, index: u32, out: &mut Vec<u32>) {
        // Layout walks live nodes only, and internal links always point at live
        // slots: the bare index chain is the whole traversal.
        out.extend(
            self.arena
                .children_of(NodeIndex::new(index))
                .map(NodeIndex::get),
        );
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
        let slot = &mut self.bounds[index as usize];
        if *slot == r {
            return;
        }
        *slot = r;
        self.turn_moved |= self.turn[index as usize].turns();
        self.note_moved(index);
        // A segmented control's thumb and hairlines sit on its segments'
        // boxes: a segment that moves repaints them.
        let parent = self.arena.parent_of(NodeIndex::new(index));
        if !parent.is_none()
            && matches!(
                self.content_payload[parent.slot()].as_deref(),
                Some(Content::Segments(_))
            )
        {
            self.note_moved(parent.get());
        }
    }

    #[inline]
    fn set_content(&mut self, index: u32, content: Vec2) {
        let i = index as usize;
        self.content[i] = content;
        // The offset keeps within the new range; one taken the other way
        // round (the direction turned) keeps its distance from the start.
        let Some(id) = self.arena.live_id(index) else {
            return;
        };
        let mut offset = self.scroll[i];
        if offset == Vec2::ZERO {
            return;
        }
        let ((x0, x1), (y0, y1)) = (
            self.scroll_span(id, Axis::Row),
            self.scroll_span(id, Axis::Column),
        );
        if (offset.x > 0.0 && x0 < 0.0) || (offset.x < 0.0 && x1 > 0.0) {
            offset.x = -offset.x;
        }
        let next = Vec2 {
            x: offset.x.clamp(x0, x1),
            y: offset.y.clamp(y0, y1),
        };
        if next != self.scroll[i] {
            self.scroll[i] = next;
            self.mark_dirty(id, DirtyClass::TRANSFORM);
        }
    }

    #[inline(always)]
    fn rules(&self, index: u32) -> Option<&BoxRules> {
        if self.ruled == 0 {
            return None;
        }
        match self.rules_at[index as usize] {
            NO_RULES => None,
            slot => Some(&self.box_rules[slot as usize].0),
        }
    }

    #[inline(always)]
    fn keep_within(&mut self, index: u32, within: Vec2) -> Option<&BoxRules> {
        if self.ruled == 0 {
            return None;
        }
        match self.rules_at[index as usize] {
            NO_RULES => None,
            slot => {
                let kept = &mut self.box_rules[slot as usize];
                kept.1 = within;
                Some(&kept.0)
            }
        }
    }

    #[inline]
    fn kept_within(&self, index: u32) -> Vec2 {
        let slot = self.rules_at[index as usize];
        if self.ruled == 0 || slot == NO_RULES {
            layout::INDEFINITE
        } else {
            self.box_rules[slot as usize].1
        }
    }

    #[inline]
    fn row_lane(&self, index: u32) -> u16 {
        self.row_lane
            .get(index as usize)
            .copied()
            .unwrap_or(crate::virtual_grid::FULL_LANE)
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
    fn grid_gap_pct(&self, index: u32) -> [f32; 2] {
        self.grid_tracks[index as usize]
            .as_ref()
            .map_or([0.0; 2], |t| t.gap_pct)
    }

    #[inline]
    fn placement(&self, index: u32) -> Option<&Placement> {
        match self.placed_at[index as usize] {
            NO_PLACEMENT => None,
            slot => Some(&self.placements[slot as usize]),
        }
    }

    #[inline]
    fn content_natural(&self, index: u32) -> Option<Vec2> {
        let content = self.content_payload[index as usize].as_deref();
        match self.check_marks[index as usize] {
            Some((mark, shown)) => {
                let (text, baseline) = caption_of(content);
                Some(mark.natural(text, baseline, shown))
            }
            None => content.map(|c| c.natural()),
        }
    }

    #[inline]
    fn content_min_width(&self, index: u32) -> f32 {
        let content = self.content_payload[index as usize].as_deref();
        let min = content.map_or(0.0, |c| c.min_width());
        match self.check_marks[index as usize] {
            Some((mark, shown)) => {
                let (text, baseline) = caption_of(content);
                let lead = mark.natural(text, baseline, shown).x - text.x;
                lead + min
            }
            None => min,
        }
    }

    #[inline]
    fn content_baseline(&self, index: u32) -> Option<f32> {
        let content = self.content_payload[index as usize].as_deref();
        let baseline = content.and_then(|c| c.baseline());
        match self.check_marks[index as usize] {
            Some((mark, shown)) => {
                let (text, line) = caption_of(content);
                baseline.map(|b| b + mark.place(text, line, Vec2::ZERO, false, shown).text.y)
            }
            None => baseline,
        }
    }

    #[inline]
    fn subgrid_axes(&self, index: u32) -> (bool, bool) {
        match self.layout[index as usize] {
            LayoutInput::Grid {
                subgrid_columns,
                subgrid_rows,
                ..
            } => (subgrid_columns, subgrid_rows),
            _ => (false, false),
        }
    }

    #[inline]
    fn hidden(&self, index: u32) -> bool {
        self.concealed(index as usize)
    }

    #[inline]
    fn out_of_flow(&self, index: u32) -> bool {
        self.skips_layer(index)
    }

    fn report_length(&mut self, index: u32, issue: crate::length::LengthIssue) {
        self.report_length_issue(index, issue);
    }

    fn take_grid_scratch(&mut self) -> crate::layout::GridScratch {
        self.grid_scratch.pop().unwrap_or_default()
    }

    fn put_grid_scratch(&mut self, scratch: crate::layout::GridScratch) {
        self.grid_scratch.push(scratch);
    }

    fn request_text_reflow(&mut self, index: u32, width: f32) {
        // Eligibility lives entirely here (the trait method is a pure data
        // write). A reflow is enqueued only for a text run that depends on its
        // box's width and whose assigned width differs from the width it was
        // last shaped at.
        let Some(Content::Text {
            max_content,
            shaped_at_width,
            width_use,
            ..
        }) = self.content_payload[index as usize].as_deref()
        else {
            return;
        };
        // Quantize the assigned width to integer physical px before comparing:
        // a continuous drag-resize assigns a slightly different sub-pixel width
        // every frame, and reshaping on every one would defeat the text cache
        // ("unchanged width must not reshape").
        let width = match self.check_marks[index as usize] {
            Some((mark, shown)) => {
                let (text, baseline) = caption_of(self.content_payload[index as usize].as_deref());
                mark.caption_width(width, text, baseline, shown)
            }
            None => width,
        };
        let qwidth = width.round();
        const EPS: f32 = 0.5;
        let changed = match (shaped_at_width, width_use) {
            (_, WidthUse::Ignored) => false,
            // Shaped unconstrained, it lays out as it is in a box as wide as
            // it: one that wraps or ends in an ellipsis needs a narrower box,
            // one that aligns any other.
            (None, WidthUse::Narrower) => qwidth < max_content - EPS,
            (None, WidthUse::Any) => (qwidth - max_content).abs() > EPS,
            // Shaped at some width: it reflows when the assigned width moved.
            (Some(w), _) => (w - qwidth).abs() > EPS,
        };
        if !changed {
            return;
        }
        if let Some(id) = self.arena.live_id(index) {
            self.text_reflows.push((id, qwidth));
        }
    }
}

/// The natural size and first-line baseline of a check's caption `content`:
/// nothing for no caption.
pub(crate) fn caption_of(content: Option<&Content>) -> (Vec2, f32) {
    content.map_or((Vec2::ZERO, 0.0), |c| {
        (c.natural(), c.baseline().unwrap_or(0.0))
    })
}

/// Whether a node's natural size does not follow its children's: it is fixed
/// on both axes, or an absolute container, which no child enters into. It is
/// the boundary where a rising MEASURE invalidation stops.
#[inline]
fn measures_alone(input: LayoutInput) -> bool {
    matches!(input, LayoutInput::Absolute { .. }) || fixed_on_both_axes(input)
}

/// Whether a node's own size request is a hard pixel size on both axes. Such a
/// node's box cannot change when a descendant's natural size does.
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
    pub(crate) store: &'a mut NodeStore,
    /// Reactive state cells, present only for a `with_reactive` cx. `None` for
    /// a `new`-built (node-only) cx.
    states: Option<&'a mut StateStore>,
    /// Compiled state→node edges, present only for a `with_reactive` cx.
    bindings: Option<&'a mut BindingTable>,
    /// Driver-owned virtual-list registry, present only for a `with_reactive`
    /// cx. A `virtual_list` call registers its per-list state here keyed by the
    /// viewport's node index; `None` for a node-only or child-body cx (neither
    /// declares a top-level virtual list).
    pub(crate) lists: Option<&'a mut crate::virtual_list::VirtualLists>,
    /// Driver-owned edit-buffer registry, present only for a `with_reactive` cx.
    /// A `text_input` call registers the editable node's retained [`Buffer`] here
    /// keyed by that node's index (mirroring `lists`); `None` for a node-only or
    /// child-body cx.
    text_edits: Option<&'a mut crate::text_edit::TextEdits>,
    /// Driver-owned semantic-state projections, present only for a `with_reactive`
    /// cx. A control registers a projection here through
    /// [`bind_semantic_state`](BuildCx::bind_semantic_state); the projection reads
    /// the control's reactive cell in the flush phase and writes the node's
    /// `semantic_state` column, so the semantics derive pass stays single-layer.
    /// `None` for a node-only or child-body cx.
    projectors: Option<&'a mut SemanticProjector>,
    /// Parent cursor stack; the top is the current insertion parent.
    pub(crate) stack: Vec<NodeId>,
    /// The placement to apply to the next child authored inside the current
    /// grid closure, then cleared. `None` outside a grid or once consumed.
    pending_placement: Option<Placement>,
    /// The active grid's creation-time name tables (line names + template areas),
    /// so `place_named`/`place_area` inside the grid closure resolve names to
    /// indices. Cold and boxed — set on entering a `grid()` closure, restored to
    /// the enclosing grid's tables (or `None`) on exit, so nested grids resolve
    /// against their own names. `None` outside any grid.
    grid_names: Option<Box<GridNames>>,
    /// Per-window chrome facts for a caption widget, seeded only on the facade's
    /// build path through [`with_chrome`](Self::with_chrome). Defaults to
    /// `(Native, None)` for every other cx (node-only, child-body, tests), so a
    /// caption authored outside a self-drawn-chrome window reads a sane default.
    /// Cold and `Copy` — read once at build to choose the caption's structure.
    chrome: ChromeContext,
    /// The tree root, set by the first declared node.
    root: Option<NodeId>,
}

impl crate::Controls for BuildCx<'_> {
    fn control<P: 'static>(&self, id: crate::ControlId) -> Option<Rc<P>> {
        self.states.as_deref()?.controls.get(id)
    }
}

/// The creation-time name tables of one grid, cloned out of its [`GridStyle`] so
/// `place_named`/`place_area` can resolve names while the grid closure runs. Cold:
/// only grids with named lines or template areas allocate one.
struct GridNames {
    /// Column line names → 0-based line indices.
    column_line_names: crate::grid::LineNames,
    /// Row line names → 0-based line indices.
    row_line_names: crate::grid::LineNames,
    /// Template-areas table, if the grid declared one.
    areas: Option<crate::grid::GridAreas>,
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
            text_edits: None,
            projectors: None,
            stack: Vec::new(),
            pending_placement: None,
            grid_names: None,
            chrome: ChromeContext::default(),
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
        text_edits: &'a mut crate::text_edit::TextEdits,
        projectors: &'a mut SemanticProjector,
    ) -> Self {
        BuildCx {
            store,
            states: Some(states),
            bindings: Some(bindings),
            lists: Some(lists),
            text_edits: Some(text_edits),
            projectors: Some(projectors),
            stack: Vec::new(),
            pending_placement: None,
            grid_names: None,
            chrome: ChromeContext::default(),
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
            text_edits: None,
            projectors: None,
            stack: vec![parent],
            pending_placement: None,
            grid_names: None,
            chrome: ChromeContext::default(),
            root: None,
        }
    }

    /// Lends the cx a state store alone: enough for the controls it builds to
    /// bind their handles, without the reactive tables.
    #[inline]
    pub fn with_states(mut self, states: &'a mut StateStore) -> Self {
        self.states = Some(states);
        self
    }

    /// Lends the cx the virtual-list registry, so a list it declares — one in a
    /// list's row, say — is registered and reconciled like any other.
    #[inline]
    pub fn with_lists(mut self, lists: &'a mut crate::virtual_list::VirtualLists) -> Self {
        self.lists = Some(lists);
        self
    }

    /// Seed the per-window chrome facts a caption widget reads at build time,
    /// returning the cx so the facade can chain it after
    /// [`with_reactive`](Self::with_reactive). Only the facade calls this — it
    /// holds the platform seam that resolves the [`ChromeContext`] — so every
    /// other build path keeps the `(Native, None)` default. A builder-style
    /// setter rather than a constructor parameter, so existing `with_reactive`
    /// call sites (tests, docs, the node-only path) need no change.
    #[inline]
    pub fn with_chrome(mut self, chrome: ChromeContext) -> Self {
        self.chrome = chrome;
        self
    }

    /// The per-window chrome facts seeded for this build (default `(Native,
    /// None)` when unseeded). A caption widget reads this to choose whether to
    /// reserve a leading spacer for native OS buttons or to draw its own —
    /// driven by the data contract, not `target_os`.
    #[inline]
    pub fn chrome(&self) -> ChromeContext {
        self.chrome
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
            justify: style.justify,
            size: style.size,
        };
        let id = self.push_node(input, style.style);
        self.stack.push(id);
        children(self);
        self.stack.pop();
        Handle { id }
    }

    /// Declare a stack and its children: each child sits in the stack's
    /// content box at its own alignment (a preceding [`BuildCx::place`] of a
    /// [`StackPlacement`]) or the stack's, and the stack fits the largest.
    /// Children paint by ascending layer, those of one layer in order.
    pub fn stack(&mut self, style: StackStyle, children: impl FnOnce(&mut BuildCx<'_>)) -> Handle {
        let input = LayoutInput::Stack {
            padding: style.padding,
            align: style.align,
            size: style.size,
        };
        self.container(input, style.style, children)
    }

    /// Declare an absolute container and its children: each child is pinned
    /// by the [`AbsoluteInsets`] a preceding [`BuildCx::place`] gives it to
    /// the edges of the content box, at its top start when it names none.
    /// No child enters into the container's size.
    pub fn absolute(
        &mut self,
        style: AbsoluteStyle,
        children: impl FnOnce(&mut BuildCx<'_>),
    ) -> Handle {
        let input = LayoutInput::Absolute {
            padding: style.padding,
            size: style.size,
        };
        self.container(input, style.style, children)
    }

    /// Declare a container of `input` and its children, a placement no child
    /// took not leaking to a sibling.
    fn container(
        &mut self,
        input: LayoutInput,
        style: BoxStyle,
        children: impl FnOnce(&mut BuildCx<'_>),
    ) -> Handle {
        let id = self.push_node(input, style);
        self.stack.push(id);
        children(self);
        self.stack.pop();
        self.pending_placement = None;
        Handle { id }
    }

    /// Declare `children` under the already-declared node `handle`, after
    /// any it has: under a leaf they lie over its box and never size it, as
    /// a video's controls over its picture.
    pub fn inside<R>(&mut self, handle: Handle, children: impl FnOnce(&mut BuildCx<'_>) -> R) -> R {
        self.stack.push(handle.id);
        let out = children(self);
        self.stack.pop();
        self.pending_placement = None;
        out
    }

    /// Declare a flex container that is an adaptive scope: the readers under
    /// it get the size class of `basis` dp when given, else of the width its
    /// parent gives it, and its subtree's lines run in `direction` when given,
    /// else in its parent's. A node-only cx builds the container alone.
    pub fn adaptive_scope(
        &mut self,
        basis: Option<f32>,
        direction: Option<crate::adaptive::LayoutDirection>,
        style: FlexStyle,
        children: impl FnOnce(&mut BuildCx<'_>),
    ) -> Handle {
        let handle = self.flex(style, children);
        if direction.is_some() {
            self.store.set_layout_direction(handle.id, direction);
        }
        if let Some(states) = self.states.as_deref_mut() {
            states.mark_adaptive_scope(handle.id, basis);
        }
        handle
    }

    /// Declare a flex container keeping its content clear of `avoid`: once
    /// laid out it is padded by its style's padding plus the part of its box
    /// `avoid` covers. A node-only cx builds the container alone.
    pub fn avoiding(
        &mut self,
        avoid: crate::adaptive::Avoid,
        style: FlexStyle,
        children: impl FnOnce(&mut BuildCx<'_>),
    ) -> Handle {
        let base = style.padding;
        let handle = self.flex(style, children);
        if let Some(states) = self.states.as_deref_mut() {
            states.mark_avoiding(handle.id, avoid, base);
        }
        handle
    }

    /// Declare a grid container and its children. The `children` closure runs
    /// with this grid as the active parent; each child lands in a cell (auto-flow
    /// unless a preceding [`BuildCx::place`] pinned it). A child inside a cell
    /// still honors its own `Sizing`: a fill child stretches to the cell, a fit or
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
            align_items: style.align_items,
            subgrid_columns: style.subgrid_columns,
            subgrid_rows: style.subgrid_rows,
            adaptive_columns: style.adaptive_columns,
            size: style.size,
        };
        let id = self.push_node(input, style.style);
        self.store.set_grid_tracks(id, style.columns, style.rows);
        // Stash this grid's creation-time name tables so `place_named`/`place_area`
        // resolve against them, saving the enclosing grid's tables so a nested grid
        // resolves against its own names and the outer grid is restored on exit.
        // Only a grid that actually declared names allocates a `GridNames`.
        let has_names = !style.column_line_names.is_empty()
            || !style.row_line_names.is_empty()
            || style.areas.is_some();
        let outer_names = self.grid_names.take();
        if has_names {
            self.grid_names = Some(Box::new(GridNames {
                column_line_names: style.column_line_names,
                row_line_names: style.row_line_names,
                areas: style.areas,
            }));
        }
        self.stack.push(id);
        children(self);
        self.stack.pop();
        // A stray placement not consumed by a child does not leak to a sibling.
        self.pending_placement = None;
        // Restore the enclosing grid's name tables (or clear, at the outermost grid).
        self.grid_names = outer_names;
        Handle { id }
    }

    /// Declare an [`LayoutInput::AbsoluteRows`] canvas and its children. The canvas
    /// places each child at its own [`set_row_offset`](NodeStore::set_row_offset)
    /// along `axis` (skipping any child with no row offset) and forces every placed
    /// child to the canvas's full cross extent; `size` fixes the canvas's own
    /// extent. The general authoring counterpart to the absolute-row placement the
    /// virtualized list drives internally — a control that positions a small set of
    /// children by an owned coordinate (a floating dock panel, a placed overlay)
    /// authors the canvas here and drives each child's offset from its reconcile
    /// step, rather than through a flex or grid flow.
    ///
    /// Children authored in the closure start with no row offset (so an
    /// un-positioned child is skipped by the canvas until its offset is set); the
    /// reconcile step assigns each mounted child its offset.
    pub fn absolute_rows(
        &mut self,
        axis: Axis,
        size: Sizing,
        children: impl FnOnce(&mut BuildCx<'_>),
    ) -> Handle {
        let id = self.push_node(
            LayoutInput::AbsoluteRows {
                axis,
                size,
                lanes: 1,
                cross_gap: 0.0,
            },
            BoxStyle::NONE,
        );
        self.stack.push(id);
        children(self);
        self.stack.pop();
        Handle { id }
    }

    /// Declare where the next child sits in its parent: a [`GridPlacement`]
    /// in a grid, a [`StackPlacement`] in a stack, [`AbsoluteInsets`] in an
    /// absolute container. With no `place` call a grid child auto-flows with
    /// span 1, a stack child takes the stack's alignment on layer 0 and an
    /// absolute child sits at the content box's top start. Ignored for a
    /// child of another kind of parent.
    pub fn place(&mut self, placement: impl Into<Placement>) {
        let placement = placement.into();
        self.pending_placement = (!placement.is_default()).then_some(placement);
    }

    /// Place the next grid child by **line name**, spanning `column_span` columns
    /// and `row_span` rows from the named start lines. Names come from the grid's
    /// [`GridStyle::column_line_names`] / [`GridStyle::row_line_names`], resolved to
    /// 0-based line indices at build time — the stored placement is the same
    /// `Option<u16>` a numeric [`place`](Self::place) produces, so the runtime path
    /// never carries a `String`. An unknown name auto-flows that
    /// axis (`None`), the same as omitting it. Pass `None` for an axis to auto-flow
    /// it explicitly.
    pub fn place_named(
        &mut self,
        column: Option<&str>,
        row: Option<&str>,
        column_span: u16,
        row_span: u16,
    ) {
        let names = self.grid_names.as_deref();
        let column = column.and_then(|name| {
            names.and_then(|n| crate::grid::resolve_line(&n.column_line_names, name))
        });
        let row = row.and_then(|name| {
            names.and_then(|n| crate::grid::resolve_line(&n.row_line_names, name))
        });
        self.place(GridPlacement {
            column,
            row,
            column_span: column_span.max(1),
            row_span: row_span.max(1),
        });
    }

    /// Place the next grid child into a named **template area** declared in the
    /// grid's [`GridStyle::areas`]. The area name resolves to an explicit
    /// [`GridPlacement`] (start column/row + spans) at build time; the runtime path
    /// stays `String`-free. An unknown area name (or a grid with no
    /// `areas` table) leaves the child to auto-flow, the same as no `place` call.
    pub fn place_area(&mut self, name: &str) {
        if let Some(placement) = self
            .grid_names
            .as_deref()
            .and_then(|n| n.areas.as_ref())
            .and_then(|areas| areas.placement(name))
        {
            self.place(placement);
        }
    }

    /// Declare a scroll viewport and its content. Like [`BuildCx::flex`] the
    /// `children` closure runs with the viewport as the active parent; the
    /// viewport lays out a single content child at its natural extent along
    /// `axis` (so overflow becomes scrollable) and clips to its own box. Author a
    /// single container child to hold the scrollable content. With
    /// [`ScrollBars::Auto`](crate::ScrollBars::Auto) the viewport also builds
    /// its scroll bar (see [`scroll_bar`](crate::scroll_bar)).
    pub fn scroll(
        &mut self,
        style: ScrollStyle,
        children: impl FnOnce(&mut BuildCx<'_>),
    ) -> Handle {
        let input = LayoutInput::Scroll {
            axis: style.axis,
            across: style.across,
            size: style.size,
            gutter: Vec2::ZERO,
        };
        let id = self.push_node(input, style.style);
        self.stack.push(id);
        children(self);
        self.stack.pop();
        self.viewport_ways(id, &style);
        Handle { id }
    }

    /// Give viewport `id` its bars, tail, refresh, snapping and page dots,
    /// as `style` says.
    fn viewport_ways(&mut self, id: NodeId, style: &ScrollStyle) {
        if style.bars == crate::scroll_bar::ScrollBars::Auto {
            self.scroll_bar(id, style.axis);
            if style.across {
                self.scroll_bar(id, crate::layout::cross_of(style.axis));
            }
        }
        if style.tail {
            self.store.set_scroll_tail(id, true);
        }
        if style.refresh {
            self.store.set_refreshable(id, true);
        }
        if style.snap.is_some() {
            self.store.set_scroll_snap(id, style.snap);
        }
        if let Some(dots) = style.dots {
            self.viewport_dots(id, style.axis, dots);
        }
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
        self.virtual_list_inner(style, item_count, Box::new(item), None)
    }

    /// Like [`Self::virtual_list`], but with stable item keys: `key_of` maps a
    /// logical index to a data-defined [`ItemKey`](crate::virtual_list::ItemKey)
    /// that identifies the row independent of its position. On a reorder or a
    /// mid-list insert/delete, the reconcile matches a mounted host to a row by
    /// this key and re-anchors the surviving host — preserving its body, per-row
    /// widget state (in-row scroll, text caret, expand/collapse), and focus —
    /// instead of rebuilding whichever row now sits at that index.
    ///
    /// Prefer this whenever the list's data can reorder or splice and rows carry
    /// per-row state. A keyless [`Self::virtual_list`] treats the logical index as
    /// identity, which is correct and cheaper for append-only / scroll-only lists.
    pub fn virtual_list_keyed(
        &mut self,
        style: VirtualListStyle,
        item_count: usize,
        key_of: impl Fn(usize) -> crate::virtual_list::ItemKey + 'static,
        item: impl FnMut(usize, &mut BuildCx<'_>) + 'static,
    ) -> Handle {
        self.virtual_list_inner(style, item_count, Box::new(item), Some(Box::new(key_of)))
    }

    /// Shared body of the keyless and keyed constructors: builds the scroll
    /// viewport + fixed-extent canvas and registers the list state, with `key_of`
    /// deciding index-identity (`None`) vs keyed (`Some`).
    fn virtual_list_inner(
        &mut self,
        style: VirtualListStyle,
        item_count: usize,
        item: crate::virtual_list::ItemBuilder,
        key_of: Option<crate::virtual_list::KeyOf>,
    ) -> Handle {
        // The viewport is an ordinary scroll node: it reuses the whole scroll
        // machinery (range, scroll_by, hit-test narrowing, the scroll router)
        // unchanged.
        let viewport = self.push_node(
            LayoutInput::Scroll {
                axis: style.axis,
                across: style.across,
                size: style.size,
                gutter: Vec2::ZERO,
            },
            style.style,
        );

        let lists = self
            .lists
            .as_mut()
            .expect("virtual_list() requires a with_reactive BuildCx");
        crate::virtual_list::attach_list(
            self.store,
            lists,
            viewport,
            crate::virtual_list::ListShape {
                axis: style.axis,
                estimated_row: style.estimated_row,
                overscan: style.overscan as usize,
                pages: style.pages,
                grid: style.grid,
            },
            item_count,
            item,
            key_of,
        );
        self.viewport_ways(
            viewport,
            &ScrollStyle {
                axis: style.axis,
                across: style.across,
                size: style.size,
                style: style.style,
                bars: style.bars,
                tail: style.tail,
                refresh: style.refresh,
                snap: style.snap,
                dots: style.dots,
            },
        );
        Handle { id: viewport }
    }

    /// Binds environment-dependent `lengths` to the node `handle` declared;
    /// they fold at the first layout (see [`NodeStore::bind_lengths`]).
    pub fn bind_lengths(&mut self, handle: Handle, lengths: crate::length::NodeLengths) {
        self.store.bind_lengths(handle.id, lengths);
    }

    /// Reserve room for `additional` more nodes before building them (see
    /// [`NodeStore::reserve_nodes`]).
    pub fn reserve_nodes(&mut self, additional: usize) {
        self.store.reserve_nodes(additional);
    }

    /// Bound `handle`'s box beyond its size request and give it a margin and
    /// a padding percentage (see [`NodeStore::set_box_rules`]).
    pub fn set_box_rules(&mut self, handle: Handle, rules: BoxRules) {
        self.store.set_box_rules(handle.id, rules);
    }

    /// Declare a leaf node.
    pub fn leaf(&mut self, style: LeafStyle) -> Handle {
        let id = self.push_node(LayoutInput::Leaf { size: style.size }, style.style);
        Handle { id }
    }

    /// Declare an editable text leaf: a leaf node plus a retained edit
    /// [`Buffer`](crate::text_edit::Buffer) seeded from `request` and `look`, and registered
    /// in the driver-owned [`TextEdits`](crate::text_edit::TextEdits) registry,
    /// keyed by this node. Mirrors `virtual_list`: the heap-heavy buffer lives in
    /// the driver registry beside the store, not in a hot node column, and
    /// [`reconcile`](crate::text_edit::reconcile) folds queued intents into it and
    /// re-declares its text each frame. What the buffer draws is also declared
    /// on the node so the first frame shapes it. A cx without the registry
    /// (one a structure hook builds with) queues the field's seed instead,
    /// which the next reconcile registers its buffer from. Returns the handle
    /// so the widget body chains focus/handler/semantics onto it.
    pub fn text_input(
        &mut self,
        style: LeafStyle,
        request: TextRequest,
        look: crate::text_edit::FieldLook,
    ) -> Handle {
        let id = self.push_node(LayoutInput::Leaf { size: style.size }, style.style);
        let number = look.number.is_some();
        let seed = crate::text_edit::FieldSeed {
            request,
            valued: true,
            look,
        };
        let buffer = crate::text_edit::Buffer::with_seed(&seed);
        self.store.set_text_request(id, buffer.request());
        self.store.make_field(id);
        self.store.set_number_field(id, number);
        match self.text_edits.as_mut() {
            Some(edits) => edits.register(id, Box::new(buffer)),
            None => self.store.seed_text(id, seed),
        }
        Handle { id }
    }

    /// Leaves the nodes inside `handle` out of hit testing, so a press on
    /// any of them lands on `handle` itself: a row whose label and glyphs
    /// are its look. Call it once they are built.
    pub fn unhit_below(&mut self, handle: Handle) {
        let mut stack: Vec<NodeId> = self.store.arena().children(handle.id).collect();
        while let Some(id) = stack.pop() {
            self.store.set_hittable(id, false);
            stack.extend(self.store.arena().children(id));
        }
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

    /// Attach a key handler to an already-declared node and return the handle so
    /// registration chains inline. The mirror of `on_pointer` for the focused-node
    /// key route: the closure is stored cold and driven by the key router only
    /// while this node is on the focused node's ancestry chain (not hit-tested).
    /// An interactive control uses it to give a pointer action a keyboard
    /// equivalent (Enter/Space activation).
    pub fn on_key(
        &mut self,
        handle: Handle,
        handler: impl FnMut(&mut EventCx<'_>) + 'static,
    ) -> Handle {
        self.store.set_key_handler(handle.id, Box::new(handler));
        handle
    }

    /// Make an already-declared node take long presses (default: it takes
    /// none).
    pub fn long_pressable(&mut self, handle: Handle, takes: bool) -> Handle {
        self.store.set_long_pressable(handle.id, takes);
        handle
    }

    /// Make an already-declared node take drags (default: it takes none).
    pub fn draggable(&mut self, handle: Handle, takes: bool) -> Handle {
        self.store.set_draggable(handle.id, takes);
        handle
    }

    /// Make an already-declared node drag along `axis` alone, or along
    /// either with `None` (the default): a press that travels across it is
    /// left to a viewport above.
    pub fn drag_axis(&mut self, handle: Handle, axis: Option<crate::layout::Axis>) -> Handle {
        self.store.set_drag_axis(handle.id, axis);
        handle
    }

    /// Make an already-declared node hear the wheel over it before any
    /// viewport above (see [`NodeStore::set_takes_wheel`]).
    pub fn takes_wheel(&mut self, handle: Handle, takes: bool) -> Handle {
        self.store.set_takes_wheel(handle.id, takes);
        handle
    }

    /// Make an already-declared node take pinches (default: it takes none).
    pub fn pinchable(&mut self, handle: Handle, takes: bool) -> Handle {
        self.store.set_pinchable(handle.id, takes);
        handle
    }

    /// Make an already-declared node a viewer of its one content child
    /// within `limits`, or no longer one with `None` (see
    /// [`NodeStore::set_viewer`]).
    pub fn viewer(
        &mut self,
        handle: Handle,
        limits: Option<crate::viewer::ViewerLimits>,
    ) -> Handle {
        self.store.set_viewer(handle.id, limits);
        handle
    }

    /// Make an already-declared node a swipe row of `parts`, or no longer
    /// one with `None` (see [`NodeStore::set_swipe_row`]).
    pub fn swipe_row(
        &mut self,
        handle: Handle,
        parts: Option<crate::row_gesture::SwipeParts>,
    ) -> Handle {
        self.store.set_swipe_row(handle.id, parts);
        handle
    }

    /// Let a wheel turning up and down scroll an already-declared viewport
    /// along rows (see [`NodeStore::set_wheel_across`]).
    pub fn wheel_across(&mut self, handle: Handle, takes: bool) -> Handle {
        self.store.set_wheel_across(handle.id, takes);
        handle
    }

    /// Make an already-declared node a reorderable list, or no longer one
    /// (see [`NodeStore::set_reorder_list`]).
    pub fn reorder_list(&mut self, handle: Handle, takes: bool) -> Handle {
        self.store.set_reorder_list(handle.id, takes);
        handle
    }

    /// Make an already-declared node a handle of the reorderable row it lies
    /// in (see [`NodeStore::set_reorder_handle`]).
    pub fn reorder_handle(&mut self, handle: Handle) -> Handle {
        self.store.set_reorder_handle(handle.id, true);
        handle
    }

    /// Lay `floater` out against `anchor` the way `how` says, as an anchored
    /// layer (see [`NodeStore::set_anchor`]).
    pub fn anchored(
        &mut self,
        floater: Handle,
        anchor: Handle,
        how: crate::anchored::Anchoring,
    ) -> Handle {
        self.store.set_anchor(floater.id, Some((anchor.id, how)));
        floater
    }

    /// Move `arrow`, laid out at the top-left of anchored layer `floater`,
    /// to where the layer's arrow points at its anchor.
    pub fn anchor_arrow(&mut self, floater: Handle, arrow: Handle) -> Handle {
        self.store.set_anchor_arrow(floater.id, Some(arrow.id));
        floater
    }

    /// Make an already-declared node a sheet of `spec`, dimming with `scrim`
    /// as it shows: a layer over the whole surface whose first child is the
    /// panel, put away until presented (see [`NodeStore::set_sheet`]).
    pub fn sheet(
        &mut self,
        handle: Handle,
        spec: crate::sheet::SheetSpec,
        scrim: Option<Rgba>,
    ) -> Handle {
        self.store.set_layer(handle.id, true);
        self.store.set_hittable(handle.id, true);
        self.store.set_sheet(handle.id, Some(spec));
        if let Some(scrim) = scrim {
            self.store.set_sheet_scrim(handle.id, scrim);
        }
        handle
    }

    /// Make an already-declared node a drag source offering `data` (see
    /// [`NodeStore::set_drag_source`]).
    pub fn drag_source(&mut self, handle: Handle, data: crate::drag_drop::DragData) -> Handle {
        self.store.set_drag_source(handle.id, Some(data));
        handle
    }

    /// Make an already-declared node the layer drag source `source` shows
    /// while it is carried (see [`NodeStore::set_drag_preview`]).
    pub fn drag_preview(&mut self, source: Handle, preview: Handle) -> Handle {
        self.store.set_layer(preview.id, true);
        self.store.set_drag_preview(source.id, Some(preview.id));
        preview
    }

    /// Make an already-declared node a drop target (see
    /// [`NodeStore::set_drop_target`]).
    pub fn drop_target(&mut self, handle: Handle, target: crate::drag_drop::DropTarget) -> Handle {
        self.store.set_drop_target(handle.id, Some(target));
        handle
    }

    /// Make an already-declared node take pastes of `kinds` (see
    /// [`NodeStore::set_paste_target`]); its key handler hears them
    /// ([`EventCx::pasted`](crate::EventCx::pasted)).
    pub fn paste_target(
        &mut self,
        handle: Handle,
        kinds: crate::clipboard::ClipboardKinds,
    ) -> Handle {
        self.store.set_paste_target(handle.id, Some(kinds));
        handle
    }

    /// Open sheet `sheet` at detent `detent`, by its place among its detents
    /// least first (see [`NodeStore::set_sheet_detent`]).
    pub fn sheet_detent(&mut self, sheet: Handle, detent: usize) -> Handle {
        self.store.set_sheet_detent(sheet.id, detent);
        sheet
    }

    /// Make layer `layer` go when a press lands outside it (see
    /// [`NodeStore::set_light_dismiss`]).
    pub fn light_dismiss(&mut self, layer: Handle) -> Handle {
        self.store.set_light_dismiss(layer.id, true);
        layer
    }

    /// Mark an already-declared node focusable (default: not focusable), so it can
    /// receive keyboard focus and thus key events. Returns the handle so authoring
    /// chains inline. Moving focus and repainting the focus ring are the input
    /// router's job; this only declares the node eligible.
    pub fn focusable(&mut self, handle: Handle, focusable: bool) -> Handle {
        self.store.set_focusable(handle.id, focusable);
        handle
    }

    /// Whether an already-declared node wears the focus ring while the
    /// keyboard focused it (default `true`): `false` for one that shows its
    /// focus its own way. Returns the handle so authoring chains inline.
    pub fn focus_ring(&mut self, handle: Handle, wears: bool) -> Handle {
        self.store.set_focus_ring(handle.id, wears);
        handle
    }

    /// Make the mouse show `cursor` over an already-declared node and its
    /// subtree. Returns the handle so authoring chains inline.
    pub fn cursor(&mut self, handle: Handle, cursor: crate::cursor::CursorIcon) -> Handle {
        self.store.set_cursor(handle.id, Some(cursor));
        handle
    }

    /// Declare an already-built node a draggable self-drawn-caption region.
    /// Returns the handle so authoring chains inline. A self-drawn-chrome caption
    /// registers its blank band (the bar minus its buttons/interactive controls)
    /// so the facade, after layout, reads the node's world box and hands it to the
    /// platform's window-drag back-channel — a press there begins a native window
    /// drag instead of routing a pointer event. A no-op signal on windows the
    /// platform never drives with drag regions (native chrome, headless).
    pub fn register_draggable(&mut self, handle: Handle) -> Handle {
        self.store.push_draggable_region(handle.id);
        handle
    }

    /// Mark an already-declared node hidden (default: shown), folding it and its
    /// subtree out of layout and paint without a structural rebuild. Returns the
    /// handle so authoring chains inline. A hide/show control builds every panel
    /// once and hides all but the selected one here; the input router flips the
    /// flag at runtime via [`EventCx::set_hidden`].
    pub fn set_hidden(&mut self, handle: Handle, hidden: bool) -> Handle {
        self.store.set_hidden(handle.id, hidden);
        handle
    }

    /// Page an already-declared node: it shows only its child at `page`
    /// ([`NodeStore::set_page`]).
    pub fn page(&mut self, handle: Handle, page: Option<usize>) -> Handle {
        self.store
            .set_page(handle.id, page, crate::navigation::PageTransition::None);
        handle
    }

    /// Tag an already-declared node a hero, which flies between the pages of
    /// a navigation stack to the node of the same tag
    /// ([`NodeStore::set_hero`]).
    pub fn hero(&mut self, handle: Handle, tag: &str) -> Handle {
        self.store.set_hero(handle.id, Some(tag));
        handle
    }

    /// Clip an already-declared node's subtree to its box, or stop
    /// ([`NodeStore::set_clip`]).
    pub fn clip(&mut self, handle: Handle, clip: bool) -> Handle {
        self.store.set_clip(handle.id, clip);
        handle
    }

    /// Make `key` the access key of an already-declared node: with Alt held
    /// it reaches the node ([`crate::access`]).
    pub fn access_key(&mut self, handle: Handle, key: char) -> Handle {
        self.store.set_access_key(handle.id, Some(key));
        handle
    }

    /// Make an already-declared node the window's menu bar, which Alt alone
    /// and F10 reach ([`crate::access`]).
    pub fn menu_bar(&mut self, handle: Handle) -> Handle {
        self.store.set_menu_bar(Some(handle.id));
        handle
    }

    /// Make an already-declared node and its subtree paint at `opacity`.
    pub fn opacity(&mut self, handle: Handle, opacity: f32) -> Handle {
        self.store.set_opacity(handle.id, opacity);
        handle
    }

    /// Make an already-declared node cast `shadow` under its box.
    pub fn shadow(&mut self, handle: Handle, shadow: crate::shadow::Shadow) -> Handle {
        self.store.set_shadow(handle.id, Some(shadow));
        handle
    }

    /// Make an already-declared node's box glass: it blurs, saturates and
    /// tints what lies behind it as `backdrop` says.
    pub fn backdrop(&mut self, handle: Handle, backdrop: crate::backdrop::Backdrop) -> Handle {
        self.store.set_backdrop(handle.id, Some(backdrop));
        handle
    }

    /// Make a press on an already-declared node spread ink over its box, or
    /// not (default: not).
    pub fn ripple(&mut self, handle: Handle, on: bool) -> Handle {
        self.store.set_ripple(handle.id, on);
        handle
    }

    /// Mark an already-declared node an overlay (default: not an overlay), so it
    /// and its subtree paint in the top layer, over every non-overlay node
    /// regardless of tree position. Returns the handle so authoring chains inline.
    /// The overlay family (Popup/Modal/Sheet/Toast) authors its floating content
    /// once and marks it here; unlike hiding, it changes only paint order, not
    /// layout.
    pub fn set_overlay(&mut self, handle: Handle, overlay: bool) -> Handle {
        self.store.set_overlay(handle.id, overlay);
        handle
    }

    /// Make a declared node count the clicks pressed at or under it, so its
    /// handlers tell a double or triple click or tap from a single one
    /// ([`EventCx::clicks`](crate::context::EventCx::clicks)). Returns the
    /// handle.
    pub fn count_clicks(&mut self, handle: Handle) -> Handle {
        self.store.count_clicks(handle.id);
        handle
    }

    /// Make a declared node a roving group, Tab stopping once among the
    /// focusable nodes under it (see [`NodeStore::set_roving`]). Returns the
    /// handle.
    pub fn roving(&mut self, handle: Handle) -> Handle {
        self.store.set_roving(handle.id, true);
        handle
    }

    /// The first node of `handle`'s subtree, itself first, that takes the
    /// focus, in tree order.
    pub fn focusable_within(&self, handle: Handle) -> Option<Handle> {
        let arena = self.store.arena();
        let mut stack = vec![handle.id];
        while let Some(node) = stack.pop() {
            if self.store.focusable(node) {
                return Some(Handle { id: node });
            }
            let at = stack.len();
            stack.extend(arena.children(node));
            stack[at..].reverse();
        }
        None
    }

    /// Make `name` the accessible name of a declared node that authors
    /// semantics, as a caption beside it names it. Returns the handle.
    pub fn accessible_name(&mut self, handle: Handle, name: &str) -> Handle {
        if let Some(semantics) = self.store.semantics(handle.id) {
            let named = semantics.clone().with_label(name);
            self.store.set_semantics(handle.id, named);
        }
        handle
    }

    /// Attach authored semantics (role + label) to an already-declared node, for
    /// an accessible tree. Mirrors `on_pointer`: associates a node with its
    /// accessible facts. Returns the handle so authoring chains inline.
    pub fn semantics(&mut self, handle: Handle, semantics: Semantics) -> Handle {
        self.store.set_semantics(handle.id, semantics);
        handle
    }

    /// Make a declared node a check drawing `mark` beside its caption (see
    /// [`NodeStore::set_check_mark`]). Returns the handle so authoring chains
    /// inline.
    pub fn check_mark(&mut self, handle: Handle, mark: CheckMark) -> Handle {
        self.store.set_check_mark(handle.id, Some(mark));
        handle
    }

    /// Attach an initial live [`SemanticState`] (checked / value / expanded /
    /// selected) to an already-declared node, so its first accessibility snapshot
    /// carries the state and not only the authored role. Mirrors [`semantics`],
    /// which authors the cold role + label; this authors the live facet the derive
    /// pass reads from the node's side column. A control whose state later changes
    /// through a reactive cell reaches the same column via the flush-phase
    /// projection; a control whose state is warm model data (a tree row) refreshes
    /// it from its own reconcile step. Returns the handle so authoring chains inline.
    ///
    /// [`semantics`]: Self::semantics
    pub fn semantic_state(&mut self, handle: Handle, state: SemanticState) -> Handle {
        self.store.set_semantic_state(handle.id, state);
        handle
    }

    /// Say where the node `handle` names stands in a table (see
    /// [`NodeStore::set_table_facts`]).
    pub fn table_facts(&mut self, handle: Handle, facts: crate::semantics::TableFacts) -> Handle {
        self.store.set_table_facts(handle.id, facts);
        handle
    }

    /// Show the node `active` names as the descendant active in `owner`
    /// while `owner` keeps the focus (see
    /// [`NodeStore::set_active_descendant`]): a grid's cursor cell built as
    /// its row mounts.
    pub fn active_descendant(&mut self, owner: NodeId, active: Handle) -> Handle {
        self.store.set_active_descendant(owner, Some(active.id));
        active
    }

    /// Declare text to draw on an already-declared node. Mirrors `semantics`:
    /// associates a node with an unshaped [`TextRequest`], which the shaping
    /// tier drains and turns into a [`Content::Text`] payload after build (see
    /// [`NodeStore::take_text_requests`]). `viso-ui` never shapes it itself.
    /// Returns the handle so authoring chains inline.
    pub fn text_request(&mut self, handle: Handle, request: TextRequest) -> Handle {
        self.store.set_text_request(handle.id, request);
        handle
    }

    /// Show a 3D scene on an already-declared node and return its slot in
    /// the store's scenes: fill the slot's frame and
    /// [`commit_scene`](NodeStore::commit_scene) it to show it. `None` for a
    /// stale handle.
    pub fn scene(&mut self, handle: Handle) -> Option<SceneSlot> {
        self.store.set_scene(handle.id)
    }

    /// Declare an image to draw on an already-declared node. Mirrors
    /// `text_request` but needs no shaping step: the texture is already resident,
    /// so this writes the [`Content::Image`] payload directly. `uv` selects the
    /// source sub-rect (normalized `0..1`), `tint` modulates it (white = as-is),
    /// and `natural` is the image's intrinsic pixel size — what a `Length::Fit`
    /// axis on this node measures against. Returns the handle so authoring chains
    /// inline.
    pub fn image(
        &mut self,
        handle: Handle,
        texture: TextureId,
        uv: Rect,
        tint: Rgba,
        natural: Vec2,
    ) -> Handle {
        self.store.set_content_payload(
            handle.id,
            Content::Image {
                texture,
                uv,
                tint,
                natural,
            },
        );
        handle
    }

    /// Show `asset` on an already-declared node, fitted into its box and
    /// tinted: its bytes are offered to the store's asset table, which the
    /// facade decodes and, for a raster, uploads; until then the node draws
    /// nothing. Its natural size is the asset's once decoded. Returns the
    /// handle so authoring chains inline.
    pub fn asset(
        &mut self,
        handle: Handle,
        asset: &Asset,
        fit: AssetFit,
        tint: Option<Rgba>,
    ) -> Handle {
        self.store.offer_asset(asset);
        self.store.set_asset(handle.id, asset.id, fit, tint);
        handle
    }

    /// Declare that `handle` shows the video `source` plays, placed into its
    /// box by `fit` and multiplied by `tint` (see [`NodeStore::set_video`]):
    /// paused at its start until [`set_playback`](NodeStore::set_playback)
    /// asks otherwise. Returns the handle so authoring chains inline.
    pub fn video(
        &mut self,
        handle: Handle,
        source: &crate::video::VideoSource,
        fit: AssetFit,
        tint: Option<Rgba>,
    ) -> Handle {
        self.store.set_video(handle.id, source, fit, tint);
        handle
    }

    /// Ask video node `handle`'s picture to show mirrored, or not; `None`
    /// mirrors a camera facing the user (see [`NodeStore::set_video_mirror`]).
    pub fn video_mirror(&mut self, handle: Handle, mirror: Option<bool>) -> Handle {
        self.store.set_video_mirror(handle.id, mirror);
        handle
    }

    /// Ask video node `handle` to play as `playback` says, when it asked
    /// otherwise last build (see [`NodeStore::declare_playback`]).
    pub fn playback(&mut self, handle: Handle, playback: crate::video::Playback) -> Handle {
        self.store.declare_playback(handle.id, playback);
        handle
    }

    /// Show `loading` in place of asset node `handle`'s picture while it
    /// loads and `failed` once it fails, hiding each otherwise (see
    /// [`NodeStore::set_asset_fallbacks`]).
    pub fn asset_fallbacks(
        &mut self,
        handle: Handle,
        loading: Option<Handle>,
        failed: Option<Handle>,
    ) -> Handle {
        self.store
            .set_asset_fallbacks(handle.id, loading.map(|h| h.id), failed.map(|h| h.id));
        handle
    }

    /// Hold the animation asset node `handle` shows still, on its first
    /// frame, or let it play (see [`NodeStore::set_asset_still`]).
    pub fn asset_still(&mut self, handle: Handle, still: bool) -> Handle {
        self.store.set_asset_still(handle.id, still);
        handle
    }

    /// Declare a vector path to draw on an already-declared node. Mirrors
    /// `image` but carries geometry instead of a texture: there is no
    /// shaping/decode step, so this writes the [`Content::Path`] payload
    /// directly. `cmds` are the outline commands in the node's local space
    /// (paint shifts them to the node's world origin), `fill`/`stroke` paint the
    /// interior/outline, and `natural` is the path's intrinsic size — what a
    /// `Length::Fit` axis on this node measures against. Returns the handle so
    /// authoring chains inline.
    pub fn path(
        &mut self,
        handle: Handle,
        cmds: Vec<PathCmd>,
        fill: Option<Rgba>,
        stroke: Option<Stroke>,
        natural: Vec2,
    ) -> Handle {
        self.store.set_content_payload(
            handle.id,
            Content::Path {
                cmds,
                fill,
                stroke,
                natural,
            },
        );
        handle
    }

    /// Mints the handle of a control built later in this build, so closures
    /// declared before it (its own included) can capture it. Give it to the
    /// control's builder (`.handle(h)`); its `build` binds it. A build without
    /// a state store (a layout-only build) mints a detached handle, whose calls
    /// do nothing.
    pub fn handle<H: crate::ControlHandle>(&mut self) -> H {
        H::from_id(match self.states.as_deref_mut() {
            Some(states) => states.controls.reserve(),
            None => crate::ControlId::DETACHED,
        })
    }

    /// Binds control `id` to `parts`, which node `owner` (the control's root)
    /// keeps: the handle reaches them until the node is freed. A stale `owner`
    /// keeps nothing, so the handle finds nothing.
    pub fn bind_control<P: 'static>(&mut self, id: crate::ControlId, owner: NodeId, parts: Rc<P>) {
        if let Some(states) = self.states.as_deref_mut() {
            states.controls.bind(id, &parts);
        }
        self.store.own_control_parts(owner, parts);
    }

    /// Allocate a reactive state cell with an initial value, returning its id.
    /// App-scoped: the app stashes the id and reads/writes it from handlers via
    /// an event context. Cold, build-time — not a hot path. Requires a cx built
    /// with [`BuildCx::with_reactive`]; a node-only cx has no state store and
    /// this panics (a facade-construction invariant — a user only ever receives
    /// a reactive cx, through `Application::build`).
    ///
    /// Under `viso run` the cell carries across a Rust warm restart, keyed by
    /// where it is allocated: one whose call site an edit moved starts from
    /// `initial`.
    #[cfg_attr(feature = "hot-reload", track_caller)]
    pub fn state(&mut self, initial: StateValue) -> StateId {
        let id = self
            .states
            .as_mut()
            .expect("state() requires a with_reactive BuildCx")
            .alloc_local(initial);
        #[cfg(feature = "hot-reload")]
        crate::state_origin::allocated(
            id,
            crate::state_origin::site_key(std::panic::Location::caller()),
        );
        id
    }

    /// A cell a `component!` or `view!` expansion allocates: one of the
    /// component's states under its symbol's `key`, which carries across a
    /// Rust warm restart, or, with `None`, a cell only the expansion reads.
    #[doc(hidden)]
    pub fn __macro_state(&mut self, initial: StateValue, key: Option<StateKey>) -> StateId {
        let id = self
            .states
            .as_mut()
            .expect("state() requires a with_reactive BuildCx")
            .alloc_local(initial);
        #[cfg(feature = "hot-reload")]
        if let Some(key) = key {
            crate::state_origin::allocated(id, key);
        }
        #[cfg(not(feature = "hot-reload"))]
        let _ = key;
        id
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

    /// Build `f` as one instance of `template`: the nodes and states
    /// it creates fill blocks of their own and the edges it binds are the
    /// instance's, shared through `template` with every instance that binds
    /// the same way. The instance lives while one of its nodes does; once its
    /// last node is freed, [`BindingTable::collect`] frees its states and
    /// edges and keeps its blocks for the next instance. Without a
    /// [`BuildCx::with_reactive`] cx it just runs `f`.
    pub fn instance<R>(&mut self, template: &mut Template, f: impl FnOnce(&mut Self) -> R) -> R {
        let (Some(states), Some(bindings)) =
            (self.states.as_deref_mut(), self.bindings.as_deref_mut())
        else {
            return f(self);
        };
        bindings.begin_instance(template, self.store, states);
        let out = f(self);
        if let (Some(states), Some(bindings)) =
            (self.states.as_deref_mut(), self.bindings.as_deref_mut())
        {
            bindings.end_instance(template, self.store, states);
        }
        out
    }

    /// Runs `f` over this build's stores as a [`StructureCx`], for a view that
    /// mounts its control-flow regions under the nodes it just built. A mount
    /// frees nothing, so the context's effect store is a fresh empty one; a
    /// build without the virtual-list registry (a list row's body) lends an
    /// empty one too, so a list it mounts is not registered.
    /// Requires a [`BuildCx::with_reactive`] cx (see [`BuildCx::state`]).
    ///
    /// [`StructureCx`]: crate::structure::StructureCx
    pub fn structure<R>(
        &mut self,
        f: impl FnOnce(&mut crate::structure::StructureCx<'_>) -> R,
    ) -> R {
        let mut effects = crate::reactive::EffectStore::default();
        let mut spare = crate::virtual_list::VirtualLists::new();
        let mut cx = crate::structure::StructureCx {
            store: &mut *self.store,
            states: self
                .states
                .as_deref_mut()
                .expect("structure() requires a with_reactive BuildCx"),
            bindings: self
                .bindings
                .as_deref_mut()
                .expect("structure() requires a with_reactive BuildCx"),
            effects: &mut effects,
            lists: match self.lists.as_deref_mut() {
                Some(lists) => lists,
                None => &mut spare,
            },
        };
        f(&mut cx)
    }

    /// Watches the node `handle` names for the notices `kinds` names (see
    /// [`NodeStore::watch`]).
    pub fn watch(
        &mut self,
        handle: Handle,
        kinds: crate::notice::NoticeKinds,
        handler: crate::notice::NoticeHandler,
    ) {
        self.store.watch(handle.id, kinds, handler);
    }

    /// The node the next declared child attaches under, `None` before the
    /// first node of a root build.
    pub fn parent(&self) -> Option<NodeId> {
        self.stack.last().copied()
    }

    /// Give `node` an interaction-state box selection: record the resting /
    /// hover / pressed variants on the node and bind each present driving cell
    /// so a press/hover flip re-selects and repaints just this node.
    ///
    /// This is the shared control primitive for interaction feedback — a button
    /// uses it now, a toggle/slider the same way later. It sets the node's
    /// [`InteractionStyle`](crate::InteractionStyle) (marking STYLE + PAINT so the
    /// first [`resolve_interaction_styles`](NodeStore::resolve_interaction_styles)
    /// picks the current box) and binds each of the selection's `Some` cells to
    /// the node with `STYLE | PAINT`: STYLE drives the re-selection pass, PAINT
    /// re-emits the node. Returns the handle so wiring chains inline. Requires a
    /// [`BuildCx::with_reactive`] cx (see [`BuildCx::state`]).
    pub fn interaction_style(&mut self, node: Handle, interaction: InteractionStyle) -> Handle {
        self.store.set_interaction_style(node.id, interaction);
        let bindings = self
            .bindings
            .as_mut()
            .expect("interaction_style() requires a with_reactive BuildCx");
        let class = DirtyClass::STYLE | DirtyClass::PAINT;
        for cell in [interaction.pressed_cell, interaction.hover_cell]
            .into_iter()
            .flatten()
        {
            bindings.bind(cell, node.id, class);
        }
        node
    }

    /// Register a semantic-state projection for `node`: `project` reads the
    /// control's reactive cell(s) through the read-only [`ComputeCx`] and returns
    /// the live accessibility [`SemanticState`] to write into the node's side
    /// column. The projection re-runs in the flush phase whenever a cell it read
    /// changes, keeping the semantics derive pass single-layer — the derive reads
    /// only the node column (`&self`), never the state store.
    ///
    /// This runs the projection once now, so the node carries its live state from
    /// the first frame (before any interaction). Returns the handle so wiring
    /// chains inline. A control pairs this with its [`bind`](Self::bind) PAINT
    /// edge: one cell drives both the visual repaint and the semantic-state
    /// column. A cx without the projector registry, as
    /// [`BuildCx::with_parent`] makes, leaves the projection for the next
    /// flush to adopt and run.
    pub fn bind_semantic_state(
        &mut self,
        node: Handle,
        project: impl FnMut(&mut ComputeCx<'_>) -> SemanticState + 'static,
    ) -> Handle {
        // Without the registry — parts a view builds inside a node — the
        // projection waits on the store for the flush to adopt and run it.
        let Some(projectors) = self.projectors.as_deref_mut() else {
            self.store
                .pending_projections
                .push(node.id, Box::new(project));
            return node;
        };
        let states = self
            .states
            .as_deref()
            .expect("bind_semantic_state() requires a with_reactive BuildCx");
        let id = projectors.alloc(node.id, project);
        // Seed the first-frame value: both stores are live here at build time, so
        // the projection can run immediately and its result lands in the node's
        // `semantic_state` column before the first derive.
        projectors.project(id, states, self.store);
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
        if let Some(placement) = self.pending_placement.take() {
            self.store.keep_placement(id.index() as usize, placement);
        }
        id
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::Span;
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
    const BLUE: Rgba = Rgba {
        r: 0.0,
        g: 0.0,
        b: 1.0,
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
    fn an_idle_frame_visits_nothing() {
        let mut store = NodeStore::new();
        let root = {
            let mut cx = BuildCx::new(&mut store);
            cx.flex(FlexStyle::default(), |cx| {
                for _ in 0..64 {
                    cx.leaf(LeafStyle::default());
                }
            })
            .id()
        };
        let mut scratch = Vec::new();
        let mut roots = Vec::new();
        store.layout(root, surface(400.0, 300.0), &mut scratch);
        store.clear_dirty();

        assert!(!store.any_dirty());
        assert!(store.dirty_lists().marked().is_empty(), "nothing to visit");
        assert_eq!(
            store.relayout_dirty(root, surface(400.0, 300.0), &mut scratch, &mut roots),
            (0, 0)
        );
        let mut out = Vec::new();
        assert_eq!(store.repaint_dirty(root, &mut out), Painted::Nothing);
        assert!(store.derive_semantics_dirty(root).is_none());
    }

    #[test]
    fn a_relayout_redoes_only_the_shallowest_dirty_roots() {
        let mut store = NodeStore::new();
        let (root, a, a_leaf, b) = {
            let mut cx = BuildCx::new(&mut store);
            let mut ids = (None, None, None);
            let root = cx
                .flex(FlexStyle::default(), |cx| {
                    let a = cx.flex(FlexStyle::default(), |cx| {
                        ids.1 = Some(cx.leaf(LeafStyle::default()).id());
                    });
                    ids.0 = Some(a.id());
                    ids.2 = Some(cx.flex(FlexStyle::default(), |_| {}).id());
                })
                .id();
            (root, ids.0.unwrap(), ids.1.unwrap(), ids.2.unwrap())
        };
        let mut scratch = Vec::new();
        let mut roots = Vec::new();
        store.layout(root, surface(400.0, 300.0), &mut scratch);
        store.clear_dirty();

        // A LAYOUT container and a LAYOUT node inside it redo once, from the
        // container; an unrelated sibling is its own root.
        store.mark_dirty(a_leaf, DirtyClass::LAYOUT);
        store.mark_dirty(a, DirtyClass::LAYOUT);
        store.mark_dirty(b, DirtyClass::LAYOUT);
        store.relayout_dirty(root, surface(400.0, 300.0), &mut scratch, &mut roots);
        roots.sort_by_key(|r| r.index());
        let mut want = vec![a, b];
        want.sort_by_key(|r| r.index());
        assert_eq!(roots, want);
    }

    /// A label's shaped text: `n` glyphs, on one to three lines.
    fn label_text(n: u32) -> Content {
        Content::Text {
            ink: None,
            glyphs: Vec::new(),
            atlas: TextureId::new(1),
            color_glyphs: Vec::new(),
            color_atlas: None,
            color: RED,
            natural: Vec2 {
                x: n as f32 * 5.0,
                y: 10.0 * (1 + n % 3) as f32,
            },
            baseline: 8.0,
            shaped_at_width: None,
            width_use: WidthUse::Ignored,
            min_content: n as f32 * 5.0,
            max_content: n as f32 * 5.0,
        }
    }

    /// The sizes a row of the differential layout test requests.
    const ROW_SIZES: [Sizing; 6] = [
        Sizing {
            width: Length::Fixed(200.0),
            height: Length::Fixed(20.0),
        },
        Sizing {
            width: Length::Fill { weight: 1.0 },
            height: Length::Fixed(20.0),
        },
        Sizing {
            width: Length::Fit,
            height: Length::Fixed(20.0),
        },
        Sizing {
            width: Length::Fill { weight: 1.0 },
            height: Length::Fit,
        },
        Sizing {
            width: Length::Fixed(150.0),
            height: Length::Fill { weight: 1.0 },
        },
        Sizing {
            width: Length::Fit,
            height: Length::Fill { weight: 2.0 },
        },
    ];

    const ALIGNS: [Align; 4] = [Align::Start, Align::Center, Align::End, Align::Stretch];

    /// A label hugging its text.
    const LABEL: LeafStyle = LeafStyle {
        size: Sizing {
            width: Length::Fit,
            height: Length::Fit,
        },
        style: BoxStyle::NONE,
    };

    #[test]
    fn an_incremental_relayout_places_every_node_where_a_whole_layout_does() {
        let mut lcg = 0x9e37_79b9_7f4a_7c15u64;
        let mut below = |n: u32| {
            lcg = lcg
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((lcg >> 33) % u64::from(n)) as u32
        };
        let screen = surface(300.0, 600.0);
        let mut alone = 0;
        for seed in 0..24u32 {
            let mut store = NodeStore::new();
            let mut nodes = Vec::new();
            let mut rows = Vec::new();
            let mut labels = Vec::new();
            let root = {
                let mut cx = BuildCx::new(&mut store);
                cx.flex(
                    FlexStyle {
                        axis: Axis::Column,
                        gap: 2.0,
                        align: ALIGNS[seed as usize % 4],
                        size: Sizing::fixed(300.0, 600.0),
                        ..FlexStyle::default()
                    },
                    |cx| {
                        for r in 0..12u32 {
                            let row = cx.flex(
                                FlexStyle {
                                    axis: Axis::Row,
                                    gap: 3.0,
                                    padding: Inset::all(1.0),
                                    align: ALIGNS[(r + seed) as usize % 4],
                                    size: ROW_SIZES[((r + seed) % 6) as usize],
                                    ..FlexStyle::default()
                                },
                                |cx| {
                                    nodes.push(
                                        cx.leaf(LeafStyle {
                                            size: Sizing::fixed(8.0, 8.0),
                                            ..LeafStyle::default()
                                        })
                                        .id(),
                                    );
                                    labels.push(cx.leaf(LABEL).id());
                                    if r % 4 == 0 {
                                        let boxed = cx.flex(
                                            FlexStyle {
                                                axis: Axis::Column,
                                                align: ALIGNS[r as usize % 3],
                                                size: Sizing::fixed(60.0, 18.0),
                                                ..FlexStyle::default()
                                            },
                                            |cx| labels.push(cx.leaf(LABEL).id()),
                                        );
                                        nodes.push(boxed.id());
                                    }
                                },
                            );
                            rows.push(row.id());
                        }
                    },
                )
                .id()
            };
            for &label in &labels {
                store.set_content_payload(label, label_text(1 + below(9)));
            }
            nodes.extend(&rows);
            nodes.extend(&labels);
            nodes.push(root);
            let mut scratch = Vec::new();
            let mut roots = Vec::new();
            store.layout(root, screen, &mut scratch);
            store.clear_dirty();
            for frame in 0..200 {
                for _ in 0..1 + below(3) {
                    match below(10) {
                        0..=4 => {
                            let label = labels[below(labels.len() as u32) as usize];
                            store.set_content_payload(label, label_text(below(12)));
                        }
                        5 | 6 => {
                            let node = if below(2) == 0 {
                                rows[below(rows.len() as u32) as usize]
                            } else {
                                labels[below(labels.len() as u32) as usize]
                            };
                            store.set_hidden(node, !store.hidden(node));
                        }
                        7 | 8 => {
                            let row = rows[below(rows.len() as u32) as usize];
                            store.set_fixed_size(row, ROW_SIZES[below(6) as usize]);
                        }
                        _ => {
                            let row = rows[below(rows.len() as u32) as usize];
                            store.set_flex_child_weight(row, Axis::Column, 1.0 + below(3) as f32);
                        }
                    }
                }
                store.relayout_dirty(root, screen, &mut scratch, &mut roots);
                alone += roots
                    .iter()
                    .filter(|&&r| store.dirty(r).intersects(DirtyClass::MEASURE))
                    .count();
                let incremental: Vec<Rect> = nodes.iter().map(|&n| store.bounds(n)).collect();
                store.layout(root, screen, &mut scratch);
                for (&node, kept) in nodes.iter().zip(&incremental) {
                    assert_eq!(
                        *kept,
                        store.bounds(node),
                        "seed {seed} frame {frame}: {node:?} redone from {roots:?}"
                    );
                }
                store.clear_dirty();
            }
        }
        assert!(alone > 100, "{alone} re-measures placed alone");
    }

    fn leaf(w: f32, h: f32) -> LeafStyle {
        LeafStyle {
            size: Sizing::fixed(w, h),
            ..LeafStyle::default()
        }
    }

    fn at(r: Rect) -> (f32, f32, f32, f32) {
        (r.x, r.y, r.w, r.h)
    }

    #[test]
    fn a_stack_layers_its_children_at_their_alignments_and_fits_the_largest() {
        let mut store = NodeStore::new();
        let mut ids = Vec::new();
        let root = {
            let mut cx = BuildCx::new(&mut store);
            cx.flex(FlexStyle::default(), |cx| {
                let stack = cx.stack(
                    StackStyle {
                        align: Alignment2D::Center,
                        padding: Inset::all(10.0),
                        ..StackStyle::default()
                    },
                    |cx| {
                        ids.push(cx.leaf(leaf(100.0, 60.0)).id());
                        cx.place(StackPlacement {
                            align: Some(Alignment2D::BottomEnd),
                            layer: 0,
                        });
                        ids.push(cx.leaf(leaf(40.0, 20.0)).id());
                        ids.push(cx.leaf(leaf(20.0, 20.0)).id());
                        ids.push(
                            cx.leaf(LeafStyle {
                                size: Sizing::fill(),
                                ..LeafStyle::default()
                            })
                            .id(),
                        );
                    },
                );
                ids.insert(0, stack.id());
            })
            .id()
        };
        let mut scratch = Vec::new();
        store.layout(root, surface(400.0, 300.0), &mut scratch);
        let bounds: Vec<_> = ids.iter().map(|&id| at(store.bounds(id))).collect();
        assert_eq!(bounds[0], (0.0, 0.0, 120.0, 80.0), "the largest, padded");
        assert_eq!(bounds[1], (10.0, 10.0, 100.0, 60.0));
        assert_eq!(bounds[2], (70.0, 50.0, 40.0, 20.0), "its own alignment");
        assert_eq!(bounds[3], (50.0, 30.0, 20.0, 20.0), "the stack's");
        assert_eq!(bounds[4], (10.0, 10.0, 100.0, 60.0), "a fill fills");
    }

    #[test]
    fn an_absolute_pins_its_children_by_their_insets_and_fits_none() {
        let mut store = NodeStore::new();
        let mut ids = Vec::new();
        let root = {
            let mut cx = BuildCx::new(&mut store);
            cx.flex(FlexStyle::default(), |cx| {
                let pinned = cx.absolute(
                    AbsoluteStyle {
                        size: Sizing::fixed(200.0, 100.0),
                        ..AbsoluteStyle::default()
                    },
                    |cx| {
                        cx.place(AbsoluteInsets {
                            start: Some(Span::dp(5.0)),
                            top: Some(Span::dp(6.0)),
                            ..AbsoluteInsets::NONE
                        });
                        ids.push(cx.leaf(leaf(20.0, 10.0)).id());
                        cx.place(AbsoluteInsets {
                            end: Some(Span::pct(10.0)),
                            bottom: Some(Span::dp(4.0)),
                            ..AbsoluteInsets::NONE
                        });
                        ids.push(cx.leaf(leaf(20.0, 10.0)).id());
                        cx.place(AbsoluteInsets {
                            start: Some(Span::dp(10.0)),
                            end: Some(Span::dp(30.0)),
                            ..AbsoluteInsets::NONE
                        });
                        ids.push(
                            cx.leaf(LeafStyle {
                                size: Sizing {
                                    width: Length::Fit,
                                    height: Length::Fixed(8.0),
                                },
                                ..LeafStyle::default()
                            })
                            .id(),
                        );
                        ids.push(cx.leaf(leaf(10.0, 10.0)).id());
                    },
                );
                ids.insert(0, pinned.id());
                let fit = cx.absolute(
                    AbsoluteStyle {
                        padding: Inset::all(3.0),
                        size: Sizing {
                            width: Length::Fit,
                            height: Length::Fit,
                        },
                        ..AbsoluteStyle::default()
                    },
                    |cx| {
                        cx.leaf(leaf(50.0, 50.0));
                    },
                );
                ids.push(fit.id());
            })
            .id()
        };
        let mut scratch = Vec::new();
        store.layout(root, surface(400.0, 300.0), &mut scratch);
        let bounds: Vec<_> = ids.iter().map(|&id| at(store.bounds(id))).collect();
        assert_eq!(bounds[1], (5.0, 6.0, 20.0, 10.0));
        assert_eq!(bounds[2], (160.0, 86.0, 20.0, 10.0), "from the end, by 10%");
        assert_eq!(
            bounds[3],
            (10.0, 0.0, 160.0, 8.0),
            "a fit stretched between"
        );
        assert_eq!(bounds[4], (0.0, 0.0, 10.0, 10.0), "at the top start");
        assert_eq!(bounds[5], (200.0, 0.0, 6.0, 6.0), "only its padding");
    }

    #[test]
    fn a_stack_paints_and_hits_by_layer_and_reads_in_source_order() {
        let mut store = NodeStore::new();
        let mut ids = Vec::new();
        let full = LeafStyle {
            size: Sizing::fill(),
            ..LeafStyle::default()
        };
        let root = {
            let mut cx = BuildCx::new(&mut store);
            cx.stack(
                StackStyle {
                    size: Sizing::fixed(100.0, 100.0),
                    ..StackStyle::default()
                },
                |cx| {
                    for (color, layer) in [(RED, 1), (GREEN, 0), (BLUE, -1)] {
                        cx.place(StackPlacement { align: None, layer });
                        ids.push(
                            cx.leaf(LeafStyle {
                                style: BoxStyle::solid(color),
                                ..full
                            })
                            .id(),
                        );
                    }
                },
            )
            .id()
        };
        let mut scratch = Vec::new();
        store.layout(root, surface(100.0, 100.0), &mut scratch);
        let mut out = Vec::new();
        store.repaint(root, &mut out);
        store.clear_dirty();
        let colors = |out: &[viso_render::Primitive]| -> Vec<Rgba> {
            out.iter()
                .filter_map(|p| match p {
                    viso_render::Primitive::Quad(q) => Some(q.color),
                    _ => None,
                })
                .collect()
        };
        assert_eq!(colors(&out), [BLUE, GREEN, RED]);
        assert_eq!(crate::hit_test(&store, root, 50.0, 50.0), Some(ids[0]));
        let read: Vec<NodeId> = store.arena().children(root).collect();
        assert_eq!(read, ids, "the reading order is the source's");

        // A layer change paints the whole stack again in its new order.
        store.set_placement(
            ids[0],
            Placement::Stack(StackPlacement {
                align: None,
                layer: -2,
            }),
        );
        assert!(matches!(
            store.repaint_dirty(root, &mut out),
            Painted::Whole(_)
        ));
        assert_eq!(colors(&out), [RED, BLUE, GREEN]);
        assert_eq!(crate::hit_test(&store, root, 50.0, 50.0), Some(ids[1]));
        store.clear_dirty();

        // A look change splices into the layered order.
        store.set_fill(ids[2], RED);
        assert!(!matches!(
            store.repaint_dirty(root, &mut out),
            Painted::Whole(_)
        ));
        let mut whole = Vec::new();
        crate::paint_tree(&store, root, &mut whole);
        assert_eq!(out, whole);
    }

    #[test]
    fn a_child_of_an_absolute_or_a_fixed_stack_re_measures_alone() {
        let mut store = NodeStore::new();
        let mut ids = Vec::new();
        let root = {
            let mut cx = BuildCx::new(&mut store);
            cx.flex(
                FlexStyle {
                    axis: Axis::Column,
                    ..FlexStyle::default()
                },
                |cx| {
                    cx.absolute(AbsoluteStyle::default(), |cx| {
                        cx.place(AbsoluteInsets {
                            end: Some(Span::dp(4.0)),
                            ..AbsoluteInsets::NONE
                        });
                        ids.push(cx.leaf(leaf(10.0, 10.0)).id());
                    });
                    cx.stack(
                        StackStyle {
                            align: Alignment2D::Center,
                            size: Sizing::fixed(100.0, 100.0),
                            ..StackStyle::default()
                        },
                        |cx| ids.push(cx.leaf(leaf(10.0, 10.0)).id()),
                    );
                },
            )
            .id()
        };
        let screen = surface(400.0, 300.0);
        let mut scratch = Vec::new();
        let mut roots = Vec::new();
        store.layout(root, screen, &mut scratch);
        store.clear_dirty();
        store.set_fixed_size(ids[0], Sizing::fixed(30.0, 20.0));
        store.set_fixed_size(ids[1], Sizing::fixed(40.0, 20.0));
        store.relayout_dirty(root, screen, &mut scratch, &mut roots);
        assert_eq!(roots, ids, "each redone alone");
        assert_eq!(at(store.bounds(ids[0])), (366.0, 0.0, 30.0, 20.0));
        let stack = store.bounds(store.arena().parent(ids[1]).unwrap());
        assert_eq!(at(store.bounds(ids[1])), (30.0, stack.y + 40.0, 40.0, 20.0));
    }

    #[test]
    fn an_incremental_relayout_and_repaint_of_stacks_and_absolutes_match_whole_ones() {
        let mut lcg = 0x2545_f491_4f6c_dd1du64;
        let mut below = |n: u32| {
            lcg = lcg
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((lcg >> 33) % u64::from(n)) as u32
        };
        const ALIGNED: [Alignment2D; 4] = [
            Alignment2D::TopStart,
            Alignment2D::Center,
            Alignment2D::BottomEnd,
            Alignment2D::Top,
        ];
        let screen = surface(300.0, 600.0);
        for seed in 0..12u32 {
            let mut store = NodeStore::new();
            let mut labels = Vec::new();
            let mut nodes = Vec::new();
            let root = {
                let mut cx = BuildCx::new(&mut store);
                cx.flex(
                    FlexStyle {
                        axis: Axis::Column,
                        size: Sizing::fixed(300.0, 600.0),
                        ..FlexStyle::default()
                    },
                    |cx| {
                        for r in 0..6u32 {
                            let size = if (r + seed) % 2 == 0 {
                                Sizing::fixed(120.0, 60.0)
                            } else {
                                Sizing {
                                    width: Length::Fit,
                                    height: Length::Fit,
                                }
                            };
                            let fill = |k: u32| BoxStyle::solid([RED, GREEN, BLUE][k as usize % 3]);
                            let body = |cx: &mut BuildCx<'_>, labels: &mut Vec<NodeId>| {
                                for k in 0..3 {
                                    labels.push(
                                        cx.leaf(LeafStyle {
                                            style: fill(k + r),
                                            ..LABEL
                                        })
                                        .id(),
                                    );
                                }
                            };
                            let node = if r % 3 == 2 {
                                cx.absolute(
                                    AbsoluteStyle {
                                        size,
                                        ..AbsoluteStyle::default()
                                    },
                                    |cx| body(cx, &mut labels),
                                )
                            } else {
                                cx.stack(
                                    StackStyle {
                                        align: ALIGNED[(r + seed) as usize % 4],
                                        size,
                                        ..StackStyle::default()
                                    },
                                    |cx| body(cx, &mut labels),
                                )
                            };
                            nodes.push(node.id());
                        }
                    },
                )
                .id()
            };
            for &label in &labels {
                store.set_content_payload(label, label_text(1 + below(9)));
            }
            nodes.extend(&labels);
            let mut scratch = Vec::new();
            let mut roots = Vec::new();
            let mut out = Vec::new();
            store.layout(root, screen, &mut scratch);
            store.repaint(root, &mut out);
            store.clear_dirty();
            for frame in 0..120 {
                for _ in 0..1 + below(3) {
                    let label = labels[below(labels.len() as u32) as usize];
                    match below(6) {
                        0 | 1 => store.set_content_payload(label, label_text(below(12))),
                        2 => store.set_hidden(label, !store.hidden(label)),
                        3 => {
                            let span = |n: u32| (n > 0).then(|| Span::dp(n as f32 * 3.0));
                            let placement = match store.layout_input(store.parent(label).unwrap()) {
                                LayoutInput::Absolute { .. } => {
                                    Placement::Absolute(AbsoluteInsets {
                                        top: span(below(3)),
                                        end: span(below(3)),
                                        bottom: span(below(3)),
                                        start: span(below(3)),
                                    })
                                }
                                _ => Placement::Stack(StackPlacement {
                                    align: (below(2) == 0).then(|| ALIGNED[below(4) as usize]),
                                    layer: below(3) as i32 - 1,
                                }),
                            };
                            store.set_placement(label, placement);
                        }
                        4 => {
                            let lengths = [
                                Length::Fit,
                                Length::Fixed(12.0),
                                Length::Fill { weight: 1.0 },
                                Length::Relative {
                                    fixed: 4.0,
                                    pct: 0.25,
                                },
                            ];
                            store.set_fixed_size(
                                label,
                                Sizing {
                                    width: lengths[below(4) as usize],
                                    height: lengths[below(4) as usize],
                                },
                            );
                        }
                        _ => store.set_fill(label, [RED, GREEN, BLUE][below(3) as usize]),
                    }
                }
                store.relayout_dirty(root, screen, &mut scratch, &mut roots);
                let incremental: Vec<Rect> = nodes.iter().map(|&n| store.bounds(n)).collect();
                store.repaint_dirty(root, &mut out);
                let mut whole = Vec::new();
                crate::paint_tree(&store, root, &mut whole);
                assert_eq!(out, whole, "seed {seed} frame {frame}: the paint");
                store.layout(root, screen, &mut scratch);
                for (&node, kept) in nodes.iter().zip(&incremental) {
                    assert_eq!(
                        *kept,
                        store.bounds(node),
                        "seed {seed} frame {frame}: {node:?} redone from {roots:?}"
                    );
                }
                store.clear_dirty();
            }
        }
    }

    #[cfg(feature = "inspect")]
    #[test]
    fn a_watched_store_journals_each_direct_mark_and_its_cause() {
        let mut store = NodeStore::new();
        let mut states = StateStore::new();
        let leaf = {
            let mut cx = BuildCx::new(&mut store);
            cx.leaf(LeafStyle::default()).id()
        };
        let cell = states.alloc(StateValue::Int(0));
        let mut bindings = BindingTable::new();
        bindings.bind(cell, leaf, DirtyClass::PAINT);

        // Unwatched, nothing is recorded and no cause is held.
        store.mark_dirty(leaf, DirtyClass::LAYOUT);
        assert_eq!(store.set_dirty_cause(DirtyCause::Input), DirtyCause::Other);
        let mut marks = Vec::new();
        assert_eq!(store.take_dirty_marks(&mut marks), 0);
        assert!(marks.is_empty());

        store.journal_dirty(true);
        assert!(store.journals_dirty());
        store.set_dirty_cause(DirtyCause::Input);
        store.mark_dirty(leaf, DirtyClass::STYLE);
        store.mark_dirty(leaf, DirtyClass::HIT_TEST);
        store.set_dirty_cause(DirtyCause::Other);
        states.set(cell, StateValue::Int(1));
        let mut pending = Vec::new();
        states.take_pending(&mut pending);
        store.flush_state_transactions(&pending, &bindings);
        store.take_dirty_marks(&mut marks);
        assert_eq!(
            marks,
            [
                DirtyMark {
                    node: leaf,
                    class: DirtyClass::STYLE | DirtyClass::HIT_TEST,
                    cause: DirtyCause::Input,
                },
                DirtyMark {
                    node: leaf,
                    class: DirtyClass::PAINT,
                    cause: DirtyCause::State(cell),
                },
            ]
        );
        assert_eq!(
            store.set_dirty_cause(DirtyCause::Other),
            DirtyCause::Other,
            "a binding's cause ends with its edges"
        );

        store.journal_dirty(false);
        store.mark_dirty(leaf, DirtyClass::PAINT);
        marks.clear();
        store.take_dirty_marks(&mut marks);
        assert!(marks.is_empty());
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
                    size: Sizing::fixed(100.0, 50.0),
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
                            size: Sizing::fill(),
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
    fn build_cx_place_named_resolves_a_column_line_to_its_index() {
        use crate::grid::{GridStyle, TrackSizing};
        let mut store = NodeStore::new();
        let (grid, child) = {
            let mut cx = BuildCx::new(&mut store);
            let mut child_id = None;
            let g = cx.grid(
                GridStyle {
                    columns: vec![TrackSizing::Fixed(50.0), TrackSizing::Fixed(50.0)],
                    rows: vec![TrackSizing::Fixed(50.0)],
                    size: Sizing::fixed(100.0, 50.0),
                    // Line 1 is the boundary before the second column.
                    column_line_names: vec![("content".into(), 1)],
                    row_line_names: vec![("top".into(), 0)],
                    ..Default::default()
                },
                |cx| {
                    cx.place_named(Some("content"), Some("top"), 1, 1);
                    child_id = Some(
                        cx.leaf(LeafStyle {
                            size: Sizing::fill(),
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
        // "content" == line 1 → column 1 → x starts at 50.
        assert_eq!(store.bounds(child).x, 50.0);
    }

    #[test]
    fn build_cx_place_area_resolves_to_the_named_block() {
        use crate::grid::{GridAreas, GridStyle, TrackSizing};
        let mut store = NodeStore::new();
        let (grid, child) = {
            let mut cx = BuildCx::new(&mut store);
            let mut child_id = None;
            let g = cx.grid(
                GridStyle {
                    columns: vec![TrackSizing::Fixed(50.0), TrackSizing::Fixed(50.0)],
                    rows: vec![TrackSizing::Fixed(40.0), TrackSizing::Fixed(40.0)],
                    size: Sizing::fixed(100.0, 80.0),
                    // "head" spans both columns on row 0; "main" is the bottom-right cell.
                    areas: Some(GridAreas::from_rows([["head", "head"], ["side", "main"]])),
                    ..Default::default()
                },
                |cx| {
                    cx.place_area("main");
                    child_id = Some(
                        cx.leaf(LeafStyle {
                            size: Sizing::fill(),
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
        crate::layout::layout(&mut store, grid.index(), surface(100.0, 80.0), &mut scratch);
        // "main" == column 1, row 1 → x=50, y=40, single cell 50x40.
        assert_eq!(
            store.bounds(child),
            Rect {
                x: 50.0,
                y: 40.0,
                w: 50.0,
                h: 40.0
            }
        );
    }

    #[test]
    fn build_cx_place_area_spans_a_multi_column_area() {
        use crate::grid::{GridAreas, GridStyle, TrackSizing};
        let mut store = NodeStore::new();
        let (grid, child) = {
            let mut cx = BuildCx::new(&mut store);
            let mut child_id = None;
            let g = cx.grid(
                GridStyle {
                    columns: vec![TrackSizing::Fixed(50.0), TrackSizing::Fixed(50.0)],
                    rows: vec![TrackSizing::Fixed(40.0), TrackSizing::Fixed(40.0)],
                    size: Sizing::fixed(100.0, 80.0),
                    areas: Some(GridAreas::from_rows([["head", "head"], ["side", "main"]])),
                    ..Default::default()
                },
                |cx| {
                    cx.place_area("head");
                    child_id = Some(
                        cx.leaf(LeafStyle {
                            size: Sizing::fill(),
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
        crate::layout::layout(&mut store, grid.index(), surface(100.0, 80.0), &mut scratch);
        // "head" spans both columns on row 0 → x=0, y=0, full width 100, one row tall.
        assert_eq!(
            store.bounds(child),
            Rect {
                x: 0.0,
                y: 0.0,
                w: 100.0,
                h: 40.0
            }
        );
    }

    #[test]
    fn alloc_keeps_arrays_index_aligned() {
        let mut store = NodeStore::new();
        let a = store.alloc(
            LayoutInput::Leaf {
                size: Sizing::fixed(1.0, 1.0),
            },
            BoxStyle::NONE,
        );
        let b = store.alloc(
            LayoutInput::Leaf {
                size: Sizing::fixed(2.0, 2.0),
            },
            BoxStyle::NONE,
        );
        assert_eq!(a.index(), 0);
        assert_eq!(b.index(), 1);
        // Every parallel array grew in lockstep with the arena.
        assert_eq!(store.bounds.len(), 2);
        assert_eq!(store.transform_root.len(), 2);
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
    fn set_text_content_marks_semantics_but_image_content_does_not() {
        // `text content -> MEASURE + LAYOUT + PAINT + SEMANTICS`, but image/path
        // content carries no intrinsic accessible name, so it stays
        // MEASURE | LAYOUT | PAINT. A reactive text change re-shaped in
        // place must re-derive the accessibility tree; an image swap must not.
        use viso_render::GlyphInstanceData;
        let mut store = NodeStore::new();
        let sink: std::rc::Rc<std::cell::RefCell<Vec<NodeId>>> =
            std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        {
            let capture = std::rc::Rc::clone(&sink);
            let mut cx = BuildCx::new(&mut store);
            cx.flex(FlexStyle::default(), |cx| {
                capture
                    .borrow_mut()
                    .push(cx.leaf(LeafStyle::default()).id());
                capture
                    .borrow_mut()
                    .push(cx.leaf(LeafStyle::default()).id());
            });
        }
        let (text_leaf, image_leaf) = {
            let ids = sink.borrow();
            (ids[0], ids[1])
        };

        // A one-glyph text run: SEMANTICS joins the paint/measure classes.
        store.clear_dirty();
        store.set_content_payload(
            text_leaf,
            Content::Text {
                ink: None,
                glyphs: vec![GlyphInstanceData {
                    rect: Rect {
                        x: 0.0,
                        y: 0.0,
                        w: 8.0,
                        h: 10.0,
                    },
                    uv: Rect {
                        x: 0.0,
                        y: 0.0,
                        w: 1.0,
                        h: 1.0,
                    },
                }],
                atlas: TextureId::new(1),
                color_glyphs: Vec::new(),
                color_atlas: None,
                color: RED,
                natural: Vec2 { x: 8.0, y: 10.0 },
                baseline: 8.0,
                shaped_at_width: None,
                width_use: WidthUse::Ignored,
                min_content: 8.0,
                max_content: 8.0,
            },
        );
        assert!(
            store.dirty(text_leaf).intersects(DirtyClass::SEMANTICS),
            "replacing a text run in place marks SEMANTICS"
        );
        assert!(
            store
                .dirty(text_leaf)
                .intersects(DirtyClass::MEASURE | DirtyClass::LAYOUT | DirtyClass::PAINT),
            "and still marks the layout/paint classes content always carries"
        );

        // An image swap: only MEASURE | LAYOUT | PAINT — no accessible name.
        store.clear_dirty();
        store.set_content_payload(
            image_leaf,
            Content::Image {
                texture: TextureId::new(2),
                uv: Rect {
                    x: 0.0,
                    y: 0.0,
                    w: 1.0,
                    h: 1.0,
                },
                tint: RED,
                natural: Vec2 { x: 16.0, y: 16.0 },
            },
        );
        assert!(
            !store.dirty(image_leaf).intersects(DirtyClass::SEMANTICS),
            "image content carries no intrinsic accessible name, so no SEMANTICS mark"
        );
        assert!(
            store
                .dirty(image_leaf)
                .intersects(DirtyClass::MEASURE | DirtyClass::LAYOUT | DirtyClass::PAINT),
            "image content still re-measures, re-lays-out, and repaints"
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
    fn a_fresh_node_is_not_hidden() {
        let mut store = NodeStore::new();
        let id = {
            let mut cx = BuildCx::new(&mut store);
            cx.leaf(LeafStyle::default()).id()
        };
        assert!(!store.hidden(id), "hiding is opt-in, default false");
    }

    #[test]
    fn set_hidden_round_trips_and_marks_layout_and_paint() {
        let mut store = NodeStore::new();
        let id = {
            let mut cx = BuildCx::new(&mut store);
            cx.leaf(LeafStyle::default()).id()
        };
        store.clear_dirty();
        store.set_hidden(id, true);
        assert!(store.hidden(id));
        let d = store.dirty(id);
        assert!(
            d.contains(DirtyClass::MEASURE | DirtyClass::LAYOUT),
            "a hidden node measures to zero, so its parent re-places it"
        );
        assert!(d.contains(DirtyClass::PAINT), "hiding repaints");
        store.set_hidden(id, false);
        assert!(!store.hidden(id));
    }

    #[test]
    fn set_hidden_guards_stale_handles() {
        let mut store = NodeStore::new();
        let id = {
            let mut cx = BuildCx::new(&mut store);
            cx.leaf(LeafStyle::default()).id()
        };
        store.clear();
        store.set_hidden(id, true); // dead handle: no-op, no panic
        let reused = {
            let mut cx = BuildCx::new(&mut store);
            cx.leaf(LeafStyle::default()).id()
        };
        assert!(!store.hidden(reused), "alloc defaults hidden to false");
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
        // Set focus + a handler, rebuild wholesale (a rebuild frees by
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
                        size: Sizing::fixed(10.0, 20.0),
                        style: BoxStyle::solid(RED),
                    });
                    cx.leaf(LeafStyle {
                        size: Sizing::fixed(30.0, 12.0),
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
                        size: Sizing::fixed(40.0, 10.0),
                        ..Default::default()
                    }));
                    ids.1 = Some(cx.leaf(LeafStyle {
                        size: Sizing {
                            width: crate::layout::Length::Fill { weight: 1.0 },
                            height: crate::layout::Length::Fixed(10.0),
                        },
                        ..Default::default()
                    }));
                    ids.2 = Some(cx.leaf(LeafStyle {
                        size: Sizing {
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
                        size: Sizing::fixed(20.0, 40.0),
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
                            size: Sizing::fixed(50.0, 80.0),
                            ..Default::default()
                        },
                        |cx| {
                            leaf_h = Some(cx.leaf(LeafStyle {
                                size: Sizing::fixed(30.0, 20.0),
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
    fn on_key_and_focusable_declare_a_keyboard_activatable_node() {
        use crate::binding::BindingTable;
        use crate::input::{Key, KeyEvent, Modifiers};
        use crate::state::{StateStore, StateValue};

        let mut store = NodeStore::new();
        let mut states = StateStore::new();
        // A cell the key handler bumps, standing in for a control's action.
        let fired = states.alloc(StateValue::Int(0));
        let bindings = BindingTable::new();

        let leaf;
        {
            let mut cx = BuildCx::new(&mut store);
            let h = cx.leaf(LeafStyle::default());
            // Both authoring seams chain off the same handle: mark focusable so
            // the node can receive key events, then attach the key handler.
            let h = cx.focusable(h, true);
            leaf = cx
                .on_key(h, move |ev| {
                    if let Some(k) = ev.key()
                        && k.pressed
                        && matches!(k.key, Key::Enter)
                    {
                        let now = match ev.get(fired) {
                            Some(StateValue::Int(n)) => n,
                            _ => 0,
                        };
                        ev.set(fired, StateValue::Int(now + 1));
                    }
                })
                .id();
        }
        assert!(store.focusable(leaf), "focusable seam marked the node");
        assert!(
            store.has_key_handler(leaf),
            "on_key seam registered a handler"
        );

        // Same take/restore router discipline as the pointer path: move the key
        // handler out, drive it with an EventCx over the state stores, restore.
        let mut handler = store.take_key_handler(leaf).expect("key handler present");
        assert!(!store.has_key_handler(leaf), "slot empty while borrowed");
        {
            let key = KeyEvent {
                key: Key::Enter,
                pressed: true,
                repeat: false,
                modifiers: Modifiers::default(),
            };
            let mut ev = EventCx::__new_key(&mut states, &bindings, &key);
            handler(&mut ev);
        }
        store.restore_key_handler(leaf, handler);
        assert!(store.has_key_handler(leaf), "key handler restored");
        assert_eq!(states.get(fired), Some(StateValue::Int(1)));
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
                    size: Sizing::fixed(1.0, 1.0),
                    style: BoxStyle::solid(RED),
                })
                .id();
            let b = cx
                .leaf(LeafStyle {
                    size: Sizing::fixed(1.0, 1.0),
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
    fn resolve_interaction_styles_gates_on_style_and_selection() {
        use crate::state::{StateStore, StateValue};
        use crate::style::InteractionStyle;

        let mut store = NodeStore::new();
        let mut states = StateStore::new();
        let pressed_cell = states.alloc(StateValue::Bool(false));
        let hover_cell = states.alloc(StateValue::Bool(false));

        // Two literal leaves; only the first carries an interaction selection.
        let (interactive, literal) = {
            let mut cx = BuildCx::new(&mut store);
            let a = cx
                .leaf(LeafStyle {
                    size: Sizing::fixed(1.0, 1.0),
                    style: BoxStyle::solid(RED),
                })
                .id();
            let b = cx
                .leaf(LeafStyle {
                    size: Sizing::fixed(1.0, 1.0),
                    style: BoxStyle::solid(GREEN),
                })
                .id();
            (a, b)
        };
        let selection = InteractionStyle {
            resting: BoxStyle::solid(RED),
            hover: BoxStyle::solid(GREEN),
            pressed: BoxStyle::solid(BLUE),
            pressed_cell: Some(pressed_cell),
            hover_cell: Some(hover_cell),
        };
        store.set_interaction_style(interactive, selection);
        // The literal leaf is STYLE-dirty but carries no selection: skipped.
        store.mark_dirty(literal, DirtyClass::STYLE);

        // set_interaction_style marked STYLE, so the pass selects resting now.
        let count = store.resolve_interaction_styles(&states);
        assert_eq!(count, 1, "only the selecting node re-resolves");
        assert_eq!(store.style(interactive).fill, RED, "resting selected");
        assert_eq!(store.style(literal).fill, GREEN, "literal untouched");

        // A clean frame (no STYLE dirt) re-selects nothing.
        store.clear_dirty();
        assert_eq!(store.resolve_interaction_styles(&states), 0);
        assert_eq!(store.style(interactive).fill, RED, "unchanged when clean");
    }

    #[test]
    fn resolve_interaction_styles_reselects_on_cell_flip() {
        use crate::binding::BindingTable;
        use crate::state::{StateStore, StateValue};
        use crate::style::InteractionStyle;

        let mut store = NodeStore::new();
        let mut states = StateStore::new();
        let mut bindings = BindingTable::new();
        let pressed_cell = states.alloc(StateValue::Bool(false));
        let hover_cell = states.alloc(StateValue::Bool(false));

        let node = {
            let mut cx = BuildCx::new(&mut store);
            cx.leaf(LeafStyle::default()).id()
        };
        let selection = InteractionStyle {
            resting: BoxStyle::solid(RED),
            hover: BoxStyle::solid(GREEN),
            pressed: BoxStyle::solid(BLUE),
            pressed_cell: Some(pressed_cell),
            hover_cell: Some(hover_cell),
        };
        store.set_interaction_style(node, selection);
        for cell in [pressed_cell, hover_cell] {
            bindings.bind(cell, node, DirtyClass::STYLE | DirtyClass::PAINT);
        }
        store.resolve_interaction_styles(&states);
        assert_eq!(store.style(node).fill, RED, "starts resting");
        store.clear_dirty();

        // Flip hover on: the flush re-marks the bound node STYLE, the pass swaps.
        states.set(hover_cell, StateValue::Bool(true));
        let mut pending = Vec::new();
        states.take_pending(&mut pending);
        store.flush_state_transactions(&pending, &bindings);
        assert!(store.dirty(node).intersects(DirtyClass::STYLE));
        assert_eq!(store.resolve_interaction_styles(&states), 1);
        assert_eq!(store.style(node).fill, GREEN, "hover selected after flip");

        // Flip pressed on too: pressed wins.
        store.clear_dirty();
        states.set(pressed_cell, StateValue::Bool(true));
        pending.clear();
        states.take_pending(&mut pending);
        store.flush_state_transactions(&pending, &bindings);
        store.resolve_interaction_styles(&states);
        assert_eq!(store.style(node).fill, BLUE, "pressed wins over hover");
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
        let mut text_edits = crate::text_edit::TextEdits::new();
        let mut projectors = crate::reactive::SemanticProjector::new();
        let mut cx = BuildCx::with_reactive(
            &mut store,
            &mut states,
            &mut bindings,
            &mut lists,
            &mut text_edits,
            &mut projectors,
        );
        let id = cx.state(StateValue::Int(0));
        assert_eq!(states.get(id), Some(StateValue::Int(0)));
    }

    #[test]
    fn build_cx_chrome_defaults_native_and_reads_back_seed() {
        use crate::window::{ChromeContext, WindowChrome};

        let mut store = NodeStore::new();
        let mut states = StateStore::new();
        let mut bindings = BindingTable::new();
        let mut lists = crate::virtual_list::VirtualLists::new();
        let mut text_edits = crate::text_edit::TextEdits::new();
        let mut projectors = crate::reactive::SemanticProjector::new();

        // A cx built without `with_chrome` reads the neutral default: a caption
        // authored outside a self-drawn-chrome window sees `(Native, None)`.
        let cx = BuildCx::with_reactive(
            &mut store,
            &mut states,
            &mut bindings,
            &mut lists,
            &mut text_edits,
            &mut projectors,
        );
        assert_eq!(cx.chrome(), ChromeContext::default());
        assert_eq!(cx.chrome().chrome, WindowChrome::Native);
        assert_eq!(cx.chrome().buttons_width, None);

        // The facade seeds the per-window chrome facts through `with_chrome`; a
        // caption reads them straight back — the build-time half of the data
        // contract (mode) plus the later-frame native-button reserve width.
        let seeded = ChromeContext {
            chrome: WindowChrome::SelfDrawn,
            buttons_width: Some(78.0),
            ..Default::default()
        };
        let cx = cx.with_chrome(seeded);
        assert_eq!(cx.chrome(), seeded);
        assert_eq!(cx.chrome().chrome, WindowChrome::SelfDrawn);
        assert_eq!(cx.chrome().buttons_width, Some(78.0));
    }

    #[test]
    fn build_cx_bind_wires_a_static_edge() {
        let mut store = NodeStore::new();
        let mut states = StateStore::new();
        let mut bindings = BindingTable::new();
        let mut lists = crate::virtual_list::VirtualLists::new();
        let mut text_edits = crate::text_edit::TextEdits::new();
        let mut projectors = crate::reactive::SemanticProjector::new();
        let node;
        let count;
        {
            let mut cx = BuildCx::with_reactive(
                &mut store,
                &mut states,
                &mut bindings,
                &mut lists,
                &mut text_edits,
                &mut projectors,
            );
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
        let mut text_edits = crate::text_edit::TextEdits::new();
        let mut projectors = crate::reactive::SemanticProjector::new();
        let s;
        let a;
        let b;
        {
            let mut cx = BuildCx::with_reactive(
                &mut store,
                &mut states,
                &mut bindings,
                &mut lists,
                &mut text_edits,
                &mut projectors,
            );
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
                    size: Sizing::fixed(100.0, 100.0),
                    ..Default::default()
                },
                |cx| {
                    content_id = Some(
                        cx.leaf(LeafStyle {
                            size: Sizing::fixed(100.0, 300.0),
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
    fn a_row_read_right_to_left_scrolls_from_its_right_edge() {
        use crate::adaptive::LayoutDirection;
        let mut store = NodeStore::new();
        let mut content = None;
        let root = {
            let mut cx = BuildCx::new(&mut store);
            cx.scroll(
                ScrollStyle {
                    axis: Axis::Row,
                    size: Sizing::fixed(100.0, 50.0),
                    ..Default::default()
                },
                |cx| {
                    content = Some(
                        cx.leaf(LeafStyle {
                            size: Sizing::fixed(300.0, 50.0),
                            ..Default::default()
                        })
                        .id(),
                    );
                },
            );
            cx.root().unwrap()
        };
        let content = content.unwrap();
        let (mut scratch, mut redo) = (Vec::new(), Vec::new());
        store.layout(root, surface(100.0, 50.0), &mut scratch);
        store.scroll_by(root, Vec2 { x: 30.0, y: 0.0 });
        assert_eq!(store.scroll_span(root, Axis::Row), (0.0, 200.0));

        // Turned, the content's right edge meets the viewport's, and the
        // offset keeps its distance from the start.
        store.set_layout_direction(root, Some(LayoutDirection::Rtl));
        store.relayout_dirty(root, surface(100.0, 50.0), &mut scratch, &mut redo);
        assert_eq!(store.bounds(content).x, -200.0);
        assert_eq!(store.scroll_span(root, Axis::Row), (-200.0, 0.0));
        assert_eq!(store.scroll(root).x, -30.0);
        assert_eq!(store.content_shift(root).x, 30.0, "the content moves right");

        store.scroll_by(root, Vec2 { x: 50.0, y: 0.0 });
        assert_eq!(store.scroll(root).x, 0.0, "nothing right of the start");
        store.scroll_by(root, Vec2 { x: -500.0, y: 0.0 });
        assert_eq!(store.scroll(root).x, -200.0, "the left end");
        store.set_scroll(root, Vec2 { x: 40.0, y: 0.0 });
        assert_eq!(store.scroll(root).x, 0.0);
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
        assert_eq!(
            store.dirty(viewport),
            DirtyClass::TRANSFORM,
            "a scroll only moves"
        );

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

    // ---- translate ----

    /// A single 100×100 leaf laid out at the origin — the subject of a slide.
    fn translate_scene() -> (NodeStore, NodeId) {
        let mut store = NodeStore::new();
        let root = {
            let mut cx = BuildCx::new(&mut store);
            cx.leaf(LeafStyle {
                size: Sizing::fixed(100.0, 100.0),
                ..Default::default()
            });
            cx.root().unwrap()
        };
        let mut scratch = Vec::new();
        store.layout(root, surface(100.0, 100.0), &mut scratch);
        (store, root)
    }

    #[test]
    fn set_translate_moves_the_node_itself_and_its_subtree() {
        // A viewport-with-content scene lets us assert that a translate on the
        // container moves the container's *own* world rect — the key asymmetry
        // versus scroll, which leaves the viewport's rect in place.
        let (mut store, viewport, content) = scroll_scene();
        // Before translating, world equals bounds everywhere.
        assert_eq!(store.world(viewport), store.bounds(viewport));
        assert_eq!(store.world(content), store.bounds(content));

        store.set_translate(viewport, Vec2 { x: 12.0, y: -34.0 });

        // Both the node itself and its subtree move by the translate (the node
        // moves, carrying its content along).
        let vb = store.bounds(viewport);
        assert_eq!(
            store.world(viewport),
            Rect {
                x: vb.x + 12.0,
                y: vb.y - 34.0,
                w: vb.w,
                h: vb.h,
            },
            "the translated node's own world rect moves by the translate"
        );
        let cb = store.bounds(content);
        assert_eq!(
            store.world(content),
            Rect {
                x: cb.x + 12.0,
                y: cb.y - 34.0,
                w: cb.w,
                h: cb.h,
            },
            "the subtree moves with the node"
        );
    }

    #[test]
    fn set_translate_is_never_clamped() {
        // Unlike scroll, a translate accepts arbitrarily large or negative
        // offsets — a slide starts a node fully off-screen.
        let (mut store, root) = translate_scene();
        let off = Vec2 {
            x: -500.0,
            y: 800.0,
        };
        store.set_translate(root, off);
        assert_eq!(
            store.translate(root),
            off,
            "large/negative offsets are stored verbatim, not clamped"
        );
    }

    #[test]
    fn a_first_translate_makes_a_root_and_later_ones_only_move_it() {
        let (mut store, root) = translate_scene();
        store.clear_dirty();
        store.set_translate(root, Vec2 { x: 0.0, y: 50.0 });
        assert!(store.is_transform_root(root));
        assert_eq!(
            store.dirty(root),
            DirtyClass::TRANSFORM | DirtyClass::PAINT,
            "the paint that records the new root is the only paint"
        );
        store.clear_dirty();
        store.set_translate(root, Vec2 { x: 0.0, y: 0.0 });
        assert_eq!(store.dirty(root), DirtyClass::TRANSFORM);
        assert!(store.is_transform_root(root), "a root stays one");
    }

    #[test]
    fn a_scroll_moves_its_root_without_painting_again() {
        let (mut store, viewport, content) = scroll_scene();
        let mut out = Vec::new();
        store.repaint(viewport, &mut out);
        assert_eq!(store.transforms().len(), 1);
        assert_eq!(store.transforms().painted(), &[[0.0, 0.0]]);
        store.clear_dirty();

        store.scroll_by(viewport, Vec2 { x: 0.0, y: 40.0 });
        assert_eq!(
            store.repaint_dirty(viewport, &mut out),
            Painted::Nothing,
            "nothing repaints"
        );
        assert!(store.update_transforms());
        assert_eq!(
            store.transforms().places(),
            &[viso_render::Affine2::translate(0.0, -40.0)]
        );
        assert_eq!(store.transforms().painted(), &[[0.0, 0.0]]);
        assert!(!store.update_transforms(), "a second update moves nothing");
        assert_eq!(store.world(content).y, store.bounds(content).y - 40.0);
    }

    #[test]
    fn a_displacement_snaps_to_the_device_grid() {
        let (mut store, viewport, content) = scroll_scene();
        store.set_length_env(crate::length::LengthEnv {
            scale_factor: 2.0,
            ..store.length_env()
        });
        store.scroll_by(viewport, Vec2 { x: 0.0, y: 10.3 });
        assert_eq!(store.world(content).y, store.bounds(content).y - 10.5);
    }

    #[test]
    fn zero_translate_leaves_world_equal_to_bounds() {
        let (store, root) = translate_scene();
        // A never-translated node resolves world byte-equal to bounds.
        assert_eq!(store.world(root), store.bounds(root));
    }

    #[test]
    fn translate_by_accumulates_and_a_zero_delta_is_a_noop() {
        let (mut store, root) = translate_scene();
        store.translate_by(root, Vec2 { x: 10.0, y: 20.0 });
        store.translate_by(root, Vec2 { x: 5.0, y: -4.0 });
        assert_eq!(store.translate(root), Vec2 { x: 15.0, y: 16.0 });

        store.clear_dirty();
        store.translate_by(root, Vec2::ZERO);
        assert!(
            store.dirty(root).is_empty(),
            "a zero delta schedules nothing"
        );

        // Setting the same absolute value is likewise a no-op.
        store.set_translate(root, Vec2 { x: 15.0, y: 16.0 });
        assert!(
            store.dirty(root).is_empty(),
            "an unchanged set schedules nothing"
        );
    }

    #[test]
    fn translate_writers_no_op_on_a_stale_handle() {
        let (mut store, root) = translate_scene();
        // Free the node, invalidating the handle.
        store.clear();
        // A stale write neither panics nor schedules.
        store.set_translate(root, Vec2 { x: 1.0, y: 1.0 });
        store.translate_by(root, Vec2 { x: 1.0, y: 1.0 });
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
        let old = store.arena().first_child(parent).unwrap();
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
            store.arena().first_child(parent),
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
            store.arena().first_child(parent).is_none(),
            "parent is left untouched",
        );
    }

    // --- width-aware text reflow recorder + queue ---------------------------
    //
    // These drive the `viso-ui` half of the two-phase reflow: the
    // Flex layout pass records a reflow request for a wrap-eligible text leaf
    // whose assigned box width disagrees with the width its run was shaped at,
    // the store queues it, and the facade (not exercised here) drains and
    // reshapes. We stand in for the facade's shaper with a hand-built
    // `Content::Text` whose `natural`/`shaped_at_width`/`width_use` we control,
    // so the recorder's eligibility and the queue's contents are observable
    // without a font stack.

    const CLR: Rgba = Rgba {
        r: 1.0,
        g: 1.0,
        b: 1.0,
        a: 1.0,
    };

    /// A stand-in shaped run: `natural` is the run's current extent, `at` the
    /// width it was shaped at (`None` = unconstrained single line), `wrap`
    /// whether it opted into soft wrapping. This is exactly the subset of a real
    /// `Content::Text` the reflow recorder reads.
    fn text_run(natural: Vec2, at: Option<f32>, wrap: bool) -> Content {
        Content::Text {
            ink: None,
            glyphs: Vec::new(),
            atlas: TextureId::new(1),
            color_glyphs: Vec::new(),
            color_atlas: None,
            color: CLR,
            natural,
            baseline: 0.0,
            shaped_at_width: at,
            width_use: if wrap {
                WidthUse::Narrower
            } else {
                WidthUse::Ignored
            },
            min_content: natural.x,
            max_content: natural.x,
        }
    }

    /// Build `Column { leaf(width) }` filling `surf`, attach `content` to the
    /// leaf, run measure+layout once, and return `(store, column, leaf)`.
    fn wrap_scene(width: Length, content: Content, surf: Rect) -> (NodeStore, NodeId, NodeId) {
        let mut store = NodeStore::new();
        let (col, leaf) = {
            let mut cx = BuildCx::new(&mut store);
            let mut leaf_id = None;
            let c = cx.flex(
                FlexStyle {
                    axis: Axis::Column,
                    size: Sizing::fill(),
                    // Stretch so a Fill-width child takes the column's cross
                    // extent (the surface width) rather than hugging its own
                    // natural run width — the realistic wrapped-paragraph setup.
                    align: Align::Stretch,
                    ..Default::default()
                },
                |cx| {
                    leaf_id = Some(
                        cx.leaf(LeafStyle {
                            size: Sizing {
                                width,
                                height: Length::Fit,
                            },
                            ..Default::default()
                        })
                        .id(),
                    );
                },
            );
            (c.id(), leaf_id.unwrap())
        };
        store.set_content_payload(leaf, content);
        let mut scratch = Vec::new();
        crate::layout::measure(&mut store, col.index(), &mut scratch);
        crate::layout::layout(&mut store, col.index(), surf, &mut scratch);
        (store, col, leaf)
    }

    #[test]
    fn fill_soft_wrap_text_narrower_than_natural_enqueues_its_box_width() {
        // A 300px single-line run in a 100px-wide Fill column: the assigned box
        // (100) is narrower than the natural (300), so the leaf must reflow.
        let (mut store, _col, leaf) = wrap_scene(
            Length::fill(),
            text_run(Vec2 { x: 300.0, y: 20.0 }, None, true),
            surface(100.0, 400.0),
        );
        let mut q = Vec::new();
        store.take_text_reflows(&mut q);
        assert_eq!(
            q,
            vec![(leaf, 100.0)],
            "a soft-wrap Fill leaf narrower than its run enqueues its box width"
        );
    }

    #[test]
    fn non_wrapping_fill_text_never_enqueues_a_reflow() {
        // Same geometry, but the run did not opt into wrapping: it clips
        // single-line and no reflow is recorded.
        let (mut store, _col, _leaf) = wrap_scene(
            Length::fill(),
            text_run(Vec2 { x: 300.0, y: 20.0 }, None, false),
            surface(100.0, 400.0),
        );
        let mut q = Vec::new();
        store.take_text_reflows(&mut q);
        assert!(q.is_empty(), "a non-wrapping run never reflows");
    }

    #[test]
    fn fit_width_soft_wrap_text_reflows_only_to_a_box_narrower_than_it() {
        // A Fit width sizes to the run's max-content width, so its box is
        // narrower only where something bounds it — here the stretching
        // column — and a box as wide as the run never reflows.
        let (mut store, _col, leaf) = wrap_scene(
            Length::Fit,
            text_run(Vec2 { x: 300.0, y: 20.0 }, None, true),
            surface(100.0, 400.0),
        );
        let mut q = Vec::new();
        store.take_text_reflows(&mut q);
        assert_eq!(q, [(leaf, 100.0)], "the stretched box bounds it");
        let (mut store, _col, _leaf) = wrap_scene(
            Length::Fit,
            text_run(Vec2 { x: 300.0, y: 20.0 }, None, true),
            surface(400.0, 400.0),
        );
        store.take_text_reflows(&mut q);
        assert!(q.is_empty(), "a box as wide as the run");
    }

    #[test]
    fn fill_soft_wrap_text_wider_than_box_does_not_reflow_when_box_is_wide() {
        // The box (400) is wider than the natural single-line run (300): no
        // wrapping is needed, so nothing is enqueued.
        let (mut store, _col, _leaf) = wrap_scene(
            Length::fill(),
            text_run(Vec2 { x: 300.0, y: 20.0 }, None, true),
            surface(400.0, 400.0),
        );
        let mut q = Vec::new();
        store.take_text_reflows(&mut q);
        assert!(
            q.is_empty(),
            "a box wider than the single-line run needs no wrap"
        );
    }

    #[test]
    fn reflow_writeback_marks_layout_not_semantics_and_converges() {
        // Full Phase-A -> queue -> (facade stand-in reshape) -> Phase-A loop,
        // asserting: the write-back dirties MEASURE|LAYOUT|PAINT but not
        // SEMANTICS, and a second layout pass enqueues nothing (convergence).
        let surf = surface(100.0, 400.0);
        let (mut store, col, leaf) = wrap_scene(
            Length::fill(),
            text_run(Vec2 { x: 300.0, y: 20.0 }, None, true),
            surf,
        );

        let mut q = Vec::new();
        store.take_text_reflows(&mut q);
        assert_eq!(q, vec![(leaf, 100.0)], "first pass enqueues the box width");

        // Stand in for the facade: reshape the run at width 100 — three rows of
        // ~100px each, so the natural shrinks to <=100 wide and grows to 60 tall
        // — and write it back via the reflow-only path.
        store.clear_dirty();
        store.set_reflowed_content(
            leaf,
            text_run(Vec2 { x: 100.0, y: 60.0 }, Some(100.0), true),
        );

        let d = store.dirty(leaf);
        assert!(
            d.contains(DirtyClass::MEASURE)
                && d.contains(DirtyClass::LAYOUT)
                && d.contains(DirtyClass::PAINT),
            "a reflow write-back re-measures/lays-out/paints, got {d:?}"
        );
        assert!(
            !d.contains(DirtyClass::SEMANTICS),
            "a width-only reshape must not dirty the semantics tree, got {d:?}"
        );

        // Re-run layout: the leaf is now shaped at exactly its assigned width, so
        // the recorder finds no mismatch and the queue settles empty.
        let mut scratch = Vec::new();
        crate::layout::measure(&mut store, col.index(), &mut scratch);
        crate::layout::layout(&mut store, col.index(), surf, &mut scratch);
        store.take_text_reflows(&mut q);
        assert!(
            q.is_empty(),
            "after reshaping at the assigned width, the reflow queue converges"
        );

        // The wrapped leaf grew to the reshaped run's height.
        assert!(
            (store.bounds(leaf).h - 60.0).abs() <= 0.5,
            "the wrapped Fill leaf takes the reshaped run height, got {}",
            store.bounds(leaf).h
        );
    }

    #[test]
    fn subpixel_width_jitter_does_not_reenqueue_a_settled_reflow() {
        // A run already shaped at 100 sits in a box whose width jitters by a
        // sub-pixel amount (a drag-resize frame). The quantize-to-integer +
        // 0.5px guard must not re-enqueue a reshape.
        let (mut store, col, _leaf) = wrap_scene(
            Length::fill(),
            text_run(Vec2 { x: 100.0, y: 60.0 }, Some(100.0), true),
            surface(100.3, 400.0),
        );
        let mut q = Vec::new();
        store.take_text_reflows(&mut q);
        assert!(
            q.is_empty(),
            "a sub-pixel width change must not reflow a settled run, got {q:?}"
        );

        // A whole-pixel narrowing past the guard *does* re-fire.
        let surf2 = surface(80.0, 400.0);
        let mut scratch = Vec::new();
        crate::layout::measure(&mut store, col.index(), &mut scratch);
        crate::layout::layout(&mut store, col.index(), surf2, &mut scratch);
        store.take_text_reflows(&mut q);
        assert_eq!(
            q.len(),
            1,
            "a whole-pixel narrowing re-fires the reflow, got {q:?}"
        );
        assert_eq!(q[0].1, 80.0, "the new box width is recorded");
    }

    #[test]
    fn fixed_width_soft_wrap_text_reflows_to_the_fixed_box() {
        // A Fixed width is layout-derived (a hard box), so a soft-wrap run
        // narrower than the box wraps to it just like Fill.
        let (mut store, _col, leaf) = wrap_scene(
            Length::Fixed(120.0),
            text_run(Vec2 { x: 300.0, y: 20.0 }, None, true),
            surface(400.0, 400.0),
        );
        let mut q = Vec::new();
        store.take_text_reflows(&mut q);
        assert_eq!(
            q,
            vec![(leaf, 120.0)],
            "a Fixed-width soft-wrap leaf reflows to its fixed box"
        );
    }
}
