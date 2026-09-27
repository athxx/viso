//! View body typing: every node's property bindings against its schema (the
//! built-in widget baseline or a user component's inputs), handler bodies, and the
//! structural `if`/`for`/`match` regions.
//!
//! Property checks: an unknown property is `E3101`, a property bound twice in one
//! body (by `:` or `bind`) is `E3102`, `bind` on a property that is not two-way is
//! `E3103`, and a `Percent` value on a property without a percent basis is `E3104`.
//! A node type the schema baseline does not list and that is no component of this
//! module is not checked.

use std::collections::HashMap;

use super::infer::{InferCx, TypeEnv};
use super::ty::Ty;
use super::widget::{self, PropLookup, WidgetSchema};
use crate::ast::{
    AstNode, ElseBranch, NodeBody, PropertyBinding, PropertyPath, TwoWayBinding, TypePath,
    ViewBlock, ViewFor, ViewIf, ViewItem, ViewMatch,
};
use crate::diag::Diagnostic;
use crate::resolve::suggest::{Candidate, attach, nearest};
use crate::resolve::{Resolution, ResolvedRef, SymbolId};
use crate::syntax::{SyntaxKind, SyntaxNode, TextRange};

/// One `input` of a user component, as a property of its nodes.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct InputProp {
    pub(crate) name: String,
    pub(crate) ty: Ty,
    /// Whether `@bindable(..)` pairs the input with an event, making it two-way.
    pub(crate) two_way: bool,
    pub(crate) declared_at: TextRange,
}

/// What the view walk needs beyond type inference: the inputs of the components the
/// module declares.
pub(crate) trait ViewEnv: TypeEnv {
    /// The inputs of the component `component`, when this module declares it.
    fn component_inputs(&self, component: SymbolId) -> Option<&[InputProp]>;
}

/// Types a component's view block, appending what it finds to `diagnostics`.
pub(crate) fn check_view(
    refs: &[ResolvedRef],
    env: &dyn ViewEnv,
    block: &ViewBlock,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let symbols = refs
        .iter()
        .filter_map(|r| match r.to {
            Resolution::Symbol(id) => Some((r.range, id)),
            Resolution::Local(_) => None,
        })
        .collect();
    let mut walk = ViewWalk {
        cx: InferCx::new(refs, env),
        env,
        symbols,
        diagnostics: Vec::new(),
    };
    walk.items(block.items(), None);
    diagnostics.extend(walk.cx.into_diagnostics());
    diagnostics.extend(walk.diagnostics);
}

/// The schema a node body's properties are checked against.
struct Owner<'e> {
    /// The node type's name, for messages.
    name: String,
    /// A user component's inputs (looked up before the common properties).
    inputs: &'e [InputProp],
    schema: WidgetSchema,
}

/// A property as the schema declares it.
struct Declared {
    ty: Option<Ty>,
    two_way: bool,
    percent_basis: bool,
}

struct ViewWalk<'a> {
    cx: InferCx<'a>,
    env: &'a dyn ViewEnv,
    symbols: HashMap<TextRange, SymbolId>,
    diagnostics: Vec<Diagnostic>,
}

impl<'a> ViewWalk<'a> {
    /// Walks the items of one block or node body; properties in it belong to `owner`.
    fn items(&mut self, items: impl Iterator<Item = ViewItem>, owner: Option<&Owner<'a>>) {
        let mut bound: HashMap<String, TextRange> = HashMap::new();
        for item in items {
            match item {
                ViewItem::Named(node) => self.node(node.ty(), node.body()),
                ViewItem::Anonymous(node) => self.node(node.ty(), node.body()),
                ViewItem::Property(binding) => self.property(&binding, owner, &mut bound),
                ViewItem::TwoWayBinding(binding) => self.two_way(&binding, owner, &mut bound),
                ViewItem::Handler(handler) => {
                    if let Some(body) = handler.body() {
                        self.cx.check_handler(&body);
                    }
                }
                ViewItem::If(view_if) => self.view_if(&view_if, owner),
                ViewItem::For(view_for) => self.view_for(&view_for, owner),
                ViewItem::Match(view_match) => self.view_match(&view_match, owner),
                ViewItem::Fill(fill) => {
                    if let Some(body) = fill.body() {
                        self.items(body.items(), None);
                    }
                }
            }
        }
    }

