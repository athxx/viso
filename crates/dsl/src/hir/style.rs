//! Styles as a node applies them (§59, U12.1): what `styles: [A, B]` gives a
//! node, and the selectors a `when` reads.
//!
//! A node's styles apply left to right, each after its bases (depth first, in
//! order), each style's bindings and `when` blocks in source order; a later
//! binding of a property overrides an earlier one, and a property the node
//! binds itself overrides them all. So each property a node's styles give it
//! ends up as one value: the last unconditional binding, the base, with the
//! `when` bindings after it as arms, the latest first, the first whose
//! selector holds winning.
//!
//! The view checker and the UI IR both read a node's styles through [`plan`],
//! which numbers the values the runtime evaluates — the base and the arms of
//! each look property, and the node's own properties a selector reads — the
//! same way for both: the checker lowers each at its part of the node's site,
//! and the UI IR names the part.

use std::collections::HashMap;

use viso_behavior::native::NativeWidget;
use viso_ui::Interaction;

use crate::ast::{
    AssignablePath, AstNode, BinaryExpr, Expr, Item, ParenExpr, PathExpr, PropertyBinding,
    PropertyPath, StyleDecl, StyleWhen, TypePath, UnaryExpr, ViewItem,
};
use crate::ir::ui_ir::{UiLook, UiLookArm, UiWhen};
use crate::resolve::{Resolution, ResolvedRef, SymbolId};
use crate::syntax::{SyntaxKind, TextRange};

/// The interaction selectors and the state each reads.
pub(crate) const INTERACTION_SELECTORS: [(&str, Interaction); 4] = [
    ("hover", Interaction::Hovered),
    ("pressed", Interaction::Pressed),
    ("focused", Interaction::Focused),
    ("focus_visible", Interaction::FocusVisible),
];

/// The standard selectors a node's own property decides: the selector, the
/// property, and whether the selector is its negation.
pub(crate) const PROPERTY_SELECTORS: [(&str, &str, bool); 3] = [
    ("disabled", "enabled", true),
    ("checked", "checked", false),
    ("invalid", "invalid", false),
];

/// The standard selectors no widget supports yet.
pub(crate) const UNSUPPORTED_SELECTORS: [&str; 3] = ["selected", "expanded", "dragging"];

/// The standard selectors a user component's own `@selector` may not
/// redefine: those the runtime decides.
pub(crate) const RESERVED_SELECTORS: [&str; 6] = [
    "hover",
    "pressed",
    "focused",
    "focus_visible",
    "disabled",
    "dragging",
];

/// The look properties the runtime delivers per node, which a selector may
/// switch.
const SWITCHED: [&str; 2] = ["background", "opacity"];

/// The look properties the runtime delivers per node that a selector does
/// not switch.
const DELIVERED: [&str; 2] = ["transition.background", "transition.opacity"];

/// The selectors the native widget `widget` supports.
pub(crate) fn widget_selectors(widget: &NativeWidget) -> Vec<&'static str> {
    let mut names = Vec::new();
    if widget.event("hover_enter").is_some() {
        names.extend(INTERACTION_SELECTORS.map(|(name, _)| name));
    }
    for (name, property, _) in PROPERTY_SELECTORS {
        if widget.property(property).is_some_and(|p| p.ty == "Bool") {
            names.push(name);
        }
    }
    names
}

/// The styles one module declares, by symbol, and the symbol each name that
/// resolves to one of them names.
#[derive(Debug, Default)]
pub(crate) struct StyleBook {
    styles: HashMap<SymbolId, StyleDecl>,
    heads: HashMap<TextRange, SymbolId>,
}

