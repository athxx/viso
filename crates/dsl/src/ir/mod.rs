//! UI IR / Binding IR — the view-lowering passes (AGENTS sections 21.2, 59).
//!
//! [`crate::hir::lower`] types each component; these passes walk a component's
//! `view` block (a `ui!` fragment is the view of a stateless one) and lower it
//! into:
//!
//! - **UI IR** ([`ui_ir`]): a static retained-tree template — a [`ui_ir::UiTree`]
//!   of [`ui_ir::UiNode`]s with folded compile-time style, control-flow regions,
//!   and the properties still needing a runtime binding. Mounted once, never
//!   rebuilt per frame (section 59).
//! - the **property → dirty-class** table ([`dirty_map`]) that tags every binding
//!   with exactly what a write invalidates (section 11).
//! - the **Binding IR** ([`binding_ir`]) and keyed-list analysis ([`keys`]) over
//!   that template.
//!
//! The emitter lives on the `viso-ui-macros` side, so `viso-dsl` keeps zero
//! UI-runtime and zero proc-macro dependencies. Nothing here reconstructs a heap
//! tree per frame; the length/axis/dirty mirrors carry just enough for the
//! emitter to rebuild the runtime style structs.

pub mod binding_ir;
pub mod dirty_map;
mod grid;
pub mod keys;
mod length;
mod place;
pub mod ui_ir;

pub use binding_ir::{
    BindingEdge, BindingIr, BindingKind, InstanceSources, NodeKey, ViewUnits, lower_bindings,
    lower_view_reactivity, source_invalidations,
};
pub use dirty_map::{DirtyClass, property_dirty_class};
pub use keys::{KEYLESS_STATEFUL_FOR, KeyIr, KeyedFor, analyze_keys};
pub use ui_ir::{
    Align, Avoid, AxisIr, Justify, LengthIr, LengthsIr, ListIr, NodeKind, PendingProperty, ScopeIr,
    StyleIr, TermsIr, UiFor, UiHandler, UiIf, UiIfArm, UiInputArm, UiInstance, UiItem, UiLook,
    UiLookArm, UiMatch, UiMatchArm, UiMemberWhen, UiNode, UiStyled, UiStyledInput, UiTree, UiWhen,
};

use std::collections::HashMap;
use std::rc::Rc;

use viso_behavior::native::{FlexAxis, MigratableState, Natives, WidgetNode};
use viso_schema::view::{ControlKind, LayoutSlot};

use crate::ast::{
    AstNode, ComponentDecl, Expr, NodeBody, PathExpr, PropertyBinding, PropertyPath, TypePath,
    ViewBlock, ViewFor, ViewIf, ViewItem, ViewMatch,
};
use crate::hir::ComponentSchema;
use crate::hir::style::{self, StyleBooks};
use crate::resolve::{Resolution, ResolvedRef, SymbolId};
use crate::syntax::span::TextRange;
pub use length::Consts;
use length::{Lowered, Padding};
use ui_ir::{Laid, Placement, StackPlacement};

/// A lowered view: its template, every node type name it wrote that neither a
/// registered widget nor a component of the library declares (each with the
/// span of its type path), and every component node that does not mount. A
/// view lowered with the Rust scope keeps each unknown type as a
/// [`NodeKind::Component`] node.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LoweredView {
    /// The retained-tree template.
    pub tree: UiTree,
    /// The unknown node type names, in source order.
    pub unknown: Vec<(String, TextRange)>,
    /// Each component node the view cannot inline, and why.
    pub unmounted: Vec<Unmounted>,
    /// Each node of a component with `@persist` state the view inlines: such
    /// a component persists only as a view's own.
    pub persisting: Vec<(TextRange, String)>,
    /// Each grid template and child cell that names no cells, and why.
    pub misplaced: Vec<Unmounted>,
}

/// A node or region a view cannot mount.
#[derive(Debug, Clone, PartialEq)]
pub struct Unmounted {
    /// What does not mount, where its view writes it.
    pub at: TextRange,
    /// Why.
    pub reason: String,
    /// The node or `use` of the mounted view's file that places the
    /// component or template it is in, when it is not the mounted one's.
    pub placed_at: Option<TextRange>,
}

/// The components a view may inline: the ones declared in the same unit as
/// the mounted component, with the node types that name them.
#[derive(Debug, Default)]
pub struct ComponentLibrary<'a> {
    /// Each source unit's names, by the unit's index.
    units: Vec<LibraryUnit>,
    components: Vec<LibraryComponent<'a>>,
    /// The styles of the units, by the unit's index.
    styles: StyleBooks,
    /// The constants of the units, which fold into their nodes' style.
    consts: Rc<Consts>,
    /// What a unit the library lacks resolves through: nothing.
    none: LibraryUnit,
}

/// One source unit of a [`ComponentLibrary`]: what the views it declares
/// resolve their names through.
#[derive(Debug, Default)]
pub struct LibraryUnit {
    /// The declaration each resolved name names, by the name's range: the
    /// component of a node-type head.
    heads: HashMap<TextRange, SymbolId>,
}

impl LibraryUnit {
    /// The unit whose names resolve through `refs`.
    pub(crate) fn new(refs: &[ResolvedRef]) -> Self {
        let heads = refs
            .iter()
            .filter_map(|r| match r.to {
                Resolution::Symbol(id) => Some((r.range, id)),
                Resolution::Local(_)
                | Resolution::Native(_)
                | Resolution::Env
                | Resolution::Theme => None,
            })
            .collect();
        LibraryUnit { heads }
    }
}

/// One component of a [`ComponentLibrary`].
#[derive(Debug)]
pub struct LibraryComponent<'a> {
    /// Its typed schema.
    pub schema: &'a ComponentSchema,
    /// Its declaration, whose view is inlined.
    pub decl: ComponentDecl,
    /// The source unit that declares it.
    pub unit: u32,
    /// Each two-way input, by its index among the inputs, and the event that
    /// writes it back.
    pub write_backs: Vec<(usize, String)>,
    /// Whether a state of it is `@persist`.
    pub persists: bool,
    /// Its `@selector` members, which a style's `when` on its nodes reads.
    pub selectors: Vec<String>,
}

impl<'a> ComponentLibrary<'a> {
    /// The library of `components`, declared across `units`, whose nodes
    /// apply `styles`.
    pub(crate) fn new(
        units: Vec<LibraryUnit>,
        components: Vec<LibraryComponent<'a>>,
        styles: StyleBooks,
    ) -> Self {
        ComponentLibrary {
            units,
            components,
            styles,
            consts: Rc::new(Consts::default()),
            none: LibraryUnit::default(),
        }
    }

    /// The library with the constants `consts` its views fold.
    pub(crate) fn with_consts(mut self, consts: Rc<Consts>) -> Self {
        self.consts = consts;
        self
    }

    /// The component `id`.
    pub(crate) fn component(&self, id: SymbolId) -> Option<&LibraryComponent<'a>> {
        self.components.iter().find(|c| c.schema.symbol == id)
    }

    fn get(&self, id: SymbolId) -> Option<&LibraryComponent<'a>> {
        self.component(id)
    }

    /// Source unit `unit`; an empty one for a unit the library lacks.
    fn unit(&self, unit: u32) -> &LibraryUnit {
        self.units.get(unit as usize).unwrap_or(&self.none)
    }
}

/// Lowers the view `block` of the component `root` into a [`UiTree`], inlining
/// every node of a component of `library`: its own view mounts in the node's
/// place as instance `i` of [`UiTree::instances`], with the caller's
/// arguments, event handlers and slot fills wired in. The caller's other
/// properties and handlers apply to the inlined view's single root node.
///
/// A component node that cannot be inlined mounts nothing of its caller's
/// wiring and is reported in [`LoweredView::unmounted`]: a component mounting
/// itself, properties or
/// standard handlers on a component whose view is not exactly one node, a slot
/// placed deeper in `for`/`match` regions than its caller's node, a `bind`
/// through `using`, and a component another unit declares.
///
/// With `rust_scope`, as for a `ui!` fragment, a type that no widget and no
/// component of `library` declares is a component of the surrounding Rust
/// scope.
pub fn lower_component_view(
    block: &ViewBlock,
    natives: &Natives,
    library: &ComponentLibrary<'_>,
    root: SymbolId,
    rust_scope: bool,
) -> LoweredView {
    let mut lowering = Lowering::new(natives, library, Some(root));
    lowering.rust_scope = rust_scope;
    let out = lowering.items(block.items());
    lowering.finish(out)
}

