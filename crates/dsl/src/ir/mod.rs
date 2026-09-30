//! UI IR / Binding IR — the view-lowering passes (AGENTS sections 21.2, 59).
//!
//! Slice K/L/M build the `.vs` frontend up to Typed HIR: [`crate::hir::lower`]
//! produces a [`crate::hir::ComponentSchema`] per component, but the view tree
//! never entered HIR — `ComponentSchema::view` holds only a `TextRange`. These
//! passes close that gap. They re-walk the view AST (a `ui!` `ViewFragment` or a
//! component's `view` block) and lower it into:
//!
//! - **UI IR** ([`ui_ir`]): a static retained-tree template — a [`ui_ir::UiTree`]
//!   of [`ui_ir::UiNode`]s with folded compile-time style, control-flow regions,
//!   and the properties still needing a runtime binding. Mounted once, never
//!   rebuilt per frame (section 59).
//! - the **property → dirty-class** table ([`dirty_map`]) that tags every binding
//!   with exactly what a write invalidates (section 11).
//!
//! The Binding IR pass (`binding_ir`), keyed-list pass (`keys`), and the emitter
//! land alongside these in later sections; the emitter itself lives on the
//! `viso-ui-macros` side so `viso-dsl` keeps zero UI-runtime and zero
//! proc-macro dependencies — this module exports only the IR data structures and
//! the `lower_view` entry point over them.
//!
//! Nothing here reconstructs a heap tree per frame and nothing depends on
//! `viso-ui`; the length/axis/dirty mirrors carry just enough for the emitter to
//! rebuild the runtime style structs.

pub mod binding_ir;
pub mod dirty_map;
pub mod keys;
mod length;
pub mod ui_ir;

pub use binding_ir::{
    BindingEdge, BindingIr, BindingKind, InstanceSources, NodeKey, lower_bindings,
    lower_view_bindings,
};
pub use dirty_map::{DirtyClass, property_dirty_class};
pub use keys::{KEYLESS_STATEFUL_FOR, KeyIr, KeyedFor, analyze_keys};
pub use ui_ir::{
    AxisIr, LengthIr, NodeKind, PendingProperty, StyleIr, UiFor, UiHandler, UiIf, UiIfArm,
    UiInstance, UiItem, UiMatch, UiMatchArm, UiNode, UiTree,
};

use std::collections::HashMap;

use viso_behavior::native::{FlexAxis, Natives, WidgetNode};
use viso_view::ControlKind;

use crate::ast::{
    AstNode, ComponentDecl, Expr, NodeBody, PathExpr, PropertyBinding, PropertyPath, TypePath,
    ViewBlock, ViewFor, ViewIf, ViewItem, ViewMatch,
};
use crate::hir::ComponentSchema;
use crate::resolve::{Resolution, ResolvedRef, SymbolId};
use crate::syntax::span::TextRange;

/// A lowered view: its template, every node type name it wrote that neither a
/// registered widget nor a component of the library declares (each with the
/// span of its type path), and every component node that does not mount. A
/// `ui!` fragment keeps each unknown type as a [`NodeKind::Component`] node.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LoweredView {
    /// The retained-tree template.
    pub tree: UiTree,
    /// The unknown node type names, in source order.
    pub unknown: Vec<(String, TextRange)>,
    /// Each component node the view cannot inline, and why.
    pub unmounted: Vec<(TextRange, String)>,
}

/// The components a view may inline: the ones declared in the same unit as
/// the mounted component, with the node types that name them.
#[derive(Debug, Default)]
pub struct ComponentLibrary<'a> {
    /// The declaration each resolved name names, by the name's range: the
    /// component of a node-type head.
    heads: HashMap<TextRange, SymbolId>,
    components: Vec<LibraryComponent<'a>>,
}

/// One component of a [`ComponentLibrary`].
#[derive(Debug)]
pub struct LibraryComponent<'a> {
    /// Its typed schema.
    pub schema: &'a ComponentSchema,
    /// Its declaration, whose view is inlined.
    pub decl: ComponentDecl,
    /// Each two-way input, by its index among the inputs, and the event that
    /// writes it back.
    pub write_backs: Vec<(usize, String)>,
}

