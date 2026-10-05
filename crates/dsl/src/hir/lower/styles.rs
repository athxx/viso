//! `style` declarations (§59, U2.3, U12.1): a style is for a node type its
//! file names — a native widget or a user component (`E2001` otherwise); its
//! bases are styles of the same file (`E2001` for a name that resolves to
//! nothing, `E2103` for one that is no style or is for another type) on no
//! cycle (`E2003`). It binds only properties the target's schema marks
//! styleable — a widget's look and layout properties, a component's
//! `@styleable` inputs (`E3101`) — each once per block (`E3102`), to pure
//! values (`E2502`) of the property's type (`E2103`). A `when` selector is
//! names combined with `!`, `&&` and `||` (`E2103`), each one the target
//! supports (`E2001`).
//!
//! A component member marked `@styleable` is an input, and one marked
//! `@selector` a `Bool` input, state or computed that redefines no standard
//! selector the runtime decides (`E3710`).

use std::collections::{HashMap, HashSet};

use crate::ast::{AstNode, ComponentDecl, StyleDecl};
use crate::diag::{Diagnostic, Related};
use crate::hir::effect::BodyContext;
use crate::hir::infer::{InferCx, TypeEnv};
use crate::hir::style::{
    INTERACTION_SELECTORS, RESERVED_SELECTORS, Selector, StyleBook, StyleItem,
    UNSUPPORTED_SELECTORS, path_name, style_items, target_name, widget_selectors,
};
use crate::hir::ty::Ty;
use crate::hir::view::InputProp;
use crate::hir::widget::{self, PropLookup, value_ty};
use crate::resolve::{Resolution, ResolvedRef, SymbolId};
use crate::syntax::{SyntaxKind, SyntaxNode, TextRange};

use super::{ModuleEnv, attribute_is, check_body};

/// The names of the members of `decl` marked `@selector`, in order.
pub(super) fn selectors_of(decl: &ComponentDecl) -> Vec<String> {
    marked(decl, "selector")
        .into_iter()
        .filter_map(|(_, member)| member_name(&member))
        .collect()
}

/// Each member of `decl` the attribute `name` marks, with the attribute.
fn marked(decl: &ComponentDecl, name: &str) -> Vec<(SyntaxNode, SyntaxNode)> {
    let mut out = Vec::new();
    let mut pending: Vec<SyntaxNode> = Vec::new();
    for child in decl.syntax().children() {
        if child.kind() == SyntaxKind::Attribute {
            if attribute_is(&child, name) {
                pending.push(child);
            }
            continue;
        }
        out.extend(pending.drain(..).map(|attr| (attr, child.clone())));
    }
    out
}

fn member_name(member: &SyntaxNode) -> Option<String> {
    let name = member
        .children_with_tokens()
        .into_iter()
        .filter_map(|e| e.as_token().cloned())
        .find(|t| matches!(t.kind(), SyntaxKind::Ident | SyntaxKind::RawIdent))?;
    Some(name.text().trim_start_matches("r#").to_string())
}

/// Checks the `@styleable` and `@selector` members of the component `decl`,
/// whose members are typed already.
pub(super) fn check_members(
    decl: &ComponentDecl,
    env: &ModuleEnv<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for (attr, member) in marked(decl, "styleable") {
        if member.kind() != SyntaxKind::InputDecl {
            diagnostics.push(Diagnostic::error(
                "E3710",
                attr.text_range(),
                "`@styleable` marks an `input`",
            ));
        }
    }
    for (attr, member) in marked(decl, "selector") {
        let at = attr.text_range();
        let kinds = [
            SyntaxKind::InputDecl,
            SyntaxKind::StateDecl,
            SyntaxKind::ComputedDecl,
        ];
        if !kinds.contains(&member.kind()) {
            diagnostics.push(Diagnostic::error(
                "E3710",
                at,
                "`@selector` marks an `input`, `state` or `computed`",
            ));
            continue;
        }
        let Some(name) = member_name(&member) else {
            continue;
        };
        if RESERVED_SELECTORS.contains(&name.as_str()) {
            diagnostics.push(Diagnostic::error(
                "E3710",
                at,
                format!(
                    "`{name}` is a standard selector the runtime decides; a component's \
                     `@selector` takes another name"
                ),
            ));
            continue;
        }
        let ty = super::MemberEnv::member_symbol(env, &name)
            .and_then(|id| env.resolution_ty(&Resolution::Symbol(id)));
        if let Some(ty) = ty
            && ty != Ty::Bool
            && !ty.has_unknown()
        {
            diagnostics.push(Diagnostic::error(
                "E3710",
                at,
                format!("`@selector` marks a `Bool` member, and `{name}` is not one"),
            ));
        }
    }
}