/// The state of one view lowering.
struct Lowering<'a, 'l> {
    natives: &'a Natives,
    library: &'a ComponentLibrary<'l>,
    unknown: Vec<(String, TextRange)>,
    unmounted: Vec<Unmounted>,
    persisting: Vec<(TextRange, String)>,
    misplaced: Vec<Unmounted>,
    instances: Vec<UiInstance>,
    /// Each component view being lowered, the mounted one first.
    frames: Vec<Frame>,
    /// The frame whose items are being lowered.
    current: usize,
    /// How many `for`/`match` regions enclose the items being lowered.
    depth: u32,
    /// Whether any control-flow region encloses the items being lowered.
    guarded: bool,
    /// How many unnamed nodes of each component each instance has inlined.
    ordinals: HashMap<(u32, SymbolId), u32>,
    /// Whether an unknown type names a component of the surrounding Rust
    /// scope, as in a `ui!` fragment.
    rust_scope: bool,
    /// The caller's `override part` bindings for the next node to lower, a
    /// part, and the instance they belong to.
    overriding: Option<(Vec<ViewItem>, u32)>,
    /// The `NodeRef` key of the named node being lowered, which the first
    /// retained node it lowers to takes.
    naming: Option<u32>,
}

/// One component view being lowered.
struct Frame {
    /// Its instance: `0` for the mounted component.
    instance: u32,
    /// The source unit that declares its component.
    unit: u32,
    /// The components being inlined into each other down to this one.
    chain: Vec<SymbolId>,
    /// What the caller fills each slot with.
    fills: Vec<(String, Vec<ViewItem>)>,
    /// The caller's `override part` bindings and handlers, by part.
    overrides: Vec<(String, Vec<ViewItem>)>,
    /// What the caller's `replace part` puts in each part's place.
    replaces: Vec<(String, Vec<ViewItem>)>,
    /// The caller's frame, and the regions enclosing the caller's node.
    caller: Option<(usize, u32)>,
}

impl<'a, 'l> Lowering<'a, 'l> {
    fn new(
        natives: &'a Natives,
        library: &'a ComponentLibrary<'l>,
        root: Option<SymbolId>,
    ) -> Self {
        Lowering {
            natives,
            library,
            unknown: Vec::new(),
            unmounted: Vec::new(),
            persisting: Vec::new(),
            misplaced: Vec::new(),
            instances: Vec::new(),
            frames: vec![Frame {
                instance: 0,
                unit: root
                    .and_then(|root| library.get(root))
                    .map_or(0, |c| c.unit),
                chain: root.into_iter().collect(),
                fills: Vec::new(),
                overrides: Vec::new(),
                replaces: Vec::new(),
                caller: None,
            }],
            current: 0,
            depth: 0,
            guarded: false,
            ordinals: HashMap::new(),
            rust_scope: false,
            overriding: None,
            naming: None,
        }
    }

    fn finish(self, items: Vec<UiItem>) -> LoweredView {
        LoweredView {
            tree: UiTree {
                items,
                instances: self.instances,
                unit: self.frames[0].unit,
            },
            unknown: self.unknown,
            unmounted: self.unmounted,
            persisting: self.persisting,
            misplaced: self.misplaced,
        }
    }

    /// The instance whose view is being lowered.
    fn instance(&self) -> u32 {
        self.frames[self.current].instance
    }

