//! Blocks and statements: bindings, assignments, control flow, `emit` and
//! `start`.

use super::super::ir::{BinaryOp, Const, Inst, Num, PathStep, Reg};
use super::{LoopCx, Lower, Lowerer, Place};
use viso_behavior::TaskPolicy;

use crate::ast::{AssignablePath, AstNode, CallExpr, Expr, StartStmt};
use crate::hir::Ty;
use crate::hir::infer::body::{child_of, emit_args, is_name};
use crate::hir::infer::list::{LIST_METHODS, edits_receiver};
use crate::hir::infer::{child_exprs, first_child_expr, is_assign_op};
use crate::resolve::{LocalSlot, Resolution};
use crate::syntax::{SyntaxKind, SyntaxNode};

/// The storage an assignment writes.
enum Root {
    /// A local's register.
    Local(Reg),
    /// A component state slot.
    State(u32),
}

/// One step from an assignment's root to the place it writes.
enum Step {
    Field(u32),
    Index(Expr),
}

impl Lowerer<'_, '_> {
    /// Lowers the statements of `block`, returning its tail value when `value` is
    /// asked for and the block ends in one.
    pub(super) fn block(&mut self, block: &SyntaxNode, value: bool) -> Lower<Option<Reg>> {
        let stmts = block.children();
        for (i, stmt) in stmts.iter().enumerate() {
            let last = i + 1 == stmts.len();
            let range = stmt.text_range();
            let tail = self.at(range, |l| {
                if last && value {
                    l.tail(stmt)
                } else {
                    l.stmt(stmt).map(|()| None)
                }
            })?;
            if tail.is_some() {
                return Ok(tail);
            }
        }
        Ok(None)
    }

    /// A block's last statement as its value, when it is one; otherwise it lowers
    /// as a statement.
    fn tail(&mut self, stmt: &SyntaxNode) -> Lower<Option<Reg>> {
        let has_semi = stmt
            .children_with_tokens()
            .into_iter()
            .filter_map(|e| e.as_token().cloned())
            .any(|t| t.kind() == SyntaxKind::Semi);
        match stmt.kind() {
            SyntaxKind::ExprStmt if !has_semi => match first_child_expr(stmt) {
                Some(value) => self.expr(&value).map(Some),
                None => Ok(None),
            },
            SyntaxKind::IfStmt => self.if_chain(stmt, true),
            SyntaxKind::MatchStmt if !has_semi => self.match_value(stmt, true),
            _ => self.stmt(stmt).map(|()| None),
        }
    }

    fn stmt(&mut self, stmt: &SyntaxNode) -> Lower<()> {
        match stmt.kind() {
            SyntaxKind::ExprStmt => {
                if let Some(value) = first_child_expr(stmt) {
                    self.expr(&value)?;
                }
                Ok(())
            }
            SyntaxKind::LetStmt => {
                let Some(pattern) = child_of(stmt, SyntaxKind::Pattern) else {
                    return self.bail("a `let` without a pattern");
                };
                let Some(init) = first_child_expr(stmt) else {
                    return self.bail("a `let` without an initializer");
                };
                let ty = self.ty(&init)?;
                let src = self.expr(&init)?;
                self.destructure(&pattern, src, &ty)
            }
            SyntaxKind::AssignStmt => self.assign(stmt),
            SyntaxKind::ReturnStmt => {
                let src = match first_child_expr(stmt) {
                    Some(value) => self.expr(&value)?,
                    None => self.unit(),
                };
                self.emit(Inst::Return { src });
                Ok(())
            }
            SyntaxKind::BreakStmt => {
                if let Some(value) = first_child_expr(stmt) {
                    self.expr(&value)?;
                }
                let jump = self.jump();
                match self.frame().loops.last_mut() {
                    Some(cx) => cx.breaks.push(jump),
                    None => return self.bail("`break` outside a loop"),
                }
                Ok(())
            }
            SyntaxKind::ContinueStmt => {
                let jump = self.jump();
                match self.frame().loops.last_mut() {
                    Some(cx) => cx.continues.push(jump),
                    None => return self.bail("`continue` outside a loop"),
                }
                Ok(())
            }
            SyntaxKind::WhileStmt => {
                let Some(cond) = first_child_expr(stmt) else {
                    return self.bail("a `while` without a condition");
                };
                let top = self.here();
                let test = self.expr(&cond)?;
                let exit = self.jump_if(test, false);
                let cx = self.loop_body(stmt)?;
                self.emit(Inst::Jump { target: top });
                self.close_loop(cx, top, &[exit]);
                Ok(())
            }
            SyntaxKind::LoopStmt => {
                let top = self.here();
                let cx = self.loop_body(stmt)?;
                self.emit(Inst::Jump { target: top });
                self.close_loop(cx, top, &[]);
                Ok(())
            }
            SyntaxKind::ForStmt => self.for_loop(stmt),
            SyntaxKind::IfStmt => self.if_chain(stmt, false).map(|_| ()),
            SyntaxKind::MatchStmt => self.match_value(stmt, false).map(|_| ()),
            SyntaxKind::EmitStmt => self.emit_event(stmt),
            // An effect's cleanup lowers as the closure its body returns.
            SyntaxKind::CleanupClause => Ok(()),
            SyntaxKind::StartStmt => self.start(stmt),
            SyntaxKind::TransactionStmt => match child_of(stmt, SyntaxKind::Block) {
                Some(block) => self.block(&block, false).map(|_| ()),
                None => Ok(()),
            },
            kind => self.bail(format!("`{kind:?}` statements are not supported yet")),
        }
    }

