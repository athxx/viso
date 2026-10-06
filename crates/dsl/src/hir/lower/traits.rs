//! Traits, impls, generic parameters and aliases (§26, §30–§32, §78, §79):
//! collected into the package's [`TraitTable`] as each module's scope is
//! built, then checked as the module lowers — each impl against its trait,
//! each bound against a trait, impls against each other — and their member
//! bodies typed and lowered like any callable's.

use std::collections::HashMap;

use crate::ast::{AssocItem, AstNode, CompilationUnit, ImplDecl, Item, TraitDecl};
use crate::diag::{Diagnostic, Related};
use crate::hir::effect::{BodyContext, EffectClass};
use crate::hir::generic::{
    Alias, AutoTrait, Bound, GenericParam, Generics, ImplInfo, ItemKind, Owner, ParamKind,
    TraitInfo, TraitRef, apply,
};
use crate::resolve::{NameInterner, SymbolId, SymbolTable};
use crate::syntax::{SyntaxKind, SyntaxNode, SyntaxToken, TextRange};

use super::{
    Callable, CapabilityGraphBuilder, Declarations, Def, FunctionKind, ModuleEnv, ModuleScope,
    PercentSources, Ty, check_callable, check_const,
};

/// The symbol declared at each name span of a module.
pub(super) type DeclAt = HashMap<TextRange, SymbolId>;

/// How many instantiations deep a chain of generic calls may go before it is
/// taken to recurse without bound (`E2203`).
const MAX_INSTANCE_DEPTH: u32 = 32;

/// How many instantiations a package may make in all.
const MAX_INSTANCES: usize = 4096;

/// How many type nodes an instantiation's arguments may have in all.
const MAX_INSTANCE_SIZE: usize = 256;

/// The body of a generic callable, which each instantiation types and lowers
/// again with its parameters substituted.
pub(super) struct GenericBody {
    /// The module declaring it.
    module: usize,
    /// The component it is a member of.
    component: Option<SymbolId>,
    /// Its `fn`/`action`/`task` declaration.
    node: SyntaxNode,
    /// Its name in the Behavior IR.
    name: String,
}

/// Records the body of `symbol` when it is generic.
fn record_body(
    decls: &mut Declarations,
    scope: &ModuleScope,
    symbol: SymbolId,
    node: &SyntaxNode,
    component: Option<SymbolId>,
    name: String,
) {
    let callable = matches!(
        node.kind(),
        SyntaxKind::FnDecl | SyntaxKind::ActionDecl | SyntaxKind::TaskDecl
    );
    if let (true, Some(module), Some(_)) = (callable, scope.home, decls.traits.generics_of(symbol))
    {
        decls.bodies.insert(
            symbol,
            GenericBody {
                module,
                component,
                node: node.clone(),
                name,
            },
        );
    }
}

/// The callable a generic body declares.
fn callable_of(body: &GenericBody, symbol: SymbolId) -> Option<Callable> {
    use crate::ast::{ActionDecl, FnDecl, TaskDecl};
    let node = body.node.clone();
    let (kind, context, params, ret, block, clause) = match node.kind() {
        SyntaxKind::FnDecl => {
            let f = FnDecl::cast(node)?;
            (
                FunctionKind::Fn,
                BodyContext::Fn,
                f.params(),
                f.return_type(),
                f.body(),
                f.capability_clause(),
            )
        }
        SyntaxKind::ActionDecl => {
            let a = ActionDecl::cast(node)?;
            (
                FunctionKind::Action,
                BodyContext::Action,
                a.params(),
                a.return_type(),
                a.body(),
                a.capability_clause(),
            )
        }
        SyntaxKind::TaskDecl => {
            let t = TaskDecl::cast(node)?;
            (
                FunctionKind::Task,
                BodyContext::Task,
                t.params(),
                t.return_type(),
                t.body(),
                t.capability_clause(),
            )
        }
        _ => return None,
    };
    Some(Callable {
        name: body.name.clone(),
        kind,
        context,
        symbol: Some(symbol),
        params,
        ret,
        body: block,
        clause,
    })
}