impl<'a> ComponentLibrary<'a> {
    /// The library of `components`, whose node-type heads resolve through
    /// `refs`.
    pub fn new(refs: &[ResolvedRef], components: Vec<LibraryComponent<'a>>) -> Self {
        let heads = refs
            .iter()
            .filter_map(|r| match r.to {
                Resolution::Symbol(id) => Some((r.range, id)),
                Resolution::Local(_) | Resolution::Native(_) => None,
            })
            .collect();
        ComponentLibrary { heads, components }
    }

    /// The component `id`.
    fn get(&self, id: SymbolId) -> Option<&LibraryComponent<'a>> {
        self.components.iter().find(|c| c.schema.symbol == id)
    }
}

/// Lowers a `ui!` view fragment's items into a [`UiTree`].
///
/// This is the shared-frontend entry the emitter drives after
/// tokenize/parse/resolve: it walks the fragment's top-level [`ViewItem`]s and
/// produces the static template. Each node lowers to the retained node its
/// widget declaration in `natives` names; a `Fragment` splices its children into
/// the enclosing item list, and a type no widget declares is reported in
/// [`LoweredView::unknown`] and kept as a [`NodeKind::Component`] node, a
/// component of the surrounding Rust scope. Such a node takes no properties,
/// handlers or children; any it has is reported in [`LoweredView::unmounted`].
/// Property values that fold to compile-time constants become style; everything
/// else is recorded as a [`PendingProperty`] for the Binding IR pass to resolve
/// against the resolver's refs and the component schema.
pub fn lower_fragment_items(
    items: impl Iterator<Item = ViewItem>,
    natives: &Natives,
) -> LoweredView {
    let library = ComponentLibrary::default();
    let mut lowering = Lowering::new(natives, &library, None);
    lowering.rust_scope = true;
    let mut out = Vec::new();
    for item in items {
        lowering.item(item, &mut out);
    }
    lowering.finish(out)
}