    /// Lowers a loop statement's body in a fresh loop context.
    fn loop_body(&mut self, stmt: &SyntaxNode) -> Lower<LoopCx> {
        self.frame().loops.push(LoopCx::default());
        let lowered = match child_of(stmt, SyntaxKind::Block) {
            Some(block) => self.block(&block, false).map(|_| ()),
            None => Ok(()),
        };
        let cx = self.frame().loops.pop().unwrap_or_default();
        lowered.map(|()| cx)
    }

    /// Points a finished loop's `continue`s at `next` and its `break`s and `exits`
    /// past it.
    fn close_loop(&mut self, cx: LoopCx, next: u32, exits: &[usize]) {
        for at in &cx.continues {
            self.patch(*at, next);
        }
        self.patch_here(&cx.breaks);
        self.patch_here(exits);
    }

    /// `for pattern in iterable { .. }` over a list or a range: the iterable is
    /// evaluated once, then each element binds the pattern in turn.
    fn for_loop(&mut self, stmt: &SyntaxNode) -> Lower<()> {
        let Some(iterable) = first_child_expr(stmt) else {
            return self.bail("a `for` without an iterable");
        };
        let Some(pattern) = child_of(stmt, SyntaxKind::Pattern) else {
            return self.bail("a `for` without a pattern");
        };
        let ty = self.ty(&iterable)?;
        match ty {
            Ty::List(elem) => {
                let src = self.expr(&iterable)?;
                let list = self.copy(src);
                let len = self.reg();
                self.emit(Inst::Len {
                    dst: len,
                    src: list,
                });
                let i = self.constant(Const::Int(0));
                let top = self.here();
                let more = self.reg();
                self.emit(Inst::Binary {
                    dst: more,
                    op: super::super::ir::BinaryOp::Lt(Num::I64),
                    lhs: i,
                    rhs: len,
                });
                let exit = self.jump_if(more, false);
                let item = self.reg();
                self.emit(Inst::Index {
                    dst: item,
                    list,
                    index: i,
                });
                self.destructure(&pattern, item, &elem)?;
                let cx = self.loop_body(stmt)?;
                let next = self.here();
                self.increment(i, Num::I64);
                self.emit(Inst::Jump { target: top });
                self.close_loop(cx, next, &[exit]);
                Ok(())
            }
            Ty::Range(elem) | Ty::RangeInclusive(elem) => {
                let inclusive = matches!(self.ty(&iterable)?, Ty::RangeInclusive(_));
                let Some(num) = super::num_of(&elem).filter(|n| !n.is_float()) else {
                    return self.bail("only an integer range can be iterated");
                };
                // A range written in the head is iterated from its bounds, so
                // the loop builds no range value.
                let bounds = range_bounds(&iterable);
                let (i, hi) = if let [lo, hi] = &bounds[..] {
                    let fields = self.operands(&[lo.clone(), hi.clone()])?;
                    (self.copy(fields[0]), self.copy(fields[1]))
                } else {
                    let src = self.expr(&iterable)?;
                    let i = self.reg();
                    self.emit(Inst::Field {
                        dst: i,
                        src,
                        index: 0,
                    });
                    let hi = self.reg();
                    self.emit(Inst::Field {
                        dst: hi,
                        src,
                        index: 1,
                    });
                    (i, hi)
                };
                let top = self.here();
                let more = self.reg();
                let op = if inclusive {
                    super::super::ir::BinaryOp::Le(num)
                } else {
                    super::super::ir::BinaryOp::Lt(num)
                };
                self.emit(Inst::Binary {
                    dst: more,
                    op,
                    lhs: i,
                    rhs: hi,
                });
                let exit = self.jump_if(more, false);
                let item = self.copy(i);
                self.destructure(&pattern, item, &elem)?;
                let cx = self.loop_body(stmt)?;
                let next = self.here();
                let mut exits = vec![exit];
                if inclusive {
                    // The last element may be the type's maximum, so stop before
                    // stepping past it.
                    let last = self.reg();
                    self.emit(Inst::Binary {
                        dst: last,
                        op: super::super::ir::BinaryOp::Eq,
                        lhs: i,
                        rhs: hi,
                    });
                    exits.push(self.jump_if(last, true));
                }
                self.increment(i, num);
                self.emit(Inst::Jump { target: top });
                self.close_loop(cx, next, &exits);
                Ok(())
            }
            _ => self.bail("only a `List` or a range can be iterated"),
        }
    }

