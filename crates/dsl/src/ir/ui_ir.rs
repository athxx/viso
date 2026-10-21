//! UI IR — the static retained-tree template a view lowers to (AGENTS section 59).
//!
//! A declarative view is *not* a per-frame rebuild. The compiler lowers it once
//! into this UI IR: a tree of [`UiNode`]s carrying folded static style, the
//! properties that still need a runtime binding, and control-flow wrappers for
//! `if`/`for`/`match`. The emitter walks this tree to produce a `viso_ui`
//! builder closure that mounts the retained nodes exactly once; ordinary state
//! updates then flow through the Binding IR onto the mounted nodes, never
//! reconstructing the tree.
//!
//! This module is the *shape* of the IR plus the static-style folding that turns
//! literal property values (`width: 12dp`, `axis: column`) into concrete layout
//! inputs. A property whose value is not a compile-time constant is not folded —
//! it is recorded as a [`PendingProperty`] for the Binding IR pass (section 3)
//! to turn into a `StateId -> (node, DirtyClass)` edge. The length/axis
//! mirrors below carry just enough for the emitter to construct the runtime
//! `Size`/`Axis`/style structs.

use crate::ir::dirty_map::{DirtyClass, property_dirty_class};
use crate::resolve::SymbolId;
use crate::syntax::span::TextRange;
use viso_behavior::native::MigratableState;
use viso_schema::Interaction;
pub use viso_schema::layout::{
    AbsoluteInsets, AdaptiveColumns, AlignItems, Alignment2D, AutoRepeat, BoxRules, GridPlacement,
    Inset, Placement, Span, SpanInset, StackPlacement, TrackMax, TrackSizing,
};
pub use viso_schema::{Align, Justify};
pub use viso_schema::{Avoid, LayoutDirection};

/// The retained-tree template a view fragment or component view lowers to.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct UiTree {
    /// The top-level items, in source order.
    pub items: Vec<UiItem>,
    /// The user-component instances the view inlines, in pre-order: instance
    /// `i` is `instances[i - 1]`, and instance `0` is the view's own component.
    pub instances: Vec<UiInstance>,
}

impl UiTree {
    /// The inlined instance `instance`, or `None` for the view's own component.
    pub fn instance(&self, instance: u32) -> Option<&UiInstance> {
        let index = instance.checked_sub(1)?;
        self.instances.get(index as usize)
    }
}

/// One user-component node a view inlines: the component's own view mounts in
/// its place, its functions running against the mounted view's component with
/// the caller's arguments and handlers wired in.
#[derive(Debug, Clone, PartialEq)]
pub struct UiInstance {
    /// The component.
    pub component: SymbolId,
    /// The component's name, as the node placing it spells its type.
    pub name: String,
    /// The node or `use` that places it, in its parent's view: the call
    /// site of every node its view mounts.
    pub origin: TextRange,
    /// The instance whose view names the node.
    pub parent: u32,
    /// A name stable across edits that keep the path of named nodes and
    /// component types to it: the parent's, then the node's local name or its
    /// type and ordinal among the parent's instances of that type.
    pub identity: String,
    /// How many `for`/`match` regions enclose the node, counted from the
    /// mounted view's root: the scope values the instance's functions take
    /// before their own.
    pub depth: u32,
    /// Whether a control-flow region encloses the node: its states then live
    /// with the region content that mounts it, one set per mount, instead of
    /// in the mounted component.
    pub regional: bool,
    /// The value of each input the caller binds, by input slot: the source
    /// range of the value, whose entry the parent registers there.
    pub args: Vec<(u32, TextRange)>,
    /// The caller's handler of each of the component's events it handles, by
    /// event name: the range the parent registers it at.
    pub handlers: Vec<(String, TextRange)>,
}