/// What a style may bind and select on: the target's schema.
enum Target<'e> {
    Widget(widget::WidgetSchema),
    Component {
        symbol: SymbolId,
        inputs: &'e [InputProp],
        selectors: &'e [String],
    },
}

/// The styles of `book` on a base cycle.
pub(super) fn cyclic(book: &StyleBook) -> HashSet<SymbolId> {
    let edges: HashMap<SymbolId, Vec<SymbolId>> = book
        .iter()
        .map(|(id, style)| {
            let bases = style
                .bases()
                .iter()
                .filter_map(|b| book.base(b).map(|(id, _)| id))
                .collect();
            (id, bases)
        })
        .collect();
    let mut cyclic = HashSet::new();
    for &start in edges.keys() {
        let mut seen = HashSet::new();
        let mut stack = edges[&start].clone();
        while let Some(next) = stack.pop() {
            if next == start {
                cyclic.insert(start);
                break;
            }
            if seen.insert(next) {
                stack.extend(edges.get(&next).into_iter().flatten());
            }
        }
    }
    cyclic
}

/// Checks the style `decl` of the module whose styles are `book`; `cyclic`
/// holds those on a base cycle.
pub(super) fn check(
    decl: &StyleDecl,
    book: &StyleBook,
    cyclic: &HashSet<SymbolId>,
    refs: &[ResolvedRef],
    env: &ModuleEnv<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let name = decl.name().map(|t| t.text()).unwrap_or_default();
    let at = decl
        .name()
        .map_or(decl.syntax().text_range(), |t| t.text_range());
    if env
        .scope
        .declared
        .get(&decl.syntax().text_range())
        .is_some_and(|id| cyclic.contains(id))
    {
        diagnostics.push(Diagnostic::error(
            "E2003",
            at,
            format!("the style `{name}` is based on itself"),
        ));
    }
    let target = decl.target().and_then(|target| {
        let found = self::target(&target, refs, env);
        if found.is_none() {
            diagnostics.push(Diagnostic::error(
                "E2001",
                target.syntax().text_range(),
                format!(
                    "no component or widget is named `{}` for `{name}` to style",
                    target.syntax().text()
                ),
            ));
        }
        found
    });
    let target_text = target_name(decl);

    for base in decl.bases() {
        let range = base.syntax().text_range();
        let text = base.syntax().text().to_string();
        match book.base(&base) {
            Some((_, style)) if target_name(style) == target_text => {}
            Some((_, style)) => diagnostics.push(Diagnostic::error(
                "E2103",
                range,
                format!(
                    "the base `{text}` styles `{}`, and `{name}` styles `{}`",
                    target_name(style).unwrap_or_default(),
                    target_text.clone().unwrap_or_default(),
                ),
            )),
            None => {
                let head = base.segments().next().map(|t| t.text_range());
                let resolved = refs
                    .iter()
                    .any(|r| Some(r.range) == head && matches!(r.to, Resolution::Symbol(_)));
                let (code, message) = if resolved {
                    (
                        "E2103",
                        format!(
                            "`{text}` names no style of this file; a style's bases are styles \
                             its own file declares"
                        ),
                    )
                } else {
                    ("E2001", format!("no style `{text}` to base `{name}` on"))
                };
                diagnostics.push(Diagnostic::error(code, range, message));
            }
        }
    }

    let Some(target) = target else {
        return;
    };
    let mut cx = InferCx::new(refs, env);
    let mut top: HashMap<String, TextRange> = HashMap::new();
    for item in style_items(decl) {
        match item {
            StyleItem::Binding(binding) => {
                binding_check(&binding, &target, &mut top, &mut cx, refs, env, diagnostics)
            }
            StyleItem::When(when) => {
                if let Some(selector) = when.selector() {
                    selector_check(&selector, &target, env, diagnostics);
                }
                let mut bound = HashMap::new();
                for binding in when.bindings() {
                    binding_check(
                        &binding,
                        &target,
                        &mut bound,
                        &mut cx,
                        refs,
                        env,
                        diagnostics,
                    );
                }
            }
        }
    }
    diagnostics.extend(cx.into_diagnostics());
}