    /// `reg += 1` at `num`.
    fn increment(&mut self, reg: Reg, num: Num) {
        let one = self.constant(Const::Int(1));
        self.emit(Inst::Binary {
            dst: reg,
            op: super::super::ir::BinaryOp::Add(num),
            lhs: reg,
            rhs: one,
        });
    }

    /// An `if` chain. With `value`, each branch's tail value lands in one
    /// register, and a missing `else` gives `()`.
    pub(super) fn if_chain(&mut self, node: &SyntaxNode, value: bool) -> Lower<Option<Reg>> {
        let dst = if value { Some(self.reg()) } else { None };
        let mut ends = Vec::new();
        self.if_arm(node, dst, &mut ends)?;
        self.patch_here(&ends);
        Ok(dst)
    }

    fn if_arm(&mut self, node: &SyntaxNode, dst: Option<Reg>, ends: &mut Vec<usize>) -> Lower<()> {
        let Some(cond) = first_child_expr(node) else {
            return self.bail("an `if` without a condition");
        };
        let test = self.expr(&cond)?;
        let skip = self.jump_if(test, false);
        let cond = cond.syntax().clone();
        let mut branches = node.children().into_iter().filter(|c| {
            !c.ptr_eq(&cond)
                && matches!(
                    c.kind(),
                    SyntaxKind::Block | SyntaxKind::IfStmt | SyntaxKind::IfExpr
                )
        });
        let Some(then) = branches.next() else {
            return self.bail("an `if` without a block");
        };
        self.branch(&then, dst)?;
        let otherwise = branches.next();
        ends.push(self.jump());
        self.patch_here(&[skip]);
        match otherwise {
            Some(next) if next.kind() == SyntaxKind::Block => self.branch(&next, dst),
            Some(next) => {
                let next = match next.kind() {
                    SyntaxKind::IfStmt | SyntaxKind::IfExpr => next,
                    _ => return self.bail("an `else` that is neither a block nor an `if`"),
                };
                self.if_arm(&next, dst, ends)
            }
            None => {
                if let Some(dst) = dst {
                    self.emit(Inst::Const {
                        dst,
                        value: Const::Unit,
                    });
                }
                Ok(())
            }
        }
    }