/// Lowers every instantiation the package's bodies make, and those they
/// make in turn, in the order they are first reserved. A chain deeper than
/// [`MAX_INSTANCE_DEPTH`] recurses without bound (`E2203`).
pub(super) fn lower_instances(
    decls: &Declarations,
    envs: &[Option<ModuleEnv<'_>>],
    resolved: &[crate::resolve::ResolvedModule],
    behavior: &std::cell::RefCell<crate::behavior::lower::ProgramBuilder>,
    per_module: &mut [Vec<Diagnostic>],
) {
    let mut reported = std::collections::HashSet::new();
    let mut lowered = 0usize;
    loop {
        let pending = behavior.borrow_mut().take_pending();
        if pending.is_empty() {
            break;
        }
        for instance in pending {
            let fail = |reason: &str| {
                let def = Def {
                    name: String::new(),
                    kind: FunctionKind::Fn,
                    symbol: None,
                    module: 0,
                    into: Some(instance.func),
                };
                crate::behavior::lower::unsupported(
                    &mut behavior.borrow_mut(),
                    def,
                    reason,
                    TextRange::empty(0.into()),
                );
            };
            let Some(body) = decls.bodies.get(&instance.symbol) else {
                fail("has no body to run");
                continue;
            };
            let (Some(env), Some(module)) = (
                envs.get(body.module).and_then(Option::as_ref),
                resolved.get(body.module),
            ) else {
                fail("has no body to run");
                continue;
            };
            lowered += 1;
            let size: usize = instance
                .args
                .iter()
                .map(|a| a.size(MAX_INSTANCE_SIZE + 1))
                .sum();
            if instance.depth > MAX_INSTANCE_DEPTH
                || lowered > MAX_INSTANCES
                || size > MAX_INSTANCE_SIZE
            {
                if reported.insert(instance.symbol)
                    && let Some(out) = per_module.get_mut(body.module)
                {
                    let at =
                        name_token(&body.node).map_or(body.node.text_range(), |t| t.text_range());
                    out.push(Diagnostic::error(
                        "E2203",
                        at,
                        format!(
                            "instantiating `{}` recurses without bound: each instantiation needs                              another with larger type arguments",
                            body.name
                        ),
                    ));
                }
                fail("instantiates itself without bound");
                continue;
            }
            if instance
                .args
                .iter()
                .any(|a| a.has_unknown() || a.has_param())
            {
                fail("has an undetermined type argument");
                continue;
            }
            let Some(callable) = callable_of(body, instance.symbol) else {
                fail("has no body to run");
                continue;
            };
            let subst: crate::hir::generic::Subst = decls
                .traits
                .generics_of(instance.symbol)
                .map(|g| g.symbols())
                .unwrap_or_default()
                .into_iter()
                .zip(instance.args.iter().cloned())
                .collect();
            if let Some(component) = body.component {
                env.component.set(component);
            }
            let args: Vec<String> = instance
                .args
                .iter()
                .map(|a| decls.types.describe(a))
                .collect();
            let def = Def {
                name: format!("{}<{}>", body.name, args.join(", ")),
                kind: callable.kind,
                symbol: None,
                module: body.module,
                into: Some(instance.func),
            };
            behavior.borrow_mut().set_depth(instance.depth);
            // The generic body reported its diagnostics when it was typed once.
            super::check_signature(
                &module.refs,
                env,
                &callable,
                &mut Vec::new(),
                &mut PercentSources::default(),
                Some(def),
                &subst,
            );
        }
    }
    behavior.borrow_mut().set_depth(0);
}

/// The generic declarations of `cu`, outermost first: each item, and each
/// member of a component, a system, a trait or an impl, that has parameters.
fn generic_nodes(cu: &CompilationUnit) -> Vec<SyntaxNode> {
    let mut out = Vec::new();
    for item in cu.items() {
        let node = match item {
            Item::Export(e) => match e.declaration() {
                Some(inner) => inner.syntax().clone(),
                None => continue,
            },
            other => other.syntax().clone(),
        };
        out.push(node.clone());
        for member in node.children() {
            if matches!(
                member.kind(),
                SyntaxKind::FnDecl
                    | SyntaxKind::ActionDecl
                    | SyntaxKind::TaskDecl
                    | SyntaxKind::ConstDecl
                    | SyntaxKind::AssocTypeDecl
            ) {
                out.push(member);
            }
        }
    }
    out
}

/// The `Self` keyword token of a trait or an impl.
fn self_keyword(node: &SyntaxNode) -> Option<SyntaxToken> {
    let kw = match node.kind() {
        SyntaxKind::TraitDecl => SyntaxKind::TraitKw,
        SyntaxKind::ImplDecl => SyntaxKind::ImplKw,
        _ => return None,
    };
    node.children_with_tokens()
        .into_iter()
        .filter_map(|e| e.as_token().cloned())
        .find(|t| t.kind() == kw)
}

/// Makes the module's type annotations see its generic parameters and each
/// trait's `Self` as [`Ty::Param`], and each impl's `Self` as its target.
pub(super) fn prepare(cu: &CompilationUnit, decl_at: &DeclAt, scope: &mut ModuleScope) {
    let mut params = std::collections::HashSet::new();
    let mut impls = Vec::new();
    for node in generic_nodes(cu) {
        for name in crate::resolve::generic_param_names(&node) {
            if let Some(&p) = decl_at.get(&name.text_range()) {
                params.insert(p);
            }
        }
        if node.kind() == SyntaxKind::ConstDecl
            && let Some(name) = name_token(&node)
            && let Some(&c) = decl_at.get(&name.text_range())
        {
            scope.declared.entry(node.text_range()).or_insert(c);
        }
        if let Some(kw) = self_keyword(&node)
            && let Some(&p) = decl_at.get(&kw.text_range())
        {
            if node.kind() == SyntaxKind::TraitDecl {
                params.insert(p);
            } else {
                impls.push((p, node));
            }
        }
    }
    let to_param = |ty: &mut Ty| {
        if let Ty::Named(id, args) = ty
            && args.is_empty()
            && params.contains(id)
        {
            *ty = Ty::Param(*id);
        }
    };
    for ty in scope.nominal.values_mut() {
        to_param(ty);
    }
    for (p, node) in impls {
        let target = ImplDecl::cast(node)
            .and_then(|i| i.target())
            .map_or(Ty::Unknown, |t| scope.annotation(&t));
        scope.self_types.insert(p, target.clone());
        for ty in scope.nominal.values_mut() {
            if *ty == Ty::named(p) {
                *ty = target.clone();
            }
        }
    }
}

/// Expands every alias, and every associated type a known type gives, in
/// the types the package declares, once every module's are collected.
pub(super) fn normalize_declarations(decls: &mut Declarations) {
    let table = std::mem::take(&mut decls.traits);
    let n = |t: &mut Ty| *t = table.normalize(t);
    for (ps, r) in decls.signatures.values_mut() {
        ps.iter_mut().for_each(n);
        n(r);
    }
    for facts in decls.facts.values_mut() {
        n(&mut facts.ty);
    }
    for fields in decls.types.records.values_mut() {
        fields.iter_mut().for_each(|f| n(&mut f.ty));
    }
    for variants in decls.types.enums.values_mut() {
        for v in variants {
            match &mut v.payload {
                crate::hir::infer::VariantPayload::Tuple(ts) => ts.iter_mut().for_each(n),
                crate::hir::infer::VariantPayload::Record(fs) => {
                    fs.iter_mut().for_each(|f| n(&mut f.ty));
                }
                crate::hir::infer::VariantPayload::Unit => {}
            }
        }
    }
    for (component, inputs) in decls.inputs.iter_mut() {
        // A use of a generic component takes any argument for an input of a
        // parameter's type.
        let own = table
            .generics_of(*component)
            .map(|g| g.symbols())
            .unwrap_or_default();
        for input in inputs {
            n(&mut input.ty);
            input.ty = input.ty.subst(&|p| own.contains(&p).then_some(Ty::Unknown));
        }
    }
    decls.traits = table;
}

/// The parameters and inline bounds of the `<..>` list under `node`.
fn params_of(node: &SyntaxNode, decl_at: &DeclAt, scope: &ModuleScope) -> Generics {
    let mut generics = Generics::default();
    let lists = node
        .children()
        .into_iter()
        .filter(|c| c.kind() == SyntaxKind::GenericParams);
    for g in lists.flat_map(|l| l.children()) {
        if g.kind() != SyntaxKind::GenericParam {
            continue;
        }
        let tokens: Vec<SyntaxToken> = g
            .children_with_tokens()
            .into_iter()
            .filter_map(|e| e.as_token().cloned())
            .collect();
        let Some(name) = tokens
            .iter()
            .find(|t| matches!(t.kind(), SyntaxKind::Ident | SyntaxKind::RawIdent))
        else {
            continue;
        };
        let Some(&symbol) = decl_at.get(&name.text_range()) else {
            continue;
        };
        let is_const = tokens.iter().any(|t| t.kind() == SyntaxKind::ConstKw);
        let kind = if is_const {
            ParamKind::Const(scope.annotation_of(&g))
        } else {
            for bound in bounds_after_colon(&g) {
                if let Some(tr) = trait_ref(&bound, scope) {
                    generics.bounds.push(Bound {
                        subject: Ty::Param(symbol),
                        tr,
                    });
                }
            }
            ParamKind::Type
        };
        generics.params.push(GenericParam {
            symbol,
            name: name.text().to_string(),
            kind,
        });
    }
    for clause in node
        .children()
        .into_iter()
        .filter(|c| c.kind() == SyntaxKind::WhereClause)
    {
        let mut subject: Option<Ty> = None;
        let mut after_colon = false;
        for element in clause.children_with_tokens() {
            match element.kind() {
                SyntaxKind::Colon => after_colon = true,
                SyntaxKind::Comma => {
                    after_colon = false;
                    subject = None;
                }
                kind if kind.is_type() => {
                    let Some(ty) = element.as_node() else {
                        continue;
                    };
                    if after_colon {
                        if let (Some(subject), Some(tr)) = (&subject, trait_ref(ty, scope)) {
                            generics.bounds.push(Bound {
                                subject: subject.clone(),
                                tr,
                            });
                        }
                    } else {
                        subject = Some(scope.annotation(ty));
                    }
                }
                _ => {}
            }
        }
    }
    generics
}

/// The type paths after the `:` (and before any `=`) of a generic parameter
/// or an associated type.
fn bounds_after_colon(node: &SyntaxNode) -> Vec<SyntaxNode> {
    let mut out = Vec::new();
    let mut on = false;
    for element in node.children_with_tokens() {
        match element.kind() {
            SyntaxKind::Colon => on = true,
            SyntaxKind::Eq => on = false,
            SyntaxKind::TypePath if on => out.extend(element.as_node().cloned()),
            _ => {}
        }
    }
    out
}

/// The trait a bound names, as the annotation resolves it; a bound naming no
/// declaration (a native trait, or an unknown name the resolver reported)
/// is none.
fn trait_ref(bound: &SyntaxNode, scope: &ModuleScope) -> Option<TraitRef> {
    match scope.annotation(bound) {
        Ty::Named(symbol, args) => Some(TraitRef { symbol, args }),
        _ => None,
    }
}

/// What one member of a trait or an impl is.
fn item_kind(member: &AssocItem) -> ItemKind {
    match member {
        AssocItem::Fn(_) => ItemKind::Fn,
        AssocItem::Action(_) => ItemKind::Action,
        AssocItem::Task(_) => ItemKind::Task,
        AssocItem::Const(_) => ItemKind::Const,
        AssocItem::Type(_) => ItemKind::Type,
    }
}

/// A callable member's parameters, return type, body and capability clause.
type CallableParts = (
    Vec<crate::ast::Param>,
    Option<crate::ast::ReturnType>,
    Option<crate::ast::Block>,
    Option<crate::ast::CapabilityClause>,
);

/// The parameters, return type and body of a callable member.
fn callable_parts(member: &AssocItem) -> Option<CallableParts> {
    Some(match member {
        AssocItem::Fn(f) => (f.params(), f.return_type(), f.body(), f.capability_clause()),
        AssocItem::Action(a) => (a.params(), a.return_type(), a.body(), a.capability_clause()),
        AssocItem::Task(t) => (t.params(), t.return_type(), t.body(), t.capability_clause()),
        AssocItem::Const(_) | AssocItem::Type(_) => return None,
    })
}

/// Records the generics of `symbol` (the enclosing ones first) and the
/// bounds each of its parameters carries.
fn record_generics(decls: &mut Declarations, symbol: SymbolId, generics: Generics) {
    for b in &generics.bounds {
        if let Ty::Param(p) = b.subject {
            let bounds = decls.traits.param_bounds.entry(p).or_default();
            if !bounds.contains(&b.tr) {
                bounds.push(b.tr.clone());
            }
        }
    }
    for p in &generics.params {
        decls.types.names.insert(p.symbol, p.name.clone());
        decls.traits.params.insert(p.symbol, p.clone());
    }
    if !generics.params.is_empty() {
        decls.traits.generics.insert(symbol, generics);
    }
}

/// `outer` followed by `inner`.
fn joined(outer: &Generics, inner: Generics) -> Generics {
    let mut all = outer.clone();
    all.params.extend(inner.params);
    all.bounds.extend(inner.bounds);
    all
}

/// Collects the generics, traits, impls and aliases `cu` declares, and the
/// signature and facts of each trait and impl member.
pub(super) fn collect(
    cu: &CompilationUnit,
    table: &SymbolTable,
    decl_at: &DeclAt,
    scope: &ModuleScope,
    interner: &mut NameInterner,
    decls: &mut Declarations,
) {
    for item in cu.items() {
        let decl = match item {
            Item::Export(e) => match e.declaration() {
                Some(inner) => inner,
                None => continue,
            },
            other => other,
        };
        let node = decl.syntax().clone();
        match &decl {
            Item::Trait(t) => collect_trait(t, table, decl_at, scope, interner, decls),
            Item::Impl(i) => collect_impl(i, decl_at, scope, decls),
            Item::TypeAlias(a) => {
                let Some(symbol) = a.name().and_then(|n| decl_at.get(&n.text_range())) else {
                    continue;
                };
                let generics = params_of(&node, decl_at, scope);
                decls.traits.aliases.insert(
                    *symbol,
                    Alias {
                        params: generics.symbols(),
                        body: scope.annotation_of(&node),
                    },
                );
                record_generics(decls, *symbol, generics);
            }
            Item::Component(_) | Item::System(_) => {
                let outer = params_of(&node, decl_at, scope);
                for member in node.children() {
                    if let Some(name) = crate::resolve::ident_tokens(&member).into_iter().next()
                        && matches!(
                            member.kind(),
                            SyntaxKind::FnDecl | SyntaxKind::ActionDecl | SyntaxKind::TaskDecl
                        )
                        && let Some(&symbol) = decl_at.get(&name.text_range())
                    {
                        // A generic component's members share its one lowering:
                        // only their own parameters instantiate them.
                        let own = params_of(&member, decl_at, scope);
                        record_generics(decls, symbol, own);
                        let owner =
                            name_token(&node).and_then(|n| decl_at.get(&n.text_range()).copied());
                        let label = format!(
                            "{}.{}",
                            name_token(&node)
                                .map(|t| t.text().to_string())
                                .unwrap_or_default(),
                            name.text()
                        );
                        record_body(decls, scope, symbol, &member, owner, label);
                    }
                }
                if let Some(&symbol) = name_token(&node).and_then(|n| decl_at.get(&n.text_range()))
                {
                    record_generics(decls, symbol, outer);
                }
            }
            _ => {
                if let Some(name) = name_token(&node)
                    && let Some(&symbol) = decl_at.get(&name.text_range())
                {
                    let generics = params_of(&node, decl_at, scope);
                    record_generics(decls, symbol, generics);
                    record_body(decls, scope, symbol, &node, None, name.text().to_string());
                }
            }
        }
    }
}

/// The name token of a declaration.
fn name_token(node: &SyntaxNode) -> Option<SyntaxToken> {
    crate::resolve::ident_tokens(node).into_iter().next()
}

fn collect_trait(
    decl: &TraitDecl,
    table: &SymbolTable,
    decl_at: &DeclAt,
    scope: &ModuleScope,
    interner: &mut NameInterner,
    decls: &mut Declarations,
) {
    let node = decl.syntax();
    let Some(name) = decl.name() else {
        return;
    };
    let Some(&symbol) = decl_at.get(&name.text_range()) else {
        return;
    };
    let Some(&self_param) = self_keyword(node).and_then(|kw| decl_at.get(&kw.text_range())) else {
        return;
    };
    let generics = params_of(node, decl_at, scope);
    let own_args: Vec<Ty> = generics.symbols().into_iter().map(Ty::Param).collect();
    let this = TraitRef {
        symbol,
        args: own_args,
    };
    let supertraits: Vec<TraitRef> = decl
        .supertraits()
        .filter_map(|b| trait_ref(b.syntax(), scope))
        .collect();
    // Within the trait, `Self` meets it (and so its supertraits).
    let mut outer = Generics {
        params: vec![GenericParam {
            symbol: self_param,
            name: "Self".into(),
            kind: ParamKind::Type,
        }],
        bounds: vec![Bound {
            subject: Ty::Param(self_param),
            tr: this.clone(),
        }],
    };
    outer.params.extend(generics.params.iter().cloned());
    outer.bounds.extend(generics.bounds.iter().cloned());
    let members = table.members(symbol);
    let self_ty = Ty::Param(self_param);
    let mut items = Vec::new();
    for member in decl.members() {
        let Some(member_name) = member.name() else {
            continue;
        };
        let text = member_name.text().to_string();
        let ns = if matches!(member, AssocItem::Type(_)) {
            crate::resolve::Namespace::Type
        } else {
            crate::resolve::Namespace::Value
        };
        let Some(member_symbol) = members
            .and_then(|m| m.get(interner.intern(&text), ns))
            .map(|s| s.id)
        else {
            continue;
        };
        let item = collect_item(
            &member,
            member_symbol,
            &outer,
            Some(&self_ty),
            decl_at,
            scope,
            decls,
        );
        decls
            .traits
            .owners
            .insert(member_symbol, Owner::Trait(symbol));
        record_body(
            decls,
            scope,
            member_symbol,
            member.syntax(),
            None,
            format!("{}::{text}", name.text()),
        );
        items.push(item);
    }
    // `Self` carries the trait as its bound wherever the trait's members are.
    record_generics(decls, self_param, outer);
    decls.traits.generics.remove(&self_param);
    record_generics(decls, symbol, generics.clone());
    decls.types.names.insert(symbol, name.text().to_string());
    decls.types.names.insert(self_param, "Self".into());
    scope.place(decls, symbol);
    decls.traits.traits.insert(
        symbol,
        TraitInfo {
            name: name.text().to_string(),
            home: scope.home,
            self_param,
            generics,
            supertraits,
            items,
            at: name.text_range(),
        },
    );
}

/// One member of a trait (`self_ty` its `Self`) or an impl (`self_ty` its
/// target): its generics, the outer ones first, and a callable's signature.
fn collect_item(
    member: &AssocItem,
    symbol: SymbolId,
    outer: &Generics,
    self_ty: Option<&Ty>,
    decl_at: &DeclAt,
    scope: &ModuleScope,
    decls: &mut Declarations,
) -> crate::hir::generic::Item {
    let node = member.syntax();
    let kind = item_kind(member);
    let own = params_of(node, decl_at, scope);
    record_generics(decls, symbol, joined(outer, own));
    let mut receiver = false;
    let mut has_body = false;
    let mut ty = None;
    let mut bounds = Vec::new();
    match member {
        AssocItem::Type(t) => {
            ty = t.value().map(|v| scope.annotation(&v));
            has_body = ty.is_some();
            bounds = t
                .bounds()
                .iter()
                .filter_map(|b| trait_ref(b.syntax(), scope))
                .collect();
        }
        AssocItem::Const(c) => {
            let annotated = scope.annotation_of(node);
            has_body = c.value().is_some();
            decls.facts.insert(
                symbol,
                super::MemberFacts {
                    ty: annotated.clone(),
                    effect: None,
                    is_reactive_source: false,
                    kind: crate::resolve::SymbolKind::Const,
                },
            );
            ty = Some(annotated);
        }
        _ => {
            if let Some((params, ret, body, _)) = callable_parts(member) {
                receiver = params.first().is_some_and(|p| p.self_token().is_some());
                has_body = body.is_some();
                let effect = match kind {
                    ItemKind::Action => EffectClass::Action,
                    ItemKind::Task => EffectClass::Task,
                    _ => EffectClass::Read,
                };
                scope.record_callable(decls, symbol, &params, ret, effect, self_ty);
            }
        }
    }
    crate::hir::generic::Item {
        name: member
            .name()
            .map(|n| n.text().to_string())
            .unwrap_or_default(),
        symbol,
        kind,
        receiver,
        has_body,
        ty,
        bounds,
        at: member.name().map_or(node.text_range(), |n| n.text_range()),
    }
}

fn collect_impl(decl: &ImplDecl, decl_at: &DeclAt, scope: &ModuleScope, decls: &mut Declarations) {
    let node = decl.syntax();
    let Some(kw) = self_keyword(node) else {
        return;
    };
    let Some(&self_param) = decl_at.get(&kw.text_range()) else {
        return;
    };
    let generics = params_of(node, decl_at, scope);
    let target = scope
        .self_types
        .get(&self_param)
        .cloned()
        .unwrap_or(Ty::Unknown);
    let trait_ref = decl.trait_path().and_then(|t| trait_ref(t.syntax(), scope));
    let index = decls.traits.impls.len();
    let mut items = Vec::new();
    for member in decl.members() {
        let Some(name) = member.name() else {
            continue;
        };
        let Some(&symbol) = decl_at.get(&name.text_range()) else {
            continue;
        };
        let item = collect_item(
            &member,
            symbol,
            &generics,
            Some(&target),
            decl_at,
            scope,
            decls,
        );
        decls.traits.owners.insert(symbol, Owner::Impl(index));
        decls.types.names.insert(symbol, item.name.clone());
        let target_text: String = decl
            .target()
            .map(|t| {
                t.text()
                    .to_string()
                    .chars()
                    .filter(|c| !c.is_whitespace())
                    .collect()
            })
            .unwrap_or_default();
        record_body(
            decls,
            scope,
            symbol,
            member.syntax(),
            None,
            format!("{target_text}::{}", item.name),
        );
        scope.place(decls, symbol);
        items.push(item);
    }
    decls.types.names.insert(self_param, "Self".into());
    decls.traits.impls.push(ImplInfo {
        home: scope.home,
        at: kw.text_range(),
        self_param,
        generics,
        target,
        trait_ref,
        items,
    });
}

/// Finds the prelude's auto traits and `TwoWayConverter`.
pub(super) fn standard(decls: &mut Declarations) {
    for (symbol, info) in &decls.traits.traits {
        if info.home.is_some() {
            continue;
        }
        let auto = match info.name.as_str() {
            "Clone" => AutoTrait::Clone,
            "Eq" => AutoTrait::Eq,
            "Hash" => AutoTrait::Hash,
            "TwoWayConverter" => {
                decls.traits.converter = Some(*symbol);
                continue;
            }
            _ => continue,
        };
        decls.traits.auto.insert(*symbol, auto);
    }
}

// --- checks and bodies -----------------------------------------------------

/// The name a member of an impl lowers under: `Target::name`.
fn member_name(target: &Ty, env: &ModuleEnv<'_>, name: &str) -> String {
    let schemas = &env.decls.types;
    format!("{}::{name}", schemas.describe(target))
}

/// Checks a trait: its bounds name traits, and each default body types
/// against its signature.
#[allow(clippy::too_many_arguments)]
pub(super) fn check_trait(
    decl: &TraitDecl,
    refs: &[crate::resolve::ResolvedRef],
    env: &ModuleEnv<'_>,
    diagnostics: &mut Vec<Diagnostic>,
    cap: &mut CapabilityGraphBuilder,
    percent: &mut PercentSources,
) {
    let Some(symbol) = decl
        .name()
        .and_then(|n| env.scope.decl_at.get(&n.text_range()))
    else {
        return;
    };
    let Some(info) = env.decls.traits.traits.get(symbol) else {
        return;
    };
    check_bounds_name_traits(decl.syntax(), env, diagnostics);
    for member in decl.members() {
        check_bounds_name_traits(member.syntax(), env, diagnostics);
        let Some(item) = member
            .name()
            .and_then(|n| info.items.iter().find(|i| i.at == n.text_range()))
        else {
            continue;
        };
        lower_member(
            &member,
            item.symbol,
            &format!("{}::{}", info.name, item.name),
            refs,
            env,
            diagnostics,
            cap,
            percent,
        );
    }
}

/// Reports each bound under `node`'s generics that names no trait (`E2201`).
fn check_bounds_name_traits(
    node: &SyntaxNode,
    env: &ModuleEnv<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let mut bounds: Vec<SyntaxNode> = Vec::new();
    for child in node.children() {
        match child.kind() {
            SyntaxKind::GenericParams => {
                for g in child.children() {
                    bounds.extend(bounds_after_colon(&g));
                }
            }
            SyntaxKind::WhereClause => {
                let mut after = false;
                for e in child.children_with_tokens() {
                    match e.kind() {
                        SyntaxKind::Colon => after = true,
                        SyntaxKind::Comma => after = false,
                        SyntaxKind::TypePath if after => bounds.extend(e.as_node().cloned()),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    if node.kind() == SyntaxKind::TraitDecl
        && let Some(t) = TraitDecl::cast(node.clone())
    {
        bounds.extend(t.supertraits().map(|b| b.syntax().clone()));
    }
    for bound in bounds {
        if let Ty::Named(symbol, _) = env.scope.annotation(&bound)
            && !env.decls.traits.traits.contains_key(&symbol)
        {
            diagnostics.push(Diagnostic::error(
                "E2201",
                bound.text_range(),
                format!("`{}` is not a trait", bound.text()),
            ));
        }
    }
}

/// Types (and, unless generic, lowers) one callable or const member.
#[allow(clippy::too_many_arguments)]
fn lower_member(
    member: &AssocItem,
    symbol: SymbolId,
    name: &str,
    refs: &[crate::resolve::ResolvedRef],
    env: &ModuleEnv<'_>,
    diagnostics: &mut Vec<Diagnostic>,
    cap: &mut CapabilityGraphBuilder,
    percent: &mut PercentSources,
) {
    if let AssocItem::Const(c) = member {
        check_const(c, refs, env, diagnostics, percent);
        let _ = symbol;
        return;
    }
    let Some((params, ret, body, clause)) = callable_parts(member) else {
        return;
    };
    let (kind, context) = match member {
        AssocItem::Action(_) => (FunctionKind::Action, BodyContext::Action),
        AssocItem::Task(_) => (FunctionKind::Task, BodyContext::Task),
        _ => (FunctionKind::Fn, BodyContext::Fn),
    };
    let callable = Callable {
        name: name.to_owned(),
        kind,
        context,
        symbol: Some(symbol),
        params,
        ret,
        body,
        clause,
    };
    check_callable(&callable, refs, env, diagnostics, cap, percent);
}

/// Checks an impl: its trait is one, its target meets the trait's
/// supertraits and the trait's bounds, it gives each required member and
/// only the trait's, each with the trait's signature; then types and lowers
/// its members.
#[allow(clippy::too_many_arguments)]
pub(super) fn check_impl(
    decl: &ImplDecl,
    refs: &[crate::resolve::ResolvedRef],
    env: &ModuleEnv<'_>,
    diagnostics: &mut Vec<Diagnostic>,
    cap: &mut CapabilityGraphBuilder,
    percent: &mut PercentSources,
) {
    let Some(kw) = self_keyword(decl.syntax()) else {
        return;
    };
    let table = &env.decls.traits;
    let Some(imp) = table.impls.iter().find(|i| i.at == kw.text_range()) else {
        return;
    };
    check_bounds_name_traits(decl.syntax(), env, diagnostics);
    if let (Some(path), None) = (decl.trait_path(), &imp.trait_ref) {
        diagnostics.push(Diagnostic::error(
            "E2201",
            path.syntax().text_range(),
            format!("`{}` is not a trait", path.syntax().text()),
        ));
    }
    if let Some(of) = &imp.trait_ref {
        match table.traits.get(&of.symbol) {
            None => diagnostics.push(Diagnostic::error(
                "E2201",
                decl.trait_path()
                    .map_or(kw.text_range(), |p| p.syntax().text_range()),
                format!(
                    "`{}` is not a trait",
                    decl.trait_path()
                        .map(|p| p.syntax().text())
                        .unwrap_or_default()
                ),
            )),
            Some(info) => check_conformance(decl, imp, of, info, env, diagnostics),
        }
    }
    for member in decl.members() {
        check_bounds_name_traits(member.syntax(), env, diagnostics);
        let Some(item) = member
            .name()
            .and_then(|n| imp.items.iter().find(|i| i.at == n.text_range()))
        else {
            continue;
        };
        let name = member_name(&imp.target, env, &item.name);
        lower_member(
            &member,
            item.symbol,
            &name,
            refs,
            env,
            diagnostics,
            cap,
            percent,
        );
    }
}

/// Checks that an `impl Trait for T` gives what `Trait` requires.
fn check_conformance(
    decl: &ImplDecl,
    imp: &ImplInfo,
    of: &TraitRef,
    info: &TraitInfo,
    env: &ModuleEnv<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let table = &env.decls.traits;
    let at = decl
        .trait_path()
        .map_or(imp.at, |p| p.syntax().text_range());
    let describe = |t: &Ty| env.decls.types.describe(t);
    if of.args.len() != info.generics.params.len() {
        diagnostics.push(Diagnostic::error(
            "E2201",
            at,
            format!(
                "`{}` takes {} type argument(s), {} given",
                info.name,
                info.generics.params.len(),
                of.args.len()
            ),
        ));
        return;
    }
    // The trait's parameters and `Self` as this impl has them.
    let mut subst: crate::hir::generic::Subst = vec![(info.self_param, imp.target.clone())];
    subst.extend(
        info.generics
            .symbols()
            .into_iter()
            .zip(of.args.iter().cloned()),
    );
    for sup in &info.supertraits {
        let sup = sup.map(&|t| apply(t, &subst));
        if !table.satisfies(&imp.target, &sup) {
            let name = table
                .traits
                .get(&sup.symbol)
                .map_or("?", |t| t.name.as_str());
            diagnostics.push(Diagnostic::error(
                "E2201",
                at,
                format!(
                    "`{}` implements `{}` only where it implements its supertrait `{name}`",
                    describe(&imp.target),
                    info.name
                ),
            ));
        }
    }
    for item in &imp.items {
        let Some(required) = info.items.iter().find(|i| i.name == item.name) else {
            diagnostics.push(
                Diagnostic::error(
                    "E2201",
                    item.at,
                    format!("`{}` is not a member of `{}`", item.name, info.name),
                )
                .with_related(Related::new(
                    info.at,
                    format!("`{}` is declared here", info.name),
                )),
            );
            continue;
        };
        if required.kind != item.kind || required.receiver != item.receiver {
            diagnostics.push(Diagnostic::error(
                "E2201",
                item.at,
                format!(
                    "`{}` is not declared as `{}` declares it",
                    item.name, info.name
                ),
            ));
            continue;
        }
        let differs = match item.kind {
            ItemKind::Type => false,
            ItemKind::Const => {
                let want = required.ty.as_ref().map(|t| apply(t, &subst));
                want.is_some_and(|w| Some(&w) != item.ty.as_ref())
            }
            _ => {
                let mine = env.decls.signatures.get(&item.symbol);
                let theirs = env.decls.signatures.get(&required.symbol).map(|(ps, r)| {
                    (
                        ps.iter()
                            .map(|p| table.normalize(&apply(p, &subst)))
                            .collect::<Vec<_>>(),
                        table.normalize(&apply(r, &subst)),
                    )
                });
                let mine = mine.map(|(ps, r)| {
                    (
                        ps.iter().map(|p| table.normalize(p)).collect::<Vec<_>>(),
                        table.normalize(r),
                    )
                });
                let own = |s: SymbolId| table.generics_of(s).map_or(0, |g| g.params.len());
                mine != theirs
                    || own(item.symbol).saturating_sub(imp.generics.params.len())
                        != own(required.symbol).saturating_sub(info.generics.params.len() + 1)
            }
        };
        if differs {
            diagnostics.push(
                Diagnostic::error(
                    "E2201",
                    item.at,
                    format!(
                        "`{}` has another signature than `{}` declares",
                        item.name, info.name
                    ),
                )
                .with_related(Related::new(required.at, "declared here".to_string())),
            );
        }
    }
    let missing: Vec<&str> = info
        .items
        .iter()
        .filter(|i| !i.has_body && imp.item(&i.name).is_none())
        .map(|i| i.name.as_str())
        .collect();
    if !missing.is_empty() {
        diagnostics.push(Diagnostic::error(
            "E2201",
            at,
            format!(
                "`{}` does not implement `{}`: it lacks {}",
                describe(&imp.target),
                info.name,
                missing
                    .iter()
                    .map(|m| format!("`{m}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        ));
    }
}

/// Reports each pair of impls that could both apply to one type (`E2202`):
/// two impls of one trait, or two inherent impls giving one member name.
pub(super) fn check_overlap(decls: &Declarations, per_module: &mut [Vec<Diagnostic>]) {
    let impls = &decls.traits.impls;
    for (j, b) in impls.iter().enumerate() {
        for a in &impls[..j] {
            let clash = match (&a.trait_ref, &b.trait_ref) {
                (None, None) => b.items.iter().find(|i| a.item(&i.name).is_some()).map(|i| {
                    format!(
                        "both give `{}` to `{}`",
                        i.name,
                        decls.types.describe(&b.target)
                    )
                }),
                (Some(x), Some(y)) if x.symbol == y.symbol => Some(format!(
                    "two impls of `{}` apply to `{}`",
                    decls
                        .traits
                        .traits
                        .get(&x.symbol)
                        .map_or("?", |t| t.name.as_str()),
                    decls.types.describe(&b.target)
                )),
                _ => None,
            };
            let Some(message) = clash else {
                continue;
            };
            if !decls.traits.overlap(a, b) {
                continue;
            }
            let Some(module) = b.home else {
                continue;
            };
            let mut d = Diagnostic::error("E2202", b.at, format!("overlapping impls: {message}"));
            if a.home == b.home {
                d = d.with_related(Related::new(a.at, "the other impl".to_string()));
            }
            if let Some(out) = per_module.get_mut(module) {
                out.push(d);
            }
        }
    }
}