/// The schema of the style target `target`.
fn target<'e>(
    target: &crate::ast::TypePath,
    refs: &[ResolvedRef],
    env: &'e ModuleEnv<'_>,
) -> Option<Target<'e>> {
    let segments: Vec<_> = target.segments().collect();
    let [head] = segments.as_slice() else {
        return None;
    };
    let symbol = refs.iter().find_map(|r| match r.to {
        Resolution::Symbol(id) if r.range == head.text_range() => Some(id),
        _ => None,
    });
    if let Some(symbol) = symbol
        && let Some(inputs) = env.decls.inputs.get(&symbol)
    {
        return Some(Target::Component {
            symbol,
            inputs,
            selectors: env.decls.selectors.get(&symbol).map_or(&[], Vec::as_slice),
        });
    }
    widget::widget(env.natives, &head.text()).map(Target::Widget)
}

/// Checks one binding of a style against `target`; `bound` holds the
/// properties its block bound before.
fn binding_check(
    binding: &crate::ast::PropertyBinding,
    target: &Target<'_>,
    bound: &mut HashMap<String, TextRange>,
    cx: &mut InferCx<'_>,
    refs: &[ResolvedRef],
    env: &ModuleEnv<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let (Some(path), Some(value)) = (binding.path(), binding.value()) else {
        return;
    };
    let name = path_name(&path);
    let range = path.syntax().text_range();
    if let Some(first) = bound.get(&name) {
        let mut diagnostic = Diagnostic::error(
            "E3102",
            range,
            format!("the property `{name}` is already bound in this block"),
        );
        diagnostic
            .related
            .push(Related::new(*first, "first bound here"));
        diagnostics.push(diagnostic);
    } else {
        bound.insert(name.clone(), range);
    }
    let segments: Vec<&str> = name.split('.').collect();
    let want: Result<Option<Ty>, String> = match target {
        Target::Widget(schema) => match schema.lookup(&segments) {
            PropLookup::Known(spec) if spec.styleable => Ok(value_ty(spec.ty)),
            PropLookup::Known(_) => Err(format!(
                "a style does not bind `{name}`: `{}` does not mark it styleable",
                schema.native().name
            )),
            PropLookup::Unknown => Err(format!(
                "`{}` has no property `{name}`",
                schema.native().name
            )),
        },
        Target::Component { inputs, symbol, .. } => match inputs.iter().find(|i| i.name == name) {
            Some(input) if input.styleable => {
                Ok(Some(input.ty.clone()).filter(|t| !t.has_unknown()))
            }
            Some(_) => Err(format!(
                "a style binds only `@styleable` inputs, and `{name}` is not one"
            )),
            None => Err(format!(
                "`{}` has no styleable input `{name}`",
                env.decls
                    .types
                    .names
                    .get(symbol)
                    .cloned()
                    .unwrap_or_default()
            )),
        },
    };
    match want {
        Ok(Some(want)) => {
            let _ = cx.infer_promoted(&value, &want);
        }
        Ok(None) => {
            let _ = cx.infer_expr(&value, None);
        }
        Err(message) => {
            diagnostics.push(Diagnostic::error("E3101", range, message));
            let _ = cx.infer_expr(&value, None);
        }
    }
    check_body(refs, BodyContext::View, env, value.syntax(), diagnostics);
}

/// Checks the selector `selector` against what `target` supports.
fn selector_check(
    selector: &crate::ast::Expr,
    target: &Target<'_>,
    env: &ModuleEnv<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let selector = match Selector::of(selector) {
        Ok(selector) => selector,
        Err(at) => {
            diagnostics.push(Diagnostic::error(
                "E2103",
                at,
                "a selector is a selector name, or selectors combined with `!`, `&&` and `||`",
            ));
            return;
        }
    };
    let (supported, of): (Vec<String>, String) = match target {
        Target::Widget(schema) => (
            widget_selectors(schema.native())
                .into_iter()
                .map(str::to_owned)
                .collect(),
            schema.native().name.to_string(),
        ),
        Target::Component {
            symbol, selectors, ..
        } => (
            INTERACTION_SELECTORS
                .iter()
                .map(|(n, _)| (*n).to_owned())
                .chain(selectors.iter().cloned())
                .collect(),
            env.decls
                .types
                .names
                .get(symbol)
                .cloned()
                .unwrap_or_default(),
        ),
    };
    let mut names = Vec::new();
    selector.names(&mut names);
    for (name, at) in names {
        if supported.contains(&name) {
            continue;
        }
        let message = if UNSUPPORTED_SELECTORS.contains(&name.as_str()) {
            format!("no widget supports the selector `{name}` yet")
        } else {
            format!(
                "`{of}` has no selector `{name}`; it supports {}",
                if supported.is_empty() {
                    "none".to_string()
                } else {
                    supported
                        .iter()
                        .map(|n| format!("`{n}`"))
                        .collect::<Vec<_>>()
                        .join(", ")
                }
            )
        };
        diagnostics.push(Diagnostic::error("E2001", at, message));
    }
}