    /// One branch block, its tail value moved into `dst` when one is wanted.
    pub(super) fn branch(&mut self, block: &SyntaxNode, dst: Option<Reg>) -> Lower<()> {
        let value = self.block(block, dst.is_some())?;
        if let Some(dst) = dst {
            let src = match value {
                Some(src) => src,
                None => self.unit(),
            };
            self.emit(Inst::Move { dst, src });
        }
        Ok(())
    }

    /// `target op= value;`: the target's indices evaluate, then the value; a
    /// compound assignment then reads the target, and the result is written back.
    fn assign(&mut self, stmt: &SyntaxNode) -> Lower<()> {
        let exprs = child_exprs(stmt);
        let [target, value] = exprs.as_slice() else {
            return self.bail("an assignment needs a target and a value");
        };
        let op = stmt
            .children_with_tokens()
            .into_iter()
            .filter_map(|e| e.as_token().cloned())
            .map(|t| t.kind())
            .find(|k| is_assign_op(*k));
        let (root, path) = self.target(target)?;
        let mut src = self.expr(value)?;
        if let Some(op) = op.and_then(compound_op) {
            let (lt, rt) = (self.ty(target)?, self.ty(value)?);
            let current = self.read(&root, &path);
            src = self.operate(op, current, &lt, src, &rt)?;
        }
        self.write(root, path, src);
        Ok(())
    }

    /// The root of the place `target` and the path to it, its indices
    /// evaluated in order.
    fn target(&mut self, target: &Expr) -> Lower<(Root, Vec<PathStep>)> {
        let (root, steps) = self.place(target)?;
        let mut path = Vec::with_capacity(steps.len());
        for step in steps {
            path.push(match step {
                Step::Field(index) => PathStep::Field(index),
                Step::Index(index) => {
                    let reg = self.expr(&index)?;
                    PathStep::Index(if self.is_local(reg) {
                        self.copy(reg)
                    } else {
                        reg
                    })
                }
            });
        }
        Ok((root, path))
    }

    /// `recv.name(args)` on a list, `None` when `name` is no list method. A
    /// method editing the list evaluates the place, then the arguments, then
    /// reads the list, edits it and writes it back.
    pub(super) fn list_call(&mut self, call: &CallExpr) -> Lower<Option<Reg>> {
        let Some(field) = call
            .callee()
            .and_then(|c| crate::ast::FieldExpr::cast(c.syntax().clone()))
        else {
            return Ok(None);
        };
        let (Some(recv), Some(name)) = (field.receiver(), field.field()) else {
            return Ok(None);
        };
        let name = name.text();
        if !LIST_METHODS.contains(&name.as_str())
            || !matches!(self.cx.type_of(&recv), Some(Ty::List(_)))
        {
            return Ok(None);
        }
        let args: Vec<Expr> = emit_args(call.syntax())
            .into_iter()
            .map(|(_, e)| e)
            .collect();
        if !edits_receiver(&name) {
            let list = self.expr(&recv)?;
            let args = self.operands(&args)?;
            return self.list_read(&name, list, &args).map(Some);
        }
        let (root, path) = self.target(&recv)?;
        let args = self.operands(&args)?;
        let mut list = self.read(&root, &path);
        let result = match (name.as_str(), args.as_slice()) {
            ("push", &[item]) => {
                self.emit(Inst::Push { list, item });
                self.unit()
            }
            ("insert", &[index, item]) => {
                self.emit(Inst::Insert { list, index, item });
                self.unit()
            }
            ("remove", &[index]) => {
                let dst = self.reg();
                self.emit(Inst::Remove { dst, list, index });
                dst
            }
            ("pop", []) => {
                let len = self.reg();
                self.emit(Inst::Len {
                    dst: len,
                    src: list,
                });
                let zero = self.constant(Const::Int(0));
                let dst = self.constant(Const::Nil);
                let empty = self.reg();
                self.emit(Inst::Binary {
                    dst: empty,
                    op: BinaryOp::Eq,
                    lhs: len,
                    rhs: zero,
                });
                let skip = self.jump_if(empty, true);
                let one = self.constant(Const::Int(1));
                let last = self.reg();
                self.emit(Inst::Binary {
                    dst: last,
                    op: BinaryOp::Sub(Num::I64),
                    lhs: len,
                    rhs: one,
                });
                self.emit(Inst::Remove {
                    dst,
                    list,
                    index: last,
                });
                self.patch_here(&[skip]);
                dst
            }
            ("clear", []) => {
                let zero = self.constant(Const::Int(0));
                self.emit(Inst::Truncate { list, len: zero });
                self.unit()
            }
            ("retain", &[keep]) => {
                let kept = self.reg();
                self.emit(Inst::List {
                    dst: kept,
                    items: Vec::new(),
                });
                self.each(list, |this, item| {
                    let yes = this.reg();
                    this.emit(Inst::CallValue {
                        dst: yes,
                        callee: keep,
                        args: vec![item],
                    });
                    let skip = this.jump_if(yes, false);
                    this.emit(Inst::Push { list: kept, item });
                    this.patch_here(&[skip]);
                });
                list = kept;
                self.unit()
            }
            _ => return self.bail(format!("`{name}` with these arguments")),
        };
        self.write(root, path, list);
        Ok(Some(result))
    }