/// Lowers a component's `view` block into a [`UiTree`] like a fragment, but
/// an unknown type mounts nothing. No other component is inlined.
pub fn lower_view_block(block: &ViewBlock, natives: &Natives) -> LoweredView {
    let library = ComponentLibrary::default();
    let mut lowering = Lowering::new(natives, &library, None);
    let out = lowering.items(block.items());
    lowering.finish(out)
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
pub fn lower_component_view(
    block: &ViewBlock,
    natives: &Natives,
    library: &ComponentLibrary<'_>,
    root: SymbolId,
) -> LoweredView {
    let mut lowering = Lowering::new(natives, library, Some(root));
    let out = lowering.items(block.items());
    lowering.finish(out)
}

/// The state of one view lowering.
struct Lowering<'a, 'l> {
    natives: &'a Natives,
    library: &'a ComponentLibrary<'l>,
    unknown: Vec<(String, TextRange)>,
    unmounted: Vec<(TextRange, String)>,
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
}

/// One component view being lowered.
struct Frame {
    /// Its instance: `0` for the mounted component.
    instance: u32,
    /// The components being inlined into each other down to this one.
    chain: Vec<SymbolId>,
    /// What the caller fills each slot with.
    fills: Vec<(String, Vec<ViewItem>)>,
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
            instances: Vec::new(),
            frames: vec![Frame {
                instance: 0,
                chain: root.into_iter().collect(),
                fills: Vec::new(),
                caller: None,
            }],
            current: 0,
            depth: 0,
            guarded: false,
            ordinals: HashMap::new(),
            rust_scope: false,
        }
    }

    fn finish(self, items: Vec<UiItem>) -> LoweredView {
        LoweredView {
            tree: UiTree {
                items,
                instances: self.instances,
            },
            unknown: self.unknown,
            unmounted: self.unmounted,
        }
    }

    /// The instance whose view is being lowered.
    fn instance(&self) -> u32 {
        self.frames[self.current].instance
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
                self.node(
                    node.ty(),
                    local_name,
                    node.body(),
                    node.syntax().text_range(),
                    out,
                );
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
            ViewItem::If(vi) => out.push(UiItem::If(self.lower_if(&vi))),
            ViewItem::For(vf) => out.push(UiItem::For(self.lower_for(&vf))),
            ViewItem::Match(vm) => out.push(UiItem::Match(self.lower_match(&vm))),
            ViewItem::Property(_)
            | ViewItem::Handler(_)
            | ViewItem::TwoWayBinding(_)
            | ViewItem::Fill(_) => {}
        }
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
        let Some(ty) = ty else { return };
        let type_name = type_name_of(&ty);
        let head = ty.segments().next().map(|t| t.text_range());
        if let Some(id) = head.and_then(|head| self.library.heads.get(&head).copied()) {
            if self.library.get(id).is_some() {
                self.component(id, type_name, local_name, body, origin, out);
            } else {
                self.unmounted.push((
                    ty.syntax().text_range(),
                    format!(
                        "`{type_name}` is declared in another file; a view inlines the components of its own file"
                    ),
                ));
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
            WidgetNode::Grid => NodeKind::Grid,
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

        let instance = self.instance();
        let control = ControlKind::of(&type_name);
        let mut pending = Vec::new();
        let mut handlers = Vec::new();
        // Write-backs lead the node's handlers, so an author's handler for the
        // same event sees the bound state already written.
        let mut write_backs = 0;
        let mut control_reads = Vec::new();
        let mut children = Vec::new();
        for member in body.iter().flat_map(|b| b.members()) {
            match member {
                ViewItem::Property(prop) => {
                    if let (Some(kind), Some(name), Some(value)) =
                        (control, single_segment(prop.path()), prop.value())
                        && kind.input(&name).is_some()
                    {
                        control_reads.push((name, value.syntax().text_range()));
                    }
                    fold_property(&prop, instance, &mut style, &mut pending)
                }
                ViewItem::Handler(h) => handlers.push(UiHandler {
                    event: h.event().map(|t| t.text()).unwrap_or_default(),
                    origin: h.syntax().text_range(),
                    instance,
                }),
                ViewItem::TwoWayBinding(bind) => {
                    let (Some(name), Some(source)) = (single_segment(bind.target()), bind.source())
                    else {
                        continue;
                    };
                    if bind.using_ty().is_some() {
                        self.unmounted.push((
                            bind.syntax().text_range(),
                            "a `bind … using` converter is not mounted yet; bind a property of the source's type".to_string(),
                        ));
                        continue;
                    }
                    let Some(event) = widget.write_back(&name) else {
                        continue;
                    };
                    if control.is_some_and(|kind| kind.input(&name).is_some()) {
                        control_reads.push((name, source.syntax().text_range()));
                    }
                    handlers.insert(
                        write_backs,
                        UiHandler {
                            event: event.name.to_string(),
                            origin: bind.syntax().text_range(),
                            instance,
                        },
                    );
                    write_backs += 1;
                }
                other => self.child(other, &mut children),
            }
        }
        out.push(UiItem::Node(UiNode {
            type_name,
            local_name,
            kind,
            style,
            pending,
            handlers,
            control_reads,
            children,
            origin,
            instance,
        }));
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
            self.unmounted.push((
                member.syntax().text_range(),
                "a Rust component mounts its own view; it takes no properties, handlers or children here".to_string(),
            ));
        }
        let path: Vec<String> = ty.segments().map(|t| t.text()).collect();
        out.push(UiItem::Node(UiNode {
            type_name: path.join("::"),
            local_name,
            kind: NodeKind::Component,
            style: StyleIr::default(),
            pending: Vec::new(),
            handlers: Vec::new(),
            control_reads: Vec::new(),
            children: Vec::new(),
            origin,
            instance: self.instance(),
        }));
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
            self.unmounted.push((
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

    /// Inlines a node of the library component `id` onto `out`.
    fn component(
        &mut self,
        id: SymbolId,
        type_name: String,
        local_name: Option<String>,
        body: Option<NodeBody>,
        origin: TextRange,
        out: &mut Vec<UiItem>,
    ) {
        let library = self.library;
        let Some(component) = library.get(id) else {
            return;
        };
        let schema = component.schema;
        if self.frames[self.current].chain.contains(&id) {
            self.unmounted.push((
                origin,
                format!("`{type_name}` mounts itself, which would never end"),
            ));
            return;
        }
        let parent = self.instance();
        let mut args = Vec::new();
        let mut handlers = Vec::new();
        let mut forwarded = Vec::new();
        let mut forwarded_handlers = Vec::new();
        let mut fills: Vec<(String, Vec<ViewItem>)> = Vec::new();
        let default_slot = schema
            .slots
            .iter()
            .find(|s| s.default)
            .map(|s| s.name.clone());
        let input_slot = |name: &str| schema.inputs.iter().position(|i| i.name == name);
        for member in body.iter().flat_map(|b| b.members()) {
            match member {
                ViewItem::Property(prop) => {
                    let segments: Vec<String> = prop
                        .path()
                        .map(|p| p.segments().map(|t| t.text()).collect())
                        .unwrap_or_default();
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
                        self.unmounted.push((
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
        self.instances.push(UiInstance {
            component: id,
            parent,
            identity,
            depth: self.depth,
            regional: self.guarded,
            args,
            handlers,
        });
        let instance = self.instances.len() as u32;
        let mut chain = self.frames[self.current].chain.clone();
        chain.push(id);
        self.frames.push(Frame {
            instance,
            chain,
            fills,
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
                    for prop in &forwarded {
                        fold_property(prop, parent, &mut root.style, &mut root.pending);
                    }
                    for h in &forwarded_handlers {
                        root.handlers.push(UiHandler {
                            event: h.event().map(|t| t.text()).unwrap_or_default(),
                            origin: h.syntax().text_range(),
                            instance: parent,
                        });
                    }
                }
                _ => self.unmounted.push((
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
    instance: u32,
    style: &mut StyleIr,
    pending: &mut Vec<PendingProperty>,
) {
    let Some(path) = prop.path() else { return };
    // The bound property is the path's leading segment (`width`, `axis`, `text`).
    let Some(name) = path.segments().next().map(|t| t.text()) else {
        return;
    };
    let Some(value) = prop.value() else { return };

    if fold_static(&name, &value, style) {
        return;
    }
    pending.push(PendingProperty::new(
        name,
        value.syntax().text_range(),
        instance,
    ));
}

/// Attempts to fold a property value into static style. Returns `true` when the
/// value was a compile-time constant this property recognizes and folded it;
/// `false` leaves the property to become a pending binding.
fn fold_static(name: &str, value: &Expr, style: &mut StyleIr) -> bool {
    match name {
        "width" => match length::fold_size(value) {
            Some(len) => {
                style.width = Some(len);
                true
            }
            None => false,
        },
        "height" => match length::fold_size(value) {
            Some(len) => {
                style.height = Some(len);
                true
            }
            None => false,
        },
        "gap" | "spacing" => match length::fold_gap(value) {
            Some(n) => {
                style.gap = Some(n);
                true
            }
            None => false,
        },
        "axis" | "direction" => match fold_axis(value) {
            Some(axis) => {
                style.axis = Some(axis);
                true
            }
            None => false,
        },
        // Any other property with a constant value is still a static property,
        // but StyleIr models only the load-bearing layout fields today; other
        // constants are left to the emitter via a pending record so nothing is
        // silently dropped. Returning false routes them to `pending`.
        _ => false,
    }
}

/// Folds an expression into an [`AxisIr`], if it is the `row`/`column` identifier.
fn fold_axis(value: &Expr) -> Option<AxisIr> {
    match path_ident(value)?.as_str() {
        "row" => Some(AxisIr::Row),
        "column" | "col" => Some(AxisIr::Column),
        _ => None,
    }
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
