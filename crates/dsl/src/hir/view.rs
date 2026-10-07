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
//!
//! Slot checks: a node's bare structure items fill its default slot, so a node
//! type without one that has any is `E3003`; `fill` naming a slot the node type
//! does not declare is `E3501`; and each slot must take as many nodes as its
//! cardinality admits — counted over every arm of a region, a `for` taking any
//! number — or it is `E3502`, as are bare items next to `fill` of the default
//! slot and a second `fill` of a single slot. A `SlotOutlet` names a slot of the
//! component whose view it is in (`E3501` otherwise); a second outlet of one
//! slot, or one inside a `for`, would place the caller's nodes twice and is
//! `E3502`.

use std::cell::RefCell;
use std::collections::HashMap;

use viso_behavior::native::{Natives, SlotCardinality, WidgetNode};
use viso_view::ControlKind;

use super::access;
use super::generic::Converter;
use super::infer::{InferCx, MatchCheck, TypeEnv};
use super::lower::{PartInfo, TargetProfile};
use super::nodes::HirSlot;
use super::percent::{Carry, PercentFacts, PercentSources};
use super::style::{self, Own, PartValue, StyleBook, StyleUse};
use super::ty::Ty;
use super::widget::{self, ChildProps, PropLookup, WidgetSchema, value_ty};
use crate::ast::{
    AssignablePath, AstNode, ElseBranch, EventHandler, Expr, FillClause, NodeBody, PartOverride,
    PartReplace, PathExpr, PropertyBinding, PropertyPath, TemplateUse, TwoWayBinding, TypePath,
    ViewBlock, ViewFor, ViewIf, ViewItem, ViewMatch,
};
use crate::behavior::lower::{
    Def, ProgramBuilder, RegionEntry, lower_handler, lower_region_entry, lower_write_back,
    unsupported_with,
};
use crate::behavior::{FunctionKind, Site};
use crate::diag::{Diagnostic, Related, Severity};
use crate::resolve::suggest::{Candidate, attach, nearest};
use crate::resolve::{Resolution, ResolvedRef, SymbolId};
use crate::syntax::{SyntaxNode, SyntaxToken, TextRange};

/// One `input` of a user component, as a property of its nodes.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct InputProp {
    pub(crate) name: String,
    pub(crate) ty: Ty,
    /// Whether `@bindable(..)` pairs the input with an event, making it two-way.
    pub(crate) two_way: bool,
    /// Whether `@styleable` lets a style bind it.
    pub(crate) styleable: bool,
    pub(crate) declared_at: TextRange,
    /// The input's member symbol, which references to it in its component resolve to.
    pub(crate) symbol: Option<SymbolId>,
    /// Whether it has a default, so a template's `use` may leave it out.
    pub(crate) has_default: bool,
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

    /// The slots of the component `component`.
    fn component_slots(&self, component: SymbolId) -> Option<&[HirSlot]>;

    /// The parts the view of the component or template `component` exposes.
    fn component_parts(&self, component: SymbolId) -> &[PartInfo];

    /// Whether `component` is a template, placed by `use`.
    fn is_template(&self, component: SymbolId) -> bool;

    /// The prelude type named `name`.
    fn standard_type(&self, name: &str) -> Option<SymbolId>;

    /// The native registry the view's widgets are declared in.
    fn widgets(&self) -> &Natives;

    /// The styles the view's module declares.
    fn styles(&self) -> Option<&StyleBook>;

    /// What the package is built for, which sets how strict the
    /// accessibility and localization checks are.
    fn profile(&self) -> &TargetProfile;
}

/// Where a view's event handlers lower to: the component layout registered last
/// in `builder`, for the component `component` of module `module`.
pub(crate) struct HandlerSink<'a> {
    /// The package's behavior.
    pub builder: &'a RefCell<ProgramBuilder>,
    /// The module the view is declared in.
    pub module: usize,
    /// The component's name.
    pub component: &'a str,
}

/// Types the view block of the component `component`, appending what it finds to
/// `diagnostics` and what its handlers and patterns define names as to `percent`;
/// returns the bindings the module's percent flow settles. With a `sink`, every
/// event handler lowers to a handler function of the component.
pub(crate) fn check_view<'a>(
    refs: &'a [ResolvedRef],
    env: &'a dyn ViewEnv,
    component: Option<SymbolId>,
    block: &ViewBlock,
    diagnostics: &mut Vec<Diagnostic>,
    percent: &mut PercentSources,
    sink: Option<HandlerSink<'a>>,
) -> PercentFlow {
    let symbols = refs
        .iter()
        .filter_map(|r| match r.to {
            Resolution::Symbol(id) => Some((r.range, id)),
            Resolution::Local(_) | Resolution::Native(_) | Resolution::Env | Resolution::Theme => {
                None
            }
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
        sink,
        regions: Vec::new(),
        preserves: HashMap::new(),
        outlets: HashMap::new(),
        repeated: 0,
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
                Resolution::Local(_)
                | Resolution::Native(_)
                | Resolution::Env
                | Resolution::Theme => None,
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
                            .push(Related::new(origin, "the `Percent` comes from here"));
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
                diagnostic
                    .related
                    .push(Related::new(*used_at, reason.to_string()));
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
    /// The slots its callers fill: the component's, else the widget's.
    slots: Vec<SlotSpec<'e>>,
    schema: WidgetSchema,
}

impl Owner<'_> {
    fn slot(&self, name: &str) -> Option<&SlotSpec<'_>> {
        self.slots.iter().find(|s| s.name == name)
    }

    fn default_slot(&self) -> Option<&SlotSpec<'_>> {
        self.slots.iter().find(|s| s.default)
    }

    /// Whether `path` is a property a view-driven native node reads: a
    /// control's value or range, a label's text, or its look.
    fn reads_control(&self, path: &PropertyPath) -> bool {
        self.component.is_none()
            && ControlKind::of(&self.name)
                .input(&path_text(path))
                .is_some()
    }

    /// Whether this is a `SlotOutlet`.
    fn is_outlet(&self) -> bool {
        self.component.is_none() && self.schema.native().node == WidgetNode::Outlet
    }
}

/// One slot of a node type.
#[derive(Debug, Clone, Copy)]
struct SlotSpec<'e> {
    name: &'e str,
    cardinality: SlotCardinality,
    default: bool,
    /// Where a user component declares it.
    declared_at: Option<TextRange>,
}

/// How many nodes some structure items mount: at least `min`, at most `max`
/// (`None` for no bound).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Count {
    min: usize,
    max: Option<usize>,
}

impl Count {
    const NONE: Count = Count {
        min: 0,
        max: Some(0),
    };
    const ONE: Count = Count {
        min: 1,
        max: Some(1),
    };
    const ANY: Count = Count { min: 0, max: None };