    /// A list method that only reads `list`.
    fn list_read(&mut self, name: &str, list: Reg, args: &[Reg]) -> Lower<Reg> {
        let len = self.reg();
        self.emit(Inst::Len {
            dst: len,
            src: list,
        });
        Ok(match (name, args) {
            ("len", []) => len,
            ("is_empty", []) => {
                let zero = self.constant(Const::Int(0));
                let dst = self.reg();
                self.emit(Inst::Binary {
                    dst,
                    op: BinaryOp::Eq,
                    lhs: len,
                    rhs: zero,
                });
                dst
            }
            ("get", &[index]) => self.element(list, len, index),
            ("first", []) => {
                let index = self.constant(Const::Int(0));
                self.element(list, len, index)
            }
            ("last", []) => {
                let one = self.constant(Const::Int(1));
                let index = self.reg();
                self.emit(Inst::Binary {
                    dst: index,
                    op: BinaryOp::Sub(Num::I64),
                    lhs: len,
                    rhs: one,
                });
                self.element(list, len, index)
            }
            ("contains", &[wanted]) => {
                let found = self.constant(Const::Bool(false));
                self.each(list, |this, item| {
                    let same = this.reg();
                    this.emit(Inst::Binary {
                        dst: same,
                        op: BinaryOp::Eq,
                        lhs: item,
                        rhs: wanted,
                    });
                    let skip = this.jump_if(same, false);
                    let yes = this.constant(Const::Bool(true));
                    this.emit(Inst::Move {
                        dst: found,
                        src: yes,
                    });
                    this.patch_here(&[skip]);
                });
                found
            }
            _ => return self.bail(format!("`{name}` with these arguments")),
        })
    }

    /// `Some(list[index])` when `index` lies in `0..len`, else `None`.
    fn element(&mut self, list: Reg, len: Reg, index: Reg) -> Reg {
        let dst = self.constant(Const::Nil);
        let zero = self.constant(Const::Int(0));
        let below = self.reg();
        self.emit(Inst::Binary {
            dst: below,
            op: BinaryOp::Lt(Num::I64),
            lhs: index,
            rhs: zero,
        });
        let low = self.jump_if(below, true);
        let inside = self.reg();
        self.emit(Inst::Binary {
            dst: inside,
            op: BinaryOp::Lt(Num::I64),
            lhs: index,
            rhs: len,
        });
        let high = self.jump_if(inside, false);
        self.emit(Inst::Index { dst, list, index });
        self.patch_here(&[low, high]);
        dst
    }

    /// Runs `body` with each element of `list` in turn, in a fresh register.
    fn each(&mut self, list: Reg, mut body: impl FnMut(&mut Self, Reg)) {
        let len = self.reg();
        self.emit(Inst::Len {
            dst: len,
            src: list,
        });
        let i = self.constant(Const::Int(0));
        let top = self.here();
        let more = self.reg();
        self.emit(Inst::Binary {
            dst: more,
            op: BinaryOp::Lt(Num::I64),
            lhs: i,
            rhs: len,
        });
        let exit = self.jump_if(more, false);
        let item = self.reg();
        self.emit(Inst::Index {
            dst: item,
            list,
            index: i,
        });
        body(self, item);
        self.increment(i, Num::I64);
        self.emit(Inst::Jump { target: top });
        self.patch_here(&[exit]);
    }

