//! Static effect checking — the doc's call matrix over the resolved call graph.
//!
//! Every callable carries an [`EffectClass`]: a `fn` is `Pure` or `Read` (it reads
//! state but mutates nothing), an `action` is a synchronous `Action` mutation, and a
//! `task` is an asynchronous `Task`. Every *body* is walked in a [`BodyContext`] — the
//! kind of member it is (a `view`, a `computed`, a plain `fn`, an `action`, an `event`
//! handler, or a `task`) — and each call the body makes is checked against what that
//! context is allowed to invoke:
//!
//! - **View / Computed / `fn`** may only call pure/read callables (`fn`). A call that
//!   carries an `Action` or `Task` effect is a *side effect in a reactive/pure context*
//!   — `E2502` in a view/computed, `E2501` in a plain `fn`.
//! - **Action / Event** may call `fn` and `action`, and may start a `task`. A direct
//!   (non-spawned) `task` call is still `E2501` — starting one is a spawn, not an
//!   ordinary call, and the spawn form is task-async (a placeholder this slice).
//! - **Task** may call `fn` and, directly, other `task`s (it awaits them). Calling an
//!   `action` from a task is `E2501` (a task mutates through actions it *starts*, not by
//!   calling them synchronously mid-body).
//!
//! Effect checking is deliberately decoupled from the HIR node types (built in a later
//! section) through the [`EffectEnv`] trait, exactly as [`crate::hir::infer`] is: the
//! only thing the checker needs from the surrounding program is *what effect class the
//! callable behind a name has*, which the environment answers. That keeps this module
//! testable against a stub environment before component lowering exists.
//!
//! Task-async spawn/await (`start task` / `await task`) has no dedicated expression
//! grammar yet (it is an advanced placeholder this slice), so the matrix is checked at
//! the granularity the grammar exposes: ordinary call expressions resolved to a callee
//! whose effect class is known. When the spawn/await forms land, their body-context
//! rules extend the same matrix.

use std::collections::HashMap;

use crate::ast::{AstNode, CallExpr, Expr, PathExpr};
use crate::diag::Diagnostic;
use crate::resolve::{Resolution, ResolvedRef};
use crate::syntax::{SyntaxKind, SyntaxNode, TextRange};

/// The effect a callable carries — the doc's four-way classification.
///
/// `Pure` reads nothing observable and mutates nothing; `Read` observes reactive state
/// but does not mutate (both are what a `fn` may be); `Action` is a synchronous
/// mutation; `Task` is asynchronous work. The ordering is by "purity": a context that
/// permits an effect permits every lower one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectClass {
    /// No observable read or write.
    Pure,
    /// Reads reactive state, mutates nothing. A `fn` may be `Pure` or `Read`.
    Read,
    /// A synchronous mutation. What an `action` carries.
    Action,
    /// Asynchronous, cancelable work. What a `task` carries.
    Task,
}

impl EffectClass {
    /// Whether this effect is a side effect (a write or async work) as opposed to a pure
    /// or read-only observation. View/Computed bodies must be free of these.
    fn is_side_effect(self) -> bool {
        matches!(self, EffectClass::Action | EffectClass::Task)
    }
}

/// The kind of body being effect-checked — the left axis of the call matrix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyContext {
    /// A `view` body. Reactive and pure: may call only `fn`.
    View,
    /// A `computed` body. Pure derivation: may call only `fn`.
    Computed,
    /// A plain `fn` body. Pure/read: may call only `fn`.
    Fn,
    /// An `action` body. May call `fn`/`action` (and start tasks).
    Action,
    /// An `event` handler body. Same call rights as an action.
    Event,
    /// A `task` body. May call `fn` and, directly, other `task`s.
    Task,
}

impl BodyContext {
    /// Whether this is a reactive/pure context whose side-effect violations are `E2502`
    /// (a side effect where none is allowed) rather than the generic `E2501`.
    fn is_reactive(self) -> bool {
        matches!(self, BodyContext::View | BodyContext::Computed)
    }

