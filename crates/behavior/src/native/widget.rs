//! Native widget declarations: the node types a view instantiates, each with
//! the properties, property groups, events and slots its schema declares.
//!
//! A [`NativeWidget`] is static data a [`NativeLibrary`](super::NativeLibrary)
//! lists next to its functions and handle types. The compiler checks a view's
//! nodes against it — property types, `bind` targets, percent bases, event
//! payloads, slot cardinality — and lowers each node to the retained node kind
//! it declares ([`WidgetNode`]). Types are named by their DSL spelling
//! (`Option<MixedLength>`, `Sizing`); the compiler checks the ones it models
//! and treats the others as opaque schema types.

/// The retained node a widget lowers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WidgetNode {
    /// A flex container: along the inline axis, the block axis, or the axis
    /// its `axis` property names.
    Flex(FlexAxis),
    /// A block-axis flex container that re-establishes the size class for
    /// its subtree from its own width.
    AdaptiveScope,
    /// A block-axis flex container padded by the part of its box the safe
    /// area covers.
    SafeArea,
    /// A block-axis flex container padded by the part of its box the
    /// software keyboard covers.
    KeyboardAvoiding,
    /// A track grid.
    Grid,
    /// A single-child scroll viewport.
    Scroll,
    /// A virtualized list, which mounts its items itself.
    VirtualList,
    /// A node without authored children.
    Leaf,
    /// A zero-node grouping: its children join the parent's slot, and it takes
    /// no properties.
    Fragment,
    /// Where a component's view places one of its slots: the nodes its caller
    /// fills the slot with, or none when the caller leaves it empty.
    Outlet,
}

/// The main axis of a [`WidgetNode::Flex`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FlexAxis {
    /// The inline axis.
    Row,
    /// The block axis.
    Column,
    /// The axis the node's `axis` property names, the block axis by default.
    Property,
}

/// One property of a widget.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WidgetProperty {
    /// Its name.
    pub name: &'static str,
    /// Its value type, by its DSL spelling.
    pub ty: &'static str,
    /// Whether it may be the left side of `bind`: the widget reports its
    /// changes through the event [`NativeWidget::write_back`] names.
    pub two_way: bool,
    /// Whether it resolves a `Percent` against a basis.
    pub percent_basis: bool,
    /// Whether a style may bind it: it is part of how the node looks, not of
    /// what it shows or does.
    pub styleable: bool,
}

impl WidgetProperty {
    /// A one-way property of type `ty`, without a percent basis.
    pub const fn new(name: &'static str, ty: &'static str) -> WidgetProperty {
        WidgetProperty {
            name,
            ty,
            two_way: false,
            percent_basis: false,
            styleable: false,
        }
    }

    /// A style may bind it.
    pub const fn styleable(mut self) -> WidgetProperty {
        self.styleable = true;
        self
    }

    /// Resolves a `Percent` against a basis.
    pub const fn based(mut self) -> WidgetProperty {
        self.percent_basis = true;
        self
    }

    /// May be the left side of `bind`.
    pub const fn two_way(mut self) -> WidgetProperty {
        self.two_way = true;
        self
    }
}

/// Properties written under one prefix, `prefix.member`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PropertyGroup {
    /// The prefix, such as `semantics`.
    pub prefix: &'static str,
    /// Its members.
    pub members: &'static [WidgetProperty],
}

impl PropertyGroup {
    /// The member `name`.
    pub fn member(&self, name: &str) -> Option<&'static WidgetProperty> {
        self.members.iter().find(|p| p.name == name)
    }
}

/// An event a widget raises.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WidgetEvent {
    /// Its name.
    pub name: &'static str,
    /// The record its payload is, if it has one.
    pub payload: Option<&'static str>,
    /// Whether it bubbles up the ancestor route after its target.
    pub bubbles: bool,
}

impl WidgetEvent {
    /// A target-only event carrying a `payload` record.
    pub const fn new(name: &'static str, payload: &'static str) -> WidgetEvent {
        WidgetEvent {
            name,
            payload: Some(payload),
            bubbles: false,
        }
    }

    /// An input event routed capture → target → bubble.
    pub const fn routed(name: &'static str, payload: &'static str) -> WidgetEvent {
        WidgetEvent {
            bubbles: true,
            ..WidgetEvent::new(name, payload)
        }
    }

    /// A target-only event without a payload.
    pub const fn signal(name: &'static str) -> WidgetEvent {
        WidgetEvent {
            name,
            payload: None,
            bubbles: false,
        }
    }
}

/// How many nodes a slot takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SlotCardinality {
    /// Exactly one (`Slot<Node>`).
    One,
    /// Zero or one (`OptionalSlot<Node>`).
    Optional,
    /// Any number (`SlotList<Node>`).
    Many,
}

impl SlotCardinality {
    /// The slot type's name.
    pub fn type_name(self) -> &'static str {
        match self {
            SlotCardinality::One => "Slot<Node>",
            SlotCardinality::Optional => "OptionalSlot<Node>",
            SlotCardinality::Many => "SlotList<Node>",
        }
    }

    /// Whether `count` nodes satisfy it.
    pub fn admits(self, count: usize) -> bool {
        match self {
            SlotCardinality::One => count == 1,
            SlotCardinality::Optional => count <= 1,
            SlotCardinality::Many => true,
        }
    }
}