    /// The nodes a slot of `cardinality` may take.
    fn of(cardinality: SlotCardinality) -> Count {
        match cardinality {
            SlotCardinality::One => Count::ONE,
            SlotCardinality::Optional => Count {
                min: 0,
                max: Some(1),
            },
            SlotCardinality::Many => Count::ANY,
        }
    }

    /// Both, one after the other.
    fn then(self, next: Count) -> Count {
        Count {
            min: self.min + next.min,
            max: self.max.zip(next.max).map(|(a, b)| a + b),
        }
    }

    /// Either one or the other.
    fn or(self, other: Count) -> Count {
        Count {
            min: self.min.min(other.min),
            max: self.max.zip(other.max).map(|(a, b)| a.max(b)),
        }
    }

    /// Whether every count this can be satisfies `cardinality`.
    fn fits(self, cardinality: SlotCardinality) -> bool {
        let slot = Count::of(cardinality);
        self.min >= slot.min
            && match (self.max, slot.max) {
                (_, None) => true,
                (Some(have), Some(most)) => have <= most,
                (None, Some(_)) => false,
            }
    }

    fn describe(self) -> String {
        match (self.min, self.max) {
            (min, Some(max)) if min == max => format!("{min}"),
            (min, Some(max)) => format!("{min} to {max}"),
            (0, None) => "any number of".to_string(),
            (min, None) => format!("{min} or more"),
        }
    }
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
    /// Whether it shows text a user reads.
    localizable: bool,
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
    /// Where handlers lower to.
    sink: Option<HandlerSink<'a>>,
    /// The pattern and value type of each enclosing `for`/`match` region, outermost
    /// first: what a handler inside receives after its payload.
    regions: Vec<(SyntaxNode, Ty)>,
    /// Each `preserve` identity the view names, at its first use.
    preserves: HashMap<String, TextRange>,
    /// Each slot of the component a `SlotOutlet` places, at its first outlet.
    outlets: HashMap<String, TextRange>,
    /// How many `for` bodies enclose the items being walked.
    repeated: usize,
}

impl<'a> ViewWalk<'a> {
    /// Walks the items of one block or node body.
    fn items(&mut self, items: impl Iterator<Item = ViewItem>, scope: Scope<'_, 'a>) {
        let mut bound: HashMap<String, TextRange> = HashMap::new();
        for item in items {
            match item {
                ViewItem::Named(node) => self.node(node.ty(), node.body(), scope.inner),
                ViewItem::Anonymous(node) => self.node(node.ty(), node.body(), scope.inner),
                ViewItem::Part(part) => self.node(part.ty(), part.body(), scope.inner),
                ViewItem::Use(template) => self.template_use(&template, scope.inner),
                ViewItem::Override(o) => self.part_override(&o, scope.owner),
                ViewItem::Replace(r) => self.part_replace(&r, scope.owner),
                ViewItem::Property(binding) => self.property(&binding, scope, &mut bound),
                ViewItem::TwoWayBinding(binding) => self.two_way(&binding, scope, &mut bound),
                ViewItem::Handler(handler) => self.handler(&handler, scope.owner),
                ViewItem::If(view_if) => self.view_if(&view_if, scope),
                ViewItem::For(view_for) => self.view_for(&view_for, scope),
                ViewItem::Match(view_match) => self.view_match(&view_match, scope),
                ViewItem::Fill(fill) => {
                    // A fill's nodes are the filled node's children when it is a
                    // widget; a component places them where its view says.
                    let fill_scope = Scope {
                        owner: None,
                        outer: scope.inner,
                        inner: scope.inner,
                    };
                    if let Some(body) = fill.body() {
                        self.items(body.items(), fill_scope);
                    }
                }
            }
        }
    }

    /// A node whose parent is `parent`.
    fn node(&mut self, ty: Option<TypePath>, body: Option<NodeBody>, parent: Parent<'_>) {
        let at = ty.as_ref().map(|ty| ty.syntax().text_range());
        let owner = ty.and_then(|ty| self.owner_of(&ty));
        if let (Some(owner), Some(at)) = (&owner, at)
            && owner.component.is_some_and(|c| self.env.is_template(c))
        {
            self.diagnostics.push(Diagnostic::error(
                "E2103",
                at,
                format!(
                    "`{0}` is a template, which `use {0}(..);` places, not a node",
                    owner.name
                ),
            ));
            return;
        }
        if let (Some(owner), Some(at)) = (&owner, at) {
            if owner.is_outlet() {
                self.outlet(body.as_ref(), at);
            }
            self.slots(owner, body.as_ref(), at);
        }
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
        if let Some(owner) = &owner
            && owner.component.is_none()
        {
            self.styled(owner, &body);
            if !owner.schema.native().interactive
                && let Some(at) = at
            {
                let strict = self.env.profile().a11y_strict;
                access::check_node(
                    owner.schema.native(),
                    at,
                    &body,
                    strict,
                    &mut self.diagnostics,
                );
            }
        }
    }

    /// Lowers each value the styles of a native node of `owner` (whose body is
    /// `body`) have the runtime evaluate, at its part of the node's styles.
    /// The style declaration reports what is wrong with a value; typing it
    /// here again only decides whether it lowers.
    fn styled(&mut self, owner: &Owner<'a>, body: &NodeBody) {
        let Some(book) = self.env.styles() else {
            return;
        };
        let members: Vec<ViewItem> = body.members().collect();
        let Some(value) = members.iter().find_map(|m| match m {
            ViewItem::Property(p) if p.path().is_some_and(|path| path_text(&path) == "styles") => {
                p.value()
            }
            _ => None,
        }) else {
            return;
        };
        let Ok(uses) = style::uses(book, &value) else {
            return;
        };
        let list = style::applying(&uses, &owner.name);
        if list.is_empty() {
            return;
        }
        let plan = style::plan(book, &list, &style::own_bindings(&members));
        let at = value.syntax().text_range();
        for part in &plan.parts {
            let site = Site::styled(at, part.part);
            match &part.value {
                PartValue::Look { property, value } => {
                    let segments: Vec<&str> = property.split('.').collect();
                    let want = match owner.schema.lookup(&segments) {
                        PropLookup::Known(spec) => value_ty(spec.ty),
                        PropLookup::Unknown => None,
                    };
                    let mark = self.cx.diagnostics().len();
                    let _ = match &want {
                        Some(want) => self.cx.infer_promoted(value, want),
                        None => self.cx.infer_expr(value, None),
                    };
                    let failed = self.cx.rewind_diagnostics(mark);
                    let entry = RegionEntry::Value(value);
                    self.entry_at(failed, "style", site, &entry, value.syntax().text_range());
                }
                PartValue::Own(Own::Value(value)) => {
                    let entry = RegionEntry::Value(value);
                    self.entry_at(false, "style", site, &entry, value.syntax().text_range());
                }
                PartValue::Own(Own::Lens(source)) => {
                    let entry = RegionEntry::Lens(source, None);
                    self.entry_at(false, "style", site, &entry, source.syntax().text_range());
                }
            }
        }
    }