/// One item in a view block: a mounted node or a control-flow region.
#[derive(Debug, Clone, PartialEq)]
pub enum UiItem {
    /// A mounted retained node (`Column { ... }`, `node label: Text { ... }`).
    Node(Box<UiNode>),
    /// A conditional region (`if cond { ... } else { ... }`).
    If(UiIf),
    /// A keyed repetition (`for item in items key item.id { ... }`).
    For(UiFor),
    /// A selection region (`match scrutinee { ... }`).
    Match(UiMatch),
}

/// A mounted retained node and everything the emitter needs to build it.
#[derive(Debug, Clone, PartialEq)]
pub struct UiNode {
    /// The component/primitive type name (`Column`, `Text`, `Button`, …).
    pub type_name: String,
    /// The optional local name from `node name: Type { ... }`, used as the seed
    /// for a stable identity where one is needed.
    pub local_name: Option<String>,
    /// The key a component's actions and handlers reach the node by as a
    /// `NodeRef` (the instance whose view names it in the high 16 bits, its
    /// place among that component's named nodes in the low 16); the root of
    /// a named component node takes its name's.
    pub node_ref: Option<u32>,
    /// Which builder call this node maps to.
    pub kind: NodeKind,
    /// The style folded from the node's compile-time-constant properties.
    pub style: StyleIr,
    /// Properties whose value is not a compile-time constant: each is a binding
    /// candidate the Binding IR pass resolves to a reactive source (or reports
    /// as an unbound/dynamic property). Carries the value expression's span so
    /// the binding pass can run `collect_reads` over it.
    pub pending: Vec<PendingProperty>,
    /// Event handlers declared on the node, by event name and source span.
    pub handlers: Vec<UiHandler>,
    /// The value each property of a view-driven native node reads its current
    /// value or range from (`checked`, `value`, `selected`, `min`, `max`,
    /// `step`, a label's `text`), by property name, the span of the expression or
    /// `bind` source, and the instance whose layout site registers its entry: the
    /// node's own, or a caller's for an `override part` binding.
    pub control_reads: Vec<(String, TextRange, u32)>,
    /// The look the node's styles give it, beyond what it binds itself.
    pub styled: Option<UiStyled>,
    /// The names of the styles its `styles` applies, in the order they
    /// apply: what an inspector selects it by as a class.
    pub styles: Vec<String>,
    /// The role its `semantics.role` names, `None` when it names none.
    pub role: Option<viso_schema::Role>,
    /// The live setting its `semantics.live` names, `None` when it names
    /// none.
    pub live: Option<viso_schema::LiveRegion>,
    /// The node's children, in source order.
    pub children: Vec<UiItem>,
    /// The node declaration's source span.
    pub origin: TextRange,
    /// The component instance whose view the node belongs to: `0` for the
    /// view's own component, else an index into [`UiTree::instances`] plus one.
    pub instance: u32,
    /// The live state of the node a hot reload carries when it keeps the node,
    /// as its widget schema marks it.
    pub migratable: MigratableState,
}

/// The look properties a node's styles give it that the runtime delivers:
/// each value is an entry the view checker registers at a part of the node's
/// styles (see [`crate::behavior::Site::styled`]).
#[derive(Debug, Clone, PartialEq)]
pub struct UiStyled {
    /// The span of the node's `styles` value.
    pub at: TextRange,
    /// Each property, in the order the styles first give it.
    pub looks: Vec<UiLook>,
}

/// One look property a node's styles give it.
#[derive(Debug, Clone, PartialEq)]
pub struct UiLook {
    /// `background`, `opacity`, `transition.background` or
    /// `transition.opacity`.
    pub property: String,
    /// The part of the value it shows while no arm holds.
    pub base: Option<u32>,
    /// The values a `when` switches to, the first that holds winning.
    pub arms: Vec<UiLookArm>,
}

/// A value a style's `when` switches a look property to.
#[derive(Debug, Clone, PartialEq)]
pub struct UiLookArm {
    /// The selector, in postfix order.
    pub when: Vec<UiWhen>,
    /// The part of the value.
    pub part: u32,
}