/// A slot a widget's caller fills with nodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WidgetSlot {
    /// Its name, the one `fill` names.
    pub name: &'static str,
    /// How many nodes it takes.
    pub cardinality: SlotCardinality,
    /// Whether it is the default slot, which bare child items fill.
    pub default: bool,
}

impl WidgetSlot {
    /// The default slot `name`.
    pub const fn default(name: &'static str, cardinality: SlotCardinality) -> WidgetSlot {
        WidgetSlot {
            name,
            cardinality,
            default: true,
        }
    }
}

/// The live state of a widget's node a hot reload carries to the node that
/// replaces it in a rebuilt tree, when the diff keeps the node.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct MigratableState(u8);

impl MigratableState {
    /// Nothing carries.
    pub const NONE: MigratableState = MigratableState(0);
    /// Keyboard focus.
    pub const FOCUS: MigratableState = MigratableState(1);
    /// A scroll viewport's offset.
    pub const SCROLL: MigratableState = MigratableState(1 << 1);
    /// A text field's edit buffer: its caret, selection and text.
    pub const SELECTION: MigratableState = MigratableState(1 << 2);
    /// A look property's transition in flight.
    pub const ANIMATION: MigratableState = MigratableState(1 << 3);

    /// Both sets.
    pub const fn with(self, other: MigratableState) -> MigratableState {
        MigratableState(self.0 | other.0)
    }

    /// Whether every state in `other` carries.
    pub const fn contains(self, other: MigratableState) -> bool {
        self.0 & other.0 == other.0
    }

    /// Whether nothing carries.
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
}

/// A native widget: a node type a view instantiates by name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NativeWidget {
    /// Its type name, the one a view writes.
    pub name: &'static str,
    /// The retained node it lowers to.
    pub node: WidgetNode,
    /// Its properties, table by table; a name in an earlier table wins.
    pub properties: &'static [&'static [WidgetProperty]],
    /// The `prefix.member` groups it takes.
    pub groups: &'static [&'static PropertyGroup],
    /// The group it provides to its direct children, written in a child's
    /// body.
    pub provides: Option<&'static PropertyGroup>,
    /// Its events, table by table.
    pub events: &'static [&'static [WidgetEvent]],
    /// Its slots.
    pub slots: &'static [WidgetSlot],
    /// The live state of its node a hot reload carries.
    pub migratable: MigratableState,
}

impl NativeWidget {
    /// A widget lowering to `node` with no properties, events or slots.
    pub const fn new(name: &'static str, node: WidgetNode) -> NativeWidget {
        NativeWidget {
            name,
            node,
            properties: &[],
            groups: &[],
            provides: None,
            events: &[],
            slots: &[],
            migratable: MigratableState::NONE,
        }
    }

    /// Takes `properties`.
    pub const fn properties(mut self, properties: &'static [&'static [WidgetProperty]]) -> Self {
        self.properties = properties;
        self
    }

    /// Takes the `groups`.
    pub const fn groups(mut self, groups: &'static [&'static PropertyGroup]) -> Self {
        self.groups = groups;
        self
    }

    /// Provides `group` to its direct children.
    pub const fn provides(mut self, group: &'static PropertyGroup) -> Self {
        self.provides = Some(group);
        self
    }

    /// Raises `events`.
    pub const fn events(mut self, events: &'static [&'static [WidgetEvent]]) -> Self {
        self.events = events;
        self
    }

    /// Takes `slots`.
    pub const fn slots(mut self, slots: &'static [WidgetSlot]) -> Self {
        self.slots = slots;
        self
    }

    /// Carries `state` across a hot reload, on top of what it carries already.
    pub const fn migratable(mut self, state: MigratableState) -> Self {
        self.migratable = self.migratable.with(state);
        self
    }

    /// The property `name`.
    pub fn property(&self, name: &str) -> Option<&'static WidgetProperty> {
        self.properties
            .iter()
            .flat_map(|t| t.iter())
            .find(|p| p.name == name)
    }

    /// The group written with `prefix`.
    pub fn group(&self, prefix: &str) -> Option<&'static PropertyGroup> {
        self.groups.iter().copied().find(|g| g.prefix == prefix)
    }

    /// The event `name`.
    pub fn event(&self, name: &str) -> Option<&'static WidgetEvent> {
        self.events
            .iter()
            .flat_map(|t| t.iter())
            .find(|e| e.name == name)
    }

    /// The slot `name`.
    pub fn slot(&self, name: &str) -> Option<&'static WidgetSlot> {
        self.slots.iter().find(|s| s.name == name)
    }

    /// The default slot, if it has one.
    pub fn default_slot(&self) -> Option<&'static WidgetSlot> {
        self.slots.iter().find(|s| s.default)
    }

    /// The event a `bind` of the two-way property `name` writes back
    /// through: `<name>_changed`, or `changed` for the widget's primary (first)
    /// two-way property, when the widget raises it.
    pub fn write_back(&self, name: &str) -> Option<&'static WidgetEvent> {
        let mut two_way = self
            .properties
            .iter()
            .flat_map(|t| t.iter())
            .filter(|p| p.two_way);
        let primary = two_way.next()?.name == name;
        if !primary && !two_way.any(|p| p.name == name) {
            return None;
        }
        self.events.iter().flat_map(|t| t.iter()).find(|e| {
            e.name.strip_suffix("_changed") == Some(name) || (primary && e.name == "changed")
        })
    }
}