    /// The root and steps of an assignment target.
    fn place(&mut self, target: &Expr) -> Lower<(Root, Vec<Step>)> {
        let node = target.syntax().clone();
        match node.kind() {
            SyntaxKind::ParenExpr => match first_child_expr(&node) {
                Some(inner) => self.place(&inner),
                None => self.bail("empty parentheses"),
            },
            SyntaxKind::PathExpr => {
                let Some(head) = node
                    .children_with_tokens()
                    .into_iter()
                    .filter_map(|e| e.as_token().cloned())
                    .find(is_name)
                else {
                    return self.bail("an assignment to nothing");
                };
                match self.cx.resolution_at(head.text_range()) {
                    Some(Resolution::Local(slot)) => self.local_root(slot),
                    Some(Resolution::Symbol(id)) => match self.b.places.get(&id) {
                        Some(&(_, Place::State(slot))) => Ok((Root::State(slot), Vec::new())),
                        Some(&(_, Place::Input(_))) => self.bail("an `input` is read-only"),
                        None => self.bail("only a local or a `state` can be assigned"),
                    },
                    Some(Resolution::Native(_)) => self.bail("a native cannot be assigned"),
                    Some(Resolution::Env) => self.bail("`env` is read-only"),
                    Some(Resolution::Theme) => self.bail("`theme` is read-only"),
                    None => self.bail("an assignment to an unresolved name"),
                }
            }
            SyntaxKind::FieldExpr => {
                let Some(field) = crate::ast::FieldExpr::cast(node.clone()) else {
                    return self.bail("a malformed field access");
                };
                let (Some(recv), Some(name)) = (field.receiver(), field.field()) else {
                    return self.bail("a field access without a receiver or name");
                };
                let ty = self.ty(&recv)?;
                let index = self.field_index(&ty, name.text().trim_start_matches("r#"))?;
                let (root, mut steps) = self.place(&recv)?;
                steps.push(Step::Field(index));
                Ok((root, steps))
            }
            SyntaxKind::IndexExpr => {
                let parts = child_exprs(&node);
                let [list, index] = parts.as_slice() else {
                    return self.bail("an index expression needs a receiver and an index");
                };
                let (root, mut steps) = self.place(list)?;
                steps.push(Step::Index(index.clone()));
                Ok((root, steps))
            }
            _ => self.bail("this assignment target is not supported yet"),
        }
    }

    fn local_root(&mut self, slot: LocalSlot) -> Lower<(Root, Vec<Step>)> {
        if self.frame().captured.contains(&slot) {
            return self.bail("a closure cannot assign a local it captures");
        }
        if self.frames.len() > 1 && !self.frame().locals.contains_key(&slot) {
            return self.bail("a closure cannot assign a local it captures");
        }
        Ok((Root::Local(self.local(slot)?), Vec::new()))
    }

    /// The current value at `root` along `path`.
    fn read(&mut self, root: &Root, path: &[PathStep]) -> Reg {
        let mut src = match root {
            Root::Local(reg) => *reg,
            Root::State(slot) => {
                let dst = self.reg();
                self.emit(Inst::LoadState { dst, slot: *slot });
                dst
            }
        };
        for step in path {
            let dst = self.reg();
            self.emit(match step {
                PathStep::Field(index) => Inst::Field {
                    dst,
                    src,
                    index: *index,
                },
                PathStep::Index(index) => Inst::Index {
                    dst,
                    list: src,
                    index: *index,
                },
            });
            src = dst;
        }
        src
    }