/// One step of a selector, in postfix order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiWhen {
    /// The node is in this interaction state.
    State(Interaction),
    /// The `Bool` value at this part holds: the node's own property a
    /// selector reads.
    Part(u32),
    /// The negation of the step before.
    Not,
    /// Both steps before.
    And,
    /// Either step before.
    Or,
}

/// Which `viso_ui::BuildCx` builder call a node maps to: the retained node its
/// native widget declaration names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    /// A flex container (`Row`/`Column`/`Flex`) → `cx.flex`.
    Flex,
    /// A grid container → `cx.grid`.
    Grid,
    /// A stack → `cx.stack`.
    Stack,
    /// An absolute container → `cx.absolute`.
    Absolute,
    /// A scroll container → `cx.scroll`.
    Scroll,
    /// A virtualized list → `cx.virtual_list`.
    VirtualList,
    /// A leaf primitive (`Text`, `Button`) → `cx.leaf`.
    Leaf,
    /// A component the Rust scope around a `ui!` fragment declares with
    /// `component!`, mounted by its `build`; its [`UiNode::type_name`] is the
    /// Rust path. Only a `ui!` fragment has one.
    Component,
}

impl NodeKind {
    /// Whether this kind has a content box its `padding` insets.
    pub fn pads(self) -> bool {
        matches!(
            self,
            NodeKind::Flex | NodeKind::Grid | NodeKind::Stack | NodeKind::Absolute
        )
    }

    /// Whether this kind hosts a child region the emitter descends into.
    pub fn is_container(self) -> bool {
        matches!(
            self,
            NodeKind::Flex
                | NodeKind::Grid
                | NodeKind::Stack
                | NodeKind::Absolute
                | NodeKind::Scroll
                | NodeKind::VirtualList
        )
    }
}

/// A dimension along one axis, mirroring `viso_schema::layout::Length`. A pure-`dp`
/// constant folds to [`LengthIr::Fixed`]; a constant with a `%` term, such as
/// `50%` or `100% - 16dp`, folds to [`LengthIr::Relative`]. `%` is a ratio of the
/// parent's content box, never a fill share.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LengthIr {
    /// An exact extent in dp.
    Fixed(f32),
    /// `fixed + pct × basis`, with `pct` a fraction of the basis (`0.5` is `50%`).
    Relative { fixed: f32, pct: f32 },
    /// A weighted share of remaining space.
    Fill { weight: f32 },
    /// The content's extent within the room.
    Fit,
    /// The narrowest the content lays out in.
    MinContent,
    /// The content's unconstrained extent.
    MaxContent,
}

/// A length's five terms, mirroring `viso_schema::LengthTerms`: a length with a
/// `px`, `sp` or `em` term, folded against the node's environment at layout.
/// `pct` is a fraction of the basis (`0.5` is `50%`).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct TermsIr {
    pub dp: f32,
    pub px: f32,
    pub sp: f32,
    pub em: f32,
    pub pct: f32,
}

/// A container's four edges as lowered terms, mirroring
/// `viso_schema::EdgeTerms`: `start` and `end` follow the layout direction.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct EdgeTermsIr {
    pub top: TermsIr,
    pub end: TermsIr,
    pub bottom: TermsIr,
    pub start: TermsIr,
}

/// An absolute child's insets as lowered terms, mirroring
/// `viso_schema::InsetTerms`: `None` for an edge it is not pinned to.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct InsetTermsIr {
    pub top: Option<TermsIr>,
    pub end: Option<TermsIr>,
    pub bottom: Option<TermsIr>,
    pub start: Option<TermsIr>,
}

impl InsetTermsIr {
    /// Whether an edge reads the environment.
    pub fn needs_env(self) -> bool {
        [self.top, self.end, self.bottom, self.start]
            .iter()
            .flatten()
            .any(|t| t.needs_env())
    }
}

/// A physical inset in dp, mirroring `viso_schema::layout::Inset`.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct InsetIr {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

