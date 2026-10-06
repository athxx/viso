//! `resource` members (§38): the configuration items, each once, `load` and
//! `key` required (`E4301`); the policy list and the scope (`E4302`); a pure
//! `key` whose type is `StableKey` (`E2701`); a `load` that is a task call
//! (`E4401`) producing the declared `Result<T, E>` (`E2103`). Each lowers to an
//! effect whose dependency is the key and whose body starts the loader, with
//! the entries the view host moves the state through.

use std::collections::{BTreeSet, HashMap};
use std::time::Duration;

use viso_behavior::ResourceLoad;

use crate::ast::{AstNode, ComponentDecl, Expr, Member, PathExpr, ResourceDecl};
use crate::behavior::ir::FunctionKind;
use crate::behavior::lower::{Def, lower_resource_load, lower_value, unsupported};
use crate::diag::Diagnostic;
use crate::hir::effect::BodyContext;
use crate::hir::infer::{InferCx, TypeEnv, VariantPayload, call_args, child_exprs, const_seconds};
use crate::hir::nodes::ComponentSchema;
use crate::hir::ty::Ty;
use crate::resolve::{Resolution, ResolvedRef, SymbolId};
use crate::syntax::{SyntaxKind, SyntaxNode, TextRange};

use super::tasks::calls_task;
use super::{ModuleEnv, TYPE_ERRORS, check_body, has_errors, name_of};

/// Checks and lowers every `resource` of the component `decl`, whose schema
/// is `schema`, registering each in the component's layout.
pub(super) fn lower_resources(
    decl: &ComponentDecl,
    refs: &[ResolvedRef],
    env: &ModuleEnv<'_>,
    schema: &ComponentSchema,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for member in decl.members() {
        if let Member::Resource(resource) = member {
            lower_resource(&resource, refs, env, schema, diagnostics);
        }
    }
}

/// The policies of a resource.
#[derive(Default)]
struct Policies {
    debounce: Option<Duration>,
    cache_for: Option<Duration>,
    cache_errors: bool,
    keep_latest: bool,
}