    /// The names and styles of the unit whose view is being lowered.
    fn names(&self) -> &'a LibraryUnit {
        self.library.unit(self.frames[self.current].unit)
    }

    /// `(at, reason)` as the mounted view's file reports it, with the call
    /// site of the instance whose view it is in: a span of another file is
    /// reported at the node of the mounted file that places the component it
    /// is in.
    fn report(&self, (at, reason): (TextRange, String)) -> Unmounted {
        let root = self.frames[0].unit;
        let unit_of = |instance: u32| match instance.checked_sub(1) {
            Some(i) => self.instances[i as usize].unit,
            None => root,
        };
        let mut instance = self.instance();
        if unit_of(instance) == root {
            let placed_at = self
                .instances
                .get(instance.wrapping_sub(1) as usize)
                .map(|i| i.origin);
            return Unmounted {
                at,
                reason,
                placed_at,
            };
        }
        loop {
            let placed = &self.instances[instance as usize - 1];
            if unit_of(placed.parent) == root {
                return Unmounted {
                    at: placed.origin,
                    reason: format!("in `{}`: {reason}", placed.name),
                    placed_at: None,
                };
            }
            instance = placed.parent;
        }
    }

    /// Reports `(at, reason)`: what does not mount and why.
    fn unmount(&mut self, report: (TextRange, String)) {
        let report = self.report(report);
        self.unmounted.push(report);
    }

    /// Reports `(at, reason)`: a grid template or child cell that names no
    /// cells and why.
    fn misplace(&mut self, report: (TextRange, String)) {
        let report = self.report(report);
        self.misplaced.push(report);
    }

    /// Places each child of a grid whose `grid` template is `grid` and that
    /// names an area into the area's cells, through `if`, `for` and `match`.
    fn place_areas(&mut self, grid: Option<&ui_ir::GridIr>, children: &mut [UiItem]) {
        for child in children {
            match child {
                UiItem::Node(node) => {
                    let Some(area) = node.style.area.take() else {
                        continue;
                    };
                    if matches!(node.style.place, Some(Placement::Grid(_))) {
                        self.misplace((
                            area.at,
                            "a `grid.area` places the node by itself; drop its `grid.column`, \
                             `grid.row` and spans"
                                .to_string(),
                        ));
                        continue;
                    }
                    let areas = grid.map_or(&[][..], |grid| &grid.areas);
                    match areas.iter().find(|(name, _)| *name == area.name) {
                        Some((_, place)) => node.style.place = Some(Placement::Grid(*place)),
                        None if areas.is_empty() => self.misplace((
                            area.at,
                            format!(
                                "`{}` names an area, but this grid declares no `areas`",
                                area.name
                            ),
                        )),
                        None => self.misplace((
                            area.at,
                            format!("`{}` names no area of this grid's `areas`", area.name),
                        )),
                    }
                }
                UiItem::If(region) => {
                    for arm in &mut region.arms {
                        self.place_areas(grid, &mut arm.items);
                    }
                }
                UiItem::For(region) => self.place_areas(grid, &mut region.body),
                UiItem::Match(region) => {
                    for arm in &mut region.arms {
                        self.place_areas(grid, &mut arm.items);
                    }
                }
            }
        }
    }

    /// Lowers `items` in order.
    fn items(&mut self, items: impl Iterator<Item = ViewItem>) -> Vec<UiItem> {
        let mut out = Vec::new();
        for item in items {
            self.item(item, &mut out);
        }
        out
    }

    /// Lowers one view item onto `out`. A property, handler, two-way binding or
    /// fill carries no mounted structure on its own: it only appears inside a
    /// node body, where [`Lowering::node`] consumes it.
    fn item(&mut self, item: ViewItem, out: &mut Vec<UiItem>) {
        match item {
            ViewItem::Named(node) => {
                let local_name = node.name().map(|t| t.text());
                self.naming = node.name().and_then(|name| self.node_ref(&name));
                self.node(
                    node.ty(),
                    local_name,
                    node.body(),
                    node.syntax().text_range(),
                    out,
                );
                self.naming = None;
            }
            ViewItem::Anonymous(node) => {
                self.node(
                    node.ty(),
                    None,
                    node.body(),
                    node.syntax().text_range(),
                    out,
                );
            }
            ViewItem::Part(part) => self.part(&part, out),
            ViewItem::Use(template) => self.template_use(&template, out),
            ViewItem::If(vi) => out.push(UiItem::If(self.lower_if(&vi))),
            ViewItem::For(vf) => out.push(UiItem::For(self.lower_for(&vf))),
            ViewItem::Match(vm) => out.push(UiItem::Match(self.lower_match(&vm))),
            ViewItem::Property(_)
            | ViewItem::Handler(_)
            | ViewItem::TwoWayBinding(_)
            | ViewItem::Fill(_)
            | ViewItem::Override(_)
            | ViewItem::Replace(_) => {}
        }
    }

    /// The `NodeRef` key of the named node `name` names, `None` for one in a
    /// `for` body, which its component reaches by no `NodeRef`.
    fn node_ref(&self, name: &crate::syntax::SyntaxToken) -> Option<u32> {
        self.names().heads.get(&name.text_range())?;
        let frame = &self.frames[self.current];
        let component = self.library.get(*frame.chain.last()?)?;
        let ordinal = component
            .decl
            .view()?
            .block()?
            .referable_nodes()
            .iter()
            .position(|node| node.name().is_some_and(|n| n.text() == name.text()))?;
        let ordinal = u32::try_from(ordinal).ok().filter(|&o| o < 1 << 16)?;
        (frame.instance < 1 << 15).then_some(frame.instance << 16 | ordinal)
    }

    /// Lowers one member of a widget's body onto its children: a `fill`'s items
    /// in place, since a widget's slots are all its children.
    fn child(&mut self, member: ViewItem, children: &mut Vec<UiItem>) {
        match member {
            ViewItem::Fill(fill) => {
                for item in fill.body().iter().flat_map(|b| b.items()) {
                    self.item(item, children);
                }
            }
            other => self.item(other, children),
        }
    }

    /// Lowers a node of type `ty` onto `out`: the retained node its widget
    /// declares, with static properties folded into style, reactive ones pending
    /// and child items lowered in order; a `Fragment`'s children directly; a
    /// component's view inlined.
    fn node(
        &mut self,
        ty: Option<TypePath>,
        local_name: Option<String>,
        body: Option<NodeBody>,
        origin: TextRange,
        out: &mut Vec<UiItem>,
    ) {
        let overriding = self.overriding.take();
        let Some(ty) = ty else { return };
        let type_name = type_name_of(&ty);
        let head = ty.segments().next().map(|t| t.text_range());
        if let Some(id) = head.and_then(|head| self.names().heads.get(&head).copied()) {
            if overriding.is_some() {
                self.unmount((
                    origin,
                    format!(
                        "an `override part` binds the properties of a widget part, and this part is a `{type_name}`; replace it instead"
                    ),
                ));
            }
            if self.library.get(id).is_some() {
                let members = body.iter().flat_map(|b| b.members()).collect();
                self.component(id, type_name, local_name, members, Vec::new(), origin, out);
            }
            return;
        }
        let Some(widget) = self.natives.widget(&type_name) else {
            self.unknown.push((type_name, ty.syntax().text_range()));
            if self.rust_scope {
                self.rust_component(&ty, local_name, body.as_ref(), origin, out);
            }
            return;
        };
        let mut style = StyleIr::default();
        let kind = match widget.node {
            WidgetNode::Flex(axis) => {
                style.axis = match axis {
                    FlexAxis::Row => Some(AxisIr::Row),
                    FlexAxis::Column => Some(AxisIr::Column),
                    FlexAxis::Property => None,
                };
                NodeKind::Flex
            }
            WidgetNode::AdaptiveScope => {
                style.axis = Some(AxisIr::Column);
                style.scope = Some(ScopeIr::default());
                NodeKind::Flex
            }
            WidgetNode::SafeArea | WidgetNode::KeyboardAvoiding => {
                style.axis = Some(AxisIr::Column);
                style.avoid = Some(match widget.node {
                    WidgetNode::SafeArea => Avoid::SafeArea,
                    _ => Avoid::Keyboard,
                });
                NodeKind::Flex
            }
            WidgetNode::Grid => NodeKind::Grid,
            WidgetNode::Stack => NodeKind::Stack,
            WidgetNode::Absolute => NodeKind::Absolute,
            WidgetNode::Scroll => NodeKind::Scroll,
            WidgetNode::VirtualList => NodeKind::VirtualList,
            WidgetNode::Leaf => NodeKind::Leaf,
            WidgetNode::Fragment => {
                for member in body.iter().flat_map(|b| b.members()) {
                    self.child(member, out);
                }
                return;
            }
            WidgetNode::Outlet => {
                self.outlet(body.as_ref(), origin, out);
                return;
            }
        };

        let node_ref = self.naming.take();
        let instance = self.instance();
        let control = ControlKind::of(&type_name);
        let mut pending = Vec::new();
        let mut handlers = Vec::new();
        // Write-backs lead the node's handlers, so an author's handler for the
        // same event sees the bound state already written.
        let mut write_backs = 0;
        let mut control_reads = Vec::new();
        let mut children = Vec::new();
        // Where the node's own `padding` is written, for the diagnostics.
        let mut padding_at = None;
        let mut role = None;
        let mut live = None;
        // Each binding left to a runtime value, by property and value span,
        // and where this file reports it.
        let mut bound = Vec::new();
        // The layout properties the node sets from values as they change.
        let mut laid: Vec<String> = Vec::new();
        let own: Vec<ViewItem> = body.iter().flat_map(|b| b.members()).collect();
        let styles = self.style_names(&type_name, &own);
        let styled = self.styled(
            &type_name,
            kind,
            &own,
            instance,
            (&mut style, &mut pending, &mut bound),
        );
        // The caller's overrides replace the node's own bindings of the same
        // property and its own handlers of the same event.
        let members: Vec<(ViewItem, u32)> = match overriding {
            Some((overrides, caller)) => {
                let rebinds = |m: &ViewItem| overrides.iter().any(|o| same_binding(o, m));
                own.into_iter()
                    .filter(|m| !rebinds(m))
                    .map(|m| (m, instance))
                    .chain(overrides.iter().cloned().map(|m| (m, caller)))
                    .collect()
            }
            None => own.into_iter().map(|m| (m, instance)).collect(),
        };
        for (member, instance) in members {
            match member {
                ViewItem::Property(prop) if dotted(prop.path()).as_deref() == Some("styles") => {}
                ViewItem::Property(prop) => {
                    let (Some(name), Some(value)) = (dotted(prop.path()), prop.value()) else {
                        continue;
                    };
                    let at = value.syntax().text_range();
                    if name == "padding" {
                        padding_at = Some(at);
                    }
                    if name == "semantics.role" {
                        match authored_role(&value) {
                            Ok(authored) => role = Some(authored),
                            Err(reason) => self.unmount((at, reason)),
                        }
                    }
                    if name == "semantics.live" {
                        match authored_live(&value) {
                            Ok(authored) => live = Some(authored),
                            Err(reason) => self.unmount((at, reason)),
                        }
                    }
                    if control.input(&name).is_some() {
                        control_reads.push((name.clone(), at, instance));
                    } else if let Some(member) = name.strip_prefix("transition.")
                        && widget
                            .group("transition")
                            .is_some_and(|g| g.member(member).is_some())
                    {
                        self.unmount((
                            at,
                            format!("`{name}` does not play yet; the transitions of `translate`, `scale`, `rotation`, `opacity`, `background`, `color`, `corner_radius`, `width` and `height` do"),
                        ));
                    }
                    match fold_property(
                        &prop,
                        (kind, &self.library.consts),
                        instance,
                        &mut style,
                        &mut pending,
                    ) {
                        Fold::Folded => {}
                        Fold::Pending if layout_slot(kind, &name).is_some() => {
                            laid.push(name.clone());
                            control_reads.push((name, at, instance));
                        }
                        Fold::Pending => bound.push((name, at, at)),
                        Fold::Unsupported(reason) => self.unmount((at, reason)),
                        Fold::Invalid(reason) => self.misplace((at, reason)),
                    }
                }
                ViewItem::Handler(h) => handlers.push(UiHandler {
                    event: h.event().map(|t| t.text()).unwrap_or_default(),
                    origin: h.syntax().text_range(),
                    instance,
                    capture: is_capture(&h),
                }),
                ViewItem::TwoWayBinding(bind) => {
                    let (Some(name), Some(source)) = (single_segment(bind.target()), bind.source())
                    else {
                        continue;
                    };
                    let Some(event) = widget.write_back(&name) else {
                        continue;
                    };
                    if control.input(&name).is_some() {
                        control_reads.push((name, source.syntax().text_range(), instance));
                    }
                    handlers.insert(
                        write_backs,
                        UiHandler {
                            event: event.name.to_string(),
                            origin: bind.syntax().text_range(),
                            instance,
                            capture: false,
                        },
                    );
                    write_backs += 1;
                }
                other => self.child(other, &mut children),
            }
        }
        if style.scope.is_some()
            && let Some(at) = pending.iter().position(|p| p.name == "basis")
        {
            self.unmount((
                pending.remove(at).value,
                "an adaptive scope's `basis` is a constant `dp` length".to_string(),
            ));
        }
        let pending_padding = pending.iter().position(|p| p.name == "padding");
        if style.padding.is_some() || style.lengths().padding.is_some() || pending_padding.is_some()
        {
            let reason = if style.avoid.is_some() {
                Some(format!(
                    "a `{type_name}`'s padding is the area it avoids; pad its content instead"
                ))
            } else if !kind.pads() {
                Some(format!(
                    "a `{type_name}` has no content box to pad; pad a container around it"
                ))
            } else {
                None
            };
            if let Some(reason) = reason {
                let at = pending_padding.map(|at| pending.remove(at).value);
                style.padding = None;
                if let Some(lengths) = &mut style.lengths {
                    lengths.padding = None;
                }
                let at = padding_at
                    .or(at)
                    .unwrap_or_else(|| ty.syntax().text_range());
                self.unmount((at, reason));
            }
        }
        for (name, value, at) in bound {
            // A binding a check above took out was reported there.
            if !pending.iter().any(|p| p.value == value) {
                continue;
            }
            if let Some(reason) = unapplied(&name, &type_name, control, styled.as_ref()) {
                self.unmount((at, reason));
            }
        }
        if kind == NodeKind::Grid {
            self.place_areas(style.grid.as_deref(), &mut children);
        }
        style.settle_rules(&laid_slots(&laid));
        out.push(UiItem::Node(Box::new(UiNode {
            type_name,
            local_name,
            node_ref,
            kind,
            style,
            pending,
            handlers,
            control_reads,
            styled,
            styles,
            role,
            live,
            children,
            origin,
            instance: self.instance(),
            migratable: widget.migratable,
        })));
    }

    /// What the styles of a native node of type `type_name`, whose body holds
    /// `members`, give it: their constant bindings folded into `style` (or
    /// left `pending`, as the node's own are), and the look the runtime
    /// delivers.
    fn styled(
        &mut self,
        type_name: &str,
        kind: NodeKind,
        members: &[ViewItem],
        instance: u32,
        (style, pending, bound): (&mut StyleIr, &mut Vec<PendingProperty>, &mut Vec<Bound>),
    ) -> Option<UiStyled> {
        let value = styles_value(members)?;
        let unit = self.frames[self.current].unit;
        let books = &self.library.styles;
        let uses = style::uses(books, unit as usize, &value).ok()?;
        let list = style::applying(uses, type_name);
        if list.is_empty() {
            return None;
        }
        let plan = style::plan(books, &list, &style::own_bindings(members), type_name);
        let styles_at = value.syntax().text_range();
        // A span of a style of another unit is reported at the node's
        // `styles`, as this unit places it.
        let place = |module: usize, at: TextRange| {
            if module == unit as usize {
                at
            } else {
                styles_at
            }
        };
        let mut fold = |this: &mut Self, styled: &style::Styled| -> Fold {
            let pendings = pending.len();
            let fold = fold_property(
                &styled.binding,
                (kind, &this.library.consts),
                instance,
                style,
                pending,
            );
            if styled.module != unit as usize {
                for p in &mut pending[pendings..] {
                    p.unit = Some(styled.module as u32);
                }
            }
            fold
        };
        for styled in &plan.constants {
            let binding = &styled.binding;
            let (Some(name), Some(value)) = (dotted(binding.path()), binding.value()) else {
                continue;
            };
            let value = value.syntax().text_range();
            let at = place(styled.module, value);
            match fold(self, styled) {
                Fold::Folded => {}
                Fold::Pending => bound.push((name, value, at)),
                Fold::Unsupported(reason) => self.unmount((at, reason)),
                Fold::Invalid(reason) => self.misplace((at, reason)),
            }
        }
        for (module, at, reason) in plan.unmounted {
            self.unmount((place(module, at), reason));
        }
        // A delivered property's base folds like the node's own too, so the
        // node is built with it; one that folds and that no arm switches
        // needs no delivery.
        let mut looks = Vec::with_capacity(plan.looks.len());
        for (look, base) in plan.looks {
            if let Some(styled) = &base
                && let Some(value) = styled.binding.value()
            {
                let at = place(styled.module, value.syntax().text_range());
                match fold(self, styled) {
                    Fold::Folded if look.arms.is_empty() => continue,
                    Fold::Folded | Fold::Pending => {}
                    Fold::Unsupported(reason) => self.unmount((at, reason)),
                    Fold::Invalid(reason) => self.misplace((at, reason)),
                }
            }
            looks.push(look);
        }
        (!looks.is_empty()).then_some(UiStyled {
            at: styles_at,
            looks,
        })
    }

    /// What the styles `value` of a node of the library component `component`,
    /// whose type is `type_name` and whose own bindings are `own`, give its
    /// inputs.
    fn styled_inputs(
        &mut self,
        component: &LibraryComponent<'_>,
        type_name: &str,
        value: &Expr,
        own: &[(String, style::Own)],
    ) -> Vec<UiStyledInput> {
        let unit = self.frames[self.current].unit;
        let books = &self.library.styles;
        let Ok(uses) = style::uses(books, unit as usize, value) else {
            return Vec::new();
        };
        let list = style::applying(uses, type_name);
        if list.is_empty() {
            return Vec::new();
        }
        let plan = style::component_plan(books, &list, own, &component.selectors);
        let at = value.syntax().text_range();
        for (module, span, reason) in plan.unmounted {
            let span = if module == unit as usize { span } else { at };
            self.unmount((span, reason));
        }
        let value_of = |part: u32| {
            plan.parts
                .iter()
                .find(|p| p.part == part)
                .and_then(|p| match &p.value {
                    style::PartValue::Look { value, module, .. } => {
                        Some((*module as u32, value.syntax().text_range()))
                    }
                    style::PartValue::Own(_) => None,
                })
        };
        let schema = component.schema;
        let member = |name: &str| {
            let input = schema
                .inputs
                .iter()
                .find(|i| i.name == name)
                .map(|i| &i.meta);
            let state = || {
                schema
                    .states
                    .iter()
                    .find(|s| s.name == name)
                    .map(|s| &s.meta)
            };
            let computed = || {
                schema
                    .computeds
                    .iter()
                    .find(|c| c.name == name)
                    .map(|c| &c.meta)
            };
            input.or_else(state).or_else(computed)?.resolved_symbol
        };
        let inputs = &schema.inputs;
        plan.inputs
            .iter()
            .filter_map(|input| {
                let slot = inputs.iter().position(|i| i.name == input.input)? as u32;
                let selects = input
                    .arms
                    .iter()
                    .flat_map(|(when, _)| when)
                    .filter_map(|step| match step {
                        UiMemberWhen::Member(name) => member(name),
                        _ => None,
                    })
                    .collect();
                let parts = input.base.into_iter().chain(input.arms.iter().map(|a| a.1));
                Some(UiStyledInput {
                    slot,
                    at,
                    base: input.base,
                    arms: input
                        .arms
                        .iter()
                        .map(|(when, part)| UiInputArm {
                            when: when.clone(),
                            part: *part,
                        })
                        .collect(),
                    values: parts.filter_map(value_of).collect(),
                    selects,
                })
            })
            .collect()
    }

    /// The names of the styles among `members`' `styles` that apply to a
    /// node of type `type_name`, in list order.
    fn style_names(&self, type_name: &str, members: &[ViewItem]) -> Vec<String> {
        let unit = self.frames[self.current].unit as usize;
        let Some(uses) =
            styles_value(members).and_then(|v| style::uses(&self.library.styles, unit, &v).ok())
        else {
            return Vec::new();
        };
        style::applying(uses, type_name)
            .iter()
            .filter_map(|style| style.decl.name())
            .map(|name| name.text().trim_start_matches("r#").to_owned())
            .collect()
    }

    /// A node of the Rust-scope component `ty`, which mounts through its own
    /// `build` and so takes nothing from the fragment.
    fn rust_component(
        &mut self,
        ty: &TypePath,
        local_name: Option<String>,
        body: Option<&NodeBody>,
        origin: TextRange,
        out: &mut Vec<UiItem>,
    ) {
        if let Some(member) = body.and_then(|b| b.members().next()) {
            self.unmount((
                member.syntax().text_range(),
                "a Rust component mounts its own view; it takes no properties, handlers or children here".to_string(),
            ));
        }
        let path: Vec<String> = ty.segments().map(|t| t.text()).collect();
        out.push(UiItem::Node(Box::new(UiNode {
            type_name: path.join("::"),
            local_name,
            node_ref: self.naming.take(),
            kind: NodeKind::Component,
            style: StyleIr::default(),
            pending: Vec::new(),
            handlers: Vec::new(),
            control_reads: Vec::new(),
            styled: None,
            styles: Vec::new(),
            role: None,
            live: None,
            children: Vec::new(),
            origin,
            instance: self.instance(),
            migratable: MigratableState::NONE,
        })));
    }

    /// A `SlotOutlet`: the items the caller fills the slot with, lowered in
    /// the caller's view; nothing in the mounted component, which no caller
    /// fills.
    fn outlet(&mut self, body: Option<&NodeBody>, origin: TextRange, out: &mut Vec<UiItem>) {
        let Some((caller, depth)) = self.frames[self.current].caller else {
            return;
        };
        let name = body
            .into_iter()
            .flat_map(|b| b.members())
            .find_map(|m| match m {
                ViewItem::Property(p)
                    if p.path().is_some_and(|path| {
                        path.segments().map(|t| t.text()).collect::<Vec<_>>() == ["slot"]
                    }) =>
                {
                    p.value().as_ref().and_then(path_ident)
                }
                _ => None,
            });
        let Some(name) = name else { return };
        let Some(items) = self.frames[self.current]
            .fills
            .iter()
            .find(|(slot, _)| *slot == name)
            .map(|(_, items)| items.clone())
        else {
            return;
        };
        if self.depth != depth {
            self.unmount((
                origin,
                format!(
                    "the slot `{name}` is placed inside a `for` or `match` of the component, which its caller's items cannot run in"
                ),
            ));
            return;
        }
        let callee = std::mem::replace(&mut self.current, caller);
        for item in items {
            self.item(item, out);
        }
        self.current = callee;
    }

    /// A `part` node: what the caller's `replace part` puts in its place,
    /// lowered in the caller's view, else the node with the caller's
    /// `override part` bindings.
    fn part(&mut self, part: &crate::ast::PartNode, out: &mut Vec<UiItem>) {
        let name = part
            .name()
            .map(|t| t.text().trim_start_matches("r#").to_string());
        let origin = part.syntax().text_range();
        let frame = &self.frames[self.current];
        let replaced = name
            .as_ref()
            .and_then(|n| frame.replaces.iter().find(|(p, _)| p == n))
            .map(|(_, items)| items.clone());
        let overrides = name
            .as_ref()
            .and_then(|n| frame.overrides.iter().find(|(p, _)| p == n))
            .map(|(_, items)| items.clone());
        if replaced.is_some() || overrides.is_some() {
            let Some((caller, depth)) = frame.caller else {
                return;
            };
            if self.depth != depth {
                self.unmount((
                    origin,
                    format!(
                        "the part `{}` is inside a `for` or `match`, which its caller's `override`/`replace` cannot run in",
                        name.clone().unwrap_or_default()
                    ),
                ));
            } else if let Some(items) = replaced {
                let callee = std::mem::replace(&mut self.current, caller);
                for item in items {
                    self.item(item, out);
                }
                self.current = callee;
                return;
            } else {
                self.overriding = overrides.map(|o| (o, self.frames[caller].instance));
            }
        }
        self.node(part.ty(), name, part.body(), origin, out);
        self.overriding = None;
    }

    /// A `use` of the library template the use names, inlined like a
    /// component node whose parameters its arguments give, by position or
    /// name.
    fn template_use(&mut self, u: &crate::ast::TemplateUse, out: &mut Vec<UiItem>) {
        let Some(ty) = u.ty() else { return };
        let type_name = type_name_of(&ty);
        let Some(head) = ty.segments().next() else {
            return;
        };
        let Some(id) = self.names().heads.get(&head.text_range()).copied() else {
            return;
        };
        let Some(template) = self.library.get(id) else {
            return;
        };
        let inputs = &template.schema.inputs;
        let mut args = Vec::new();
        let mut next = 0;
        for (label, value) in u.args() {
            let slot = match label {
                Some(label) => {
                    let text = label.text();
                    let name = text.trim_start_matches("r#");
                    inputs.iter().position(|i| i.name == name)
                }
                None => {
                    next += 1;
                    (next <= inputs.len()).then_some(next - 1)
                }
            };
            if let Some(slot) = slot {
                args.push((slot as u32, value.syntax().text_range()));
            }
        }
        let members = u.body().iter().flat_map(|b| b.members()).collect();
        let origin = u.syntax().text_range();
        self.component(id, type_name, None, members, args, origin, out);
    }

    /// Inlines a node of the library component `id`, whose body holds
    /// `members`, onto `out`; `args` gives inputs ahead of the members.
    #[allow(clippy::too_many_arguments)]
    fn component(
        &mut self,
        id: SymbolId,
        type_name: String,
        local_name: Option<String>,
        members: Vec<ViewItem>,
        mut args: Vec<(u32, TextRange)>,
        origin: TextRange,
        out: &mut Vec<UiItem>,
    ) {
        let library = self.library;
        let Some(component) = library.get(id) else {
            return;
        };
        let schema = component.schema;
        if component.persists {
            self.persisting.push((origin, type_name.clone()));
        }
        if self.frames[self.current].chain.contains(&id) {
            self.unmount((
                origin,
                format!("`{type_name}` mounts itself, which would never end"),
            ));
            return;
        }
        let parent = self.instance();
        let mut handlers = Vec::new();
        let mut overrides: Vec<(String, Vec<ViewItem>)> = Vec::new();
        let mut replaces: Vec<(String, Vec<ViewItem>)> = Vec::new();
        let mut forwarded = Vec::new();
        let mut forwarded_handlers = Vec::new();
        let mut fills: Vec<(String, Vec<ViewItem>)> = Vec::new();
        let default_slot = schema
            .slots
            .iter()
            .find(|s| s.default)
            .map(|s| s.name.clone());
        let input_slot = |name: &str| schema.inputs.iter().position(|i| i.name == name);
        let styles = styles_value(&members);
        let own = style::own_bindings(&members);
        for member in members {
            match member {
                ViewItem::Override(o) => {
                    let name = o.name().map(|t| t.text()).unwrap_or_default();
                    overrides.push((
                        name.trim_start_matches("r#").to_string(),
                        o.members().collect(),
                    ));
                }
                ViewItem::Replace(r) => {
                    let name = r.name().map(|t| t.text()).unwrap_or_default();
                    replaces.push((
                        name.trim_start_matches("r#").to_string(),
                        r.body().iter().flat_map(|b| b.items()).collect(),
                    ));
                }
                ViewItem::Property(prop) => {
                    let segments: Vec<String> = prop
                        .path()
                        .map(|p| p.segments().map(|t| t.text()).collect())
                        .unwrap_or_default();
                    if segments == ["styles"] {
                        continue;
                    }
                    let slot = match segments.as_slice() {
                        [name] => input_slot(name.trim_start_matches("r#")),
                        _ => None,
                    };
                    match (slot, prop.value()) {
                        (Some(slot), Some(value)) => {
                            args.push((slot as u32, value.syntax().text_range()))
                        }
                        (Some(_), None) => {}
                        (None, _) => forwarded.push(prop),
                    }
                }
                ViewItem::Handler(h) => {
                    let event = h.event().map(|t| t.text()).unwrap_or_default();
                    let event = event.trim_start_matches("r#");
                    if schema.events.iter().any(|e| e.name == event) {
                        handlers.push((event.to_string(), h.syntax().text_range()));
                    } else {
                        forwarded_handlers.push(h);
                    }
                }
                ViewItem::TwoWayBinding(bind) => {
                    if bind.using_ty().is_some() {
                        self.unmount((
                            bind.syntax().text_range(),
                            "a `bind` to a component input writes back as is and takes no `using` converter".to_string(),
                        ));
                        continue;
                    }
                    let target: Vec<String> = bind
                        .target()
                        .map(|p| p.segments().map(|t| t.text()).collect())
                        .unwrap_or_default();
                    let Some(slot) = (match target.as_slice() {
                        [name] => input_slot(name.trim_start_matches("r#")),
                        _ => None,
                    }) else {
                        continue;
                    };
                    let Some((_, event)) = component
                        .write_backs
                        .iter()
                        .find(|(input, _)| *input == slot)
                    else {
                        continue;
                    };
                    if let Some(source) = bind.source() {
                        args.push((slot as u32, source.syntax().text_range()));
                        handlers.push((event.clone(), bind.syntax().text_range()));
                    }
                }
                ViewItem::Fill(fill) => {
                    let name = fill.name().map(|t| t.text()).unwrap_or_default();
                    let items = fill.body().iter().flat_map(|b| b.items()).collect();
                    fills.push((name.trim_start_matches("r#").to_string(), items));
                }
                item => {
                    if let Some(slot) = &default_slot {
                        match fills.iter_mut().find(|(name, _)| name == slot) {
                            Some((_, items)) => items.push(item),
                            None => fills.push((slot.clone(), vec![item])),
                        }
                    }
                }
            }
        }

        let segment = match &local_name {
            Some(name) => name.clone(),
            None => {
                let ordinal = self.ordinals.entry((parent, id)).or_insert(0);
                let segment = format!("{type_name}#{ordinal}");
                *ordinal += 1;
                segment
            }
        };
        let identity = match self.instances.get(parent.wrapping_sub(1) as usize) {
            Some(parent) => format!("{}/{segment}", parent.identity),
            None => segment,
        };
        let styled = match &styles {
            Some(value) => self.styled_inputs(component, &type_name, value, &own),
            None => Vec::new(),
        };
        self.instances.push(UiInstance {
            component: id,
            name: type_name.clone(),
            origin,
            parent,
            unit: component.unit,
            identity,
            depth: self.depth,
            regional: self.guarded,
            args,
            handlers,
            styled,
        });
        let instance = self.instances.len() as u32;
        let mut chain = self.frames[self.current].chain.clone();
        chain.push(id);
        self.frames.push(Frame {
            instance,
            unit: component.unit,
            chain,
            fills,
            overrides,
            replaces,
            caller: Some((self.current, self.depth)),
        });
        let caller = std::mem::replace(&mut self.current, self.frames.len() - 1);
        let mut inner = match component.decl.view().and_then(|v| v.block()) {
            Some(block) => self.items(block.items()),
            None => Vec::new(),
        };
        self.current = caller;

        if !forwarded.is_empty() || !forwarded_handlers.is_empty() {
            match inner.as_mut_slice() {
                [UiItem::Node(root)] => {
                    let control = ControlKind::of(&root.type_name);
                    for prop in &forwarded {
                        let (Some(name), Some(value)) = (dotted(prop.path()), prop.value()) else {
                            continue;
                        };
                        let at = value.syntax().text_range();
                        if control.input(&name).is_some() {
                            root.control_reads.push((name.clone(), at, parent));
                        }
                        let fold =
                            fold_property(
                            prop,
                            (root.kind, &library.consts),
                            parent,
                            &mut root.style,
                            &mut root.pending,
                        );
                        if matches!(fold, Fold::Pending) && layout_slot(root.kind, &name).is_some() {
                            root.control_reads.push((name.clone(), at, parent));
                            continue;
                        }
                        let reason = match fold {
                            Fold::Folded if name == "padding" && !root.kind.pads() => Some(format!(
                                "a `{}` has no content box to pad; pad a container around it",
                                root.type_name
                            )),
                            Fold::Folded => None,
                            Fold::Pending => unapplied(&name, &root.type_name, control, root.styled.as_ref()),
                            Fold::Unsupported(reason) => Some(reason),
                            Fold::Invalid(reason) => {
                                self.misplace((at, reason));
                                None
                            }
                        };
                        if let Some(reason) = reason {
                            self.unmount((at, reason));
                        }
                    }
                    let laid: Vec<String> = root
                        .control_reads
                        .iter()
                        .filter(|(name, ..)| layout_slot(root.kind, name).is_some())
                        .map(|(name, ..)| name.clone())
                        .collect();
                    root.style.unsettle_rules();
                    root.style.unsettle_insets();
                    root.style.settle_rules(&laid_slots(&laid));
                    for h in &forwarded_handlers {
                        root.handlers.push(UiHandler {
                            event: h.event().map(|t| t.text()).unwrap_or_default(),
                            origin: h.syntax().text_range(),
                            instance: parent,
                            capture: is_capture(h),
                        });
                    }
                }
                _ => self.unmount((
                    origin,
                    format!(
                        "the properties and standard handlers of `{type_name}` apply to its view's root node, but its view does not mount exactly one node"
                    ),
                )),
            }
        }
        out.extend(inner);
    }
}

