//! Styles as a node applies them: what `styles: [A, B]` gives a
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
use viso_schema::Interaction;
use viso_schema::view::{ControlKind, LayoutSlot};

use crate::ast::{
    AssignablePath, AstNode, BinaryExpr, Expr, Item, ParenExpr, PathExpr, PropertyBinding,
    PropertyPath, StyleDecl, StyleWhen, TypePath, UnaryExpr, ViewItem,
};
use crate::ir::ui_ir::{UiLook, UiLookArm, UiMemberWhen, UiWhen};
use crate::resolve::{Resolution, ResolvedRef, SymbolId};
use crate::syntax::{SyntaxKind, TextRange};

/// The interaction selectors and the state each reads.
pub(crate) const INTERACTION_SELECTORS: [(&str, Interaction); 7] = [
    ("hover", Interaction::Hovered),
    ("pressed", Interaction::Pressed),
    ("focused", Interaction::Focused),
    ("focus_visible", Interaction::FocusVisible),
    ("selected", Interaction::Selected),
    ("dragging", Interaction::Dragging),
    ("drop_over", Interaction::DropOver),
];

/// The standard selectors a node's own property decides: the selector, the
/// property, and whether the selector is its negation.
pub(crate) const PROPERTY_SELECTORS: [(&str, &str, bool); 4] = [
    ("disabled", "enabled", true),
    ("checked", "checked", false),
    ("invalid", "invalid", false),
    ("expanded", "expanded", false),
];

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

/// Whether the runtime delivers property `name` of a native node of type
/// `target` from a value it evaluates: a value its control reads, or one it
/// lays out by. A selector may switch such a property, and a value that is no
/// constant reaches the node.
fn delivers(target: &str, name: &str) -> bool {
    ControlKind::of(target).input(name).is_some() || LayoutSlot::of(name).is_some()
}

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

/// The styles one module declares, by symbol, and the style each name of the
/// module that resolves to one names, its own or an imported one.
#[derive(Debug, Default)]
pub(crate) struct StyleBook {
    styles: HashMap<SymbolId, StyleDecl>,
    heads: HashMap<TextRange, SymbolId>,
}

impl StyleBook {
    /// The styles among `items`, each the symbol `symbol_of` gives it.
    pub(crate) fn new(
        items: impl Iterator<Item = Item>,
        symbol_of: impl Fn(&StyleDecl) -> Option<SymbolId>,
    ) -> StyleBook {
        let styles = items
            .filter_map(|item| match item {
                Item::Export(e) => e.declaration(),
                other => Some(other),
            })
            .filter_map(|item| match item {
                Item::Style(s) => Some((symbol_of(&s)?, s)),
                _ => None,
            })
            .collect();
        StyleBook {
            styles,
            heads: HashMap::new(),
        }
    }

    /// The style the name at `at` names.
    pub(crate) fn named(&self, at: TextRange) -> Option<SymbolId> {
        self.heads.get(&at).copied()
    }

    /// The style `id`, if the module declares it.
    pub(crate) fn get(&self, id: SymbolId) -> Option<&StyleDecl> {
        self.styles.get(&id)
    }

    /// Every style the module declares, in no order.
    pub(crate) fn iter(&self) -> impl Iterator<Item = (SymbolId, &StyleDecl)> {
        self.styles.iter().map(|(id, s)| (*id, s))
    }

    /// The style a base path names, if it names one.
    pub(crate) fn base(&self, base: &TypePath) -> Option<SymbolId> {
        let segments: Vec<_> = base.segments().collect();
        match segments.as_slice() {
            [head] => self.named(head.text_range()),
            _ => None,
        }
    }
}

/// The styles of each module of a package, by the module's index: what a
/// node's `styles` or a style's base names, of its own module or an imported
/// one.
#[derive(Debug, Default)]
pub(crate) struct StyleBooks {
    books: Vec<StyleBook>,
    /// The module declaring each style.
    homes: HashMap<SymbolId, usize>,
    /// What a module the package lacks declares: nothing.
    none: StyleBook,
}