/// A node's lengths that fold against its environment at layout, mirroring
/// `viso_schema::NodeLengths`. The node builds with its [`StyleIr`] and binds
/// these on top.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct LengthsIr {
    pub width: Option<TermsIr>,
    pub height: Option<TermsIr>,
    pub gap: Option<TermsIr>,
    /// The node's font size, the typography source of its subtree.
    pub font_size: Option<TermsIr>,
    /// A container's padding that reads the environment or the layout
    /// direction, or has a percentage.
    pub padding: Option<EdgeTermsIr>,
    /// The node's size bounds; a greatest one `None` is unbounded.
    pub min_width: Option<TermsIr>,
    pub max_width: Option<TermsIr>,
    pub min_height: Option<TermsIr>,
    pub max_height: Option<TermsIr>,
    /// The node's margin.
    pub margin: Option<EdgeTermsIr>,
    /// A grid's row gap; its column gap is [`gap`](Self::gap).
    pub row_gap: Option<TermsIr>,
    /// An absolute child's insets, when one reads the environment.
    pub insets: Option<InsetTermsIr>,
}

impl LengthsIr {
    /// No length.
    pub const NONE: Self = Self {
        width: None,
        height: None,
        gap: None,
        font_size: None,
        padding: None,
        min_width: None,
        max_width: None,
        min_height: None,
        max_height: None,
        margin: None,
        row_gap: None,
        insets: None,
    };

    /// Whether the node binds no environment length.
    pub fn is_empty(&self) -> bool {
        *self == Self::NONE
    }

    /// Whether the lengths carry box rules: a size bound, a margin or a
    /// padding percentage.
    pub fn has_rules(&self) -> bool {
        self.min_width.is_some()
            || self.max_width.is_some()
            || self.min_height.is_some()
            || self.max_height.is_some()
            || self.margin.is_some()
            || self.padding.is_some_and(|p| p.has_pct())
    }
}

impl TermsIr {
    /// Whether a term reads the environment.
    pub fn needs_env(self) -> bool {
        self.px != 0.0 || self.sp != 0.0 || self.em != 0.0
    }

    /// The span a term without an environment part is.
    fn span(self) -> Span {
        Span {
            fixed: self.dp,
            pct: self.pct,
        }
    }

    /// The terms of a span.
    fn of(span: Span) -> Self {
        Self {
            dp: span.fixed,
            pct: span.pct,
            ..Self::default()
        }
    }
}

impl EdgeTermsIr {
    /// Whether an edge has a percentage.
    pub fn has_pct(self) -> bool {
        [self.top, self.end, self.bottom, self.start]
            .iter()
            .any(|t| t.pct != 0.0)
    }

    /// Whether an edge reads the environment, or the layout direction to
    /// place `start` and `end`.
    fn needs_env(self) -> bool {
        self.start != self.end
            || [self.top, self.end, self.bottom, self.start]
                .iter()
                .any(|t| t.needs_env())
    }
}

/// The layout axis a container arranges along, mirroring `viso_schema::layout::Axis`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AxisIr {
    Row,
    Column,
}

