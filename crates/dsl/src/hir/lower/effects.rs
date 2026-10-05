//! `effect` members (§37): the run policy against the dependency list
//! (`E4203`), pure dependency expressions, the body's reads against the
//! dependencies (`E4201`), and the lowering of each to a dependency entry and a
//! body that returns its cleanup.

use std::collections::BTreeSet;

use viso_behavior::EffectRun;

use crate::ast::{AstNode, ComponentDecl, EffectDecl, Expr, Member, PathExpr};
use crate::behavior::ir::FunctionKind;
use crate::behavior::lower::{Def, lower_effect_body, lower_effect_deps, unsupported};
use crate::diag::Diagnostic;
use crate::hir::effect::BodyContext;
use crate::hir::infer::InferCx;
use crate::hir::nodes::ComponentSchema;
use crate::hir::reads::{WithDerived, collect_reads, tracked_reads};
use crate::resolve::ResolvedRef;
use crate::syntax::SyntaxKind;

use super::{ModuleEnv, TYPE_ERRORS, check_body, has_errors, name_of};

/// Checks and lowers every `effect` of the component `decl`, whose schema is
/// `schema`, registering each in the component's layout.
pub(super) fn lower_effects(
    decl: &ComponentDecl,
    refs: &[ResolvedRef],
    env: &ModuleEnv<'_>,
    schema: &ComponentSchema,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for member in decl.members() {
        if let Member::Effect(effect) = member {
            lower_effect(&effect, refs, env, schema, diagnostics);
        }
    }
}

fn lower_effect(
    effect: &EffectDecl,
    refs: &[ResolvedRef],
    env: &ModuleEnv<'_>,
    schema: &ComponentSchema,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let name = format!("{}.{}", schema.name, name_of(effect.name()));
    let deps: Vec<Expr> = effect
        .deps()
        .map(|deps| deps.exprs().collect())
        .unwrap_or_default();
    let run = run_policy(effect, !deps.is_empty(), diagnostics);

    let mut cx = InferCx::new(refs, env);
    for dep in &deps {
        let _ = cx.infer_expr(dep, None);
        check_body(refs, BodyContext::Computed, env, dep.syntax(), diagnostics);
    }
    let Some(body) = effect.body() else {
        diagnostics.extend(cx.into_diagnostics());
        return;
    };
    cx.check_unit_body(body.syntax());
    check_body(refs, BodyContext::Effect, env, body.syntax(), diagnostics);
    check_reads(refs, env, schema, &deps, body.syntax(), diagnostics);

    let Some(run) = run else {
        diagnostics.extend(cx.into_diagnostics());
        return;
    };
    let def = |kind| Def {
        name: name.clone(),
        kind,
        symbol: None,
        module: env.module,
        into: None,
    };
    let mut b = env.behavior.borrow_mut();
    let lowered = if has_errors(cx.diagnostics()) {
        let at = body.syntax().text_range();
        let body = unsupported(&mut b, def(FunctionKind::Effect), TYPE_ERRORS, at);
        let deps = effect.deps().map(|d| {
            let at = d.syntax().text_range();
            (
                at,
                unsupported(&mut b, def(FunctionKind::RegionEntry), TYPE_ERRORS, at),
            )
        });
        (deps, (at, body))
    } else {
        let deps = effect.deps().map(|d| {
            let at = d.syntax().text_range();
            (
                at,
                lower_effect_deps(&mut b, &cx, def(FunctionKind::RegionEntry), &deps, at),
            )
        });
        let at = body.syntax().text_range();
        (
            deps,
            (
                at,
                lower_effect_body(&mut b, &cx, def(FunctionKind::Effect), &body),
            ),
        )
    };
    b.effect(lowered.0, lowered.1, run);
    drop(b);
    diagnostics.extend(cx.into_diagnostics());
}

/// The effect's run policy: `run EffectRun::<policy>` or the default its
/// dependency list implies. `None` after an `E4203`.
fn run_policy(
    effect: &EffectDecl,
    has_deps: bool,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<EffectRun> {
    let default = if has_deps {
        EffectRun::MountAndChange
    } else {
        EffectRun::Mount
    };
    let Some(run) = effect.run() else {
        return Some(default);
    };
    let at = run.syntax().text_range();
    let segments: Vec<String> = run
        .policy()
        .filter(|p| p.syntax().kind() == SyntaxKind::PathExpr)
        .and_then(|p| PathExpr::cast(p.syntax().clone()))
        .map(|p| p.segments().map(|s| s.text().to_string()).collect())
        .unwrap_or_default();
    let policy = match segments.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        ["EffectRun", "mount"] => EffectRun::Mount,
        ["EffectRun", "change"] => EffectRun::Change,
        ["EffectRun", "mount_and_change"] => EffectRun::MountAndChange,
        _ => {
            diagnostics.push(Diagnostic::error(
                "E4203",
                at,
                "an effect runs by `EffectRun::mount`, `EffectRun::change` or \
                 `EffectRun::mount_and_change`",
            ));
            return None;
        }
    };
    let conflict = match (policy, has_deps) {
        (EffectRun::Mount, true) => Some(
            "`EffectRun::mount` runs once and takes no `when (..)`; drop the list or run on \
             `change`",
        ),
        (EffectRun::Change | EffectRun::MountAndChange, false) => {
            Some("an effect that runs on change needs a `when (..)` list of what it changes with")
        }
        _ => None,
    };
    if let Some(message) = conflict {
        diagnostics.push(Diagnostic::error("E4203", at, message));
        return None;
    }
    Some(policy)
}

/// Reports each reactive value the body reads outside `untracked(..)` that the
/// dependency list does not cover (`E4201`): a read is covered when the list
/// reads it, or reads every state and input it derives from. A derived value
/// the list reads covers nothing beneath it: it may keep its value while they
/// change.
fn check_reads(
    refs: &[ResolvedRef],
    env: &ModuleEnv<'_>,
    schema: &ComponentSchema,
    deps: &[Expr],
    body: &crate::syntax::SyntaxNode,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let derived = |symbol| schema.derived.is_derived(symbol);
    let reads = WithDerived {
        env,
        derived: &derived,
    };
    let named: BTreeSet<_> = deps
        .iter()
        .flat_map(|dep| collect_reads(refs, &reads, dep))
        .collect();
    let covered: BTreeSet<_> = named
        .iter()
        .copied()
        .filter(|&symbol| !schema.derived.is_derived(symbol))
        .collect();
    let mut reported = BTreeSet::new();
    for (symbol, token) in tracked_reads(refs, &reads, body) {
        let beneath = schema.derived.flatten([symbol]);
        if named.contains(&symbol) || beneath.is_subset(&covered) || !reported.insert(symbol) {
            continue;
        }
        let what = if deps.is_empty() {
            "the effect has no `when (..)` list".to_owned()
        } else {
            "its `when (..)` list does not cover it".to_owned()
        };
        diagnostics.push(Diagnostic::error(
            "E4201",
            token.text_range(),
            format!(
                "the effect reads `{}`, but {what}; list it, or read it with `untracked(..)`",
                token.text()
            ),
        ));
    }
}