impl StyleBooks {
    /// The books of the modules, by index, each with the references its
    /// module's names resolve through.
    pub(crate) fn new(modules: Vec<(StyleBook, &[ResolvedRef])>) -> StyleBooks {
        let homes: HashMap<SymbolId, usize> = modules
            .iter()
            .enumerate()
            .flat_map(|(module, (book, _))| book.styles.keys().map(move |&id| (id, module)))
            .collect();
        let books = modules
            .into_iter()
            .map(|(mut book, refs)| {
                if !homes.is_empty() {
                    book.heads = refs
                        .iter()
                        .filter_map(|r| match r.to {
                            Resolution::Symbol(id) if homes.contains_key(&id) => {
                                Some((r.range, id))
                            }
                            _ => None,
                        })
                        .collect();
                }
                book
            })
            .collect();
        StyleBooks {
            books,
            homes,
            none: StyleBook::default(),
        }
    }

    /// The styles module `module` declares and names.
    pub(crate) fn book(&self, module: usize) -> &StyleBook {
        self.books.get(module).unwrap_or(&self.none)
    }

    /// The style `id`.
    pub(crate) fn style(&self, id: SymbolId) -> Option<Applying> {
        let module = *self.homes.get(&id)?;
        Some(Applying {
            module,
            id,
            decl: self.book(module).get(id)?.clone(),
        })
    }

    /// The style the name at `at` of module `module` names.
    pub(crate) fn named(&self, module: usize, at: TextRange) -> Option<Applying> {
        self.style(self.book(module).named(at)?)
    }

    /// The style the base `base` of a style of module `module` names.
    pub(crate) fn base(&self, module: usize, base: &TypePath) -> Option<Applying> {
        self.style(self.book(module).base(base)?)
    }

    /// Every style, in no order.
    pub(crate) fn iter(&self) -> impl Iterator<Item = (usize, SymbolId, &StyleDecl)> {
        self.books
            .iter()
            .enumerate()
            .flat_map(|(module, book)| book.iter().map(move |(id, decl)| (module, id, decl)))
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

/// A style a node applies: the module that declares it, its symbol and its
/// declaration.
#[derive(Debug, Clone)]
pub(crate) struct Applying {
    pub(crate) module: usize,
    pub(crate) id: SymbolId,
    pub(crate) decl: StyleDecl,
}

/// What one element of a node's `styles` list names.
pub(crate) enum StyleUse {
    /// A style of the package, at the element's range.
    Style(Applying, TextRange),
    /// A name that resolves to no style.
    Other(TextRange),
    /// No name at all.
    NotAName(TextRange),
}

/// What each element of the `styles` value `value` of module `module` names;
/// the value's range when it is no list.
pub(crate) fn uses(
    books: &StyleBooks,
    module: usize,
    value: &Expr,
) -> Result<Vec<StyleUse>, TextRange> {
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
                Some(head) => match books.named(module, head) {
                    Some(style) => StyleUse::Style(style, at),
                    None => StyleUse::Other(at),
                },
            }
        })
        .collect())
}

/// The styles of `uses` that are for `node_type`, in order.
pub(crate) fn applying(uses: Vec<StyleUse>, node_type: &str) -> Vec<Applying> {
    uses.into_iter()
        .filter_map(|u| match u {
            StyleUse::Style(style, _) if target_name(&style.decl).as_deref() == Some(node_type) => {
                Some(style)
            }
            _ => None,
        })
        .collect()
}

/// A binding a node's styles give it, in the order they apply.
#[derive(Debug, Clone)]
struct Applied {
    /// The module of the style it is in.
    module: usize,
    name: String,
    binding: PropertyBinding,
    /// The selector it applies under, `None` for always.
    when: Option<Expr>,
}