/// The style folded from a node's compile-time-constant properties.
///
/// Only values known at compile time land here; a reactive property stays a
/// [`PendingProperty`]. Fields left `None` take the runtime style default at
/// emit time.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct StyleIr {
    /// The container arrangement axis, when the node type or an `axis` property
    /// fixes one.
    pub axis: Option<AxisIr>,
    /// Where a flex container packs its children along its axis, when a
    /// constant `justify` places them.
    pub justify: Option<Justify>,
    /// How a flex container places its children across its axis, when a
    /// constant `align` places them.
    pub align: Option<Align>,
    /// The folded width, when a constant `width`/`size` property set one.
    pub width: Option<LengthIr>,
    /// The folded height, when a constant `height`/`size` property set one.
    pub height: Option<LengthIr>,
    /// The folded gap between children, when a constant `gap`/`spacing` set one.
    pub gap: Option<f32>,
    /// The folded padding of a container, when a constant `padding` of plain
    /// `dp` whose `start` and `end` agree set one.
    pub padding: Option<InsetIr>,
    /// The constant lengths that fold against the environment at layout;
    /// boxed, as few nodes have any. Read through [`lengths`](Self::lengths).
    pub lengths: Option<Box<LengthsIr>>,
    /// The adaptive scope the node establishes, when it is one.
    pub scope: Option<ScopeIr>,
    /// What the node keeps its content clear of, when it is an avoiding
    /// region.
    pub avoid: Option<Avoid>,
    /// The constant box rules — size bounds, margin, padding percentage —
    /// when none reads the environment or the layout direction; otherwise
    /// [`lengths`](Self::lengths) carries them all; boxed, as few nodes have
    /// any.
    pub rules: Option<Box<BoxRules>>,
    /// A grid's constant template, gaps and cell alignment, and the areas
    /// its children name; boxed, as few nodes are grids.
    pub grid: Option<Box<GridIr>>,
    /// Where a stack places a child that does not align itself, when a
    /// constant `content_align` sets it.
    pub content_align: Option<Alignment2D>,
    /// Where the node sits in its parent: the cell a grid's child takes by
    /// `grid.*`, a stack child's `stack.*` alignment and layer, an absolute
    /// child's `absolute.*` insets when none reads the environment (else
    /// [`lengths`](Self::lengths) carries them).
    pub place: Option<Placement>,
    /// The area a grid's child names by `grid.area`, until its grid resolves
    /// it to [`place`](Self::place).
    pub area: Option<Box<AreaRef>>,
    /// A `VirtualList`'s constant row estimate and overscan, when either is
    /// set.
    pub list: Option<ListIr>,
}

/// How a `VirtualList` mounts its rows: the extent a row not measured yet is
/// estimated at, and the rows mounted past each end of the viewport.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ListIr {
    pub estimate: TermsIr,
    pub overscan: u32,
}

impl Default for ListIr {
    fn default() -> Self {
        ListIr {
            estimate: TermsIr {
                dp: 30.0,
                ..TermsIr::default()
            },
            overscan: 4,
        }
    }
}

/// A grid container's constant template, gaps and cell alignment. A field
/// left `None` or empty takes the runtime grid default.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct GridIr {
    /// The explicit column template.
    pub columns: Vec<TrackSizing>,
    /// The explicit row template.
    pub rows: Vec<TrackSizing>,
    /// How a row past the explicit ones is sized.
    pub auto_rows: Option<TrackSizing>,
    /// The gap between adjacent columns, in dp.
    pub column_gap: Option<f32>,
    /// The gap between adjacent rows, in dp.
    pub row_gap: Option<f32>,
    /// How a cell's content sits in its cell on the block axis.
    pub align_items: Option<AlignItems>,
    /// A column template whose count follows the grid's width.
    pub adaptive_columns: Option<AdaptiveColumns>,
    /// Each area `areas` names and the cells it covers. Only lowering reads
    /// it, to place the children that name one.
    pub areas: Vec<(String, GridPlacement)>,
}

/// An area a grid's child names, and where it names it.
#[derive(Debug, Clone, PartialEq)]
pub struct AreaRef {
    /// The area's name.
    pub name: String,
    /// The span of the `grid.area` value.
    pub at: TextRange,
}

/// An adaptive scope: the node its subtree's `env.size_class` classifies.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ScopeIr {
    /// The constant width in dp it classifies, else the width its parent
    /// gives it.
    pub basis: Option<f32>,
    /// The constant direction its subtree's lines run in, else its parent's.
    pub direction: Option<LayoutDirection>,
}

