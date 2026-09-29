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

pub use binding_ir::{BindingEdge, BindingIr, BindingKind, NodeKey, lower_bindings};
pub use dirty_map::{DirtyClass, property_dirty_class};
pub use keys::{KEYLESS_STATEFUL_FOR, KeyIr, KeyedFor, analyze_keys};
pub use ui_ir::{
    AxisIr, LengthIr, NodeKind, PendingProperty, StyleIr, UiFor, UiHandler, UiIf, UiIfArm, UiItem,
    UiMatch, UiMatchArm, UiNode, UiTree,
};

use viso_behavior::native::{FlexAxis, Natives, WidgetNode};

use crate::ast::{
    AstNode, Expr, NodeBody, PathExpr, PropertyBinding, TypePath, ViewBlock, ViewFor, ViewIf,
    ViewItem, ViewMatch,
};
use crate::syntax::span::TextRange;

/// A lowered view: its template, and every node type name it wrote that no
/// registered widget declares (each with the span of its type path).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LoweredView {
    /// The retained-tree template.
    pub tree: UiTree,
    /// The unregistered node type names, in source order.
    pub unknown: Vec<(String, TextRange)>,
}

/// Lowers a `ui!` view fragment's items into a [`UiTree`].
///
/// This is the shared-frontend entry the emitter drives after
/// tokenize/parse/resolve: it walks the fragment's top-level [`ViewItem`]s and
/// produces the static template. Each node lowers to the retained node its
/// widget declaration in `natives` names; a `Fragment` splices its children into
/// the enclosing item list, and a type no widget declares mounts nothing and is
/// reported in [`LoweredView::unknown`]. Property values that fold to
/// compile-time constants become style; everything else is recorded as a
/// [`PendingProperty`] for the Binding IR pass to resolve against the resolver's
/// refs and the component schema.
pub fn lower_fragment_items(
    items: impl Iterator<Item = ViewItem>,
    natives: &Natives,
) -> LoweredView {
    let mut lowering = Lowering {
        natives,
        unknown: Vec::new(),
    };
    let mut out = Vec::new();
    for item in items {
        lowering.item(item, &mut out);
    }
    LoweredView {
        tree: UiTree { items: out },
        unknown: lowering.unknown,
    }
}

/// Lowers a component's `view` block into a [`UiTree`]; see
/// [`lower_fragment_items`].
pub fn lower_view_block(block: &ViewBlock, natives: &Natives) -> LoweredView {
    lower_fragment_items(block.items(), natives)
}

/// The state of one view lowering.
struct Lowering<'a> {
    natives: &'a Natives,
    unknown: Vec<(String, TextRange)>,
}

impl Lowering<'_> {
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

    /// Lowers a node of type `ty` onto `out`: the retained node its widget
    /// declares, with static properties folded into style, reactive ones pending
    /// and child items lowered in order; a `Fragment`'s children directly.
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
        let Some(widget) = self.natives.widget(&type_name) else {
            self.unknown.push((type_name, ty.syntax().text_range()));
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
                    self.item(member, out);
                }
                return;
            }
        };

        let mut pending = Vec::new();
        let mut handlers = Vec::new();
        let mut children = Vec::new();
        for member in body.iter().flat_map(|b| b.members()) {
            match member {
                ViewItem::Property(prop) => fold_property(&prop, &mut style, &mut pending),
                ViewItem::Handler(h) => handlers.push(UiHandler {
                    event: h.event().map(|t| t.text()).unwrap_or_default(),
                    origin: h.syntax().text_range(),
                }),
                other => self.item(other, &mut children),
            }
        }
        out.push(UiItem::Node(UiNode {
            type_name,
            local_name,
            kind,
            style,
            pending,
            handlers,
            children,
            origin,
        }));
    }
}

/// Folds one property binding: a compile-time-constant value updates [`StyleIr`];
/// anything else becomes a [`PendingProperty`] for the Binding IR pass.
fn fold_property(prop: &PropertyBinding, style: &mut StyleIr, pending: &mut Vec<PendingProperty>) {
    let Some(path) = prop.path() else { return };
    // The bound property is the path's leading segment (`width`, `axis`, `text`).
    let Some(name) = path.segments().next().map(|t| t.text()) else {
        return;
    };
    let Some(value) = prop.value() else { return };

    if fold_static(&name, &value, style) {
        return;
    }
    pending.push(PendingProperty::new(name, value.syntax().text_range()));
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

impl Lowering<'_> {
    /// Lowers an `if / else if / else` view region into a [`UiIf`].
    fn lower_if(&mut self, vi: &ViewIf) -> UiIf {
        let mut arms = Vec::new();
        self.collect_if_arms(vi, &mut arms);
        UiIf {
            arms,
            origin: vi.syntax().text_range(),
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
        UiFor {
            binding: vf
                .pattern()
                .and_then(|p| p.binding_name())
                .map(|t| t.text()),
            iterable: vf.iterable().map(|e| e.syntax().text_range()),
            key: vf.key().map(|e| e.syntax().text_range()),
            body: vf.body().map(|b| self.block_items(b)).unwrap_or_default(),
            origin: vf.syntax().text_range(),
        }
    }

    /// Lowers a `match scrutinee { arm, ... }` region into a [`UiMatch`].
    fn lower_match(&mut self, vm: &ViewMatch) -> UiMatch {
        let arms = vm
            .arms()
            .map(|arm| UiMatchArm {
                pattern: arm.pattern().map(|p| p.syntax().text_range()),
                guard: arm.guard().map(|e| e.syntax().text_range()),
                items: arm.body().map(|b| self.block_items(b)).unwrap_or_default(),
            })
            .collect();
        UiMatch {
            scrutinee: vm.scrutinee().map(|e| e.syntax().text_range()),
            arms,
            origin: vm.syntax().text_range(),
        }
    }

    /// Lowers the items of a nested view block, in source order.
    fn block_items(&mut self, block: ViewBlock) -> Vec<UiItem> {
        let mut out = Vec::new();
        for item in block.items() {
            self.item(item, &mut out);
        }
        out
    }
}