    /// Checks a node's `styles` value: a list of the styles of the module,
    /// each for the node's type `owner`.
    fn check_styles(&mut self, value: &Expr, owner: &Owner<'a>) {
        let Some(book) = self.env.styles() else {
            return;
        };
        let uses = match style::uses(book, value) {
            Ok(uses) => uses,
            Err(at) => {
                self.diagnostics.push(Diagnostic::error(
                    "E2103",
                    at,
                    "`styles` takes a list of styles: `[A, B]`",
                ));
                return;
            }
        };
        for item in uses {
            match item {
                StyleUse::Local(_, decl, at) => {
                    let target = style::target_name(&decl);
                    if target.as_deref() != Some(owner.name.as_str()) {
                        let name = decl.name().map(|t| t.text()).unwrap_or_default();
                        self.diagnostics.push(Diagnostic::error(
                            "E2103",
                            at,
                            format!(
                                "the style `{name}` is for `{}`, not `{}`",
                                target.unwrap_or_default(),
                                owner.name
                            ),
                        ));
                    }
                }
                StyleUse::Other(at) => {
                    let Some(id) = self.symbols.get(&at) else {
                        // The resolver reports a name that resolves to nothing.
                        continue;
                    };
                    let diagnostic = if self.env.resolution_ty(&Resolution::Symbol(*id)).is_some() {
                        Diagnostic::error("E2103", at, "`styles` names styles, and this is a value")
                    } else {
                        Diagnostic::error(
                            "E3711",
                            at,
                            "a node applies the styles its own file declares; this one is \
                             declared in another file",
                        )
                    };
                    self.diagnostics.push(diagnostic);
                }
                StyleUse::NotAName(at) => self.diagnostics.push(Diagnostic::error(
                    "E2103",
                    at,
                    "`styles` names styles: each item is a style's name",
                )),
            }
        }
    }