/// Why the binding of property `name` on a native node of type `type_name`
/// reaches no path that applies it, or `None` when one does:
/// the control values `control` delivers, the role and live setting the
/// node is built with, the transitions that play (the others are reported
/// where they are read), and the node's own `checked` and `invalid` its
/// styles' selectors read.
fn unapplied(
    name: &str,
    type_name: &str,
    control: ControlKind,
    styled: Option<&UiStyled>,
) -> Option<String> {
    let selected = || {
        styled.is_some_and(|styled| {
            styled
                .looks
                .iter()
                .flat_map(|look| &look.arms)
                .any(|arm| arm.when.iter().any(|w| matches!(w, UiWhen::Part(_))))
        })
    };
    if control.input(name).is_some() || name.starts_with("transition.") {
        return None;
    }
    match name {
        "semantics.role" | "semantics.live" => None,
        "checked" | "invalid" | "expanded" if selected() => None,
        "areas" | "grid.area" => Some(format!(
            "a `{name}` that reads a value does not lay out yet; a constant one does"
        )),
        _ => Some(format!("`{name}` does not reach a `{type_name}` yet")),
    }
}

/// The layout slot the binding of property `name` on a node of `kind` sets
/// from its value as it changes; `None` for one no such node lays out by.
fn layout_slot(kind: NodeKind, name: &str) -> Option<LayoutSlot> {
    let slot = LayoutSlot::of(name)?;
    let applies = match slot {
        LayoutSlot::Width
        | LayoutSlot::Height
        | LayoutSlot::MinWidth
        | LayoutSlot::MaxWidth
        | LayoutSlot::MinHeight
        | LayoutSlot::MaxHeight
        | LayoutSlot::Margin => kind != NodeKind::Component,
        LayoutSlot::Padding => kind.pads(),
        LayoutSlot::Gap => matches!(kind, NodeKind::Flex | NodeKind::Grid),
        LayoutSlot::Axis | LayoutSlot::Justify | LayoutSlot::Align => kind == NodeKind::Flex,
        LayoutSlot::ColumnGap
        | LayoutSlot::RowGap
        | LayoutSlot::Columns
        | LayoutSlot::Rows
        | LayoutSlot::AutoRows
        | LayoutSlot::AlignItems
        | LayoutSlot::AdaptiveColumns => kind == NodeKind::Grid,
        LayoutSlot::ContentAlign => kind == NodeKind::Stack,
        // The parent's kind was checked where the property was typed.
        LayoutSlot::GridColumn
        | LayoutSlot::GridRow
        | LayoutSlot::GridColumnSpan
        | LayoutSlot::GridRowSpan
        | LayoutSlot::StackAlign
        | LayoutSlot::StackLayer
        | LayoutSlot::AbsoluteTop
        | LayoutSlot::AbsoluteEnd
        | LayoutSlot::AbsoluteBottom
        | LayoutSlot::AbsoluteStart
        | LayoutSlot::FontSize => kind != NodeKind::Component,
        LayoutSlot::LayoutDirection => kind == NodeKind::Flex,
    };
    applies.then_some(slot)
}