impl StyleBook {
    /// The styles among `items`, each the symbol `symbol_of` gives it, whose
    /// uses resolve through `refs`.
    pub(crate) fn new(
        items: impl Iterator<Item = Item>,
        symbol_of: impl Fn(&StyleDecl) -> Option<SymbolId>,
        refs: &[ResolvedRef],
    ) -> StyleBook {
        let styles: HashMap<SymbolId, StyleDecl> = items
            .filter_map(|item| match item {
                Item::Export(e) => e.declaration(),
                other => Some(other),
            })
            .filter_map(|item| match item {
                Item::Style(s) => Some((symbol_of(&s)?, s)),
                _ => None,
            })
            .collect();
        let heads = if styles.is_empty() {
            HashMap::new()
        } else {
            refs.iter()
                .filter_map(|r| match r.to {
                    Resolution::Symbol(id) if styles.contains_key(&id) => Some((r.range, id)),
                    _ => None,
                })
                .collect()
        };
        StyleBook { styles, heads }
    }

    /// The style the name at `at` names.
    pub(crate) fn named(&self, at: TextRange) -> Option<(SymbolId, &StyleDecl)> {
        let id = *self.heads.get(&at)?;
        Some((id, self.styles.get(&id)?))
    }

    /// Every style, in no order.
    pub(crate) fn iter(&self) -> impl Iterator<Item = (SymbolId, &StyleDecl)> {
        self.styles.iter().map(|(id, s)| (*id, s))
    }

    /// The style a base path names, if it names one.
    pub(crate) fn base(&self, base: &TypePath) -> Option<(SymbolId, &StyleDecl)> {
        let segments: Vec<_> = base.segments().collect();
        match segments.as_slice() {
            [head] => self.named(head.text_range()),
            _ => None,
        }
    }
}

/// The node type a style is for: its target's name.
pub(crate) fn target_name(style: &StyleDecl) -> Option<String> {
    let target = style.target()?;
    let segments: Vec<_> = target.segments().collect();
    match segments.as_slice() {
        [head] => Some(head.text().trim_start_matches("r#").to_string()),
        _ => None,
    }
}

/// One item of a style, in source order.
pub(crate) enum StyleItem {
    Binding(PropertyBinding),
    When(StyleWhen),
}

/// The items of `style`, in source order.
pub(crate) fn style_items(style: &StyleDecl) -> impl Iterator<Item = StyleItem> {
    style.syntax().children().into_iter().filter_map(|child| {
        if let Some(binding) = PropertyBinding::cast(child.clone()) {
            return Some(StyleItem::Binding(binding));
        }
        StyleWhen::cast(child).map(StyleItem::When)
    })
}

/// A property path as one dotted name.
pub(crate) fn path_name(path: &PropertyPath) -> String {
    path.segments()
        .map(|t| t.text().trim_start_matches("r#").to_string())
        .collect::<Vec<_>>()
        .join(".")
}

/// A selector: a formula over selector names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Selector {
    /// A selector name, at its range.
    Name(String, TextRange),
    Not(Box<Selector>),
    And(Box<Selector>, Box<Selector>),
    Or(Box<Selector>, Box<Selector>),
}

impl Selector {
    /// The selector `expr` spells: names combined with `!`, `&&`, `||` and
    /// parentheses; the range of what is none otherwise.
    pub(crate) fn of(expr: &Expr) -> Result<Selector, TextRange> {
        let node = expr.syntax();
        let not_one = || node.text_range();
        match node.kind() {
            SyntaxKind::PathExpr => {
                let path = PathExpr::cast(node.clone()).ok_or_else(not_one)?;
                let segments: Vec<_> = path.segments().collect();
                match segments.as_slice() {
                    [name] => Ok(Selector::Name(
                        name.text().trim_start_matches("r#").to_string(),
                        name.text_range(),
                    )),
                    _ => Err(not_one()),
                }
            }
            SyntaxKind::ParenExpr => {
                let inner = ParenExpr::cast(node.clone())
                    .and_then(|p| p.inner())
                    .ok_or_else(not_one)?;
                Selector::of(&inner)
            }
            SyntaxKind::UnaryExpr => {
                let unary = UnaryExpr::cast(node.clone()).ok_or_else(not_one)?;
                match (unary.op().map(|t| t.kind()), unary.operand()) {
                    (Some(SyntaxKind::Bang), Some(operand)) => {
                        Ok(Selector::Not(Box::new(Selector::of(&operand)?)))
                    }
                    _ => Err(not_one()),
                }
            }
            SyntaxKind::BinaryExpr => {
                let binary = BinaryExpr::cast(node.clone()).ok_or_else(not_one)?;
                let (Some(op), Some(lhs), Some(rhs)) = (binary.op(), binary.lhs(), binary.rhs())
                else {
                    return Err(not_one());
                };
                let (lhs, rhs) = (Box::new(Selector::of(&lhs)?), Box::new(Selector::of(&rhs)?));
                match op.kind() {
                    SyntaxKind::AmpAmp => Ok(Selector::And(lhs, rhs)),
                    SyntaxKind::PipePipe => Ok(Selector::Or(lhs, rhs)),
                    _ => Err(not_one()),
                }
            }
            _ => Err(not_one()),
        }
    }