/// The bindings the styles `list` give a node, in the order they apply. A
/// base on a cycle applies once per path into it; the cycle adds nothing.
fn applied(books: &StyleBooks, list: &[Applying]) -> Vec<Applied> {
    fn walk(
        books: &StyleBooks,
        style: &Applying,
        path: &mut Vec<SymbolId>,
        out: &mut Vec<Applied>,
    ) {
        if path.contains(&style.id) {
            return;
        }
        path.push(style.id);
        for base in style.decl.bases() {
            if let Some(base) = books.base(style.module, &base) {
                walk(books, &base, path, out);
            }
        }
        let mut push = |binding: PropertyBinding, when: Option<Expr>| {
            if let (Some(path), Some(_)) = (binding.path(), binding.value()) {
                out.push(Applied {
                    module: style.module,
                    name: path_name(&path),
                    binding,
                    when,
                });
            }
        };
        for item in style_items(&style.decl) {
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
    for style in list {
        walk(books, style, &mut Vec::new(), &mut out);
    }
    out
}

/// What a node's styles give one property: the last unconditional binding and
/// the `when` bindings after it, the latest first.
struct StyledProperty {
    name: String,
    base: Option<Styled>,
    arms: Vec<(Expr, Styled)>,
}

/// A binding a style gives a node, and the module of the style.
#[derive(Debug, Clone)]
pub(crate) struct Styled {
    pub(crate) module: usize,
    pub(crate) binding: PropertyBinding,
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
        let styled = Styled {
            module: item.module,
            binding: item.binding,
        };
        match item.when {
            None => {
                property.base = Some(styled);
                property.arms.clear();
            }
            Some(when) => property.arms.insert(0, (when, styled)),
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
    /// A style's value of the look property `property`, of a style of
    /// module `module`.
    Look {
        property: String,
        value: Expr,
        module: usize,
    },
    /// The node's own `Bool` property a selector reads.
    Own(Own),
}

/// What a native node's styles give it.
#[derive(Debug, Clone, Default)]
pub(crate) struct NodePlan {
    /// The properties the runtime delivers, each with the unconditional
    /// binding its base value comes from, which also folds like the node's
    /// own.
    pub(crate) looks: Vec<(UiLook, Option<Styled>)>,
    /// The value of each part the looks name.
    pub(crate) parts: Vec<Part>,
    /// The unconditional bindings of the properties the runtime does not
    /// deliver, folded like the node's own.
    pub(crate) constants: Vec<Styled>,
    /// What does not mount, and why, with the module of the style it is in.
    pub(crate) unmounted: Vec<(usize, TextRange, String)>,
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

/// What the styles `list` give a native node of type `target` whose own
/// bindings are `own`. A selector that folds to `false` drops its arm; one
/// that folds to `true` makes its value the base.
pub(crate) fn plan(
    books: &StyleBooks,
    list: &[Applying],
    own: &[(String, Own)],
    target: &str,
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
    for property in styled(applied(books, list), &explicit) {
        let name = property.name;
        if !delivers(target, &name) {
            for (when, styled) in &property.arms {
                plan.unmounted.push((
                    styled.module,
                    when.syntax().text_range(),
                    format!("a style's `when` does not switch `{name}` of a `{target}`"),
                ));
            }
            plan.constants.extend(property.base);
            continue;
        }
        let mut base = property.base;
        let mut arms = Vec::new();
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
                Folded::Steps(steps) => arms.push((steps, binding, when.syntax().text_range())),
            }
        }
        // A layout value has no default to fall back to while no arm holds.
        if base.is_none()
            && let Some((_, styled, at)) = arms.first()
            && LayoutSlot::of(&name).is_some()
        {
            plan.unmounted.push((
                styled.module,
                *at,
                format!(
                    "a `when` that switches `{name}` needs the style to set `{name}` outside any `when` too"
                ),
            ));
            continue;
        }
        let mut look = UiLook {
            property: name.clone(),
            base: None,
            arms: Vec::new(),
        };
        if let Some(styled) = &base {
            let part = next();
            look.base = Some(part);
            plan.parts.push(Part {
                part,
                value: PartValue::Look {
                    property: name.clone(),
                    value: styled
                        .binding
                        .value()
                        .expect("an applied binding has a value"),
                    module: styled.module,
                },
            });
        }
        for (steps, styled, _) in arms {
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
                    value: styled
                        .binding
                        .value()
                        .expect("an applied binding has a value"),
                    module: styled.module,
                },
            });
            look.arms.push(UiLookArm { when, part });
        }
        plan.looks.push((look, base));
    }
    plan
}