    /// Writes `src` at `root` along `path`.
    fn write(&mut self, root: Root, path: Vec<PathStep>, src: Reg) {
        match (root, path.is_empty()) {
            (Root::Local(dst), true) => {
                self.emit(Inst::Move { dst, src });
            }
            (Root::Local(root), false) => {
                self.emit(Inst::SetPath { root, path, src });
            }
            (Root::State(slot), true) => {
                self.emit(Inst::StoreState { slot, src });
            }
            (Root::State(slot), false) => {
                let root = self.reg();
                self.emit(Inst::LoadState { dst: root, slot });
                self.emit(Inst::SetPath { root, path, src });
                self.emit(Inst::StoreState { slot, src: root });
            }
        }
    }

    /// The state slot a `bind` source names and the steps into it, its indices
    /// evaluated in order.
    pub(super) fn lens(&mut self, source: &AssignablePath) -> Lower<(u32, Vec<PathStep>)> {
        let mut parts = source.syntax().children_with_tokens().into_iter();
        let Some(head) = parts
            .by_ref()
            .filter_map(|e| e.as_token().cloned())
            .find(is_name)
        else {
            return self.bail("a `bind` without a source");
        };
        let (slot, mut ty) = match self.cx.resolution_at(head.text_range()) {
            Some(Resolution::Symbol(id)) => match self.b.places.get(&id) {
                Some(&(_, Place::State(slot))) => {
                    let ty = self.env.resolution_ty(&Resolution::Symbol(id));
                    (slot, ty.unwrap_or(Ty::Unknown))
                }
                _ => return self.bail("a `bind` writes back to a `state`"),
            },
            _ => return self.bail("a `bind` writes back to a `state`"),
        };
        let mut path = Vec::new();
        let mut after_dot = false;
        for part in parts {
            if let Some(token) = part.as_token() {
                match token.kind() {
                    SyntaxKind::Dot => after_dot = true,
                    _ if after_dot && is_name(token) => {
                        after_dot = false;
                        let text = token.text();
                        let index = self.field_index(&ty, text.trim_start_matches("r#"))?;
                        ty = match &ty {
                            Ty::Named(id, ..) => self
                                .env
                                .record_fields(*id)
                                .and_then(|fields| fields.get(index as usize))
                                .map_or(Ty::Unknown, |f| f.ty.clone()),
                            Ty::Tuple(elems) => {
                                elems.get(index as usize).cloned().unwrap_or(Ty::Unknown)
                            }
                            _ => Ty::Unknown,
                        };
                        path.push(PathStep::Field(index));
                    }
                    _ => {}
                }
            } else if let Some(index) = part.as_node().and_then(|n| Expr::cast(n.clone())) {
                let reg = self.expr(&index)?;
                path.push(PathStep::Index(if self.is_local(reg) {
                    self.copy(reg)
                } else {
                    reg
                }));
                ty = match ty {
                    Ty::List(element) => *element,
                    _ => Ty::Unknown,
                };
            }
        }
        Ok((slot, path))
    }

    /// The current value of the state `slot` along `path`.
    pub(super) fn read_state(&mut self, slot: u32, path: &[PathStep]) -> Reg {
        self.read(&Root::State(slot), path)
    }

    /// Writes `src` to the state `slot` along `path`.
    pub(super) fn write_state(&mut self, slot: u32, path: Vec<PathStep>, src: Reg) {
        self.write(Root::State(slot), path, src);
    }