/// What the layout properties `laid` a node sets from values keep in its
/// lengths: its box rules when one of them is, its insets when one of them
/// is, so a value that changes folds with the constant others.
fn laid_slots(laid: &[String]) -> Laid {
    let slots = || laid.iter().filter_map(|name| LayoutSlot::of(name));
    Laid {
        rules: slots().any(LayoutSlot::is_rule),
        insets: slots().any(LayoutSlot::is_inset),
    }
}

/// Whether the override `o` rebinds what the node's own member `own` binds:
/// the same property (by `:` or `bind`) or a handler of the same event.
fn same_binding(o: &ViewItem, own: &ViewItem) -> bool {
    let bound = |m: &ViewItem| match m {
        ViewItem::Property(p) => dotted(p.path()).map(|n| (0, n)),
        ViewItem::TwoWayBinding(b) => dotted(b.target()).map(|n| (0, n)),
        ViewItem::Handler(h) => h
            .event()
            .map(|t| (1, t.text().trim_start_matches("r#").to_string())),
        _ => None,
    };
    bound(o).is_some_and(|o| bound(own) == Some(o))
}

/// A property path's dotted text (`opacity`, `transition.opacity`).
fn dotted(path: Option<PropertyPath>) -> Option<String> {
    let segments: Vec<String> = path?
        .segments()
        .map(|t| t.text().trim_start_matches("r#").to_string())
        .collect();
    (!segments.is_empty()).then(|| segments.join("."))
}

