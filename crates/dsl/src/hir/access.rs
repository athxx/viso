//! What a view owes assistive technology and translation (U8.2, U10.3): a
//! node of a widget that is no standard interactive one, handling a pointer
//! or key gesture, has a role other than `group` and an accessible name
//! (`E3704`) and a keyboard path — it is focusable and handles `click`
//! (`E3708`); text a Localizable property shows is no concatenation and no
//! `format` with literal text (`E3705`), and under a strict profile no
//! literal either. Each is a warning, an error under the profile's strict
//! accessibility or localization checks. A component node whose caller
//! forwards gesture handlers onto its view's root is checked as that root
//! with the caller's properties and handlers added.

use viso_behavior::native::NativeWidget;

use crate::ast::{
    AstNode, CallExpr, ComponentDecl, Expr, NodeBody, PathExpr, PropertyBinding, ViewItem,
};
use crate::diag::Diagnostic;
use crate::hir::style::path_name;
use crate::hir::ty::Ty;
use crate::syntax::{SyntaxKind, SyntaxNode, TextRange};

/// The events whose handler makes a node interactive.
pub(crate) fn is_gesture(event: &str) -> bool {
    matches!(
        event,
        "click" | "tap" | "double_tap" | "long_press" | "key_down"
    ) || event.starts_with("drag_")
}

fn report(strict: bool, code: &'static str, at: TextRange, message: String) -> Diagnostic {
    if strict {
        Diagnostic::error(code, at, message)
    } else {
        Diagnostic::warning(code, at, message)
    }
}

/// The root node of a component's view: its type's name and its body.
#[derive(Debug, Clone)]
pub(crate) struct ViewRoot {
    pub(crate) ty: String,
    pub(crate) body: Option<NodeBody>,
}

/// The root node of the view of `decl`: the view's first item, when it is a
/// node.
pub(crate) fn root_of(decl: &ComponentDecl) -> Option<ViewRoot> {
    let item = decl.view()?.block()?.items().next()?;
    let (ty, body) = match item {
        ViewItem::Named(node) => (node.ty()?, node.body()),
        ViewItem::Anonymous(node) => (node.ty()?, node.body()),
        ViewItem::Part(part) => (part.ty()?, part.body()),
        _ => return None,
    };
    let segments: Vec<_> = ty.segments().collect();
    let [head] = segments.as_slice() else {
        return None;
    };
    Some(ViewRoot {
        ty: head.text().trim_start_matches("r#").to_string(),
        body,
    })
}

/// Checks a node of the widget `native` (no standard interactive one), its
/// type at `at`, whose body is `body`.
pub(crate) fn check_node(
    native: &NativeWidget,
    at: TextRange,
    body: &NodeBody,
    strict: bool,
    out: &mut Vec<Diagnostic>,
) {
    let members: Vec<ViewItem> = body.members().collect();
    check_members(native, at, &members, named(body.syntax()), strict, out);
}

/// Checks a component node at `at` whose caller's body is `body`: the
/// component's view root `root`, of the widget `native` (no standard
/// interactive one), with the properties and the gesture handlers `forwarded`
/// of the caller that land on it.
pub(crate) fn check_forwarded(
    native: &NativeWidget,
    at: TextRange,
    root: &ViewRoot,
    (body, forwarded): (&NodeBody, Vec<ViewItem>),
    strict: bool,
    out: &mut Vec<Diagnostic>,
) {
    let mut members: Vec<ViewItem> = root.body.iter().flat_map(|b| b.members()).collect();
    members.extend(forwarded);
    let named = named(body.syntax()) || root.body.as_ref().is_some_and(|b| named(b.syntax()));
    check_members(native, at, &members, named, strict, out);
}

fn check_members(
    native: &NativeWidget,
    at: TextRange,
    members: &[ViewItem],
    named: bool,
    strict: bool,
    out: &mut Vec<Diagnostic>,
) {
    let widget = native.name;
    // An event the widget does not take is reported where its handler is
    // checked.
    let events: Vec<String> = members
        .iter()
        .filter_map(|m| match m {
            ViewItem::Handler(h) => Some(h.event()?.text().trim_start_matches("r#").to_string()),
            _ => None,
        })
        .filter(|event| native.event(event).is_some())
        .collect();
    if !events.iter().any(|e| is_gesture(e)) {
        return;
    }
    // The last binding of a property wins: the caller's, after the root's.
    let own = |name: &str| {
        members.iter().rev().find_map(|m| match m {
            ViewItem::Property(p) if p.path().is_some_and(|path| path_name(&path) == name) => {
                p.value()
            }
            _ => None,
        })
    };
    let role = own("semantics.role").is_some_and(|value| !is_path(&value, &["Role", "group"]));
    if !role {
        out.push(report(
            strict,
            "E3704",
            at,
            format!(
                "this `{widget}` handles input, so it needs a `semantics.role` other than \
                 `Role::group` for assistive technology to announce it"
            ),
        ));
    }
    if !named {
        out.push(report(
            strict,
            "E3704",
            at,
            format!(
                "this `{widget}` handles input, so it needs an accessible name: a \
                 `semantics.label` or text inside it"
            ),
        ));
    }
    let focusable = own("focusable").is_some_and(|value| !is_literal(&value, "false"));
    let click = events.iter().any(|e| e == "click");
    if !(focusable && click) {
        let missing = match (focusable, click) {
            (false, false) => "`focusable: true` and an `on click` handler",
            (false, true) => "`focusable: true`",
            _ => "an `on click` handler, which Enter and Space run",
        };
        out.push(report(
            strict,
            "E3708",
            at,
            format!(
                "this `{widget}` handles pointer input with no keyboard path: it needs {missing}"
            ),
        ));
    }
}

