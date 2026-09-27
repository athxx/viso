//! View body typing: every node's property bindings against its schema (the
//! built-in widget baseline or a user component's inputs), handler bodies, and the
//! structural `if`/`for`/`match` regions (a view `match` is checked for
//! exhaustiveness and unreachable arms like a behavior one).
//!
//! Property checks: an unknown property is `E3101`, a property bound twice in one
//! body (by `:` or `bind`) is `E3102`, `bind` on a property that is not two-way is
//! `E3103`, a `bind` source that is no State Lens is `E3107` and one whose type is not
//! the property's (without a `using` converter) is `E2103`, and a value carrying a
//! `Percent` component (see [`super::percent`]) on a
//! length property without a percent basis is `E3104`. A component input has the basis
//! of the properties its component binds it to: one whose value reaches a property
//! without a basis has none, and one whose value reaches another component's input has
//! that input's; this is settled across the package once every view is walked
//! ([`check_percent_flow`], [`check_input_bases`]).
//! A node type the schema baseline does not list and that is no component of the
//! package is not checked.
//!
//! A `grid.*`/`stack.*`/`absolute.*` property is checked against the node's direct
//! parent: `Fragment`, `if`, `for` and `match` form no parent, so the parent is the
//! nearest enclosing real node in the same view block. A parent that does not
//! provide the group, or one that is not statically known (the view root, a slot
//! fill, the children of a user component), is `E3702`.

use std::collections::HashMap;

use super::infer::{InferCx, MatchCheck, TypeEnv};
use super::percent::{Carry, PercentFacts, PercentSources};
use super::ty::Ty;
use super::widget::{self, ChildProps, PropLookup, WidgetSchema};
use crate::ast::{
    AstNode, ElseBranch, EventHandler, NodeBody, PropertyBinding, PropertyPath, TwoWayBinding,
    TypePath, ViewBlock, ViewFor, ViewIf, ViewItem, ViewMatch,
};
use crate::diag::Diagnostic;
use crate::resolve::suggest::{Candidate, attach, nearest};
use crate::resolve::{Resolution, ResolvedRef, SymbolId};
use crate::syntax::{SyntaxToken, TextRange};

/// One `input` of a user component, as a property of its nodes.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct InputProp {
    pub(crate) name: String,
    pub(crate) ty: Ty,
    /// Whether `@bindable(..)` pairs the input with an event, making it two-way.
    pub(crate) two_way: bool,
    pub(crate) declared_at: TextRange,
    /// The input's member symbol, which references to it in its component resolve to.
    pub(crate) symbol: Option<SymbolId>,
}

/// An input of a component: the component and the input's index among its inputs.
type InputKey = (SymbolId, usize);

/// What one view gives the properties and inputs that care whether a value carries a
/// `Percent` component, settled by [`check_percent_flow`].
#[derive(Debug, Default)]
pub(crate) struct PercentFlow {
    /// The component whose view this is.
    own: Option<SymbolId>,
    sinks: Vec<Sink>,
}

/// One property binding whose value must not, or might not, carry a `Percent`.
#[derive(Debug)]
struct Sink {
    carry: Carry,
    /// The bound value.
    at: TextRange,
    to: SinkTo,
}

#[derive(Debug)]
enum SinkTo {
    /// A length property without a percent basis.
    Unbased(String),
    /// A component's input, whose basis is settled across the package.
    Input(InputKey),
}

/// What the view walk needs beyond type inference: the inputs of the components in
/// the package.
pub(crate) trait ViewEnv: TypeEnv {
    /// The inputs of the component `component`.
    fn component_inputs(&self, component: SymbolId) -> Option<&[InputProp]>;
}

/// Types the view block of the component `component`, appending what it finds to
/// `diagnostics` and what its handlers and patterns define names as to `percent`;
/// returns the bindings the module's percent flow settles.
pub(crate) fn check_view(
    refs: &[ResolvedRef],
    env: &dyn ViewEnv,
    component: Option<SymbolId>,
    block: &ViewBlock,
    diagnostics: &mut Vec<Diagnostic>,
    percent: &mut PercentSources,
) -> PercentFlow {
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
        flow: PercentFlow {
            own: component,
            sinks: Vec::new(),
        },
        diagnostics: Vec::new(),
    };
    walk.items(block.items(), Scope::ROOT);
    percent.extend(walk.cx.take_percent_defs());
    diagnostics.extend(walk.cx.into_diagnostics());
    diagnostics.extend(walk.diagnostics);
    walk.flow
}