/// The one segment of a property path, `None` for a longer path.
fn single_segment(path: Option<PropertyPath>) -> Option<String> {
    let path = path?;
    let mut segments = path.segments();
    let first = segments.next()?.text();
    segments
        .next()
        .is_none()
        .then(|| first.trim_start_matches("r#").to_string())
}

/// Folds one property binding: a compile-time-constant value updates [`StyleIr`];
/// anything else becomes a [`PendingProperty`] for the Binding IR pass.
fn fold_property(
    prop: &PropertyBinding,
    (kind, consts): (NodeKind, &Consts),
    instance: u32,
    style: &mut StyleIr,
    pending: &mut Vec<PendingProperty>,
) -> Fold {
    let Some(path) = prop.path() else {
        return Fold::Folded;
    };
    // The bound property is the path's leading segment (`width`, `axis`, `text`).
    let Some(name) = path.segments().next().map(|t| t.text()) else {
        return Fold::Folded;
    };
    let Some(value) = prop.value() else {
        return Fold::Folded;
    };
    let mut segments = path.segments().skip(1);
    let fold = match (segments.next(), segments.next()) {
        (None, _) => fold_static(&name, kind, &value, consts, style),
        (Some(member), None) if name == "grid" => fold_cell(&member.text(), &value, consts, style),
        (Some(member), None) if name == "stack" => {
            fold_stacked(&member.text(), &value, consts, style)
        }
        (Some(member), None) if name == "absolute" => {
            fold_inset(&member.text(), &value, consts, style)
        }
        _ => Fold::Pending,
    };
    if let Fold::Pending = fold {
        pending.push(PendingProperty::new(
            name,
            value.syntax().text_range(),
            instance,
        ));
    }
    fold
}

