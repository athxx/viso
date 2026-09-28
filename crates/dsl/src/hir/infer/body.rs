//! Blocks, statements and closures: `let` bindings, assignments, `return`
//! against the enclosing signature, loops and their `break`/`continue`, and the
//! value a block evaluates to.

use super::{InferCx, LoopFrame, child_exprs, first_child_expr, is_numeric_ty};
use crate::ast::{AstNode, Block, Expr};
use crate::diag::Diagnostic;
use crate::hir::ty::Ty;
use crate::resolve::Resolution;
use crate::resolve::suggest::{Candidate, attach, nearest};
use crate::syntax::{SyntaxKind, SyntaxNode, SyntaxToken, TextRange};

impl InferCx<'_> {
    /// Gives the local bound at `at` (a parameter or pattern name token) the type
    /// `ty`.
    pub fn bind_local(&mut self, at: TextRange, ty: Ty) {
        if let Some(Resolution::Local(slot)) = self.refs.get(&at).copied() {
            self.locals.insert(slot, ty);
        }
    }

    /// Type-checks a callable body: `params` are the parameter name spans and their
    /// types, `ret` the declared return type. With a declared return type the body's
    /// value and every `return` are checked against it; without one the callable
    /// returns `Unit`.
    pub fn check_callable(&mut self, params: &[(TextRange, Ty)], ret: Option<&Ty>, body: &Block) {
        for (at, ty) in params {
            self.bind_local(*at, ty.clone());
        }
        self.returns.push(Some(ret.cloned().unwrap_or(Ty::Unit)));
        let loops = std::mem::take(&mut self.loops);
        let _ = self.infer_block(body.syntax(), ret);
        self.loops = loops;
        self.returns.pop();
    }

    /// Type-checks an event handler body, which returns nothing.
    pub fn check_handler(&mut self, body: &Block) {
        self.returns.push(Some(Ty::Unit));
        let loops = std::mem::take(&mut self.loops);
        let _ = self.infer_block(body.syntax(), None);
        self.loops = loops;
        self.returns.pop();
    }

    /// Types a block: every statement in order, then its value — the trailing
    /// expression without `;`, else `Never` when a statement diverged, else `Unit`.
    /// An expected type the value must meet is checked (a block that ends without
    /// a value where one is expected is `E2103`).
    pub fn infer_block(&mut self, block: &SyntaxNode, expected: Option<&Ty>) -> Ty {
        let stmts = block.children();
        let mut diverges = false;
        for (i, stmt) in stmts.iter().enumerate() {
            let last = i + 1 == stmts.len();
            if last && let Some(value) = self.tail_value(stmt, expected) {
                return if diverges { Ty::Never } else { value };
            }
            diverges |= self.infer_stmt(stmt);
        }
        if diverges {
            return Ty::Never;
        }
        match expected {
            Some(want) if !matches!(want, Ty::Unit | Ty::Unknown) => {
                let range = block
                    .descendants_with_tokens()
                    .into_iter()
                    .filter_map(|e| e.as_token().cloned())
                    .rfind(|t| !t.kind().is_trivia())
                    .map_or(block.text_range(), |t| t.text_range());
                let expected = self.describe(want);
                let message = format!("this block has no value; expected `{expected}`");
                self.diagnostics.push(
                    Diagnostic::error("E2103", range, message)
                        .expecting([expected], self.describe(&Ty::Unit)),
                );
                want.clone()
            }
            _ => Ty::Unit,
        }
    }

    /// The value of a block's last statement when it is the block's tail: an
    /// expression without `;`, or (where a value is expected) an `if` or `match`
    /// statement.
    fn tail_value(&mut self, stmt: &SyntaxNode, expected: Option<&Ty>) -> Option<Ty> {
        let has_semi = stmt
            .children_with_tokens()
            .into_iter()
            .filter_map(|e| e.as_token().cloned())
            .any(|t| t.kind() == SyntaxKind::Semi);
        match stmt.kind() {
            SyntaxKind::ExprStmt if !has_semi => {
                let value = first_child_expr(stmt)?;
                Some(self.infer_expr(&value, expected))
            }
            SyntaxKind::IfStmt if expected.is_some() => {
                let tys = self.if_branches(stmt, expected);
                Some(self.unify_branches(&tys, expected, stmt))
            }
            SyntaxKind::MatchStmt if expected.is_some() && !has_semi => {
                let tys = self.check_match(stmt, expected);
                Some(self.unify_branches(&tys, expected, stmt))
            }
            _ => None,
        }
    }

    /// Types one statement, reporting whether it always diverges (control never
    /// reaches the next statement).
    fn infer_stmt(&mut self, stmt: &SyntaxNode) -> bool {
        match stmt.kind() {
            SyntaxKind::ExprStmt => match first_child_expr(stmt) {
                Some(value) => self.infer_expr(&value, None) == Ty::Never,
                None => false,
            },
            SyntaxKind::LetStmt => self.infer_let(stmt),
            SyntaxKind::AssignStmt => {
                self.infer_assign(stmt);
                false
            }
            SyntaxKind::ReturnStmt => {
                self.infer_return(stmt);
                true
            }
            SyntaxKind::BreakStmt => {
                self.infer_break(stmt);
                true
            }
            SyntaxKind::ContinueStmt => {
                if self.loops.is_empty() {
                    self.diagnostics.push(Diagnostic::error(
                        "E2803",
                        stmt.text_range(),
                        "`continue` outside a loop",
                    ));
                }
                true
            }
            SyntaxKind::WhileStmt => {
                if let Some(cond) = first_child_expr(stmt) {
                    let _ = self.infer_expr(&cond, Some(&Ty::Bool));
                }
                let _ = self.loop_body(stmt, false);
                false
            }
            SyntaxKind::LoopStmt => self.loop_body(stmt, true).is_none(),
            SyntaxKind::ForStmt => {
                let iterable = match first_child_expr(stmt) {
                    Some(iter) => self.infer_expr(&iter, None),
                    None => Ty::Unknown,
                };
                let elem = iterable.element().cloned().unwrap_or(Ty::Unknown);
                if let Some(pattern) = child_of(stmt, SyntaxKind::Pattern) {
                    self.bind_pattern(&pattern, &elem);
                    if let Some(iter) = first_child_expr(stmt) {
                        let carry = self.carry(iter.syntax());
                        self.define_pattern(&pattern, &carry);
                    }
                    self.check_irrefutable(&pattern, "a `for` pattern");
                }
                let _ = self.loop_body(stmt, false);
                false
            }
            SyntaxKind::IfStmt => {
                let tys = self.if_branches(stmt, None);
                has_else(stmt) && tys.iter().all(|t| *t == Ty::Never)
            }
            SyntaxKind::MatchStmt => {
                let tys = self.check_match(stmt, None);
                !tys.is_empty() && tys.iter().all(|t| *t == Ty::Never)
            }
            SyntaxKind::EmitStmt => {
                self.infer_emit(stmt);
                false
            }
            SyntaxKind::TransactionStmt => match child_of(stmt, SyntaxKind::Block) {
                Some(block) => self.infer_block(&block, None) == Ty::Never,
                None => false,
            },
            _ => false,
        }
    }

    /// `emit event(args);`: the event is one the enclosing component declares, and
    /// the arguments give its parameters, positionally in order or by name, each typed
    /// against its parameter. An unknown event, an argument naming no parameter or one
    /// already given, an extra argument, and a missing one are `E3202`.
    fn infer_emit(&mut self, stmt: &SyntaxNode) {
        let env = self.env;
        let args = emit_args(stmt);
        let Some(event) = stmt
            .children_with_tokens()
            .into_iter()
            .filter_map(|e| e.as_token().cloned())
            .find(is_name)
        else {
            return self.infer_loose(&args);
        };
        let params = self
            .symbol_at(event.text_range())
            .and_then(|id| env.record_fields(id));
        let Some(params) = params else {
            if let Some(events) = env
                .enclosing_component()
                .and_then(|c| env.component_events(c))
            {
                let text = event.text();
                let name = text.trim_start_matches("r#");
                let candidates = events.iter().map(|e| Candidate {
                    name: &e.name,
                    declared_at: Some(e.declared_at),
                });
                let suggestions = nearest(name, candidates);
                let range = event.text_range();
                let mut diagnostic = Diagnostic::error(
                    "E3202",
                    range,
                    format!("`emit` names an event of its component, which declares no `{name}`"),
                );
                attach(&mut diagnostic, range, &suggestions);
                self.diagnostics.push(diagnostic);
            }
            return self.infer_loose(&args);
        };
        let event = event.text().to_string();
        let mut given = vec![false; params.len()];
        let mut next = 0;
        let mut misfit = false;
        for (label, value) in &args {
            let index = match label {
                Some(label) => params.iter().position(|p| p.name == label.text()),
                None => {
                    next += 1;
                    (next <= params.len()).then(|| next - 1)
                }
            };
            let at = label
                .as_ref()
                .map_or(value.syntax().text_range(), SyntaxToken::text_range);
            let problem = match index {
                Some(i) if given[i] => {
                    Some(format!("`{}` of `{event}` is given twice", params[i].name))
                }
                Some(i) => {
                    given[i] = true;
                    let param = &params[i];
                    if param.ty.has_unknown() {
                        let _ = self.infer_expr(value, None);
                    } else {
                        let _ = self.infer_promoted(value, &param.ty);
                    }
                    None
                }
                None => Some(match label {
                    Some(label) => format!("`{event}` has no parameter `{}`", label.text()),
                    None => format!("`{event}` takes {} argument(s)", params.len()),
                }),
            };
            if let Some(message) = problem {
                misfit = true;
                let mut diagnostic = Diagnostic::error("E3202", at, message);
                if let Some(label) = label.as_ref().filter(|_| index.is_none()) {
                    let candidates = params.iter().map(|p| Candidate {
                        name: &p.name,
                        declared_at: Some(p.declared_at),
                    });
                    let text = label.text();
                    let suggestions = nearest(&text, candidates);
                    attach(&mut diagnostic, at, &suggestions);
                }
                self.diagnostics.push(diagnostic);
                let _ = self.infer_expr(value, None);
            }
        }
        // A misfit argument is likely the missing parameter misspelled or misplaced;
        // reporting both would say the same thing twice.
        if misfit {
            return;
        }
        let missing: Vec<_> = params
            .iter()
            .zip(&given)
            .filter(|(_, given)| !**given)
            .map(|(p, _)| p)
            .collect();
        if !missing.is_empty() {
            let names: Vec<String> = missing.iter().map(|p| format!("`{}`", p.name)).collect();
            let mut diagnostic = Diagnostic::error(
                "E3202",
                stmt.text_range(),
                format!("`emit {event}` is missing {}", names.join(", ")),
            );
            for p in missing {
                diagnostic
                    .related
                    .push((p.declared_at, format!("`{}` is declared here", p.name)));
            }
            self.diagnostics.push(diagnostic);
        }
    }

    /// Types arguments that have no parameter to type them against.
    fn infer_loose(&mut self, args: &[(Option<SyntaxToken>, Expr)]) {
        for (_, value) in args {
            let _ = self.infer_expr(value, None);
        }
    }

    /// `let pattern (: T)? = init;`: the initializer types against the annotation,
    /// and the pattern binds the annotated (else inferred) type. A `let` pattern
    /// must be irrefutable (`E2303` otherwise).
    fn infer_let(&mut self, stmt: &SyntaxNode) -> bool {
        let annotation = stmt
            .children()
            .into_iter()
            .find(|c| matches!(c.kind(), SyntaxKind::TypePath | SyntaxKind::TupleType))
            .map(|node| self.annotation_ty(&node, node.text_range()));
        let init = match first_child_expr(stmt) {
            Some(init) => self.infer_expr(&init, annotation.as_ref()),
            None => Ty::Unknown,
        };
        let ty = match annotation {
            Some(ty) => ty,
            None if init == Ty::Never => Ty::Unknown,
            None => init.clone(),
        };
        if let Some(pattern) = child_of(stmt, SyntaxKind::Pattern) {
            self.bind_pattern(&pattern, &ty);
            if super::lens::has_mut(stmt) {
                self.mark_mutable_in(&pattern);
            }
            if let Some(init) = first_child_expr(stmt) {
                let carry = self.carry(init.syntax());
                self.define_pattern(&pattern, &carry);
            }
            self.check_irrefutable(&pattern, "a `let` pattern");
        }
        init == Ty::Never
    }

    /// `target op= value;`: the value types against the target (a scale factor for
    /// `*=`/`/=` on a dimensional target is a bare number, not the target's type).
    fn infer_assign(&mut self, stmt: &SyntaxNode) {
        let exprs = child_exprs(stmt);
        let target = match exprs.first() {
            Some(target) => {
                let ty = self.infer_expr(target, None);
                self.check_writable(target.syntax());
                ty
            }
            None => Ty::Unknown,
        };
        let Some(value) = exprs.get(1) else {
            return;
        };
        let op = stmt
            .children_with_tokens()
            .into_iter()
            .filter_map(|e| e.as_token().cloned())
            .map(|t| t.kind())
            .find(|k| super::is_assign_op(*k));
        let typed = match op {
            Some(SyntaxKind::Eq | SyntaxKind::PlusEq | SyntaxKind::MinusEq) => true,
            Some(_) => is_numeric_ty(&target),
            None => false,
        };
        if typed && target != Ty::Unknown {
            let _ = self.infer_expr(value, Some(&target));
        } else {
            let _ = self.infer_expr(value, None);
        }
        if let Some(target) = exprs.first() {
            self.define_target(target.syntax(), value.syntax());
        }
    }

    /// `return value?;` against the enclosing callable's return type.
    fn infer_return(&mut self, stmt: &SyntaxNode) {
        let value = first_child_expr(stmt);
        let Some(frame) = self.returns.last().cloned() else {
            self.diagnostics.push(Diagnostic::error(
                "E2803",
                stmt.text_range(),
                "`return` outside a function or closure",
            ));
            if let Some(value) = &value {
                let _ = self.infer_expr(value, None);
            }
            return;
        };
        match (value, frame) {
            (Some(value), frame) => {
                let _ = self.infer_expr(&value, frame.as_ref());
            }
            (None, Some(ret)) if !matches!(ret, Ty::Unit | Ty::Unknown) => {
                let expected = self.describe(&ret);
                let message = format!("`return` needs a value of type `{expected}`");
                self.diagnostics.push(
                    Diagnostic::error("E2103", stmt.text_range(), message)
                        .expecting([expected], self.describe(&Ty::Unit)),
                );
            }
            (None, _) => {}
        }
    }

    /// `break value?;`: only inside a loop, and a value only out of `loop`.
    fn infer_break(&mut self, stmt: &SyntaxNode) {
        let value = first_child_expr(stmt);
        let Some(frame) = self.loops.last() else {
            self.diagnostics.push(Diagnostic::error(
                "E2803",
                stmt.text_range(),
                "`break` outside a loop",
            ));
            if let Some(value) = &value {
                let _ = self.infer_expr(value, None);
            }
            return;
        };
        let carries_value = frame.carries_value;
        let so_far = frame.break_ty.clone();
        let ty = match &value {
            Some(value) if !carries_value => {
                self.diagnostics.push(Diagnostic::error(
                    "E2803",
                    stmt.text_range(),
                    "`break` with a value is only allowed in `loop`",
                ));
                let _ = self.infer_expr(value, None);
                Ty::Unit
            }
            Some(value) => {
                let want = so_far.as_ref().filter(|t| **t != Ty::Unit);
                self.infer_expr(value, want)
            }
            None => Ty::Unit,
        };
        let merged = match so_far {
            Some(prev) => self.unify_branches(&[prev, ty], None, stmt),
            None => ty,
        };
        if let Some(frame) = self.loops.last_mut() {
            frame.break_ty = Some(merged);
        }
    }

    /// Types a loop statement's body block in a fresh loop frame, returning the
    /// type its `break`s carried out (`None` when nothing breaks out).
    fn loop_body(&mut self, stmt: &SyntaxNode, carries_value: bool) -> Option<Ty> {
        self.loops.push(LoopFrame {
            carries_value,
            break_ty: None,
        });
        if let Some(block) = child_of(stmt, SyntaxKind::Block) {
            let _ = self.infer_block(&block, None);
        }
        self.loops.pop().and_then(|frame| frame.break_ty)
    }

    /// Types an `if` statement's condition and branches, returning each branch's
    /// type (the `else if` chain flattened).
    fn if_branches(&mut self, stmt: &SyntaxNode, expected: Option<&Ty>) -> Vec<Ty> {
        let mut tys = Vec::new();
        if let Some(cond) = first_child_expr(stmt) {
            let _ = self.infer_expr(&cond, Some(&Ty::Bool));
        }
        for child in stmt.children() {
            match child.kind() {
                SyntaxKind::Block => tys.push(self.infer_block(&child, expected)),
                SyntaxKind::IfStmt => {
                    let nested = self.if_branches(&child, expected);
                    if has_else(&child) && nested.iter().all(|t| *t == Ty::Never) {
                        tys.push(Ty::Never);
                    } else if expected.is_some() {
                        tys.extend(nested);
                    } else {
                        tys.push(Ty::Unit);
                    }
                }
                _ => {}
            }
        }
        if !has_else(stmt) {
            tys.push(Ty::Unit);
        }
        tys
    }

    /// Types a closure `|params| (-> R)? body`. A parameter's type comes from its
    /// annotation, else from the expected function type; one that has neither is
    /// `E2401` — unless `opaque`, when the closure is an argument to a callee whose
    /// signature this pass cannot see.
    pub(super) fn infer_closure(
        &mut self,
        node: &SyntaxNode,
        expected: Option<&Ty>,
        opaque: bool,
    ) -> Ty {
        let (expected_params, expected_ret) = match expected {
            Some(Ty::Fn(params, ret)) => (Some(params), Some(ret.as_ref())),
            _ => (None, None),
        };
        let params: Vec<SyntaxNode> = child_of(node, SyntaxKind::ClosureParams)
            .map(|list| {
                list.children()
                    .into_iter()
                    .filter(|c| c.kind() == SyntaxKind::ClosureParam)
                    .collect()
            })
            .unwrap_or_default();
        let expected_params = expected_params.filter(|ps| ps.len() == params.len());
        let mut param_tys = Vec::with_capacity(params.len());
        for (i, param) in params.iter().enumerate() {
            if super::lens::has_mut(param) {
                self.mark_mutable_in(param);
            }
            let annotation = param
                .children()
                .into_iter()
                .find(|c| matches!(c.kind(), SyntaxKind::TypePath | SyntaxKind::TupleType));
            let ty = match (annotation, expected_params.and_then(|ps| ps.get(i))) {
                (Some(node), _) => self.annotation_ty(&node, node.text_range()),
                (None, Some(ty)) if *ty != Ty::Unknown => ty.clone(),
                (None, _) => {
                    if !opaque {
                        let name = child_of(param, SyntaxKind::Pattern)
                            .map_or_else(String::new, |p| p.text().to_string());
                        let message = format!(
                            "cannot infer the type of the closure parameter `{}`; annotate it",
                            name.trim()
                        );
                        self.diagnostics.push(Diagnostic::error(
                            "E2401",
                            param.text_range(),
                            message,
                        ));
                    }
                    Ty::Unknown
                }
            };
            if let Some(pattern) = child_of(param, SyntaxKind::Pattern) {
                self.bind_pattern(&pattern, &ty);
                self.check_irrefutable(&pattern, "a closure parameter");
            }
            param_tys.push(ty);
        }
        let annotated_ret = node
            .children()
            .into_iter()
            .find(|c| matches!(c.kind(), SyntaxKind::TypePath | SyntaxKind::TupleType))
            .map(|ret| self.annotation_ty(&ret, ret.text_range()));
        let ret = annotated_ret.or_else(|| expected_ret.filter(|t| **t != Ty::Unknown).cloned());

        self.returns.push(ret.clone());
        let loops = std::mem::take(&mut self.loops);
        let body_ty = match child_of(node, SyntaxKind::Block) {
            Some(block) => self.infer_block(&block, ret.as_ref()),
            None => match node.children().into_iter().rev().find_map(Expr::cast) {
                Some(value) => self.infer_expr(&value, ret.as_ref()),
                None => Ty::Unknown,
            },
        };
        self.loops = loops;
        self.returns.pop();

        let ret = ret.unwrap_or(match body_ty {
            Ty::Never => Ty::Unknown,
            other => other,
        });
        Ty::Fn(param_tys, Box::new(ret))
    }
}