impl StyleIr {
    /// Whether nothing was folded — the node takes the runtime default style.
    pub fn is_empty(&self) -> bool {
        self.axis.is_none()
            && self.justify.is_none()
            && self.align.is_none()
            && self.width.is_none()
            && self.height.is_none()
            && self.gap.is_none()
            && self.padding.is_none()
            && self.lengths().is_empty()
            && self.scope.is_none()
            && self.avoid.is_none()
            && self.rules.is_none()
            && self.grid.is_none()
            && self.place.is_none()
            && self.area.is_none()
            && self.list.is_none()
    }

    /// The lengths that fold against the environment, empty when there are none.
    pub fn lengths(&self) -> &LengthsIr {
        self.lengths.as_deref().unwrap_or(&LengthsIr::NONE)
    }

    /// Settle where the box rules live: as constant [`rules`](Self::rules)
    /// when none reads the environment or the layout direction and none is
    /// set from a value (`laid`), else all of them in the lengths, a constant
    /// padding's percentage with them. So do an absolute child's insets: a
    /// constant placement when none reads the environment and none is set
    /// from a value, else all of them in the lengths.
    pub fn settle_rules(&mut self, laid: &Laid) {
        if laid.insets {
            self.unsettle_insets();
        } else {
            self.settle_insets();
        }
        if laid.rules {
            self.unsettle_rules();
            return;
        }
        let Some(lengths) = self.lengths.as_deref_mut() else {
            return;
        };
        if !lengths.has_rules() {
            return;
        }
        let bounds = [
            lengths.min_width,
            lengths.max_width,
            lengths.min_height,
            lengths.max_height,
        ];
        let edges = [lengths.margin, lengths.padding.filter(|p| p.has_pct())];
        if bounds.iter().flatten().any(|t| t.needs_env())
            || edges.iter().flatten().any(|e| e.needs_env())
        {
            return;
        }
        let span = |terms: Option<TermsIr>, none: Span| terms.map_or(none, TermsIr::span);
        let mut rules = BoxRules {
            min_width: span(lengths.min_width.take(), Span::ZERO),
            max_width: span(lengths.max_width.take(), Span::UNBOUNDED),
            min_height: span(lengths.min_height.take(), Span::ZERO),
            max_height: span(lengths.max_height.take(), Span::UNBOUNDED),
            ..BoxRules::NONE
        };
        if let Some(m) = lengths.margin.take() {
            rules.margin = SpanInset {
                left: m.start.span(),
                top: m.top.span(),
                right: m.end.span(),
                bottom: m.bottom.span(),
            };
        }
        if let Some(p) = lengths.padding.take_if(|p| p.has_pct()) {
            rules.padding_pct = Inset {
                left: p.start.pct,
                top: p.top.pct,
                right: p.end.pct,
                bottom: p.bottom.pct,
            };
            self.padding = Some(InsetIr {
                left: p.start.dp,
                top: p.top.dp,
                right: p.end.dp,
                bottom: p.bottom.dp,
            });
        }
        if self.lengths().is_empty() {
            self.lengths = None;
        }
        self.rules = Some(Box::new(rules));
    }

    /// Move settled [`rules`](Self::rules) back into the lengths, so a
    /// property folding after the settle (a forwarded one) rebinds its rule
    /// alone before the rules settle again.
    pub fn unsettle_rules(&mut self) {
        let Some(&rules) = self.rules.take().as_deref() else {
            return;
        };
        let padding = self.padding;
        let lengths = self.lengths_mut();
        let bound = |span: Span, none: Span| (span != none).then(|| TermsIr::of(span));
        lengths.min_width = bound(rules.min_width, Span::ZERO);
        lengths.max_width = bound(rules.max_width, Span::UNBOUNDED);
        lengths.min_height = bound(rules.min_height, Span::ZERO);
        lengths.max_height = bound(rules.max_height, Span::UNBOUNDED);
        let m = rules.margin;
        lengths.margin = (m != SpanInset::ZERO).then(|| EdgeTermsIr {
            top: TermsIr::of(m.top),
            end: TermsIr::of(m.right),
            bottom: TermsIr::of(m.bottom),
            start: TermsIr::of(m.left),
        });
        let pct = rules.padding_pct;
        if pct != Inset::default() {
            let dp = padding.unwrap_or_default();
            let edge = |dp: f32, pct: f32| TermsIr {
                dp,
                pct,
                ..TermsIr::default()
            };
            lengths.padding = Some(EdgeTermsIr {
                top: edge(dp.top, pct.top),
                end: edge(dp.right, pct.right),
                bottom: edge(dp.bottom, pct.bottom),
                start: edge(dp.left, pct.left),
            });
            self.padding = None;
        }
    }

