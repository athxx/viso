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

pub mod dirty_map;
pub mod ui_ir;

pub use dirty_map::{DirtyClass, property_dirty_class};
pub use ui_ir::{
    AxisIr, LengthIr, NodeKind, PendingProperty, StyleIr, UiFor, UiHandler, UiIf, UiIfArm, UiItem,
    UiMatch, UiMatchArm, UiNode, UiTree,
};

use crate::ast::{
    AnonymousNode, AstNode, Expr, LiteralExpr, NamedNode, NodeBody, PathExpr, PropertyBinding,
    TypePath, ViewBlock, ViewFor, ViewIf, ViewItem, ViewMatch,
};
use crate::syntax::SyntaxKind;
use crate::syntax::span::TextRange;

/// Lowers a `ui!` view fragment's items into a [`UiTree`].
///
/// This is the shared-frontend entry the emitter drives after
/// tokenize/parse/resolve: it walks the fragment's top-level [`ViewItem`]s and
/// produces the static template. Property values that fold to compile-time
/// constants become style; everything else is recorded as a
/// [`PendingProperty`] for the Binding IR pass to resolve against the resolver's
/// refs and the component schema.
pub fn lower_fragment_items(items: impl Iterator<Item = ViewItem>) -> UiTree {
    UiTree {
        items: items.filter_map(lower_item).collect(),
    }
}

/// Lowers a component's `view` block into a [`UiTree`].
pub fn lower_view_block(block: &ViewBlock) -> UiTree {
    lower_fragment_items(block.items())
}

/// Lowers one view item into a [`UiItem`], or `None` for an item that carries no
/// mounted structure on its own (a bare property/handler at fragment top level,
/// which the grammar does not produce, or an unsupported advanced item).
fn lower_item(item: ViewItem) -> Option<UiItem> {
    match item {
        ViewItem::Named(node) => lower_named(&node).map(UiItem::Node),
        ViewItem::Anonymous(node) => lower_anonymous(&node).map(UiItem::Node),
        ViewItem::If(vi) => Some(UiItem::If(lower_if(&vi))),
        ViewItem::For(vf) => Some(UiItem::For(lower_for(&vf))),
        ViewItem::Match(vm) => Some(UiItem::Match(lower_match(&vm))),
        // Property/handler/two-way/fill only appear inside a node body, where
        // `lower_body` consumes them; at item level they carry no node.
        ViewItem::Property(_)
        | ViewItem::Handler(_)
        | ViewItem::TwoWayBinding(_)
        | ViewItem::Fill(_) => None,
    }
}

/// Lowers a `node name: Type { ... }` declaration.
fn lower_named(node: &NamedNode) -> Option<UiNode> {
    let type_name = type_name_of(node.ty()?);
    let local_name = node.name().map(|t| t.text());
    Some(build_node(
        type_name,
        local_name,
        node.body(),
        node.syntax().text_range(),
    ))
}

/// Lowers an anonymous `Type { ... }` declaration.
fn lower_anonymous(node: &AnonymousNode) -> Option<UiNode> {
    let type_name = type_name_of(node.ty()?);
    Some(build_node(
        type_name,
        None,
        node.body(),
        node.syntax().text_range(),
    ))
}