    /// Each name it reads, in order.
    pub(crate) fn names(&self, out: &mut Vec<(String, TextRange)>) {
        match self {
            Selector::Name(name, at) => out.push((name.clone(), *at)),
            Selector::Not(inner) => inner.names(out),
            Selector::And(a, b) | Selector::Or(a, b) => {
                a.names(out);
                b.names(out);
            }
        }
    }
}

/// What one element of a node's `styles` list names.
pub(crate) enum StyleUse {
    /// A style of the node's own module, at the element's range.
    Local(SymbolId, StyleDecl, TextRange),
    /// A name that resolves to no style of the module: a style another
    /// module declares, or something else.
    Other(TextRange),
    /// No name at all.
    NotAName(TextRange),
}

/// What each element of the `styles` value `value` names; the value's range
/// when it is no list.
pub(crate) fn uses(book: &StyleBook, value: &Expr) -> Result<Vec<StyleUse>, TextRange> {
    if value.syntax().kind() != SyntaxKind::ListExpr {
        return Err(value.syntax().text_range());
    }
    Ok(value
        .syntax()
        .children()
        .into_iter()
        .filter_map(Expr::cast)
        .map(|element| {
            let at = element.syntax().text_range();
            let head = PathExpr::cast(element.syntax().clone()).and_then(|path| {
                let segments: Vec<_> = path.segments().collect();
                match segments.as_slice() {
                    [head] => Some(head.text_range()),
                    _ => None,
                }
            });
            match head {
                None => StyleUse::NotAName(at),
                Some(head) => match book.named(head) {
                    Some((id, decl)) => StyleUse::Local(id, decl.clone(), at),
                    None => StyleUse::Other(at),
                },
            }
        })
        .collect())
}

/// The styles of `uses` that are for `node_type`, in order.
pub(crate) fn applying(uses: &[StyleUse], node_type: &str) -> Vec<(SymbolId, StyleDecl)> {
    uses.iter()
        .filter_map(|u| match u {
            StyleUse::Local(id, decl, _) if target_name(decl).as_deref() == Some(node_type) => {
                Some((*id, decl.clone()))
            }
            _ => None,
        })
        .collect()
}

/// A binding a node's styles give it, in the order they apply.
#[derive(Debug, Clone)]
struct Applied {
    name: String,
    binding: PropertyBinding,
    /// The selector it applies under, `None` for always.
    when: Option<Expr>,
}

/// The bindings the styles `list` give a node, in the order they apply. A
/// base on a cycle applies once per path into it; the cycle adds nothing.
fn applied(book: &StyleBook, list: &[(SymbolId, StyleDecl)]) -> Vec<Applied> {
    fn walk(
        book: &StyleBook,
        id: SymbolId,
        style: &StyleDecl,
        path: &mut Vec<SymbolId>,
        out: &mut Vec<Applied>,
    ) {
        if path.contains(&id) {
            return;
        }
        path.push(id);
        for base in style.bases() {
            if let Some((base, decl)) = book.base(&base) {
                let decl = decl.clone();
                walk(book, base, &decl, path, out);
            }
        }
        let mut push = |binding: PropertyBinding, when: Option<Expr>| {
            if let (Some(path), Some(_)) = (binding.path(), binding.value()) {
                out.push(Applied {
                    name: path_name(&path),
                    binding,
                    when,
                });
            }
        };
        for item in style_items(style) {
            match item {
                StyleItem::Binding(binding) => push(binding, None),
                StyleItem::When(when) => {
                    let Some(selector) = when.selector() else {
                        continue;
                    };
                    for binding in when.bindings() {
                        push(binding, Some(selector.clone()));
                    }
                }
            }
        }
        path.pop();
    }
    let mut out = Vec::new();
    for (id, style) in list {
        walk(book, *id, style, &mut Vec::new(), &mut out);
    }
    out
}