fn lower_resource(
    resource: &ResourceDecl,
    refs: &[ResolvedRef],
    env: &ModuleEnv<'_>,
    schema: &ComponentSchema,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let member = name_of(resource.name());
    let at = resource
        .name()
        .map_or(resource.syntax().text_range(), |t| t.text_range());
    let mut items: [Option<Expr>; 4] = [None, None, None, None];
    let mut malformed = false;
    for item in resource.items() {
        let (index, word) = match item.kind() {
            SyntaxKind::ResourceLoad => (0, "load"),
            SyntaxKind::ResourceKey => (1, "key"),
            SyntaxKind::ResourcePolicy => (2, "policy"),
            _ => (3, "scope"),
        };
        if items[index].is_some() {
            diagnostics.push(Diagnostic::error(
                "E4301",
                item.syntax().text_range(),
                format!("a `resource` takes one `{word} = ..;`"),
            ));
            malformed = true;
            continue;
        }
        match item.value() {
            Some(value) => items[index] = Some(value),
            None => malformed = true,
        }
    }
    let [load, key, policy, scope] = items;
    for (missing, word) in [(load.is_none(), "load"), (key.is_none(), "key")] {
        if missing && !malformed {
            diagnostics.push(Diagnostic::error(
                "E4301",
                at,
                format!("a `resource` needs `{word} = ..;`"),
            ));
        }
    }
    let policies = match &policy {
        Some(list) => parse_policies(list, diagnostics),
        None => Some(Policies::default()),
    };
    let scoped = scope
        .as_ref()
        .is_none_or(|scope| check_scope(scope, diagnostics));

    let state = schema.states.iter().find(|s| s.name == member);
    let (value, error) = match state.map(|s| &s.meta.inferred_type) {
        Some(Ty::Resource(value, error)) => ((**value).clone(), (**error).clone()),
        _ => (Ty::Unknown, Ty::Unknown),
    };
    let mut cx = InferCx::new(refs, env);
    if let Some(key) = &key {
        let ty = cx.infer_expr(key, None);
        check_body(refs, BodyContext::Computed, env, key.syntax(), diagnostics);
        if let Err(why) = stable_key(env, &ty, &mut BTreeSet::new()) {
            diagnostics.push(Diagnostic::error(
                "E2701",
                key.syntax().text_range(),
                format!(
                    "a resource key is `StableKey`, but `{}` {why}",
                    cx.describe(&ty)
                ),
            ));
        }
    }
    if let Some(load) = &load {
        let task = load.syntax().kind() == SyntaxKind::CallExpr && {
            let index: HashMap<TextRange, Resolution> =
                refs.iter().map(|r| (r.range, r.to)).collect();
            calls_task(&index, env, load.syntax())
        };
        if !task {
            diagnostics.push(Diagnostic::error(
                "E4401",
                load.syntax().text_range(),
                "a resource loads by a task call, such as `load = fetch(key);`",
            ));
        }
        let want = Ty::Result(Box::new(value), Box::new(error));
        let _ = cx.infer_expr(load, task.then_some(&want));
        for arg in call_args(load.syntax()) {
            check_body(refs, BodyContext::Computed, env, arg.syntax(), diagnostics);
        }
    }
    let (Some(load), Some(key), Some(policies), Some(symbol), true, false) = (
        load,
        key,
        policies,
        state.and_then(|s| s.meta.resolved_symbol),
        scoped,
        malformed,
    ) else {
        diagnostics.extend(cx.into_diagnostics());
        return;
    };
    let name = format!("{}.{member}", schema.name);
    let def = |kind| Def {
        name: name.clone(),
        kind,
        symbol: None,
        module: env.module,
        into: None,
    };
    let (key_at, load_at) = (key.syntax().text_range(), load.syntax().text_range());
    let mut b = env.behavior.borrow_mut();
    let (key_entry, load_entry) = if has_errors(cx.diagnostics()) {
        (
            unsupported(&mut b, def(FunctionKind::RegionEntry), TYPE_ERRORS, key_at),
            unsupported(&mut b, def(FunctionKind::Effect), TYPE_ERRORS, load_at),
        )
    } else {
        (
            lower_value(&mut b, &cx, def(FunctionKind::RegionEntry), &key),
            lower_resource_load(&mut b, &cx, def(FunctionKind::Effect), &load),
        )
    };
    let load = ResourceLoad {
        state: 0,
        write: 0,
        debounce: policies.debounce,
        cache_for: policies.cache_for,
        cache_errors: policies.cache_errors,
        keep_latest: policies.keep_latest,
    };
    let sites = [key_at, load_at, at, resource.syntax().text_range()];
    b.resource(
        symbol, &name, env.module, sites, key_entry, load_entry, load,
    );
    drop(b);
    diagnostics.extend(cx.into_diagnostics());
}

/// The policies of the list `list`, each at most once (`E4302`): `None` after
/// a problem.
fn parse_policies(list: &Expr, diagnostics: &mut Vec<Diagnostic>) -> Option<Policies> {
    let mut report = |at: TextRange, message: &str| {
        diagnostics.push(Diagnostic::error("E4302", at, message.to_owned()));
        None
    };
    if list.syntax().kind() != SyntaxKind::ListExpr {
        return report(
            list.syntax().text_range(),
            "a resource policy is a list: `policy = [ResourcePolicy::keep_latest];`",
        );
    }
    let mut policies = Policies::default();
    let mut seen = BTreeSet::new();
    let mut failed = false;
    for item in child_exprs(list.syntax()) {
        let at = item.syntax().text_range();
        let (name, arg) = match item.syntax().kind() {
            SyntaxKind::PathExpr => (segments(item.syntax()), None),
            SyntaxKind::CallExpr => {
                let callee = item
                    .syntax()
                    .children()
                    .into_iter()
                    .find(|c| c.kind() == SyntaxKind::PathExpr);
                let args = call_args(item.syntax());
                let [arg] = &args[..] else {
                    failed = true;
                    report(at, "a timed resource policy takes one duration");
                    continue;
                };
                (
                    callee.map(|c| segments(&c)).unwrap_or_default(),
                    Some(arg.clone()),
                )
            }
            _ => (Vec::new(), None),
        };
        let name: Vec<&str> = name.iter().map(String::as_str).collect();
        let policy: &'static str = match (&name[..], &arg) {
            (["ResourcePolicy", "keep_latest"], None) => "keep_latest",
            (["ResourcePolicy", "cache_errors"], None) => "cache_errors",
            (["ResourcePolicy", "debounce"], Some(_)) => "debounce",
            (["ResourcePolicy", "cache_for"], Some(_)) => "cache_for",
            _ => {
                failed = true;
                report(at, UNKNOWN);
                continue;
            }
        };
        if !seen.insert(policy) {
            failed = true;
            report(at, &format!("`ResourcePolicy::{policy}` is given twice"));
            continue;
        }
        let duration = match arg.as_ref().map(const_seconds) {
            None => None,
            Some(Some(seconds)) if seconds > 0.0 && seconds.is_finite() => {
                Some(Duration::from_secs_f64(seconds))
            }
            Some(_) => {
                failed = true;
                report(
                    at,
                    &format!("`ResourcePolicy::{policy}` takes a constant duration above zero"),
                );
                continue;
            }
        };
        match policy {
            "keep_latest" => policies.keep_latest = true,
            "cache_errors" => policies.cache_errors = true,
            "debounce" => policies.debounce = duration,
            _ => policies.cache_for = duration,
        }
    }
    if policies.cache_errors && policies.cache_for.is_none() && !failed {
        return report(
            list.syntax().text_range(),
            "`ResourcePolicy::cache_errors` caches errors as `cache_for(..)` caches values; \
             add `ResourcePolicy::cache_for(..)`",
        );
    }
    (!failed).then_some(policies)
}