/// The first direct child of `node` of kind `kind`.
/// The arguments of an `emit`, each with its `name:` label when it has one.
fn emit_args(stmt: &SyntaxNode) -> Vec<(Option<SyntaxToken>, Expr)> {
    let Some(list) = child_of(stmt, SyntaxKind::ArgumentList) else {
        return Vec::new();
    };
    list.children()
        .into_iter()
        .filter(|a| a.kind() == SyntaxKind::Argument)
        .filter_map(|arg| {
            let value = first_child_expr(&arg)?;
            let tokens: Vec<SyntaxToken> = arg
                .children_with_tokens()
                .into_iter()
                .filter_map(|e| e.as_token().cloned())
                .filter(|t| !t.kind().is_trivia())
                .collect();
            let label = match tokens.as_slice() {
                [name, colon, ..] if is_name(name) && colon.kind() == SyntaxKind::Colon => {
                    Some(name.clone())
                }
                _ => None,
            };
            Some((label, value))
        })
        .collect()
}

fn is_name(token: &SyntaxToken) -> bool {
    matches!(token.kind(), SyntaxKind::Ident | SyntaxKind::RawIdent)
}

fn child_of(node: &SyntaxNode, kind: SyntaxKind) -> Option<SyntaxNode> {
    node.children().into_iter().find(|c| c.kind() == kind)
}

/// Whether an `if` statement has an `else` branch.
fn has_else(stmt: &SyntaxNode) -> bool {
    stmt.children_with_tokens()
        .into_iter()
        .filter_map(|e| e.as_token().cloned())
        .any(|t| t.kind() == SyntaxKind::ElseKw)
}