    /// Whether a body in this context may call a callable of effect class `callee`.
    /// This is the doc's call matrix.
    fn permits(self, callee: EffectClass) -> bool {
        match self {
            // View/Computed/fn: pure and read only.
            BodyContext::View | BodyContext::Computed | BodyContext::Fn => {
                matches!(callee, EffectClass::Pure | EffectClass::Read)
            }
            // Action/Event: pure/read/action. (Starting a task is a spawn form, not an
            // ordinary call, so a direct Task call is not permitted here.)
            BodyContext::Action | BodyContext::Event => {
                matches!(
                    callee,
                    EffectClass::Pure | EffectClass::Read | EffectClass::Action
                )
            }
            // Task: pure/read and, directly, other tasks (awaited).
            BodyContext::Task => {
                matches!(
                    callee,
                    EffectClass::Pure | EffectClass::Read | EffectClass::Task
                )
            }
        }
    }
}

/// What effect checking needs to know about the surrounding program: the effect class of
/// the callable a name resolves to. Supplying this as a trait keeps the checker
/// independent of the HIR node types (which a later section builds), mirroring
/// [`crate::hir::infer::TypeEnv`].
pub trait EffectEnv {
    /// The effect class of the callable a name resolves to when used in callee position.
    /// `None` when the resolution is not a callable this checker knows about (a value
    /// call, an unresolved name, or a not-yet-modeled advanced form) — such a call is
    /// skipped by the matrix (its type-level error, if any, is `infer`'s to report).
    fn callee_effect(&self, to: &Resolution) -> Option<EffectClass>;
}

/// The effect-checking context for one body walk: the resolved-reference index, the
/// body's [`BodyContext`], the environment, and the diagnostics sink.
pub struct EffectCx<'a> {
    /// Every resolved name use keyed by its head-token span, so a callee resolves in O(1).
    refs: HashMap<TextRange, Resolution>,
    /// The kind of body being walked.
    context: BodyContext,
    /// The surrounding-program effect oracle.
    env: &'a dyn EffectEnv,
    /// Diagnostics accumulated during the walk.
    diagnostics: Vec<Diagnostic>,
}

impl<'a> EffectCx<'a> {
    /// Builds a context for a body of kind `context` from a module's resolved references
    /// and an effect environment.
    pub fn new(refs: &[ResolvedRef], context: BodyContext, env: &'a dyn EffectEnv) -> Self {
        let mut index = HashMap::with_capacity(refs.len());
        for r in refs {
            index.insert(r.range, r.to);
        }
        EffectCx {
            refs: index,
            context,
            env,
            diagnostics: Vec::new(),
        }
    }

    /// Consumes the context, returning the diagnostics it gathered.
    pub fn into_diagnostics(self) -> Vec<Diagnostic> {
        self.diagnostics
    }

    /// Borrows the diagnostics gathered so far.
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    /// Walks `expr`, checking every call it (transitively) makes against the body's call
    /// matrix. The walk is structural: it descends into every sub-expression so a call
    /// nested inside an argument, a branch, or an operand is checked too.
    pub fn check_expr(&mut self, expr: &Expr) {
        self.walk(expr.syntax());
    }

    /// Walks an arbitrary syntax node — a callable's `Block` body or a `view` body, which
    /// are not single `Expr`s — with the same call-matrix check as [`Self::check_expr`].
    /// Every call nested anywhere under `node` is checked against the body's context.
    pub fn check_node(&mut self, node: &SyntaxNode) {
        self.walk(node);
    }

    /// Recursively walks a syntax node, applying the call-matrix check at each call and
    /// descending into every child expression.
    fn walk(&mut self, node: &SyntaxNode) {
        if node.kind() == SyntaxKind::CallExpr {
            self.check_call(node);
        }
        for child in node.children() {
            self.walk(&child);
        }
    }

    /// Applies the call matrix to one call expression: resolves the callee's effect class
    /// and, if the body's context does not permit it, emits `E2502` (in a reactive/pure
    /// context) or `E2501` (otherwise).
    fn check_call(&mut self, node: &SyntaxNode) {
        let Some(callee_effect) = self.callee_effect(node) else {
            return;
        };
        if self.context.permits(callee_effect) {
            return;
        }
        let range = node.text_range();
        if self.context.is_reactive() && callee_effect.is_side_effect() {
            self.diagnostics.push(Diagnostic::error(
                "E2502",
                range,
                format!(
                    "a {} call is a side effect and is not allowed in a {} body",
                    effect_word(callee_effect),
                    context_word(self.context)
                ),
            ));
        } else {
            self.diagnostics.push(Diagnostic::error(
                "E2501",
                range,
                format!(
                    "a {} body may not call a {} callable",
                    context_word(self.context),
                    effect_word(callee_effect)
                ),
            ));
        }
    }