const UNKNOWN: &str = "a resource policy is `ResourcePolicy::keep_latest`, \
                       `ResourcePolicy::debounce(d)`, `ResourcePolicy::cache_for(d)` or \
                       `ResourcePolicy::cache_errors`";

/// Whether the scope `scope` is one a resource runs in (`E4302`): the
/// component's own, `ResourceScope::component`.
fn check_scope(scope: &Expr, diagnostics: &mut Vec<Diagnostic>) -> bool {
    let path = segments(scope.syntax());
    if path == ["ResourceScope", "component"] {
        return true;
    }
    diagnostics.push(Diagnostic::error(
        "E4302",
        scope.syntax().text_range(),
        "a resource runs in `ResourceScope::component`: each mounted instance loads and \
         caches its own",
    ));
    false
}

fn segments(path: &SyntaxNode) -> Vec<String> {
    PathExpr::cast(path.clone())
        .map(|p| p.segments().map(|s| s.text().to_string()).collect())
        .unwrap_or_default()
}

/// Whether values of `ty` are `StableKey` (§80.1): the integers, `Bool`,
/// `Char` and `String`; a tuple, record, `Option` or enum of such values. The
/// reason it is not, otherwise. `visiting` holds the declarations being
/// checked, which a recursive one refers back to.
fn stable_key(
    env: &ModuleEnv<'_>,
    ty: &Ty,
    visiting: &mut BTreeSet<SymbolId>,
) -> Result<(), &'static str> {
    match ty {
        Ty::Bool
        | Ty::I8
        | Ty::I16
        | Ty::I32
        | Ty::I64
        | Ty::U8
        | Ty::U16
        | Ty::U32
        | Ty::U64
        | Ty::Char
        | Ty::String
        | Ty::Unit
        | Ty::InferInt
        | Ty::Unknown
        | Ty::Never => Ok(()),
        Ty::F32 | Ty::F64 | Ty::InferFloat => {
            Err("is a float, whose equality is not an identity (`NaN`, `-0.0`)")
        }
        Ty::Tuple(items) => items.iter().try_for_each(|t| stable_key(env, t, visiting)),
        Ty::Option(t) => stable_key(env, t, visiting),
        Ty::Named(id, ..) => {
            if !visiting.insert(*id) {
                return Ok(());
            }
            let fields: Vec<Ty> = if let Some(fields) = env.record_fields(*id) {
                fields.iter().map(|f| f.ty.clone()).collect()
            } else if let Some(variants) = env.enum_variants(*id) {
                variants
                    .iter()
                    .flat_map(|v| match &v.payload {
                        VariantPayload::Unit => Vec::new(),
                        VariantPayload::Tuple(tys) => tys.clone(),
                        VariantPayload::Record(fields) => {
                            fields.iter().map(|f| f.ty.clone()).collect()
                        }
                    })
                    .collect()
            } else {
                return Err("is no record or enum the package declares");
            };
            let result = fields.iter().try_for_each(|t| stable_key(env, t, visiting));
            visiting.remove(id);
            result
        }
        _ => Err("is no integer, `Bool`, `Char`, `String`, or tuple, record or enum of them"),
    }
}