/// Assembles a [`UiNode`] from its type, optional local name, and body: folds
/// static properties into style, records reactive properties as pending, and
/// descends into child nodes.
fn build_node(
    type_name: String,
    local_name: Option<String>,
    body: Option<NodeBody>,
    origin: TextRange,
) -> UiNode {
    let kind = NodeKind::from_type_name(&type_name);
    let mut style = StyleIr::default();
    // A container type implies its arrangement axis before any property does.
    match type_name.as_str() {
        "Row" | "HStack" => style.axis = Some(AxisIr::Row),
        "Column" | "VStack" => style.axis = Some(AxisIr::Column),
        _ => {}
    }

    let mut pending = Vec::new();
    let mut handlers = Vec::new();
    let mut children = Vec::new();

    if let Some(body) = body {
        for member in body.members() {
            match member {
                ViewItem::Property(prop) => fold_property(&prop, &mut style, &mut pending),
                ViewItem::Handler(h) => {
                    handlers.push(UiHandler {
                        event: h.event().map(|t| t.text()).unwrap_or_default(),
                        origin: h.syntax().text_range(),
                    });
                }
                // Nested structure and control flow are children.
                ViewItem::Named(_)
                | ViewItem::Anonymous(_)
                | ViewItem::If(_)
                | ViewItem::For(_)
                | ViewItem::Match(_) => {
                    if let Some(child) = lower_item(member) {
                        children.push(child);
                    }
                }
                // Two-way bindings and fills are placeholder-lowered later
                // (advanced semantics); they mount no child node here.
                ViewItem::TwoWayBinding(_) | ViewItem::Fill(_) => {}
            }
        }
    }

    UiNode {
        type_name,
        local_name,
        kind,
        style,
        pending,
        handlers,
        children,
        origin,
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
        "width" => match fold_length(value) {
            Some(len) => {
                style.width = Some(len);
                true
            }
            None => false,
        },
        "height" => match fold_length(value) {
            Some(len) => {
                style.height = Some(len);
                true
            }
            None => false,
        },
        "gap" | "spacing" => match fold_number(value) {
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

/// Folds an expression into a [`LengthIr`], if it is a constant dimension.
///
/// `12dp`/`12px`/bare `12` fold to `Fixed`; `50%` folds to a `Fill` weight. A
/// non-constant value (a state path, an expression) yields `None` and becomes a
/// pending binding. The `fill`/`fit` length keywords are not yet expression
/// atoms in the grammar, so they never reach here; when that surface lands they
/// fold to [`LengthIr::Fill`]/[`LengthIr::Fit`] via the same seam.
fn fold_length(value: &Expr) -> Option<LengthIr> {
    let lit = LiteralExpr::cast(value.syntax().clone())?;
    let token = lit.token()?;
    match token.kind() {
        SyntaxKind::IntLiteral | SyntaxKind::FloatLiteral => {
            numeric_prefix(&token.text()).map(LengthIr::Fixed)
        }
        SyntaxKind::UnitLiteral => {
            let text = token.text();
            if text.trim_end().ends_with('%') {
                let n = numeric_prefix(text.trim_end_matches('%'))?;
                Some(LengthIr::Fill { weight: n / 100.0 })
            } else {
                // `12dp`, `12px`, `12pt`, … — a fixed logical-pixel extent.
                numeric_prefix(&text).map(LengthIr::Fixed)
            }
        }
        _ => None,
    }
}

/// Folds an expression into a plain `f32`, if it is a constant number (with or
/// without a `dp`/`px` unit suffix).
fn fold_number(value: &Expr) -> Option<f32> {
    let lit = LiteralExpr::cast(value.syntax().clone())?;
    let token = lit.token()?;
    match token.kind() {
        SyntaxKind::IntLiteral | SyntaxKind::FloatLiteral | SyntaxKind::UnitLiteral => {
            numeric_prefix(&token.text())
        }
        _ => None,
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

/// Parses the leading numeric run of a literal's spelling into an `f32`, dropping
/// any trailing unit/type suffix (`12dp` → `12.0`, `1.5f32` → `1.5`).
fn numeric_prefix(text: &str) -> Option<f32> {
    let end = text
        .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-' || c == '+'))
        .unwrap_or(text.len());
    let head = &text[..end];
    if head.is_empty() {
        return None;
    }
    head.parse::<f32>().ok()
}

/// The type name a [`TypePath`] denotes — its last segment (`ui::Text` → `Text`).
fn type_name_of(ty: TypePath) -> String {
    ty.segments().last().map(|t| t.text()).unwrap_or_default()
}

/// Lowers an `if / else if / else` view region into a [`UiIf`].
fn lower_if(vi: &ViewIf) -> UiIf {
    let mut arms = Vec::new();
    collect_if_arms(vi, &mut arms);
    UiIf {
        arms,
        origin: vi.syntax().text_range(),
    }
}

/// Walks an `if`/`else if`/`else` chain into flat arms, each with its condition
/// span (or `None` for the trailing `else`) and mounted items.
fn collect_if_arms(vi: &ViewIf, arms: &mut Vec<UiIfArm>) {
    let condition = vi.condition().map(|e| e.syntax().text_range());
    let items = vi.then_block().map(block_items).unwrap_or_default();
    arms.push(UiIfArm { condition, items });

    match vi.else_branch() {
        Some(crate::ast::ElseBranch::If(nested)) => collect_if_arms(&nested, arms),
        Some(crate::ast::ElseBranch::Block(block)) => arms.push(UiIfArm {
            condition: None,
            items: block_items(block),
        }),
        None => {}
    }
}

/// Lowers a `for pattern in iterable key key { ... }` region into a [`UiFor`].
fn lower_for(vf: &ViewFor) -> UiFor {
    UiFor {
        binding: vf
            .pattern()
            .and_then(|p| p.binding_name())
            .map(|t| t.text()),
        iterable: vf.iterable().map(|e| e.syntax().text_range()),
        key: vf.key().map(|e| e.syntax().text_range()),
        body: vf.body().map(block_items).unwrap_or_default(),
        origin: vf.syntax().text_range(),
    }
}

/// Lowers a `match scrutinee { arm, ... }` region into a [`UiMatch`].
fn lower_match(vm: &ViewMatch) -> UiMatch {
    let arms = vm
        .arms()
        .map(|arm| UiMatchArm {
            pattern: arm.pattern().map(|p| p.syntax().text_range()),
            guard: arm.guard().map(|e| e.syntax().text_range()),
            items: arm.body().map(block_items).unwrap_or_default(),
        })
        .collect();
    UiMatch {
        scrutinee: vm.scrutinee().map(|e| e.syntax().text_range()),
        arms,
        origin: vm.syntax().text_range(),
    }
}

/// Lowers the items of a nested view block, in source order.
fn block_items(block: ViewBlock) -> Vec<UiItem> {
    block.items().filter_map(lower_item).collect()
}