    fn node(&mut self, ty: Option<TypePath>, body: Option<NodeBody>) {
        let owner = ty.and_then(|ty| self.owner_of(&ty));
        if let Some(body) = body {
            self.items(body.members(), owner.as_ref());
        }
    }

    /// The schema of a node type: a component this module declares, else a built-in
    /// widget the baseline lists.
    fn owner_of(&self, ty: &TypePath) -> Option<Owner<'a>> {
        let segments: Vec<_> = ty.segments().collect();
        let [head] = segments.as_slice() else {
            return None;
        };
        let name = head.text().to_string();
        match self.symbols.get(&head.text_range()) {
            Some(id) => Some(Owner {
                name,
                inputs: self.env.component_inputs(*id)?,
                schema: WidgetSchema::user_component(),
            }),
            None => Some(Owner {
                schema: widget::builtin(&name)?,
                name,
                inputs: &[],
            }),
        }
    }

    /// `path: value;`
    fn property(
        &mut self,
        binding: &PropertyBinding,
        owner: Option<&Owner<'a>>,
        bound: &mut HashMap<String, TextRange>,
    ) {
        let declared = binding
            .path()
            .and_then(|path| self.declared(&path, owner, bound));
        let Some(value) = binding.value() else {
            return;
        };
        let ty = match declared.as_ref().and_then(|d| d.ty.as_ref()) {
            Some(want) if is_text(want) => self.cx.infer_text_value(&value, want),
            Some(want) => self.cx.infer_promoted(&value, want),
            None => self.cx.infer_expr(&value, None),
        };
        if let (Some(declared), Some(path)) = (&declared, binding.path())
            && !declared.percent_basis
            && (ty == Ty::Percent || has_percent_literal(value.syntax()))
        {
            let message = format!(
                "`{}` has no percent basis, so it does not accept a `Percent` value",
                path_text(&path)
            );
            self.diagnostics.push(Diagnostic::error(
                "E3104",
                value.syntax().text_range(),
                message,
            ));
        }
    }

    /// `bind path <=> source;`
    fn two_way(
        &mut self,
        binding: &TwoWayBinding,
        owner: Option<&Owner<'a>>,
        bound: &mut HashMap<String, TextRange>,
    ) {
        let Some(path) = binding.target() else {
            return;
        };
        if let Some(declared) = self.declared(&path, owner, bound)
            && !declared.two_way
            && let Some(owner) = owner
        {
            let message = format!(
                "`{}` on `{}` does not support `bind`: it is not a two-way property",
                path_text(&path),
                owner.name
            );
            self.diagnostics.push(Diagnostic::error(
                "E3103",
                path.syntax().text_range(),
                message,
            ));
        }
    }

    /// Records a binding of `path` (a second one is `E3102`) and looks it up on
    /// `owner` (an unknown one is `E3101`).
    fn declared(
        &mut self,
        path: &PropertyPath,
        owner: Option<&Owner<'a>>,
        bound: &mut HashMap<String, TextRange>,
    ) -> Option<Declared> {
        let text = path_text(path);
        let range = path.syntax().text_range();
        if let Some(first) = bound.get(&text) {
            let mut diagnostic = Diagnostic::error(
                "E3102",
                range,
                format!("the property `{text}` is already bound in this node"),
            );
            diagnostic
                .related
                .push((*first, "first bound here".to_string()));
            self.diagnostics.push(diagnostic);
        } else {
            bound.insert(text.clone(), range);
        }

        let owner = owner?;
        let segments: Vec<String> = path
            .segments()
            .map(|t| t.text().trim_start_matches("r#").to_string())
            .collect();
        if let [name] = segments.as_slice()
            && let Some(input) = owner.inputs.iter().find(|i| i.name == *name)
        {
            return Some(Declared {
                ty: Some(input.ty.clone()).filter(|t| !t.has_unknown()),
                two_way: input.two_way,
                percent_basis: true,
            });
        }
        let names: Vec<&str> = segments.iter().map(String::as_str).collect();
        match owner.schema.lookup(&names) {
            PropLookup::Known(spec) => Some(Declared {
                ty: spec.kind.ty(),
                two_way: spec.two_way,
                percent_basis: spec.percent_basis,
            }),
            PropLookup::Unchecked => None,
            PropLookup::Unknown => {
                let last = names.last().copied().unwrap_or_default();
                let mut candidates: Vec<Candidate<'_>> = owner
                    .schema
                    .names(&names)
                    .into_iter()
                    .map(|name| Candidate {
                        name,
                        declared_at: None,
                    })
                    .collect();
                if names.len() == 1 {
                    candidates.extend(owner.inputs.iter().map(|i| Candidate {
                        name: &i.name,
                        declared_at: Some(i.declared_at),
                    }));
                }
                let suggestions = nearest(last, candidates);
                let mut diagnostic = Diagnostic::error(
                    "E3101",
                    range,
                    format!("`{}` has no property `{text}`", owner.name),
                );
                attach(&mut diagnostic, range, &suggestions);
                self.diagnostics.push(diagnostic);
                None
            }
        }
    }

    fn view_if(&mut self, view_if: &ViewIf, owner: Option<&Owner<'a>>) {
        if let Some(condition) = view_if.condition() {
            let _ = self.cx.infer_expr(&condition, Some(&Ty::Bool));
        }
        if let Some(block) = view_if.then_block() {
            self.items(block.items(), owner);
        }
        match view_if.else_branch() {
            Some(ElseBranch::If(nested)) => self.view_if(&nested, owner),
            Some(ElseBranch::Block(block)) => self.items(block.items(), owner),
            None => {}
        }
    }

    fn view_for(&mut self, view_for: &ViewFor, owner: Option<&Owner<'a>>) {
        let iterable = match view_for.iterable() {
            Some(iterable) => self.cx.infer_expr(&iterable, None),
            None => Ty::Unknown,
        };
        let element = iterable.element().cloned().unwrap_or(Ty::Unknown);
        if let Some(pattern) = view_for.pattern() {
            self.cx.bind_pattern(pattern.syntax(), &element);
        }
        if let Some(key) = view_for.key() {
            let _ = self.cx.infer_expr(&key, None);
        }
        if let Some(body) = view_for.body() {
            self.items(body.items(), owner);
        }
    }

    fn view_match(&mut self, view_match: &ViewMatch, owner: Option<&Owner<'a>>) {
        let scrutinee = match view_match.scrutinee() {
            Some(scrutinee) => self.cx.infer_expr(&scrutinee, None),
            None => Ty::Unknown,
        };
        for arm in view_match.arms() {
            if let Some(pattern) = arm.pattern() {
                self.cx.bind_pattern(pattern.syntax(), &scrutinee);
            }
            if let Some(guard) = arm.guard() {
                let _ = self.cx.infer_expr(&guard, Some(&Ty::Bool));
            }
            if let Some(body) = arm.body() {
                self.items(body.items(), owner);
            }
        }
    }
}

/// Whether a property slot takes text (`String` or `Option<String>`).
fn is_text(ty: &Ty) -> bool {
    match ty {
        Ty::String => true,
        Ty::Option(inner) => **inner == Ty::String,
        _ => false,
    }
}

/// The dotted source text of a property path.
fn path_text(path: &PropertyPath) -> String {
    path.segments()
        .map(|t| t.text().trim_start_matches("r#").to_string())
        .collect::<Vec<_>>()
        .join(".")
}

/// Whether a value spells a `%` length anywhere (`50%`, `50% + 4dp`,
/// `Offset { x: 50%, .. }`).
fn has_percent_literal(node: &SyntaxNode) -> bool {
    node.descendants_with_tokens()
        .into_iter()
        .filter_map(|e| e.as_token().cloned())
        .any(|t| t.kind() == SyntaxKind::UnitLiteral && t.text().ends_with('%'))
}