    /// Move insets that read no environment from the lengths to the
    /// placement.
    fn settle_insets(&mut self) {
        let Some(insets) = self.lengths().insets.filter(|i| !i.needs_env()) else {
            return;
        };
        let span = |terms: Option<TermsIr>| terms.map(TermsIr::span);
        let placed = AbsoluteInsets {
            top: span(insets.top),
            end: span(insets.end),
            bottom: span(insets.bottom),
            start: span(insets.start),
        };
        self.lengths_mut().insets = None;
        if self.lengths().is_empty() {
            self.lengths = None;
        }
        self.place = (placed != AbsoluteInsets::NONE).then_some(Placement::Absolute(placed));
    }

    /// Move a settled placement's insets back into the lengths.
    pub fn unsettle_insets(&mut self) {
        if matches!(self.place, Some(Placement::Absolute(_))) {
            self.insets_mut();
        }
    }

    /// The insets an absolute child is pinned by, moved into the lengths
    /// for one more edge to fold.
    pub fn insets_mut(&mut self) -> &mut InsetTermsIr {
        if let Some(Placement::Absolute(placed)) = self.place.take() {
            let terms = |span: Option<Span>| span.map(TermsIr::of);
            self.lengths_mut().insets = Some(InsetTermsIr {
                top: terms(placed.top),
                end: terms(placed.end),
                bottom: terms(placed.bottom),
                start: terms(placed.start),
            });
        }
        self.lengths_mut()
            .insets
            .get_or_insert_with(Default::default)
    }

    /// The cell a grid's child takes, taking one on first write.
    pub fn cell_mut(&mut self) -> &mut GridPlacement {
        if !matches!(self.place, Some(Placement::Grid(_))) {
            self.place = Some(Placement::Grid(GridPlacement::default()));
        }
        match &mut self.place {
            Some(Placement::Grid(cell)) => cell,
            _ => unreachable!("a grid placement was just put"),
        }
    }

    /// A stack child's alignment and layer, taking them on first write.
    pub fn stacked_mut(&mut self) -> &mut StackPlacement {
        if !matches!(self.place, Some(Placement::Stack(_))) {
            self.place = Some(Placement::Stack(StackPlacement::default()));
        }
        match &mut self.place {
            Some(Placement::Stack(stacked)) => stacked,
            _ => unreachable!("a stack placement was just put"),
        }
    }

    /// Drop a placement that says nothing.
    pub fn settle_place(&mut self) {
        if self.place.is_some_and(|p| p.is_default()) {
            self.place = None;
        }
    }

    /// The lengths, allocated on first write.
    pub fn lengths_mut(&mut self) -> &mut LengthsIr {
        self.lengths.get_or_insert_with(Box::default)
    }

    /// The grid template, allocated on first write.
    pub fn grid_mut(&mut self) -> &mut GridIr {
        self.grid.get_or_insert_with(Box::default)
    }
}

/// What of a node's layout is set from values as they change, which keeps
/// the constant rest of it in the node's lengths to fold with them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Laid {
    /// A box rule or the padding.
    pub rules: bool,
    /// An absolute child's inset.
    pub insets: bool,
}