/// Whether the node or something inside it binds a `semantics.label` or a
/// `text`.
fn named(body: &SyntaxNode) -> bool {
    body.descendants()
        .into_iter()
        .filter_map(PropertyBinding::cast)
        .filter_map(|p| p.path())
        .any(|path| matches!(path_name(&path).as_str(), "semantics.label" | "text"))
}

fn is_path(value: &Expr, want: &[&str]) -> bool {
    PathExpr::cast(value.syntax().clone()).is_some_and(|path| {
        let segments: Vec<String> = path.segments().map(|t| t.text().to_string()).collect();
        segments == want
    })
}

fn is_literal(value: &Expr, text: &str) -> bool {
    value.syntax().kind() == SyntaxKind::LiteralExpr
        && value.syntax().text().to_string().trim() == text
}

/// Checks the value `value` of a Localizable property `property`, typed
/// `ty`.
pub(crate) fn check_text(
    property: &str,
    value: &Expr,
    type_of: &dyn Fn(&Expr) -> Option<Ty>,
    strict: bool,
    out: &mut Vec<Diagnostic>,
) {
    let value = unparen(value);
    let node = value.syntax();
    let at = node.text_range();
    let message = match node.kind() {
        SyntaxKind::BinaryExpr
            if node
                .children_with_tokens()
                .into_iter()
                .any(|e| e.kind() == SyntaxKind::Plus)
                && type_of(&value) == Some(Ty::String) =>
        {
            format!(
                "`{property}` shows text joined with `+`; word order differs between \
                 languages, so build it with `tr` and arguments"
            )
        }
        SyntaxKind::CallExpr if literal_format(&value) => format!(
            "`{property}` shows a `format` with literal text; word order differs between \
             languages, so build it with `tr` and arguments"
        ),
        SyntaxKind::LiteralExpr if strict && type_of(&value) == Some(Ty::String) => {
            format!("`{property}` shows literal text; strict localization takes it from `tr`")
        }
        _ => return,
    };
    out.push(report(strict, "E3705", at, message));
}

fn unparen(value: &Expr) -> Expr {
    let mut value = value.clone();
    while value.syntax().kind() == SyntaxKind::ParenExpr {
        let Some(inner) =
            crate::ast::ParenExpr::cast(value.syntax().clone()).and_then(|p| p.inner())
        else {
            break;
        };
        value = inner;
    }
    value
}

/// Whether `value` is `format(template, ..)` whose template has literal text
/// or more than one placeholder.
fn literal_format(value: &Expr) -> bool {
    let Some(call) = CallExpr::cast(value.syntax().clone()) else {
        return false;
    };
    let is_format = call
        .callee()
        .is_some_and(|callee| is_path(&callee, &["format"]));
    if !is_format {
        return false;
    }
    // The template is the first argument, spelled as a literal.
    let template = value
        .syntax()
        .children()
        .into_iter()
        .find(|n| n.kind() == SyntaxKind::ArgumentList)
        .and_then(|args| {
            args.children()
                .into_iter()
                .find(|n| n.kind() == SyntaxKind::Argument)
        })
        .and_then(|arg| arg.children().into_iter().find_map(Expr::cast));
    let Some(template) = template.filter(|t| t.syntax().kind() == SyntaxKind::LiteralExpr) else {
        return false;
    };
    let template = template.syntax();
    let text = template.text().to_string();
    let Some(inner) = text
        .trim()
        .strip_prefix('"')
        .and_then(|t| t.strip_suffix('"'))
    else {
        return false;
    };
    let mut placeholders = 0;
    let mut literal = false;
    let mut chars = inner.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '{' if chars.peek() == Some(&'{') => {
                chars.next();
                literal = true;
            }
            '{' => {
                placeholders += 1;
                for c in chars.by_ref() {
                    if c == '}' {
                        break;
                    }
                }
            }
            '}' if chars.peek() == Some(&'}') => {
                chars.next();
                literal = true;
            }
            c if !c.is_whitespace() => literal = true,
            _ => {}
        }
    }
    literal || placeholders > 1
}