    /// The schema of a node type: a component of the package, else a registered
    /// native widget; a name that is neither is `E2001`.
    fn owner_of(&mut self, ty: &TypePath) -> Option<Owner<'a>> {
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
                slots: self
                    .env
                    .component_slots(*id)
                    .unwrap_or_default()
                    .iter()
                    .map(|s| SlotSpec {
                        name: &s.name,
                        cardinality: s.cardinality,
                        default: s.default,
                        declared_at: Some(s.declared_at),
                    })
                    .collect(),
                schema: WidgetSchema::user_component(),
            }),
            None => {
                let Some(schema) = widget::widget(self.env.widgets(), &name) else {
                    let range = head.text_range();
                    let candidates = self.env.widgets().widgets().iter().map(|entry| Candidate {
                        name: entry.widget.name,
                        declared_at: None,
                    });
                    let suggestions = nearest(&name, candidates);
                    let mut diagnostic = Diagnostic::error(
                        "E2001",
                        range,
                        format!("no component or widget is named `{name}`"),
                    );
                    attach(&mut diagnostic, range, &suggestions);
                    self.diagnostics.push(diagnostic);
                    return None;
                };
                Some(Owner {
                    slots: schema
                        .native()
                        .slots
                        .iter()
                        .map(|s| SlotSpec {
                            name: s.name,
                            cardinality: s.cardinality,
                            default: s.default,
                            declared_at: None,
                        })
                        .collect(),
                    schema,
                    name,
                    inputs: &[],
                    component: None,
                })
            }
        }
    }

    /// Checks what fills the slots of a node of `owner` (whose type is at `at`):
    /// its bare structure items the default slot, each `fill` the slot it names.
    fn slots(&mut self, owner: &Owner<'a>, body: Option<&NodeBody>, at: TextRange) {
        let mut bare: Option<(TextRange, Count)> = None;
        let mut fills: Vec<(&str, TextRange, Count)> = Vec::new();
        let members: Vec<ViewItem> = body.iter().flat_map(|b| b.members()).collect();
        let mut named: Vec<(FillClause, SlotSpec<'_>)> = Vec::new();
        for member in &members {
            if let ViewItem::Fill(fill) = member {
                let Some(name) = fill.name() else {
                    continue;
                };
                let text = name.text();
                let text = text.trim_start_matches("r#");
                match owner.slot(text) {
                    Some(slot) => named.push((fill.clone(), *slot)),
                    None => self.unknown_slot(owner, text, name.text_range()),
                }
                continue;
            }
            let count = self.count(std::slice::from_ref(member));
            if count == Count::NONE && !is_structure(member) {
                continue;
            }
            let range = member.syntax().text_range();
            bare = Some(match bare {
                Some((first, sum)) => (first, sum.then(count)),
                None => (range, count),
            });
        }
        let default = owner.default_slot().copied();
        if let Some((first, _)) = bare
            && default.is_none()
        {
            let mut message = format!(
                "`{}` has no default slot, so it takes no child items",
                owner.name
            );
            if !owner.slots.is_empty() {
                let names: Vec<_> = owner
                    .slots
                    .iter()
                    .map(|s| format!("`{}`", s.name))
                    .collect();
                message.push_str(&format!("; fill one of its slots: {}", names.join(", ")));
            }
            self.diagnostics
                .push(Diagnostic::error("E3003", first, message));
        }
        for (fill, slot) in &named {
            let Some(name) = fill.name() else {
                continue;
            };
            let range = name.text_range();
            if slot.default
                && let Some((first, _)) = bare
            {
                let mut diagnostic = Diagnostic::error(
                    "E3502",
                    range,
                    format!(
                        "`{}` is the default slot and the bare child items already fill it;                          move them into this `fill`",
                        slot.name
                    ),
                );
                diagnostic
                    .related
                    .push(Related::new(first, "a bare child item"));
                self.diagnostics.push(diagnostic);
                continue;
            }
            if let Some((_, first, _)) = fills.iter().find(|(n, ..)| *n == slot.name)
                && slot.cardinality != SlotCardinality::Many
            {
                let mut diagnostic = Diagnostic::error(
                    "E3502",
                    range,
                    format!(
                        "the slot `{}` is a `{}` and is already filled",
                        slot.name,
                        slot.cardinality.type_name()
                    ),
                );
                diagnostic
                    .related
                    .push(Related::new(*first, "first filled here"));
                self.diagnostics.push(diagnostic);
                continue;
            }
            let items: Vec<ViewItem> = fill.body().iter().flat_map(|b| b.items()).collect();
            let count = self.count(&items);
            match fills.iter_mut().find(|(n, ..)| *n == slot.name) {
                Some(entry) => entry.2 = entry.2.then(count),
                None => fills.push((slot.name, range, count)),
            }
        }
        if let (Some(slot), Some((first, count))) = (default, bare) {
            fills.push((slot.name, first, count));
        }
        for slot in &owner.slots {
            let (range, count) = fills
                .iter()
                .find(|(n, ..)| *n == slot.name)
                .map_or((at, Count::NONE), |(_, range, count)| (*range, *count));
            if count.fits(slot.cardinality) {
                continue;
            }
            let message = format!(
                "the slot `{}` of `{}` is a `{}`, but {} nodes fill it",
                slot.name,
                owner.name,
                slot.cardinality.type_name(),
                count.describe()
            );
            let mut diagnostic = Diagnostic::error("E3502", range, message);
            if let Some((module, declared)) = slot.declared_at.and_then(|d| {
                owner
                    .component
                    .and_then(|c| self.env.declaration_site(c, d))
            }) {
                let label = format!("`{}` is declared here", slot.name);
                diagnostic.related.push(match module {
                    Some(module) => Related::in_module(module, declared, label),
                    None => Related::new(declared, label),
                });
            }
            self.diagnostics.push(diagnostic);
        }
    }

    /// `fill name` naming no slot of `owner`: `E3501`.
    fn unknown_slot(&mut self, owner: &Owner<'a>, name: &str, range: TextRange) {
        let candidates = owner.slots.iter().map(|s| Candidate {
            name: s.name,
            declared_at: s.declared_at.and_then(|d| {
                owner
                    .component
                    .and_then(|c| self.env.declaration_site(c, d))
            }),
        });
        let suggestions = nearest(name, candidates);
        let mut diagnostic = Diagnostic::error(
            "E3501",
            range,
            format!("`{}` has no slot `{name}`", owner.name),
        );
        attach(&mut diagnostic, range, &suggestions);
        self.diagnostics.push(diagnostic);
    }

    /// How many nodes `items` mount, over every arm of their regions.
    fn count(&self, items: &[ViewItem]) -> Count {
        items.iter().fold(Count::NONE, |sum, item| {
            let one = match item {
                ViewItem::Named(node) => self.node_count(node.ty(), node.body()),
                ViewItem::Anonymous(node) => self.node_count(node.ty(), node.body()),
                ViewItem::If(view_if) => {
                    let mut arms: Option<Count> = None;
                    let mut link = Some(view_if.clone());
                    let mut exhaustive = false;
                    while let Some(arm) = link.take() {
                        let block = self.block_count(arm.then_block());
                        arms = Some(arms.map_or(block, |a| a.or(block)));
                        match arm.else_branch() {
                            Some(ElseBranch::If(nested)) => link = Some(nested),
                            Some(ElseBranch::Block(block)) => {
                                let block = self.block_count(Some(block));
                                arms = arms.map(|a| a.or(block));
                                exhaustive = true;
                            }
                            None => {}
                        }
                    }
                    let arms = arms.unwrap_or(Count::NONE);
                    if exhaustive {
                        arms
                    } else {
                        arms.or(Count::NONE)
                    }
                }
                ViewItem::Match(view_match) => view_match
                    .arms()
                    .map(|arm| self.block_count(arm.body()))
                    .reduce(Count::or)
                    .unwrap_or(Count::NONE),
                ViewItem::Part(part) => self.node_count(part.ty(), part.body()),
                ViewItem::Use(_) => Count::ONE,
                ViewItem::For(_) => Count::ANY,
                ViewItem::Property(_)
                | ViewItem::Handler(_)
                | ViewItem::TwoWayBinding(_)
                | ViewItem::Fill(_)
                | ViewItem::Override(_)
                | ViewItem::Replace(_) => Count::NONE,
            };
            sum.then(one)
        })
    }

    fn block_count(&self, block: Option<ViewBlock>) -> Count {
        let items: Vec<ViewItem> = block.iter().flat_map(|b| b.items()).collect();
        self.count(&items)
    }

    /// How many nodes a node of type `ty` mounts: a `Fragment` its children, a
    /// `SlotOutlet` what its slot takes, any other one.
    fn node_count(&self, ty: Option<TypePath>, body: Option<NodeBody>) -> Count {
        let segments: Vec<_> = ty.iter().flat_map(|t| t.segments()).collect();
        let [head] = segments.as_slice() else {
            return Count::ONE;
        };
        if self.symbols.contains_key(&head.text_range()) {
            return Count::ONE;
        }
        let node = self.env.widgets().widget(&head.text()).map(|w| w.node);
        let members = || -> Vec<ViewItem> { body.iter().flat_map(|b| b.members()).collect() };
        match node {
            Some(WidgetNode::Fragment) => {
                let members = members();
                let fills = members.iter().filter_map(|m| match m {
                    ViewItem::Fill(fill) => Some(self.block_count(fill.body())),
                    _ => None,
                });
                fills.fold(self.count(&members), Count::then)
            }
            Some(WidgetNode::Outlet) => members()
                .iter()
                .find_map(|m| match m {
                    ViewItem::Property(p) => p
                        .path()
                        .filter(|path| path_text(path) == "slot")
                        .and_then(|_| p.value())
                        .and_then(|v| slot_name(&v)),
                    _ => None,
                })
                .and_then(|name| self.own_slot(&name))
                .map_or(Count::NONE, |slot| Count::of(slot.cardinality)),
            _ => Count::ONE,
        }
    }

    /// The slot `name` of the component whose view this is.
    fn own_slot(&self, name: &str) -> Option<&'a HirSlot> {
        let own = self.flow.own?;
        self.env
            .component_slots(own)?
            .iter()
            .find(|s| s.name == name)
    }

    /// A `SlotOutlet` whose type is at `at`: its `slot:` names a slot of the
    /// component whose view it is in, placed by no other outlet and not once per
    /// item of a `for`.
    fn outlet(&mut self, body: Option<&NodeBody>, at: TextRange) {
        let value = body
            .into_iter()
            .flat_map(|b| b.members())
            .find_map(|m| match m {
                ViewItem::Property(p)
                    if p.path().is_some_and(|path| path_text(&path) == "slot") =>
                {
                    p.value()
                }
                _ => None,
            });
        let Some(value) = value else {
            self.diagnostics.push(Diagnostic::error(
                "E3501",
                at,
                "a `SlotOutlet` names the slot it places with `slot: name;`",
            ));
            return;
        };
        let range = value.syntax().text_range();
        let Some(name) = slot_name(&value) else {
            self.diagnostics.push(Diagnostic::error(
                "E3501",
                range,
                "`slot:` takes the name of a slot of this component",
            ));
            return;
        };
        let declared = self
            .flow
            .own
            .and_then(|own| self.env.component_slots(own))
            .unwrap_or_default();
        if !declared.iter().any(|s| s.name == name) {
            let own = self.flow.own;
            let candidates = declared.iter().map(|s| Candidate {
                name: &s.name,
                declared_at: own.and_then(|c| self.env.declaration_site(c, s.declared_at)),
            });
            let suggestions = nearest(&name, candidates);
            let message = if own.is_some() {
                format!("this component has no slot `{name}`")
            } else {
                format!(
                    "a `SlotOutlet` places a slot of the component whose view it is in, and this view belongs to none, so it has no slot `{name}`"
                )
            };
            let mut diagnostic = Diagnostic::error("E3501", range, message);
            attach(&mut diagnostic, range, &suggestions);
            self.diagnostics.push(diagnostic);
            return;
        }
        if self.repeated > 0 {
            self.diagnostics.push(Diagnostic::error(
                "E3502",
                at,
                format!(
                    "a `SlotOutlet` inside a `for` would place the nodes of `{name}` once per item"
                ),
            ));
        }
        match self.outlets.get(&name) {
            Some(first) => {
                let mut diagnostic = Diagnostic::error(
                    "E3502",
                    at,
                    format!("the slot `{name}` is already placed by another `SlotOutlet`"),
                );
                diagnostic
                    .related
                    .push(Related::new(*first, "first placed here"));
                self.diagnostics.push(diagnostic);
            }
            None => {
                self.outlets.insert(name, at);
            }
        }
    }

    /// `use T(args) { .. };` whose parent is `parent`: `T` is a template, each
    /// argument gives one of its parameters a value of its type (each lowered
    /// as the argument entry the template reads), every parameter without a
    /// default is given, and the body holds only `fill`, `override part` and
    /// `replace part`.
    fn template_use(&mut self, u: &TemplateUse, parent: Parent<'_>) {
        let args = u.args();
        let owner = u.ty().and_then(|ty| {
            let owner = self.owner_of(&ty)?;
            if owner.component.is_some_and(|c| self.env.is_template(c)) {
                return Some(owner);
            }
            self.diagnostics.push(Diagnostic::error(
                "E2103",
                ty.syntax().text_range(),
                format!(
                    "`{0}` is no template; `use` places a template, and `{0} {{ .. }}` a node",
                    owner.name
                ),
            ));
            None
        });
        let Some(owner) = owner else {
            for (_, value) in &args {
                let _ = self.cx.infer_expr(value, None);
            }
            return;
        };
        let component = owner.component.expect("a template");
        let at = u.syntax().text_range();
        let mut given: Vec<Option<TextRange>> = vec![None; owner.inputs.len()];
        let mut next = 0;
        for (label, value) in &args {
            let errors = self.error_count();
            let range = value.syntax().text_range();
            let index = match label {
                Some(label) => {
                    let text = label.text();
                    let name = text.trim_start_matches("r#");
                    let found = owner.inputs.iter().position(|i| i.name == name);
                    if found.is_none() {
                        let candidates = owner.inputs.iter().map(|i| Candidate {
                            name: &i.name,
                            declared_at: self.env.declaration_site(component, i.declared_at),
                        });
                        let suggestions = nearest(name, candidates);
                        let mut diagnostic = Diagnostic::error(
                            "E3101",
                            label.text_range(),
                            format!("`{}` has no parameter `{name}`", owner.name),
                        );
                        attach(&mut diagnostic, label.text_range(), &suggestions);
                        self.diagnostics.push(diagnostic);
                    }
                    found
                }
                None if next < owner.inputs.len() => {
                    next += 1;
                    Some(next - 1)
                }
                None => {
                    self.diagnostics.push(Diagnostic::error(
                        "E2103",
                        range,
                        format!(
                            "`{}` takes {} arguments, and this is one more",
                            owner.name,
                            owner.inputs.len()
                        ),
                    ));
                    None
                }
            };
            let Some(index) = index else {
                let _ = self.cx.infer_expr(value, None);
                continue;
            };
            let input = &owner.inputs[index];
            if let Some(first) = given[index] {
                let mut diagnostic = Diagnostic::error(
                    "E3102",
                    range,
                    format!("the parameter `{}` is already given", input.name),
                );
                diagnostic
                    .related
                    .push(Related::new(first, "first given here"));
                self.diagnostics.push(diagnostic);
            }
            given[index] = Some(range);
            let _ = match &input.ty {
                want if want.has_unknown() => self.cx.infer_expr(value, None),
                want if is_text(want) => self.cx.infer_text_value(value, want),
                want => self.cx.infer_promoted(value, want),
            };
            // The inlined template reads its parameter through this argument.
            self.region_entry(errors, "arg", range, &RegionEntry::Value(value));
            let carry = self.cx.carry(value.syntax());
            if carry.spelled.is_some() || !carry.names.is_empty() {
                self.flow.sinks.push(Sink {
                    carry,
                    at: range,
                    to: SinkTo::Input((component, index)),
                });
            }
        }
        let missing: Vec<String> = owner
            .inputs
            .iter()
            .zip(&given)
            .filter(|(input, given)| given.is_none() && !input.has_default)
            .map(|(input, _)| format!("`{}`", input.name))
            .collect();
        if !missing.is_empty() {
            let range = u.arg_list().map_or(at, |l| l.text_range());
            self.diagnostics.push(Diagnostic::error(
                "E2103",
                range,
                format!(
                    "`{}` needs {}, which has no default",
                    owner.name,
                    missing.join(", ")
                ),
            ));
        }
        let body = u.body();
        let members: Vec<ViewItem> = body.iter().flat_map(|b| b.members()).collect();
        let mut stray = false;
        for member in &members {
            if !matches!(
                member,
                ViewItem::Fill(_) | ViewItem::Override(_) | ViewItem::Replace(_)
            ) {
                stray = true;
                self.diagnostics.push(Diagnostic::error(
                    "E3601",
                    member.syntax().text_range(),
                    "a `use` body holds only `fill`, `override part` and `replace part`;                      the template's parameters take its values",
                ));
            }
        }
        if !stray {
            let type_at = u.ty().map_or(at, |t| t.syntax().text_range());
            self.slots(&owner, body.as_ref(), type_at);
        }
        let scope = Scope {
            owner: Some(&owner),
            outer: parent,
            inner: Parent::Unknown,
        };
        let clauses = members.into_iter().filter(|m| {
            matches!(
                m,
                ViewItem::Fill(_) | ViewItem::Override(_) | ViewItem::Replace(_)
            )
        });
        self.items(clauses, scope);
    }

    /// The part `name` of the component or template `owner` places, which an
    /// `override part`/`replace part` (`what`) names: a node of another type
    /// exposes none (`E2103`), an unknown name is `E3501`.
    fn part_of(
        &mut self,
        name: Option<SyntaxToken>,
        owner: Option<&Owner<'a>>,
        what: &str,
    ) -> Option<PartInfo> {
        let name = name?;
        let text = name.text();
        let text = text.trim_start_matches("r#");
        let range = name.text_range();
        let Some((owner, component)) = owner.and_then(|o| Some((o, o.component?))) else {
            let message = match owner {
                Some(owner) => format!(
                    "`{what} part` names a part of the component or template a node                      places, and `{}` is a widget, which has none",
                    owner.name
                ),
                None => format!(
                    "`{what} part` names a part of the component or template whose node                      or `use` it is in"
                ),
            };
            self.diagnostics
                .push(Diagnostic::error("E2103", range, message));
            return None;
        };
        let parts = self.env.component_parts(component);
        if let Some(part) = parts.iter().find(|p| p.name == text) {
            return Some(part.clone());
        }
        let candidates = parts.iter().map(|p| Candidate {
            name: &p.name,
            declared_at: self.env.declaration_site(component, p.at),
        });
        let suggestions = nearest(text, candidates);
        let mut diagnostic = Diagnostic::error(
            "E3501",
            range,
            format!("`{}` has no part `{text}`", owner.name),
        );
        attach(&mut diagnostic, range, &suggestions);
        self.diagnostics.push(diagnostic);
        None
    }

    /// `override part name { .. }` in the body of a node or `use` of `owner`:
    /// its bindings and handlers are the caller's, checked against the part's
    /// node type.
    fn part_override(&mut self, o: &PartOverride, owner: Option<&Owner<'a>>) {
        let part = self.part_of(o.name(), owner, "override");
        let part_owner = part.and_then(|p| self.part_owner(&p.ty));
        let scope = Scope {
            owner: part_owner.as_ref(),
            outer: Parent::Unknown,
            inner: Parent::Unknown,
        };
        self.items(o.members(), scope);
    }

    /// `replace part name { .. }` in the body of a node or `use` of `owner`:
    /// the caller's structure, one node in the part's place.
    fn part_replace(&mut self, r: &PartReplace, owner: Option<&Owner<'a>>) {
        let part = self.part_of(r.name(), owner, "replace");
        let Some(block) = r.body() else {
            return;
        };
        let count = self.block_count(Some(block.clone()));
        if let Some(part) = part
            && count != Count::ONE
        {
            self.diagnostics.push(Diagnostic::error(
                "E3502",
                block.syntax().text_range(),
                format!(
                    "the part `{}` is one node, so what replaces it mounts one node, not {}",
                    part.name,
                    count.describe()
                ),
            ));
        }
        let scope = Scope {
            owner: None,
            outer: Parent::Unknown,
            inner: Parent::Unknown,
        };
        self.items(block.items(), scope);
    }

    /// The schema of a part's node type `ty`, written in its declaring view:
    /// a component of the module, else a registered widget. A type this view
    /// cannot see is not checked; the declaring view reports a bad one.
    fn part_owner(&mut self, ty: &TypePath) -> Option<Owner<'a>> {
        let head = ty.segments().next()?;
        if !self.symbols.contains_key(&head.text_range())
            && widget::widget(self.env.widgets(), &head.text()).is_none()
        {
            return None;
        }
        self.owner_of(ty)
    }

    /// `on event(payload) { body }`: the payload pattern binds the event's payload
    /// for the body.
    fn handler(&mut self, handler: &EventHandler, owner: Option<&Owner<'a>>) {
        let errors = self.error_count();
        let payload = handler
            .event()
            .map_or(Ty::Unknown, |event| self.event_payload(&event, owner));
        if let Some(pattern) = handler.payload() {
            self.cx.bind_pattern(pattern.syntax(), &payload);
            self.cx
                .check_irrefutable(pattern.syntax(), "a handler payload pattern");
        }
        let Some(body) = handler.body() else {
            return;
        };
        self.cx.check_handler(&body);
        let Some(sink) = &self.sink else {
            return;
        };
        let event = handler
            .event()
            .map_or_else(String::new, |e| e.text().to_string());
        let def = Def {
            name: format!("{}.on_{}", sink.component, event.trim_start_matches("r#")),
            kind: FunctionKind::Handler,
            symbol: None,
            module: sink.module,
            into: None,
        };
        let at = handler.syntax().text_range();
        let mut b = sink.builder.borrow_mut();
        let func = if self.error_count() > errors {
            let params = 1 + self.regions.len() as u32;
            unsupported_with(&mut b, def, params, "has type errors", at)
        } else {
            let pattern = handler.payload().map(|p| p.syntax().clone());
            let payload = pattern.as_ref().map(|p| (p, &payload));
            lower_handler(&mut b, &self.cx, def, payload, &self.regions, &body)
        };
        b.handler(at, func);
    }

    /// The errors reported so far.
    fn error_count(&self) -> usize {
        let is_error = |d: &&Diagnostic| d.severity == Severity::Error;
        self.cx.diagnostics().iter().filter(is_error).count()
            + self.diagnostics.iter().filter(is_error).count()
    }

    /// The payload type of the event `event` names on a node of `owner`: one the user
    /// component declares, else one of the node's schema — its own or a standard event,
    /// whose payload is a prelude record (`Unit` for a bare signal). Any other name is
    /// `E3202`; a node type without a schema is not checked.
    fn event_payload(&mut self, event: &SyntaxToken, owner: Option<&Owner<'a>>) -> Ty {
        if let Some(id) = self.symbols.get(&event.text_range()).copied() {
            return match self.env.record_fields(id) {
                Some(_) => Ty::named(id),
                None => Ty::Unknown,
            };
        }
        let Some(owner) = owner else {
            return Ty::Unknown;
        };
        let text = event.text();
        let name = text.trim_start_matches("r#");
        if let Some(spec) = owner.schema.event(name) {
            return match spec.payload {
                Some(payload) => self
                    .env
                    .standard_type(payload)
                    .map_or(Ty::Unknown, Ty::named),
                None => Ty::Unit,
            };
        }
        let declared = owner
            .component
            .and_then(|c| self.env.component_events(c))
            .unwrap_or_default();
        let candidates = declared
            .iter()
            .map(|e| Candidate {
                name: &e.name,
                declared_at: owner
                    .component
                    .and_then(|c| self.env.declaration_site(c, e.declared_at)),
            })
            .chain(owner.schema.event_names().map(|name| Candidate {
                name,
                declared_at: None,
            }));
        let suggestions = nearest(name, candidates);
        let range = event.text_range();
        let message = if owner.component.is_some() {
            format!(
                "`{}` has no event `{name}`: it is neither a standard event nor one the component declares",
                owner.name
            )
        } else {
            format!("`{}` has no event `{name}`", owner.name)
        };
        let mut diagnostic = Diagnostic::error("E3202", range, message);
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
        let errors = self.error_count();
        let declared = binding
            .path()
            .and_then(|path| self.declared(&path, scope, bound));
        let Some(value) = binding.value() else {
            return;
        };
        if scope.owner.is_some_and(Owner::is_outlet) {
            // `slot:` names a slot, checked by `outlet`; it is no value.
            return;
        }
        if let (Some(owner), Some(_)) = (scope.owner, &declared)
            && binding
                .path()
                .is_some_and(|path| path_text(&path) == "styles")
        {
            // Style names are no values: they name what the node applies.
            self.check_styles(&value, owner);
            return;
        }
        let transition = binding.path().filter(is_transition);
        let have = match declared.as_ref().and_then(|d| d.ty.as_ref()) {
            Some(want) if is_text(want) => self.cx.infer_text_value(&value, want),
            Some(want) => self.cx.infer_promoted(&value, want),
            None => self.cx.infer_expr(&value, None),
        };
        if let (Some(declared), Some(path)) = (&declared, binding.path())
            && declared.localizable
        {
            let strict = self.env.profile().i18n_strict;
            let cx = &self.cx;
            access::check_text(
                &path_text(&path),
                &value,
                &|e| cx.type_of(e).cloned(),
                strict,
                &mut self.diagnostics,
            );
        }
        let at = value.syntax().text_range();
        if let (Some(path), Some(_), Some(want)) = (
            &transition,
            &declared,
            self.env.standard_type("Transition").map(Ty::named),
        ) && !have.has_unknown()
            && have != want
        {
            let message = format!(
                "`{}` takes a `Transition`, not `{}`",
                path_text(path),
                self.cx.describe(&have),
            );
            self.diagnostics
                .push(Diagnostic::error("E3703", at, message));
        }
        let (Some(declared), Some(path)) = (&declared, binding.path()) else {
            return;
        };
        if matches!(declared.basis, Basis::Input(_)) {
            // The inlined component reads its input through this argument.
            self.region_entry(errors, "arg", at, &RegionEntry::Value(&value));
        }
        if scope.owner.is_some_and(|owner| owner.reads_control(&path)) {
            // The native node reads its current value or range here.
            self.region_entry(errors, "value", at, &RegionEntry::Value(&value));
        }
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
        let errors = self.error_count();
        let source = binding.source().map(|source| self.cx.infer_lens(&source));
        let Some(path) = binding.target() else {
            return;
        };
        let Some(declared) = self.declared(&path, scope, bound) else {
            return;
        };
        if let (Basis::Input(_), true, Some(lens), None) = (
            declared.basis,
            declared.two_way,
            binding.source(),
            binding.using_ty(),
        ) {
            self.write_back(errors, binding, &lens, true, None);
        }
        // `using C` converts through `C: TwoWayConverter<Model, View>`.
        let converter = match (binding.using_ty(), &declared.ty, &source) {
            (Some(using), Some(view), Some(model)) => {
                let converter = self.cx.converter(&using, model, view);
                if converter.is_none() {
                    // Reported; the binding is not mounted.
                    return;
                }
                converter
            }
            _ => None,
        };
        if let (Some(owner), true, Some(lens)) = (scope.owner, declared.two_way, binding.source())
            && owner.component.is_none()
            && let [name] = path_segments(&path).as_slice()
            && owner.schema.native().write_back(name).is_some()
        {
            // The native widget's change event writes the source; a control
            // reads the source as its current value.
            let reads = owner.reads_control(&path);
            self.write_back(errors, binding, &lens, reads, converter.as_ref());
        }
        if let (Some(want), Some(have), None) = (&declared.ty, &source, binding.using_ty())
            && !want.has_unknown()
            && !have.has_unknown()
            && want != have
        {
            let (expected, actual) = (self.cx.describe(want), self.cx.describe(have));
            let message = format!(
                "`bind` needs both sides to be the same type: `{}` is `{expected}`, the source is `{actual}`; convert with `using`",
                path_text(&path),
            );
            self.diagnostics.push(
                Diagnostic::error("E2103", binding.syntax().text_range(), message)
                    .expecting([expected], actual),
            );
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
                .push(Related::new(*first, "first bound here"));
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
            && let Some(group) = widget::child_props(self.env.widgets(), prefix)
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
                localizable: false,
            });
        }
        let names: Vec<&str> = segments.iter().map(String::as_str).collect();
        match owner.schema.lookup(&names) {
            PropLookup::Known(spec) => Some(Declared {
                ty: value_ty(spec.ty),
                two_way: spec.two_way,
                basis: Basis::of(spec.percent_basis),
                localizable: spec.localizable,
            }),
            PropLookup::Unknown if names.len() == 2 && names[0] == "transition" => {
                let message = format!(
                    "`{}` has no animatable property `{}` for `{text}`",
                    owner.name, names[1],
                );
                self.diagnostics
                    .push(Diagnostic::error("E3703", range, message));
                None
            }
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
                    candidates.extend(owner.inputs.iter().map(|i| {
                        Candidate {
                            name: &i.name,
                            declared_at: owner
                                .component
                                .and_then(|c| self.env.declaration_site(c, i.declared_at)),
                        }
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
                        ty: value_ty(spec.ty),
                        two_way: spec.two_way,
                        basis: Basis::of(spec.percent_basis),
                        localizable: spec.localizable,
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

    /// An `if`/`else if`/`else` chain: every condition is typed first, then the
    /// chain's arm choice lowers as one region entry, then each arm body walks.
    fn view_if(&mut self, view_if: &ViewIf, scope: Scope<'_, 'a>) {
        let errors = self.error_count();
        let mut conditions: Vec<Option<Expr>> = Vec::new();
        let mut blocks: Vec<Option<ViewBlock>> = Vec::new();
        let mut link = Some(view_if.clone());
        while let Some(arm) = link.take() {
            let condition = arm.condition();
            if let Some(condition) = &condition {
                let _ = self.cx.infer_expr(condition, Some(&Ty::Bool));
            }
            self.preserve(&arm);
            conditions.push(condition);
            blocks.push(arm.then_block());
            match arm.else_branch() {
                Some(ElseBranch::If(nested)) => link = Some(nested),
                Some(ElseBranch::Block(block)) => {
                    conditions.push(None);
                    blocks.push(Some(block));
                }
                None => {}
            }
        }
        let at = view_if.syntax().text_range();
        self.region_entry(errors, "if", at, &RegionEntry::Conditions(&conditions));
        for block in blocks.into_iter().flatten() {
            self.items(block.items(), scope);
        }
    }

    /// Records a `preserve` identity; one a sibling branch of the component
    /// already names is `E3301`.
    fn preserve(&mut self, view_if: &ViewIf) {
        let (Some(name), Some(token)) = (view_if.preserve_name(), view_if.preserve()) else {
            return;
        };
        let at = token.text_range();
        match self.preserves.get(&name) {
            Some(first) => {
                let mut diagnostic = Diagnostic::error(
                    "E3301",
                    at,
                    format!("the preserve identity \"{name}\" is already used in this component"),
                );
                diagnostic
                    .related
                    .push(Related::new(*first, "first used here"));
                self.diagnostics.push(diagnostic);
            }
            None => {
                self.preserves.insert(name, at);
            }
        }
    }

    fn view_for(&mut self, view_for: &ViewFor, scope: Scope<'_, 'a>) {
        let errors = self.error_count();
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
            let ty = self.cx.infer_expr(&key, None);
            if let Err(why) = super::stable_key::stable_key(self.cx.env(), &ty) {
                let message = format!(
                    "a `for` key is `StableKey`, but `{}` is not: {why}",
                    self.cx.describe(&ty)
                );
                self.diagnostics.push(Diagnostic::error(
                    "E2701",
                    key.syntax().text_range(),
                    message,
                ));
            }
        }
        let pattern = view_for.pattern().map(|p| p.syntax().clone());
        if let Some(iterable) = view_for.iterable() {
            let at = iterable.syntax().text_range();
            self.region_entry(errors, "for", at, &RegionEntry::Items(&iterable));
        }
        if let (Some(pattern), Some(key)) = (&pattern, view_for.key()) {
            let entry = RegionEntry::Key {
                pattern,
                element: &element,
                key: &key,
            };
            self.region_entry(errors, "key", key.syntax().text_range(), &entry);
        }
        if let Some(body) = view_for.body() {
            let pattern = pattern.unwrap_or_else(|| view_for.syntax().clone());
            self.regions.push((pattern, element));
            self.repeated += 1;
            self.items(body.items(), scope);
            self.repeated -= 1;
            self.regions.pop();
        }
    }

    fn view_match(&mut self, view_match: &ViewMatch, scope: Scope<'_, 'a>) {
        let errors = self.error_count();
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
        let mut arms: Vec<(SyntaxNode, Option<Expr>)> = Vec::new();
        let mut bodies: Vec<(SyntaxNode, Option<ViewBlock>)> = Vec::new();
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
            if let Some(pattern) = &pattern {
                self.cx
                    .add_arm(&mut check, pattern.syntax(), &ty, guard.is_some());
                arms.push((pattern.syntax().clone(), guard));
                bodies.push((pattern.syntax().clone(), arm.body()));
            }
        }
        let at = scrutinee
            .as_ref()
            .map_or(view_match.syntax().text_range(), |s| {
                s.syntax().text_range()
            });
        self.cx.finish_match(check, &ty, at);
        if let Some(scrutinee) = &scrutinee {
            self.region_entry(errors, "match", at, &RegionEntry::Value(scrutinee));
        }
        let entry = RegionEntry::Arms {
            arms: &arms,
            ty: &ty,
        };
        let origin = view_match.syntax().text_range();
        self.region_entry(errors, "arm", origin, &entry);
        for (pattern, body) in bodies {
            let Some(body) = body else {
                continue;
            };
            self.regions.push((pattern, ty.clone()));
            self.items(body.items(), scope);
            self.regions.pop();
        }
    }

    /// Lowers a region entry registered at `at` in the component's handler
    /// table; one whose expressions reported errors since `errors` cannot run.
    /// The functions a `bind` to a two-way property lowers to: when `reads`,
    /// the value the component input or native control reads (the source's
    /// current value, at the source), and the handler of the paired event
    /// writing the event's value back to the source (at the binding).
    fn write_back(
        &mut self,
        errors: usize,
        binding: &TwoWayBinding,
        source: &AssignablePath,
        reads: bool,
        converter: Option<&Converter>,
    ) {
        if reads {
            let at = source.syntax().text_range();
            let entry = RegionEntry::Lens(source, converter.map(|c| &c.to_view));
            self.region_entry(errors, "arg", at, &entry);
        }
        let Some(sink) = &self.sink else {
            return;
        };
        let def = Def {
            name: format!("{}.write_back", sink.component),
            kind: FunctionKind::Handler,
            symbol: None,
            module: sink.module,
            into: None,
        };
        let at = binding.syntax().text_range();
        let mut b = sink.builder.borrow_mut();
        let func = if self.error_count() > errors {
            let params = 1 + self.regions.len() as u32;
            unsupported_with(&mut b, def, params, "has type errors", at)
        } else {
            let to_model = converter.map(|c| &c.to_model);
            lower_write_back(&mut b, &self.cx, def, &self.regions, source, to_model, at)
        };
        b.handler(at, func);
    }

    fn region_entry(&mut self, errors: usize, what: &str, at: TextRange, entry: &RegionEntry<'_>) {
        let failed = self.error_count() > errors;
        self.entry_at(failed, what, Site::own(at), entry, at);
    }

    /// Lowers `entry`, whose source is at `at`, at `site`; one that `failed`
    /// to type lowers as unsupported.
    fn entry_at(
        &mut self,
        failed: bool,
        what: &str,
        site: Site,
        entry: &RegionEntry<'_>,
        at: TextRange,
    ) {
        let Some(sink) = &self.sink else {
            return;
        };
        let def = Def {
            name: format!("{}.{what}", sink.component),
            kind: FunctionKind::RegionEntry,
            symbol: None,
            module: sink.module,
            into: None,
        };
        let mut b = sink.builder.borrow_mut();
        let func = if failed {
            let params = self.regions.len() as u32 + u32::from(entry.takes_subject());
            unsupported_with(&mut b, def, params, "has type errors", at)
        } else {
            lower_region_entry(&mut b, &self.cx, def, &self.regions, entry, at)
        };
        b.handler_at(site, func);
    }
}

/// Whether `item` is a structure item, one that fills a slot even when it
/// mounts no node (an empty `Fragment`, a `for` over nothing).
fn is_structure(item: &ViewItem) -> bool {
    matches!(
        item,
        ViewItem::Named(_)
            | ViewItem::Anonymous(_)
            | ViewItem::Part(_)
            | ViewItem::Use(_)
            | ViewItem::If(_)
            | ViewItem::For(_)
            | ViewItem::Match(_)
    )
}

/// The slot name `slot: name;` spells: a bare identifier.
fn slot_name(value: &Expr) -> Option<String> {
    let path = PathExpr::cast(value.syntax().clone())?;
    let segments: Vec<_> = path.segments().collect();
    match segments.as_slice() {
        [name] => Some(name.text().trim_start_matches("r#").to_string()),
        _ => None,
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

/// The segments of a property path, raw-identifier prefixes stripped.
fn path_segments(path: &PropertyPath) -> Vec<String> {
    path.segments()
        .map(|t| t.text().trim_start_matches("r#").to_string())
        .collect()
}

/// Whether `path` is a member of the `transition.*` group.
fn is_transition(path: &PropertyPath) -> bool {
    path.segments()
        .next()
        .is_some_and(|head| head.text() == "transition")
        && path.segments().count() == 2
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
            Ty::Dp | Ty::Px | Ty::Sp | Ty::Em | Ty::MixedLength | Ty::Named(..) | Ty::Unknown => {
                true
            }
            Ty::Option(inner) | Ty::List(inner) => holds_length(Some(inner)),
            Ty::Tuple(tys) => tys.iter().any(|t| holds_length(Some(t))),
            _ => false,
        },
    }
}