    /// Resolves a call's callee to its effect class through the refs index and the
    /// environment. `None` when the callee is not a simple resolved path or the
    /// environment does not classify it.
    fn callee_effect(&self, node: &SyntaxNode) -> Option<EffectClass> {
        let call = CallExpr::cast(node.clone())?;
        let callee = call.callee()?;
        let callee_node = callee.syntax();
        if callee_node.kind() == SyntaxKind::PathExpr
            && let Some(head) =
                PathExpr::cast(callee_node.clone()).and_then(|p| p.segments().next())
            && let Some(to) = self.refs.get(&head.text_range())
        {
            return self.env.callee_effect(to);
        }
        None
    }
}

/// A one-word name for an effect class, for diagnostic messages.
fn effect_word(effect: EffectClass) -> &'static str {
    match effect {
        EffectClass::Pure => "pure",
        EffectClass::Read => "read",
        EffectClass::Action => "action",
        EffectClass::Task => "task",
    }
}

/// A one-word name for a body context, for diagnostic messages.
fn context_word(context: BodyContext) -> &'static str {
    match context {
        BodyContext::View => "view",
        BodyContext::Computed => "computed",
        BodyContext::Fn => "fn",
        BodyContext::Action => "action",
        BodyContext::Event => "event",
        BodyContext::Task => "task",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::AstNode;
    use crate::resolve::SymbolId;
    use crate::syntax::{SyntaxKind, SyntaxNode, tokenize};

    /// A stub effect environment: one callable name (by `SymbolId`) mapped to its effect
    /// class. Locals never resolve to a callable here.
    struct StubEnv {
        effects: HashMap<SymbolId, EffectClass>,
    }

    impl EffectEnv for StubEnv {
        fn callee_effect(&self, to: &Resolution) -> Option<EffectClass> {
            match to {
                Resolution::Symbol(id) => self.effects.get(id).copied(),
                Resolution::Local(_) => None,
            }
        }
    }

    /// Parses `src` as an expression fragment and returns its red tree root plus the
    /// first `Expr` in it.
    fn parse_fragment(src: &str) -> (SyntaxNode, Expr) {
        let tokens = tokenize(src);
        let parse = crate::syntax::grammar::parse_expr(&tokens, src);
        let root = SyntaxNode::new_root(parse.root);
        let expr = root
            .descendants()
            .into_iter()
            .find_map(Expr::cast)
            .expect("fragment contains an expression");
        (root, expr)
    }

    /// The span of the first `Ident` token in `root` whose text equals `name`.
    fn ident_range(root: &SyntaxNode, name: &str) -> TextRange {
        root.descendants_with_tokens()
            .into_iter()
            .filter_map(|e| e.as_token().cloned())
            .find(|t| t.kind() == SyntaxKind::Ident && t.text() == name)
            .expect("identifier is present")
            .text_range()
    }

    /// Builds a checker where the single callee `name` (used in `src`) resolves to a
    /// symbol classified as `effect`, walks `src` in `context`, and returns the codes it
    /// reports.
    fn check(
        src: &str,
        context: BodyContext,
        name: &str,
        effect: EffectClass,
    ) -> Vec<&'static str> {
        let (root, expr) = parse_fragment(src);
        let id = SymbolId::from_parts(1, 0);
        let refs = vec![ResolvedRef {
            range: ident_range(&root, name),
            to: Resolution::Symbol(id),
        }];
        let mut effects = HashMap::new();
        effects.insert(id, effect);
        let env = StubEnv { effects };
        let mut cx = EffectCx::new(&refs, context, &env);
        cx.check_expr(&expr);
        cx.into_diagnostics().iter().map(|d| d.code).collect()
    }

    #[test]
    fn a_fn_call_is_allowed_everywhere() {
        for ctx in [
            BodyContext::View,
            BodyContext::Computed,
            BodyContext::Fn,
            BodyContext::Action,
            BodyContext::Event,
            BodyContext::Task,
        ] {
            assert!(check("helper()", ctx, "helper", EffectClass::Read).is_empty());
            assert!(check("helper()", ctx, "helper", EffectClass::Pure).is_empty());
        }
    }

    #[test]
    fn a_view_calling_an_action_is_e2502() {
        assert_eq!(
            check("mutate()", BodyContext::View, "mutate", EffectClass::Action),
            vec!["E2502"]
        );
    }

    #[test]
    fn a_computed_calling_an_action_is_e2502() {
        assert_eq!(
            check(
                "mutate()",
                BodyContext::Computed,
                "mutate",
                EffectClass::Action
            ),
            vec!["E2502"]
        );
    }

    #[test]
    fn a_computed_calling_a_task_is_e2502() {
        assert_eq!(
            check("fetch()", BodyContext::Computed, "fetch", EffectClass::Task),
            vec!["E2502"]
        );
    }

    #[test]
    fn a_fn_calling_an_action_is_e2501_not_e2502() {
        // A plain `fn` is not a reactive context, so its violation is the generic E2501.
        assert_eq!(
            check("mutate()", BodyContext::Fn, "mutate", EffectClass::Action),
            vec!["E2501"]
        );
    }

    #[test]
    fn an_action_calling_an_action_is_allowed() {
        assert!(
            check(
                "mutate()",
                BodyContext::Action,
                "mutate",
                EffectClass::Action
            )
            .is_empty()
        );
    }

    #[test]
    fn an_event_calling_an_action_is_allowed() {
        assert!(
            check(
                "mutate()",
                BodyContext::Event,
                "mutate",
                EffectClass::Action
            )
            .is_empty()
        );
    }

    #[test]
    fn an_action_calling_a_task_directly_is_e2501() {
        // Starting a task is a spawn form, not an ordinary call; a direct task call in an
        // action body violates the matrix.
        assert_eq!(
            check("fetch()", BodyContext::Action, "fetch", EffectClass::Task),
            vec!["E2501"]
        );
    }

    #[test]
    fn a_task_calling_a_task_directly_is_allowed() {
        assert!(check("fetch()", BodyContext::Task, "fetch", EffectClass::Task).is_empty());
    }

    #[test]
    fn a_task_calling_an_action_is_e2501() {
        assert_eq!(
            check("mutate()", BodyContext::Task, "mutate", EffectClass::Action),
            vec!["E2501"]
        );
    }

    #[test]
    fn a_nested_call_is_still_checked() {
        // The action call is nested inside an argument to an allowed fn call; the walk
        // must descend into it.
        assert_eq!(
            check(
                "helper(mutate())",
                BodyContext::View,
                "mutate",
                EffectClass::Action
            ),
            vec!["E2502"]
        );
    }

    #[test]
    fn an_unclassified_callee_is_skipped() {
        // The environment classifies no symbol here, so no matrix check fires (and the
        // walk does not panic).
        let (root, expr) = parse_fragment("value()");
        let refs = vec![ResolvedRef {
            range: ident_range(&root, "value"),
            to: Resolution::Symbol(SymbolId::from_parts(9, 9)),
        }];
        let env = StubEnv {
            effects: HashMap::new(),
        };
        let mut cx = EffectCx::new(&refs, BodyContext::View, &env);
        cx.check_expr(&expr);
        assert!(cx.into_diagnostics().is_empty());
    }

    #[test]
    fn malformed_input_does_not_panic() {
        for src in ["mutate(", "()", "f(g(", "helper()"] {
            let (root, expr) = parse_fragment(src);
            let refs: Vec<ResolvedRef> = root
                .descendants_with_tokens()
                .into_iter()
                .filter_map(|e| e.as_token().cloned())
                .filter(|t| t.kind() == SyntaxKind::Ident)
                .map(|t| ResolvedRef {
                    range: t.text_range(),
                    to: Resolution::Symbol(SymbolId::from_parts(1, 0)),
                })
                .collect();
            let mut effects = HashMap::new();
            effects.insert(SymbolId::from_parts(1, 0), EffectClass::Action);
            let env = StubEnv { effects };
            let mut cx = EffectCx::new(&refs, BodyContext::View, &env);
            cx.check_expr(&expr);
            let _ = cx.into_diagnostics();
        }
    }
}