/// What a module's views say about the percent bases of component inputs, settled across
/// the package by [`check_input_bases`].
#[derive(Debug, Default)]
pub(crate) struct InputFlows {
    /// Each input a binding leaves without a basis: where, and why.
    unbased: Vec<(InputKey, TextRange, String)>,
    /// Each value of an input (`.0`) passed to another input (`.1`), where, and why.
    forwards: Vec<(InputKey, InputKey, TextRange, String)>,
    /// Each value carrying a `Percent` passed to an input, where, and the input's
    /// description.
    percent_args: Vec<(InputKey, TextRange, String)>,
}

/// Reports each value carrying a `Percent` component bound to a length property
/// without a percent basis as `E3104`, and returns what the module's `flows` say about
/// the bases of component inputs.
pub(crate) fn check_percent_flow(
    flows: &[PercentFlow],
    facts: &PercentFacts,
    env: &dyn ViewEnv,
    diagnostics: &mut Vec<Diagnostic>,
) -> InputFlows {
    let mut input_of: HashMap<SymbolId, InputKey> = HashMap::new();
    for component in flows.iter().filter_map(|f| f.own) {
        for (index, input) in env
            .component_inputs(component)
            .unwrap_or(&[])
            .iter()
            .enumerate()
        {
            if let Some(symbol) = input.symbol {
                input_of.insert(symbol, (component, index));
            }
        }
    }
    let reached = |carry: &Carry| -> Vec<InputKey> {
        let wanted =
            |n: Resolution| matches!(n, Resolution::Symbol(s) if input_of.contains_key(&s));
        facts
            .reached(carry, wanted)
            .into_iter()
            .filter_map(|n| match n {
                Resolution::Symbol(s) => input_of.get(&s).copied(),
                Resolution::Local(_) => None,
            })
            .collect()
    };
    let describe = |(component, index): InputKey| {
        let name = env.type_name(component).unwrap_or("?");
        let input = env
            .component_inputs(component)
            .and_then(|inputs| inputs.get(index))
            .map_or("?", |i| i.name.as_str());
        format!("`{input}` of `{name}`")
    };
    let mut out = InputFlows::default();
    for sink in flows.iter().flat_map(|f| &f.sinks) {
        let percent = facts.percent(&sink.carry);
        match &sink.to {
            SinkTo::Unbased(property) => {
                if let Some(origin) = percent {
                    let message = format!(
                        "`{property}` has no percent basis, so it does not accept a value \
                         with a `Percent` component"
                    );
                    let mut diagnostic = Diagnostic::error("E3104", sink.at, message);
                    if !(sink.at.start() <= origin.start() && origin.end() <= sink.at.end()) {
                        diagnostic
                            .related
                            .push((origin, "the `Percent` comes from here".to_string()));
                    }
                    diagnostics.push(diagnostic);
                }
                for key in reached(&sink.carry) {
                    out.unbased
                        .push((key, sink.at, format!("bound to `{property}` here")));
                }
            }
            SinkTo::Input(to) => {
                if percent.is_some() {
                    out.percent_args.push((*to, sink.at, describe(*to)));
                }
                for from in reached(&sink.carry) {
                    let reason = format!("passed to {} here", describe(*to));
                    out.forwards.push((from, *to, sink.at, reason));
                }
            }
        }
    }
    out
}