/// What a node's styles give one property: the last unconditional binding and
/// the `when` bindings after it, the latest first.
struct StyledProperty {
    name: String,
    base: Option<PropertyBinding>,
    arms: Vec<(Expr, PropertyBinding)>,
}

/// What `applied` gives each property the node does not bind itself, in the
/// order each is first given.
fn styled(applied: Vec<Applied>, explicit: &[String]) -> Vec<StyledProperty> {
    let mut out: Vec<StyledProperty> = Vec::new();
    for item in applied {
        if explicit.contains(&item.name) {
            continue;
        }
        let at = match out.iter().position(|p| p.name == item.name) {
            Some(at) => at,
            None => {
                out.push(StyledProperty {
                    name: item.name.clone(),
                    base: None,
                    arms: Vec::new(),
                });
                out.len() - 1
            }
        };
        let property = &mut out[at];
        match item.when {
            None => {
                property.base = Some(item.binding);
                property.arms.clear();
            }
            Some(when) => property.arms.insert(0, (when, item.binding)),
        }
    }
    out
}

/// The value a node gives one of its own properties.
#[derive(Debug, Clone)]
pub(crate) enum Own {
    /// `property: value;`
    Value(Expr),
    /// `bind property <=> source;`
    Lens(AssignablePath),
}

/// One value a node's styles have the runtime evaluate, at its part.
#[derive(Debug, Clone)]
pub(crate) struct Part {
    pub(crate) part: u32,
    pub(crate) value: PartValue,
}

/// What a part evaluates.
#[derive(Debug, Clone)]
pub(crate) enum PartValue {
    /// A style's value of the look property `property`.
    Look { property: String, value: Expr },
    /// The node's own `Bool` property a selector reads.
    Own(Own),
}

/// What a native node's styles give it.
#[derive(Debug, Clone, Default)]
pub(crate) struct NodePlan {
    /// The look properties the runtime delivers.
    pub(crate) looks: Vec<UiLook>,
    /// The value of each part the looks name.
    pub(crate) parts: Vec<Part>,
    /// The unconditional bindings of every property, folded like the node's
    /// own.
    pub(crate) constants: Vec<PropertyBinding>,
    /// What does not mount, and why.
    pub(crate) unmounted: Vec<(TextRange, String)>,
}

/// The node's own bindings: each property it binds by `:` or `bind`, and the
/// value of each.
pub(crate) fn own_bindings(members: &[ViewItem]) -> Vec<(String, Own)> {
    members
        .iter()
        .filter_map(|member| match member {
            ViewItem::Property(p) => Some((path_name(&p.path()?), Own::Value(p.value()?))),
            ViewItem::TwoWayBinding(b) => Some((path_name(&b.target()?), Own::Lens(b.source()?))),
            _ => None,
        })
        .collect()
}