/// A binding left to a runtime value: the property, the value's span, and
/// where the mounted file reports it.
type Bound = (String, TextRange, TextRange);

/// What lowering a property binding made of it.
#[derive(Debug)]
enum Fold {
    /// A constant folded into the node's style.
    Folded,
    /// Left to a binding: the value is not a constant, or the property is
    /// not one the style models.
    Pending,
    /// A constant of a property the style models that layout does not take
    /// yet, and why.
    Unsupported(String),
    /// A grid template or child cell that names no cells, and why.
    Invalid(String),
}

/// Folds the `grid.member` binding of a grid's child into its cell.
fn fold_cell(member: &str, value: &Expr, consts: &Consts, style: &mut StyleIr) -> Fold {
    let cell = match grid::fold_cell(member, value, consts) {
        Some(Ok(cell)) => cell,
        Some(Err(reason)) => return Fold::Invalid(reason),
        None => return Fold::Pending,
    };
    match cell {
        grid::Cell::Column(column) => style.cell_mut().column = column,
        grid::Cell::Row(row) => style.cell_mut().row = row,
        grid::Cell::ColumnSpan(span) => style.cell_mut().column_span = span,
        grid::Cell::RowSpan(span) => style.cell_mut().row_span = span,
        grid::Cell::Area(area) => {
            style.area = area.map(|name| {
                Box::new(ui_ir::AreaRef {
                    name,
                    at: value.syntax().text_range(),
                })
            });
        }
    }
    style.settle_place();
    Fold::Folded
}

/// Folds the `stack.member` binding of a stack's child into its place.
fn fold_stacked(member: &str, value: &Expr, consts: &Consts, style: &mut StyleIr) -> Fold {
    let Some(stacked) = place::fold_stacked(member, value, consts) else {
        return Fold::Pending;
    };
    let placed: &mut StackPlacement = style.stacked_mut();
    match stacked {
        place::Stacked::Align(align) => placed.align = align,
        place::Stacked::Layer(layer) => placed.layer = layer,
    }
    style.settle_place();
    Fold::Folded
}

/// Folds the `absolute.member` binding of an absolute child into its
/// insets.
fn fold_inset(member: &str, value: &Expr, consts: &Consts, style: &mut StyleIr) -> Fold {
    if !matches!(member, "top" | "end" | "bottom" | "start") {
        return Fold::Pending;
    }
    let Some(terms) = place::fold_inset(value, consts) else {
        return Fold::Pending;
    };
    let insets = style.insets_mut();
    *match member {
        "top" => &mut insets.top,
        "end" => &mut insets.end,
        "bottom" => &mut insets.bottom,
        _ => &mut insets.start,
    } = terms;
    Fold::Folded
}

/// The value of the `styles` property among a node's `members`.
fn styles_value(members: &[ViewItem]) -> Option<Expr> {
    members.iter().find_map(|m| match m {
        ViewItem::Property(p) if dotted(p.path()).as_deref() == Some("styles") => p.value(),
        _ => None,
    })
}

/// Folds a constant property value into static style, when the property is
/// one the style models and the value is a constant it takes.
fn fold_static(
    name: &str,
    kind: NodeKind,
    value: &Expr,
    consts: &Consts,
    style: &mut StyleIr,
) -> Fold {
    let named = consts.value_of(value);
    let value = named.as_ref().unwrap_or(value);
    let folded = |done: bool| if done { Fold::Folded } else { Fold::Pending };
    match name {
        "width" | "height" => {
            let lowered = match length::fold_sizing(value, consts) {
                Some(Ok(lowered)) => lowered,
                Some(Err(reason)) => return Fold::Unsupported(reason),
                None => return Fold::Pending,
            };
            let (layout, env) = match lowered {
                Lowered::Layout(len) => (Some(len), None),
                Lowered::Env(terms) => (None, Some(terms)),
            };
            if name == "width" {
                style.width = layout;
                if env.is_some() || style.lengths.is_some() {
                    style.lengths_mut().width = env;
                }
            } else {
                style.height = layout;
                if env.is_some() || style.lengths.is_some() {
                    style.lengths_mut().height = env;
                }
            }
            Fold::Folded
        }
        "gap" | "spacing" => folded(match length::fold_gap(value, consts) {
            Some(Lowered::Layout(LengthIr::Fixed(n))) => {
                style.gap = Some(n);
                if let Some(lengths) = &mut style.lengths {
                    lengths.gap = None;
                }
                true
            }
            Some(Lowered::Env(terms)) => {
                style.gap = None;
                style.lengths_mut().gap = Some(terms);
                true
            }
            _ => false,
        }),
        "min_width" | "max_width" | "min_height" | "max_height" => {
            let Some(bound) = length::fold_bound(value, consts, name.starts_with("max")) else {
                return Fold::Pending;
            };
            style.unsettle_rules();
            let lengths = style.lengths_mut();
            *match name {
                "min_width" => &mut lengths.min_width,
                "max_width" => &mut lengths.max_width,
                "min_height" => &mut lengths.min_height,
                _ => &mut lengths.max_height,
            } = bound;
            Fold::Folded
        }
        "margin" => folded(match length::fold_margin(value, consts) {
            Some(edges) => {
                style.unsettle_rules();
                style.lengths_mut().margin = Some(edges);
                true
            }
            None => false,
        }),
        "padding" => folded(match length::fold_padding(value, consts) {
            Some(padding) => {
                style.unsettle_rules();
                match padding {
                    Padding::Fixed(inset) => {
                        style.padding = Some(inset);
                        if let Some(lengths) = &mut style.lengths {
                            lengths.padding = None;
                        }
                    }
                    Padding::Env(edges) => {
                        style.padding = None;
                        style.lengths_mut().padding = Some(edges);
                    }
                }
                true
            }
            None => false,
        }),
        "font_size" => folded(match length::fold_font_size(value, consts) {
            Some(terms) => {
                style.lengths_mut().font_size = Some(terms);
                true
            }
            None => false,
        }),
        "basis" if style.scope.is_some() => folded(match length::fold_gap(value, consts) {
            Some(Lowered::Layout(LengthIr::Fixed(n))) => {
                style.scope.get_or_insert_default().basis = Some(n);
                true
            }
            _ => false,
        }),
        "layout_direction" if style.scope.is_some() => folded(match fold_direction(value) {
            Some(direction) => {
                style.scope.get_or_insert_default().direction = direction;
                true
            }
            None => false,
        }),
        "axis" | "direction" => folded(match fold_axis(value) {
            Some(axis) => {
                style.axis = Some(axis);
                true
            }
            None => false,
        }),
        "justify" if kind == NodeKind::Flex => {
            style.justify = Some(match enum_constant(value, "Justify").as_deref() {
                Some("start") => Justify::Start,
                Some("center") => Justify::Center,
                Some("end") => Justify::End,
                _ => return Fold::Pending,
            });
            Fold::Folded
        }
        "align" if kind == NodeKind::Flex => {
            style.align = Some(match enum_constant(value, "Align").as_deref() {
                Some("start") => Align::Start,
                Some("center") => Align::Center,
                Some("end") => Align::End,
                Some("stretch") => Align::Stretch,
                _ => return Fold::Pending,
            });
            Fold::Folded
        }
        "content_align" if kind == NodeKind::Stack => {
            folded(match place::fold_alignment(value, consts) {
                Some(align) => {
                    style.content_align = Some(align);
                    true
                }
                None => false,
            })
        }
        "estimated_extent" if kind == NodeKind::VirtualList => {
            folded(match length::fold_terms(value, consts) {
                Some(estimate) if estimate.pct == 0.0 => {
                    style.list.get_or_insert_with(ListIr::default).estimate = estimate;
                    true
                }
                _ => false,
            })
        }
        "overscan" if kind == NodeKind::VirtualList => {
            folded(match length::fold_number(value, consts) {
                Some(n) if n >= 0.0 && n.fract() == 0.0 && n <= 1024.0 => {
                    style.list.get_or_insert_with(ListIr::default).overscan = n as u32;
                    true
                }
                _ => false,
            })
        }
        _ if kind == NodeKind::Grid => fold_grid(name, value, consts, style),
        _ => Fold::Pending,
    }
}