/// A property whose value was not a compile-time constant, kept for the Binding
/// IR pass to resolve to a reactive source.
#[derive(Debug, Clone, PartialEq)]
pub struct PendingProperty {
    /// The property's leading name (`text`, `color`, …).
    pub name: String,
    /// The span of the value expression, so the binding pass can look up its
    /// resolved reads via `collect_reads`.
    pub value: TextRange,
    /// The dirty classes a write to this property invalidates (section 11).
    pub dirty: DirtyClass,
    /// The component instance whose view the value belongs to: `0` for the
    /// view's own component, else an index into [`UiTree::instances`] plus one.
    pub instance: u32,
}

impl PendingProperty {
    /// A pending property of the instance `instance` from its name and value
    /// span, tagging it with the dirty class its name maps to.
    pub fn new(name: impl Into<String>, value: TextRange, instance: u32) -> Self {
        let name = name.into();
        let dirty = property_dirty_class(&name);
        Self {
            name,
            value,
            dirty,
            instance,
        }
    }
}

/// An event handler declared on a node.
#[derive(Debug, Clone, PartialEq)]
pub struct UiHandler {
    /// The event name (`click`, `hover`, …).
    pub event: String,
    /// The handler declaration's span.
    pub origin: TextRange,
    /// The component instance whose view the handler belongs to: `0` for the
    /// view's own component, else an index into [`UiTree::instances`] plus one.
    pub instance: u32,
    /// Whether it is an `on capture` handler, run as the sample walks down.
    pub capture: bool,
}

/// A conditional region. Each branch carries the condition's span (for the
/// Binding IR pass) and the items mounted when it is taken.
#[derive(Debug, Clone, PartialEq)]
pub struct UiIf {
    /// The `if`/`else if` arms in order; the condition span is `None` for a bare
    /// trailing `else`.
    pub arms: Vec<UiIfArm>,
    /// The region declaration's span.
    pub origin: TextRange,
    /// The component instance whose view the region belongs to: `0` for the
    /// view's own component, else an index into [`UiTree::instances`] plus one.
    pub instance: u32,
}

/// One arm of a [`UiIf`].
#[derive(Debug, Clone, PartialEq)]
pub struct UiIfArm {
    /// The condition expression's span, or `None` for the trailing `else`.
    pub condition: Option<TextRange>,
    /// The `preserve` identity of the arm: leaving it caches the mounted
    /// instance instead of destroying it.
    pub preserve: Option<String>,
    /// The items mounted when this arm is taken.
    pub items: Vec<UiItem>,
}

/// A keyed repetition. The iterable and key spans feed the Binding IR / keys
/// passes (sections 10, 21.8).
#[derive(Debug, Clone, PartialEq)]
pub struct UiFor {
    /// The loop binding name (`item`).
    pub binding: Option<String>,
    /// The iterable expression's span.
    pub iterable: Option<TextRange>,
    /// The stable-key expression's span, when a `key` clause is present.
    pub key: Option<TextRange>,
    /// The per-item body items.
    pub body: Vec<UiItem>,
    /// The region declaration's span.
    pub origin: TextRange,
    /// The component instance whose view the region belongs to: `0` for the
    /// view's own component, else an index into [`UiTree::instances`] plus one.
    pub instance: u32,
}

/// A selection region. Each arm carries its pattern/guard spans and body.
#[derive(Debug, Clone, PartialEq)]
pub struct UiMatch {
    /// The scrutinee expression's span.
    pub scrutinee: Option<TextRange>,
    /// The arms in order.
    pub arms: Vec<UiMatchArm>,
    /// The region declaration's span.
    pub origin: TextRange,
    /// The component instance whose view the region belongs to: `0` for the
    /// view's own component, else an index into [`UiTree::instances`] plus one.
    pub instance: u32,
}

/// One arm of a [`UiMatch`].
#[derive(Debug, Clone, PartialEq)]
pub struct UiMatchArm {
    /// The arm pattern's span.
    pub pattern: Option<TextRange>,
    /// The optional guard expression's span.
    pub guard: Option<TextRange>,
    /// The items mounted when this arm matches.
    pub items: Vec<UiItem>,
}