    /// `start call as slot { .. };`: the arguments evaluate here; the task
    /// starts once the transaction commits, its handlers closures over the
    /// locals they read.
    fn start(&mut self, stmt: &SyntaxNode) -> Lower<()> {
        let Some(start) = StartStmt::cast(stmt.clone()) else {
            return self.bail("a malformed `start`");
        };
        let Some(call) = start.call() else {
            return self.bail("a `start` without a call");
        };
        let result = self.ty(&call)?;
        let (task, args) = self.task_call(&call)?;
        let handlers = start.handlers();
        let payload = |pattern: Option<crate::ast::Pattern>, block: Option<crate::ast::Block>| {
            Some((pattern?.syntax().clone(), block?.syntax().clone()))
        };
        let success = handlers
            .as_ref()
            .and_then(|h| h.success().next())
            .and_then(|s| payload(s.pattern(), s.block()));
        let error = handlers
            .as_ref()
            .and_then(|h| h.error().next())
            .and_then(|e| payload(e.pattern(), e.block()));
        let done = self.done_closure(success, error, &result)?;
        let cancelled = match handlers
            .as_ref()
            .and_then(|h| h.cancelled().next())
            .and_then(|c| c.block())
        {
            Some(block) => Some(self.cancelled_closure(block.syntax())?),
            None => None,
        };
        let policy = crate::hir::start::slot_policy(&start, &mut Vec::new());
        let slot = start
            .slot()
            .and_then(|s| s.name())
            .map(|name| self.b.task_slot(name.text().trim_start_matches("r#")));
        self.emit(Inst::Start {
            task,
            args,
            done,
            cancelled,
            instance: 0,
            slot,
            policy: policy.unwrap_or(TaskPolicy::KeepLatest),
        });
        Ok(())
    }

    /// `emit event(args);`: the arguments evaluate in source order and pass in the
    /// event's parameter order.
    fn emit_event(&mut self, stmt: &SyntaxNode) -> Lower<()> {
        let Some(name) = stmt
            .children_with_tokens()
            .into_iter()
            .filter_map(|e| e.as_token().cloned())
            .find(is_name)
        else {
            return self.bail("an `emit` without an event");
        };
        let Some(symbol) = self.cx.symbol_at(name.text_range()) else {
            return self.bail("this event is not declared");
        };
        let (Some(&event), Some(params)) =
            (self.b.events.get(&symbol), self.env.record_fields(symbol))
        else {
            return self.bail("this event is not declared by the component");
        };
        let params: Vec<String> = params.iter().map(|p| p.name.clone()).collect();
        let args = emit_args(stmt);
        let exprs: Vec<Expr> = args.iter().map(|(_, e)| e.clone()).collect();
        let regs = self.operands(&exprs)?;
        let mut ordered: Vec<Option<Reg>> = vec![None; params.len()];
        let mut next = 0;
        for ((label, _), reg) in args.iter().zip(regs) {
            let index = match label {
                Some(label) => {
                    let text = label.text();
                    params
                        .iter()
                        .position(|p| p == text.trim_start_matches("r#"))
                }
                None => {
                    next += 1;
                    Some(next - 1)
                }
            };
            match index.and_then(|i| ordered.get_mut(i)) {
                Some(slot) => *slot = Some(reg),
                None => return self.bail("an `emit` argument names no parameter"),
            }
        }
        let Some(args) = ordered.into_iter().collect::<Option<Vec<_>>>() else {
            return self.bail("an `emit` is missing an argument");
        };
        self.emit(Inst::Emit { event, args });
        Ok(())
    }
}

/// The binary operator a compound assignment applies.
fn compound_op(op: SyntaxKind) -> Option<SyntaxKind> {
    Some(match op {
        SyntaxKind::PlusEq => SyntaxKind::Plus,
        SyntaxKind::MinusEq => SyntaxKind::Minus,
        SyntaxKind::StarEq => SyntaxKind::Star,
        SyntaxKind::SlashEq => SyntaxKind::Slash,
        SyntaxKind::PercentEq => SyntaxKind::Percent,
        SyntaxKind::AmpEq => SyntaxKind::Amp,
        SyntaxKind::PipeEq => SyntaxKind::Pipe,
        SyntaxKind::CaretEq => SyntaxKind::Caret,
        SyntaxKind::ShlEq => SyntaxKind::Shl,
        SyntaxKind::ShrEq => SyntaxKind::Shr,
        _ => return None,
    })
}

/// The bounds of `iterable` when it is a closed range expression, through
/// parentheses.
fn range_bounds(iterable: &Expr) -> Vec<Expr> {
    let mut node = iterable.syntax().clone();
    while node.kind() == SyntaxKind::ParenExpr {
        let Some(inner) = first_child_expr(&node) else {
            return Vec::new();
        };
        node = inner.syntax().clone();
    }
    if node.kind() != SyntaxKind::RangeExpr {
        return Vec::new();
    }
    child_exprs(&node)
}