/// What the styles `list` give a native node whose own bindings are `own`. A
/// selector that folds to `false` drops its arm; one that folds to `true`
/// makes its value the base.
pub(crate) fn plan(
    book: &StyleBook,
    list: &[(SymbolId, StyleDecl)],
    own: &[(String, Own)],
) -> NodePlan {
    let explicit: Vec<String> = own.iter().map(|(name, _)| name.clone()).collect();
    let own = |name: &str| {
        own.iter()
            .find(|(n, _)| n == name)
            .map(|(_, value)| value.clone())
    };
    let mut plan = NodePlan::default();
    let mut part = 0;
    let mut next = || {
        part += 1;
        part
    };
    for property in styled(applied(book, list), &explicit) {
        let name = property.name;
        let switched = SWITCHED.contains(&name.as_str());
        if !switched {
            for (when, _) in &property.arms {
                plan.unmounted.push((
                    when.syntax().text_range(),
                    format!(
                        "a style's `when` switches `background` and `opacity`, not `{name}` yet"
                    ),
                ));
            }
        }
        if !switched && !DELIVERED.contains(&name.as_str()) {
            plan.constants.extend(property.base);
            continue;
        }
        let mut base = property.base;
        let mut arms = Vec::new();
        if switched {
            for (when, binding) in property.arms {
                let Ok(selector) = Selector::of(&when) else {
                    continue;
                };
                match fold(&selector, &own) {
                    Folded::Const(false) => {}
                    Folded::Const(true) => {
                        base = Some(binding);
                        break;
                    }
                    Folded::Steps(steps) => arms.push((steps, binding)),
                }
            }
        }
        let mut look = UiLook {
            property: name.clone(),
            base: None,
            arms: Vec::new(),
        };
        if let Some(binding) = base {
            let part = next();
            look.base = Some(part);
            plan.parts.push(Part {
                part,
                value: PartValue::Look {
                    property: name.clone(),
                    value: binding.value().expect("an applied binding has a value"),
                },
            });
            plan.constants.push(binding);
        }
        for (steps, binding) in arms {
            let mut when = Vec::with_capacity(steps.len());
            for step in steps {
                when.push(match step {
                    Step::State(state) => UiWhen::State(state),
                    Step::Own(value) => {
                        let part = next();
                        plan.parts.push(Part {
                            part,
                            value: PartValue::Own(value),
                        });
                        UiWhen::Part(part)
                    }
                    Step::Not => UiWhen::Not,
                    Step::And => UiWhen::And,
                    Step::Or => UiWhen::Or,
                });
            }
            let part = next();
            plan.parts.push(Part {
                part,
                value: PartValue::Look {
                    property: name.clone(),
                    value: binding.value().expect("an applied binding has a value"),
                },
            });
            look.arms.push(UiLookArm { when, part });
        }
        plan.looks.push(look);
    }
    plan
}

/// A step of a folded selector, in postfix order.
enum Step {
    State(Interaction),
    Own(Own),
    Not,
    And,
    Or,
}

enum Folded {
    Const(bool),
    Steps(Vec<Step>),
}

/// `selector` with each property selector replaced by the node's own value or
/// the property's default, folded where constant.
fn fold(selector: &Selector, own: &dyn Fn(&str) -> Option<Own>) -> Folded {
    match selector {
        Selector::Name(name, _) => {
            if let Some((_, state)) = INTERACTION_SELECTORS.iter().find(|(n, _)| n == name) {
                return Folded::Steps(vec![Step::State(*state)]);
            }
            let Some((_, property, negated)) = PROPERTY_SELECTORS.iter().find(|(n, ..)| n == name)
            else {
                return Folded::Const(false);
            };
            match own(property) {
                Some(value) => {
                    let mut steps = vec![Step::Own(value)];
                    if *negated {
                        steps.push(Step::Not);
                    }
                    Folded::Steps(steps)
                }
                // `enabled` defaults to `true`, `checked` and `invalid` to
                // `false`: none of the selectors holds.
                None => Folded::Const(false),
            }
        }
        Selector::Not(inner) => match fold(inner, own) {
            Folded::Const(value) => Folded::Const(!value),
            Folded::Steps(mut steps) => {
                steps.push(Step::Not);
                Folded::Steps(steps)
            }
        },
        Selector::And(a, b) | Selector::Or(a, b) => {
            let and = matches!(selector, Selector::And(..));
            match (fold(a, own), fold(b, own)) {
                (Folded::Const(x), Folded::Const(y)) => {
                    Folded::Const(if and { x && y } else { x || y })
                }
                (Folded::Const(x), other) | (other, Folded::Const(x)) => match (and, x) {
                    (true, false) => Folded::Const(false),
                    (false, true) => Folded::Const(true),
                    _ => other,
                },
                (Folded::Steps(mut x), Folded::Steps(y)) => {
                    x.extend(y);
                    x.push(if and { Step::And } else { Step::Or });
                    Folded::Steps(x)
                }
            }
        }
    }
}
