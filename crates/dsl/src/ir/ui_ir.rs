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
//! to turn into a `StateId -> (node, DirtyClass)` edge. Nothing here depends on
//! `viso-ui`; the length/axis mirrors below carry just enough for the emitter to
//! construct the runtime `Size`/`Axis`/style structs.

use crate::ir::dirty_map::{DirtyClass, property_dirty_class};
use crate::syntax::span::TextRange;

/// The retained-tree template a view fragment or component view lowers to.
#[derive(Debug, Clone, PartialEq)]
pub struct UiTree {
    /// The top-level items, in source order.
    pub items: Vec<UiItem>,
}

/// One item in a view block: a mounted node or a control-flow region.
#[derive(Debug, Clone, PartialEq)]
pub enum UiItem {
    /// A mounted retained node (`Column { ... }`, `node label: Text { ... }`).
    Node(UiNode),
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
    /// The node's children, in source order.
    pub children: Vec<UiItem>,
    /// The node declaration's source span.
    pub origin: TextRange,
}

/// Which `viso_ui::BuildCx` builder call a node maps to. The mapping is by the
/// node's type name; an unrecognized type defaults to [`NodeKind::Leaf`] (a
/// primitive with no child region) until a schema pass resolves user components.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    /// A flex container (`Row`/`Column`/`Flex`/`Stack`) → `cx.flex`.
    Flex,
    /// A grid container → `cx.grid`.
    Grid,
    /// A scroll container → `cx.scroll`.
    Scroll,
    /// A virtualized list → `cx.virtual_list`.
    VirtualList,
    /// A leaf primitive (`Text`, `Image`, an unknown type) → `cx.leaf`.
    Leaf,
}

impl NodeKind {
    /// The builder call a node type maps to. Container types get their matching
    /// container call; everything else is a leaf until component resolution lands.
    pub fn from_type_name(name: &str) -> Self {
        match name {
            "Row" | "Column" | "Flex" | "Stack" | "HStack" | "VStack" => NodeKind::Flex,
            "Grid" => NodeKind::Grid,
            "Scroll" | "ScrollView" => NodeKind::Scroll,
            "VirtualList" | "List" => NodeKind::VirtualList,
            _ => NodeKind::Leaf,
        }
    }

    /// Whether this kind hosts a child region the emitter descends into.
    pub fn is_container(self) -> bool {
        matches!(
            self,
            NodeKind::Flex | NodeKind::Grid | NodeKind::Scroll | NodeKind::VirtualList
        )
    }
}

/// A dimension along one axis, mirroring `viso_ui::layout::Length`. `Dp`/`Px`
/// units and bare numbers fold to [`LengthIr::Fixed`]; `%` and `fill` fold to
/// [`LengthIr::Fill`]; `fit`/`auto` fold to [`LengthIr::Fit`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LengthIr {
    /// A fixed extent in logical pixels.
    Fixed(f32),
    /// A weighted share of remaining space.
    Fill { weight: f32 },
    /// Sized to content.
    Fit,
}

/// The layout axis a container arranges along, mirroring `viso_ui::layout::Axis`.
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
    /// The folded width, when a constant `width`/`size` property set one.
    pub width: Option<LengthIr>,
    /// The folded height, when a constant `height`/`size` property set one.
    pub height: Option<LengthIr>,
    /// The folded gap between children, when a constant `gap`/`spacing` set one.
    pub gap: Option<f32>,
}

impl StyleIr {
    /// Whether nothing was folded — the node takes the runtime default style.
    pub fn is_empty(&self) -> bool {
        self.axis.is_none() && self.width.is_none() && self.height.is_none() && self.gap.is_none()
    }
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
}

impl PendingProperty {
    /// A pending property from its name and value span, tagging it with the
    /// dirty class its name maps to.
    pub fn new(name: impl Into<String>, value: TextRange) -> Self {
        let name = name.into();
        let dirty = property_dirty_class(&name);
        Self { name, value, dirty }
    }
}

/// An event handler declared on a node.
#[derive(Debug, Clone, PartialEq)]
pub struct UiHandler {
    /// The event name (`click`, `hover`, …).
    pub event: String,
    /// The handler declaration's span.
    pub origin: TextRange,
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
}

/// One arm of a [`UiIf`].
#[derive(Debug, Clone, PartialEq)]
pub struct UiIfArm {
    /// The condition expression's span, or `None` for the trailing `else`.
    pub condition: Option<TextRange>,
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