/// What a node of a user component's styles give one of its inputs: the part
/// of the base value and of each arm's, the latest first, with its selector
/// over the component's `@selector` members.
#[derive(Debug, Clone)]
pub(crate) struct InputPlan {
    pub(crate) input: String,
    pub(crate) base: Option<u32>,
    pub(crate) arms: Vec<(Vec<UiMemberWhen>, u32)>,
}

/// What the styles of a user-component node give it.
#[derive(Debug, Clone, Default)]
pub(crate) struct ComponentPlan {
    pub(crate) inputs: Vec<InputPlan>,
    /// The value of each part the inputs name.
    pub(crate) parts: Vec<Part>,
    /// What does not mount, and why, with the module of the style it is in.
    pub(crate) unmounted: Vec<(usize, TextRange, String)>,
}

/// What the styles `list` give a node of a user component whose `@selector`
/// members are `selectors` and whose own bindings are `own`: each input they
/// bind and the node does not. A `when` naming an interaction selector does
/// not switch an input; one naming nothing the component selects by was
/// reported at the style and drops its arm.
pub(crate) fn component_plan(
    books: &StyleBooks,
    list: &[Applying],
    own: &[(String, Own)],
    selectors: &[String],
) -> ComponentPlan {
    let explicit: Vec<String> = own.iter().map(|(name, _)| name.clone()).collect();
    let mut plan = ComponentPlan::default();
    let mut part = 0;
    let mut look = |plan: &mut ComponentPlan, name: &str, styled: &Styled| {
        part += 1;
        plan.parts.push(Part {
            part,
            value: PartValue::Look {
                property: name.to_string(),
                value: styled
                    .binding
                    .value()
                    .expect("an applied binding has a value"),
                module: styled.module,
            },
        });
        part
    };
    for property in styled(applied(books, list), &explicit) {
        let name = property.name;
        let base = property
            .base
            .as_ref()
            .map(|styled| look(&mut plan, &name, styled));
        let mut arms = Vec::new();
        for (when, styled) in &property.arms {
            let Ok(selector) = Selector::of(when) else {
                continue;
            };
            let mut names = Vec::new();
            selector.names(&mut names);
            if let Some((other, _)) = names.iter().find(|(n, _)| !selectors.contains(n)) {
                if INTERACTION_SELECTORS.iter().any(|(n, _)| n == other) {
                    plan.unmounted.push((
                        styled.module,
                        when.syntax().text_range(),
                        format!(
                            "a style's `when` switches a component's inputs by its \
                             `@selector` members; `{other}` does not switch one yet"
                        ),
                    ));
                }
                continue;
            }
            let mut steps = Vec::new();
            member_steps(&selector, &mut steps);
            arms.push((steps, look(&mut plan, &name, styled)));
        }
        if base.is_some() || !arms.is_empty() {
            plan.inputs.push(InputPlan {
                input: name,
                base,
                arms,
            });
        }
    }
    plan
}

/// `selector`, every name of it a member, as postfix steps.
fn member_steps(selector: &Selector, out: &mut Vec<UiMemberWhen>) {
    match selector {
        Selector::Name(name, _) => out.push(UiMemberWhen::Member(name.clone())),
        Selector::Not(inner) => {
            member_steps(inner, out);
            out.push(UiMemberWhen::Not);
        }
        Selector::And(a, b) | Selector::Or(a, b) => {
            member_steps(a, out);
            member_steps(b, out);
            out.push(match selector {
                Selector::And(..) => UiMemberWhen::And,
                _ => UiMemberWhen::Or,
            });
        }
    }
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