/// Folds a constant grid template property into a grid's style.
fn fold_grid(name: &str, value: &Expr, consts: &Consts, style: &mut StyleIr) -> Fold {
    let issue = |fold: Option<Result<(), String>>| match fold {
        Some(Ok(())) => Fold::Folded,
        Some(Err(reason)) => Fold::Unsupported(reason),
        None => Fold::Pending,
    };
    match name {
        "columns" | "rows" => issue(grid::fold_tracks(value, consts).map(|tracks| {
            let tracks = tracks?;
            let grid = style.grid_mut();
            if name == "columns" {
                grid.columns = tracks;
            } else {
                grid.rows = tracks;
            }
            Ok(())
        })),
        "auto_rows" => issue(grid::fold_track(value, consts).map(|track| {
            style.grid_mut().auto_rows = Some(track?);
            Ok(())
        })),
        "column_gap" | "row_gap" => issue(grid::fold_gap(value, consts).map(|gap| {
            let (fixed, env) = match gap? {
                Lowered::Layout(LengthIr::Fixed(n)) => (Some(n), None),
                Lowered::Env(terms) => (None, Some(terms)),
                Lowered::Layout(_) => return Ok(()),
            };
            let columns = name == "column_gap";
            let grid = style.grid_mut();
            *if columns {
                &mut grid.column_gap
            } else {
                &mut grid.row_gap
            } = fixed;
            if env.is_some() || style.lengths.is_some() {
                let lengths = style.lengths_mut();
                *if columns {
                    &mut lengths.gap
                } else {
                    &mut lengths.row_gap
                } = env;
            }
            Ok(())
        })),
        "align_items" => issue(grid::fold_align_items(value, consts).map(|align| {
            style.grid_mut().align_items = Some(align);
            Ok(())
        })),
        "adaptive_columns" => issue(grid::fold_adaptive(value, consts).map(|adaptive| {
            style.grid_mut().adaptive_columns = adaptive?;
            Ok(())
        })),
        "areas" => match grid::fold_areas(value, consts) {
            Some(Ok(areas)) => {
                style.grid_mut().areas = areas;
                Fold::Folded
            }
            Some(Err(reason)) => Fold::Invalid(reason),
            None => Fold::Pending,
        },
        _ => Fold::Pending,
    }
}

/// The variant a constant `Owner::variant` path names, for enum `owner`.
fn enum_constant(value: &Expr, owner: &str) -> Option<String> {
    let path = PathExpr::cast(value.syntax().clone())?;
    let segments: Vec<String> = path.segments().map(|t| t.text()).collect();
    match segments.as_slice() {
        [head, variant] if head == owner => Some(variant.clone()),
        _ => None,
    }
}

/// Folds a constant `Option<LayoutDirection>`: `LayoutDirection::ltr` or
/// `::rtl`, or `None` for the parent's.
fn fold_direction(value: &Expr) -> Option<Option<viso_schema::LayoutDirection>> {
    use viso_schema::LayoutDirection;
    if path_ident(value).as_deref() == Some("None") {
        return Some(None);
    }
    match enum_constant(value, "LayoutDirection")?.as_str() {
        "ltr" => Some(Some(LayoutDirection::Ltr)),
        "rtl" => Some(Some(LayoutDirection::Rtl)),
        _ => None,
    }
}

/// Folds an expression into an [`AxisIr`]: a constant `Axis::row` or
/// `Axis::column`, or the bare `row`/`column` identifier.
fn fold_axis(value: &Expr) -> Option<AxisIr> {
    let variant = enum_constant(value, "Axis").or_else(|| path_ident(value))?;
    match variant.as_str() {
        "row" => Some(AxisIr::Row),
        "column" | "col" => Some(AxisIr::Column),
        _ => None,
    }
}

/// The live setting a `semantics.live` value names: a constant
/// `LiveRegion::<variant>`.
fn authored_live(value: &Expr) -> Result<viso_schema::LiveRegion, String> {
    let segments: Vec<String> = PathExpr::cast(value.syntax().clone())
        .map(|path| path.segments().map(|t| t.text()).collect())
        .unwrap_or_default();
    match segments.as_slice() {
        [owner, variant] if owner == "LiveRegion" => viso_schema::view::source_live(variant)
            .ok_or_else(|| format!("`LiveRegion` has no variant `{variant}`")),
        _ => Err("`semantics.live` is a constant `LiveRegion::…`".to_string()),
    }
}

/// The role a `semantics.role` value names: a constant `Role::<variant>`.
fn authored_role(value: &Expr) -> Result<viso_schema::Role, String> {
    let segments: Vec<String> = PathExpr::cast(value.syntax().clone())
        .map(|path| path.segments().map(|t| t.text()).collect())
        .unwrap_or_default();
    let [owner, variant] = segments.as_slice() else {
        return Err("`semantics.role` is a constant `Role::…`".to_string());
    };
    if owner != "Role" {
        return Err("`semantics.role` is a constant `Role::…`".to_string());
    }
    viso_schema::view::source_role(variant).ok_or_else(|| {
        format!(
            "`Role` has no variant `{variant}`; it is one of {}",
            viso_schema::view::SOURCE_ROLES.join(", ")
        )
    })
}

/// The single identifier a path expression names (`row`, `fill`), or `None` when
/// the expression is not a bare one-segment path.
fn path_ident(value: &Expr) -> Option<String> {
    let path = PathExpr::cast(value.syntax().clone())?;
    let mut segs = path.segments();
    let first = segs.next()?;
    if segs.next().is_some() {
        return None;
    }
    Some(first.text())
}

/// The type name a [`TypePath`] denotes — its last segment (`ui::Text` → `Text`).
fn type_name_of(ty: &TypePath) -> String {
    ty.segments().last().map(|t| t.text()).unwrap_or_default()
}

impl Lowering<'_, '_> {
    /// Lowers an `if / else if / else` view region into a [`UiIf`].
    fn lower_if(&mut self, vi: &ViewIf) -> UiIf {
        let mut arms = Vec::new();
        let guarded = std::mem::replace(&mut self.guarded, true);
        self.collect_if_arms(vi, &mut arms);
        self.guarded = guarded;
        UiIf {
            arms,
            origin: vi.syntax().text_range(),
            instance: self.instance(),
        }
    }

    /// Walks an `if`/`else if`/`else` chain into flat arms, each with its condition
    /// span (or `None` for the trailing `else`) and mounted items.
    fn collect_if_arms(&mut self, vi: &ViewIf, arms: &mut Vec<UiIfArm>) {
        let condition = vi.condition().map(|e| e.syntax().text_range());
        let items = vi
            .then_block()
            .map(|b| self.block_items(b))
            .unwrap_or_default();
        arms.push(UiIfArm {
            condition,
            preserve: vi.preserve_name(),
            items,
        });

        match vi.else_branch() {
            Some(crate::ast::ElseBranch::If(nested)) => self.collect_if_arms(&nested, arms),
            Some(crate::ast::ElseBranch::Block(block)) => arms.push(UiIfArm {
                condition: None,
                preserve: None,
                items: self.block_items(block),
            }),
            None => {}
        }
    }

    /// Lowers a `for pattern in iterable key key { ... }` region into a [`UiFor`].
    fn lower_for(&mut self, vf: &ViewFor) -> UiFor {
        let body = self.enclosed(|l| vf.body().map(|b| l.block_items(b)).unwrap_or_default());
        UiFor {
            binding: vf
                .pattern()
                .and_then(|p| p.binding_name())
                .map(|t| t.text()),
            iterable: vf.iterable().map(|e| e.syntax().text_range()),
            key: vf.key().map(|e| e.syntax().text_range()),
            body,
            origin: vf.syntax().text_range(),
            instance: self.instance(),
        }
    }

    /// Runs `lower` over the items of a `for` or `match` region, which each
    /// take the region's value.
    fn enclosed<T>(&mut self, lower: impl FnOnce(&mut Self) -> T) -> T {
        let guarded = std::mem::replace(&mut self.guarded, true);
        self.depth += 1;
        let out = lower(self);
        self.depth -= 1;
        self.guarded = guarded;
        out
    }

    /// Lowers a `match scrutinee { arm, ... }` region into a [`UiMatch`].
    fn lower_match(&mut self, vm: &ViewMatch) -> UiMatch {
        let arms = self.enclosed(|l| {
            vm.arms()
                .map(|arm| UiMatchArm {
                    pattern: arm.pattern().map(|p| p.syntax().text_range()),
                    guard: arm.guard().map(|e| e.syntax().text_range()),
                    items: arm.body().map(|b| l.block_items(b)).unwrap_or_default(),
                })
                .collect()
        });
        UiMatch {
            scrutinee: vm.scrutinee().map(|e| e.syntax().text_range()),
            arms,
            origin: vm.syntax().text_range(),
            instance: self.instance(),
        }
    }

    /// Lowers the items of a nested view block, in source order.
    fn block_items(&mut self, block: ViewBlock) -> Vec<UiItem> {
        self.items(block.items())
    }
}
/// Whether `handler` is written `on capture`.
fn is_capture(handler: &crate::ast::EventHandler) -> bool {
    handler
        .phase()
        .is_some_and(|t| t.kind() == crate::SyntaxKind::CaptureKw)
}