/// Settles, from every module's [`InputFlows`] (index-parallel to `diagnostics`), which
/// component inputs have no percent basis, and reports each value carrying a `Percent`
/// passed to one as `E3104` in the module that passes it. The binding that leaves the
/// input without a basis is related when it is in the same module.
pub(crate) fn check_input_bases(modules: &[InputFlows], diagnostics: &mut [Vec<Diagnostic>]) {
    let mut unbased: HashMap<InputKey, (usize, TextRange, &str)> = HashMap::new();
    for (module, flows) in modules.iter().enumerate() {
        for (key, at, reason) in &flows.unbased {
            unbased.entry(*key).or_insert((module, *at, reason));
        }
    }
    loop {
        let mut changed = false;
        for (module, flows) in modules.iter().enumerate() {
            for (from, to, at, reason) in &flows.forwards {
                if unbased.contains_key(to) && !unbased.contains_key(from) {
                    unbased.insert(*from, (module, *at, reason));
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
    for (module, flows) in modules.iter().enumerate() {
        for (key, at, input) in &flows.percent_args {
            let Some((from, used_at, reason)) = unbased.get(key) else {
                continue;
            };
            let mut diagnostic = Diagnostic::error(
                "E3104",
                *at,
                format!("{input} has no percent basis, so it does not accept a `Percent` value"),
            );
            if *from == module {
                diagnostic.related.push((*used_at, reason.to_string()));
            }
            if let Some(out) = diagnostics.get_mut(module) {
                out.push(diagnostic);
            }
        }
    }
}

/// The schema a node body's properties are checked against.
struct Owner<'e> {
    /// The node type's name, for messages.
    name: String,
    /// A user component's inputs (looked up before the common properties).
    inputs: &'e [InputProp],
    /// The user component the node instantiates; its children fill its default slot.
    component: Option<SymbolId>,
    schema: WidgetSchema,
}

/// The direct parent of the nodes in a body, as far as parent-provided properties
/// are concerned.
#[derive(Debug, Clone, Copy)]
enum Parent<'s> {
    /// A node whose schema is known: the group it provides, if any.
    Known {
        name: &'s str,
        provides: Option<ChildProps>,
    },
    /// A node type this layer has no schema for: its groups are not checked.
    Opaque,
    /// No statically known parent: the view root, a slot fill, a user component's
    /// children.
    Unknown,
}

/// Where a body's items sit: the schema its properties belong to, the parent of the
/// node that owns them, and the parent of the nodes it contains.
#[derive(Clone, Copy)]
struct Scope<'s, 'e> {
    owner: Option<&'s Owner<'e>>,
    outer: Parent<'s>,
    inner: Parent<'s>,
}

impl Scope<'_, '_> {
    const ROOT: Self = Scope {
        owner: None,
        outer: Parent::Unknown,
        inner: Parent::Unknown,
    };
}

/// A property as the schema declares it.
struct Declared {
    ty: Option<Ty>,
    two_way: bool,
    basis: Basis,
}

/// Whether a property resolves a `Percent` against a basis.
#[derive(Clone, Copy)]
enum Basis {
    Yes,
    No,
    /// A component input: settled across the package.
    Input(InputKey),
}

impl Basis {
    fn of(percent_basis: bool) -> Self {
        if percent_basis { Basis::Yes } else { Basis::No }
    }
}

struct ViewWalk<'a> {
    cx: InferCx<'a>,
    env: &'a dyn ViewEnv,
    symbols: HashMap<TextRange, SymbolId>,
    flow: PercentFlow,
    diagnostics: Vec<Diagnostic>,
}

impl<'a> ViewWalk<'a> {
    /// Walks the items of one block or node body.
    fn items(&mut self, items: impl Iterator<Item = ViewItem>, scope: Scope<'_, 'a>) {
        let mut bound: HashMap<String, TextRange> = HashMap::new();
        for item in items {
            match item {
                ViewItem::Named(node) => self.node(node.ty(), node.body(), scope.inner),
                ViewItem::Anonymous(node) => self.node(node.ty(), node.body(), scope.inner),
                ViewItem::Property(binding) => self.property(&binding, scope, &mut bound),
                ViewItem::TwoWayBinding(binding) => self.two_way(&binding, scope, &mut bound),
                ViewItem::Handler(handler) => self.handler(&handler, scope.owner),
                ViewItem::If(view_if) => self.view_if(&view_if, scope),
                ViewItem::For(view_for) => self.view_for(&view_for, scope),
                ViewItem::Match(view_match) => self.view_match(&view_match, scope),
                ViewItem::Fill(fill) => {
                    if let Some(body) = fill.body() {
                        self.items(body.items(), Scope::ROOT);
                    }
                }
            }
        }
    }

    /// A node whose parent is `parent`.
    fn node(&mut self, ty: Option<TypePath>, body: Option<NodeBody>, parent: Parent<'_>) {
        let owner = ty.and_then(|ty| self.owner_of(&ty));
        let Some(body) = body else {
            return;
        };
        let inner = match &owner {
            Some(owner) if owner.schema.is_fragment() => parent,
            Some(owner) if owner.component.is_some() => Parent::Unknown,
            Some(owner) => Parent::Known {
                name: &owner.name,
                provides: owner.schema.provides(),
            },
            None => Parent::Opaque,
        };
        let scope = Scope {
            owner: owner.as_ref(),
            outer: parent,
            inner,
        };
        self.items(body.members(), scope);
    }

    /// The schema of a node type: a component of the package, else a built-in
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
                component: Some(*id),
                schema: WidgetSchema::user_component(),
            }),
            None => Some(Owner {
                schema: widget::builtin(&name)?,
                name,
                inputs: &[],
                component: None,
            }),
        }
    }

    /// `on event(payload) { body }`: the payload pattern binds the event's payload
    /// for the body.
    fn handler(&mut self, handler: &EventHandler, owner: Option<&Owner<'a>>) {
        let payload = handler
            .event()
            .map_or(Ty::Unknown, |event| self.event_payload(&event, owner));
        if let Some(pattern) = handler.payload() {
            self.cx.bind_pattern(pattern.syntax(), &payload);
            self.cx
                .check_irrefutable(pattern.syntax(), "a handler payload pattern");
        }
        if let Some(body) = handler.body() {
            self.cx.check_handler(&body);
        }
    }

    /// The payload type of the event `event` names on a node of `owner`. On a user
    /// component it is a standard event or one the component declares (`E3202`
    /// otherwise); a standard or widget event's payload is not modeled.
    fn event_payload(&mut self, event: &SyntaxToken, owner: Option<&Owner<'a>>) -> Ty {
        if let Some(id) = self.symbols.get(&event.text_range()).copied() {
            return match self.env.record_fields(id) {
                Some(_) => Ty::Named(id),
                None => Ty::Unknown,
            };
        }
        let text = event.text();
        let name = text.trim_start_matches("r#");
        let Some((owner, component)) = owner.and_then(|o| Some((o, o.component?))) else {
            return Ty::Unknown;
        };
        if widget::STANDARD_EVENTS.contains(&name) {
            return Ty::Unknown;
        }
        let declared = self.env.component_events(component).unwrap_or_default();
        let candidates = declared
            .iter()
            .map(|e| Candidate {
                name: &e.name,
                declared_at: Some(e.declared_at),
            })
            .chain(widget::STANDARD_EVENTS.iter().map(|&name| Candidate {
                name,
                declared_at: None,
            }));
        let suggestions = nearest(name, candidates);
        let range = event.text_range();
        let mut diagnostic = Diagnostic::error(
            "E3202",
            range,
            format!(
                "`{}` has no event `{name}`: it is neither a standard event nor one the component declares",
                owner.name
            ),
        );
        attach(&mut diagnostic, range, &suggestions);
        self.diagnostics.push(diagnostic);
        Ty::Unknown
    }

    /// `path: value;`
    fn property(
        &mut self,
        binding: &PropertyBinding,
        scope: Scope<'_, 'a>,
        bound: &mut HashMap<String, TextRange>,
    ) {
        let declared = binding
            .path()
            .and_then(|path| self.declared(&path, scope, bound));
        let Some(value) = binding.value() else {
            return;
        };
        let _ = match declared.as_ref().and_then(|d| d.ty.as_ref()) {
            Some(want) if is_text(want) => self.cx.infer_text_value(&value, want),
            Some(want) => self.cx.infer_promoted(&value, want),
            None => self.cx.infer_expr(&value, None),
        };
        let (Some(declared), Some(path)) = (&declared, binding.path()) else {
            return;
        };
        let at = value.syntax().text_range();
        let to = match declared.basis {
            Basis::Yes => return,
            Basis::No if !holds_length(declared.ty.as_ref()) => return,
            Basis::No => SinkTo::Unbased(path_text(&path)),
            Basis::Input(key) => SinkTo::Input(key),
        };
        let carry = self.cx.carry(value.syntax());
        if carry.spelled.is_some() || !carry.names.is_empty() {
            self.flow.sinks.push(Sink { carry, at, to });
        }
    }

    /// `bind path <=> source;`
    fn two_way(
        &mut self,
        binding: &TwoWayBinding,
        scope: Scope<'_, 'a>,
        bound: &mut HashMap<String, TextRange>,
    ) {
        let source = binding.source().map(|source| self.cx.infer_lens(&source));
        let Some(path) = binding.target() else {
            return;
        };
        let Some(declared) = self.declared(&path, scope, bound) else {
            return;
        };
        if let (Some(want), Some(have), None) = (&declared.ty, &source, binding.using_ty())
            && !want.has_unknown()
            && !have.has_unknown()
            && want != have
        {
            let message = format!(
                "`bind` needs both sides to be the same type: `{}` is `{}`, the source is `{}`; convert with `using`",
                path_text(&path),
                self.cx.describe(want),
                self.cx.describe(have),
            );
            self.diagnostics.push(Diagnostic::error(
                "E2103",
                binding.syntax().text_range(),
                message,
            ));
        }
        if !declared.two_way {
            let on = scope
                .owner
                .map_or(String::new(), |o| format!(" on `{}`", o.name));
            let message = format!(
                "`{}`{on} does not support `bind`: it is not a two-way property",
                path_text(&path),
            );
            self.diagnostics.push(Diagnostic::error(
                "E3103",
                path.syntax().text_range(),
                message,
            ));
        }
    }

    /// Records a binding of `path` (a second one is `E3102`) and looks it up on the
    /// scope's owner (an unknown one is `E3101`), or on its parent for a
    /// parent-provided group.
    fn declared(
        &mut self,
        path: &PropertyPath,
        scope: Scope<'_, 'a>,
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

        let segments: Vec<String> = path
            .segments()
            .map(|t| t.text().trim_start_matches("r#").to_string())
            .collect();
        if let [prefix, members @ ..] = segments.as_slice()
            && !members.is_empty()
            && let Some(group) = widget::child_props(prefix)
        {
            return self.provided(group, members, &text, range, scope.outer);
        }
        let owner = scope.owner?;
        if let [name] = segments.as_slice()
            && let Some(index) = owner.inputs.iter().position(|i| i.name == *name)
        {
            let input = &owner.inputs[index];
            let basis = owner
                .component
                .map_or(Basis::Yes, |component| Basis::Input((component, index)));
            return Some(Declared {
                ty: Some(input.ty.clone()).filter(|t| !t.has_unknown()),
                two_way: input.two_way,
                basis,
            });
        }
        let names: Vec<&str> = segments.iter().map(String::as_str).collect();
        match owner.schema.lookup(&names) {
            PropLookup::Known(spec) => Some(Declared {
                ty: spec.kind.ty(),
                two_way: spec.two_way,
                basis: Basis::of(spec.percent_basis),
            }),
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

    /// Looks up `prefix.members` (`text`, at `range`) on the group the direct
    /// `parent` provides: a parent that provides another group or none, or one that
    /// is not statically known, is `E3702`; an unknown member is `E3101`.
    fn provided(
        &mut self,
        group: ChildProps,
        members: &[String],
        text: &str,
        range: TextRange,
        parent: Parent<'_>,
    ) -> Option<Declared> {
        let message = match parent {
            Parent::Opaque => return None,
            Parent::Known {
                provides: Some(provides),
                ..
            } if provides == group => {
                let spec = match members {
                    [member] => provides.member(member),
                    _ => None,
                };
                if let Some(spec) = spec {
                    return Some(Declared {
                        ty: spec.kind.ty(),
                        two_way: spec.two_way,
                        basis: Basis::of(spec.percent_basis),
                    });
                }
                let last = members.last().map_or("", String::as_str);
                let candidates: Vec<_> = provides
                    .names()
                    .into_iter()
                    .map(|name| Candidate {
                        name,
                        declared_at: None,
                    })
                    .collect();
                let suggestions = nearest(last, candidates);
                let mut diagnostic = Diagnostic::error(
                    "E3101",
                    range,
                    format!("`{}` provides no child property `{text}`", group.container),
                );
                attach(&mut diagnostic, range, &suggestions);
                self.diagnostics.push(diagnostic);
                return None;
            }
            Parent::Known { name, .. } => format!(
                "`{text}` is provided by a `{}` parent, but this node's parent is `{name}`",
                group.container
            ),
            Parent::Unknown => format!(
                "`{text}` is provided by a `{}` parent, but this node's parent is not known \
                 statically (a view root, a slot fill or a component's children)",
                group.container
            ),
        };
        self.diagnostics
            .push(Diagnostic::error("E3702", range, message));
        None
    }

    fn view_if(&mut self, view_if: &ViewIf, scope: Scope<'_, 'a>) {
        if let Some(condition) = view_if.condition() {
            let _ = self.cx.infer_expr(&condition, Some(&Ty::Bool));
        }
        if let Some(block) = view_if.then_block() {
            self.items(block.items(), scope);
        }
        match view_if.else_branch() {
            Some(ElseBranch::If(nested)) => self.view_if(&nested, scope),
            Some(ElseBranch::Block(block)) => self.items(block.items(), scope),
            None => {}
        }
    }

    fn view_for(&mut self, view_for: &ViewFor, scope: Scope<'_, 'a>) {
        let iterable = match view_for.iterable() {
            Some(iterable) => self.cx.infer_expr(&iterable, None),
            None => Ty::Unknown,
        };
        let element = iterable.element().cloned().unwrap_or(Ty::Unknown);
        if let Some(pattern) = view_for.pattern() {
            self.cx.bind_pattern(pattern.syntax(), &element);
            if let Some(iterable) = view_for.iterable() {
                let carry = self.cx.carry(iterable.syntax());
                self.cx.define_pattern(pattern.syntax(), &carry);
            }
            self.cx
                .check_irrefutable(pattern.syntax(), "a `for` pattern");
        }
        if let Some(key) = view_for.key() {
            let _ = self.cx.infer_expr(&key, None);
        }
        if let Some(body) = view_for.body() {
            self.items(body.items(), scope);
        }
    }

    fn view_match(&mut self, view_match: &ViewMatch, scope: Scope<'_, 'a>) {
        let scrutinee = view_match.scrutinee();
        let ty = match &scrutinee {
            Some(scrutinee) => self.cx.infer_expr(scrutinee, None),
            None => Ty::Unknown,
        };
        let carry = scrutinee
            .as_ref()
            .map(|s| self.cx.carry(s.syntax()))
            .unwrap_or_default();
        let mut check = MatchCheck::new();
        for arm in view_match.arms() {
            let pattern = arm.pattern();
            if let Some(pattern) = &pattern {
                self.cx.bind_arm(&mut check, pattern.syntax(), &ty);
                self.cx.define_pattern(pattern.syntax(), &carry);
            }
            let guard = arm.guard();
            if let Some(guard) = &guard {
                let _ = self.cx.infer_expr(guard, Some(&Ty::Bool));
            }
            if let Some(body) = arm.body() {
                self.items(body.items(), scope);
            }
            if let Some(pattern) = &pattern {
                self.cx
                    .add_arm(&mut check, pattern.syntax(), &ty, guard.is_some());
            }
        }
        let at = scrutinee.map_or(view_match.syntax().text_range(), |s| {
            s.syntax().text_range()
        });
        self.cx.finish_match(check, &ty, at);
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

/// Whether a property of the declared type `ty` may hold a length that a `Percent`
/// component would need a basis for (a `Percent` itself is a ratio, not a length).
fn holds_length(ty: Option<&Ty>) -> bool {
    match ty {
        None => true,
        Some(ty) => match ty {
            Ty::Dp | Ty::Px | Ty::Sp | Ty::Em | Ty::MixedLength | Ty::Named(_) | Ty::Unknown => {
                true
            }
            Ty::Option(inner) | Ty::List(inner) => holds_length(Some(inner)),
            Ty::Tuple(tys) => tys.iter().any(|t| holds_length(Some(t))),
            _ => false,
        },
    }
}
