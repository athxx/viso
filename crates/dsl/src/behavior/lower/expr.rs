//! Expressions: every expression lowers to the register holding its value.

use viso_behavior::native::SchemaTy;
use viso_ui::adaptive::EnvField;

use super::super::ir::{
    BinaryOp, Const, DisplayKind, FuncId, Function, FunctionKind, Inst, Num, Reg, UnaryOp,
};
use super::{Frame, Lower, Lowerer, Place, assigns, num_of};
use crate::ast::{AstNode, CallExpr, CastExpr, Expr, FieldExpr, PathExpr};
use crate::hir::Ty;
use crate::hir::infer::body::{child_of, emit_args};
use crate::hir::infer::format::{Hole, Piece, template_literal, template_pieces};
use crate::hir::infer::pattern::unescape;
use crate::hir::infer::{NativeCall, VariantInfo, VariantPayload, is_spread, record_spread};
use crate::hir::infer::{
    binary_op_kind, builtin_variant, child_exprs, first_child_expr, in_base_unit, is_integer_ty,
    parse_float_literal, parse_int_literal, split_unit_literal, unary_op_kind, unify_numeric,
};
use crate::resolve::{Resolution, SymbolId, SymbolKind};
use crate::syntax::{SyntaxKind, SyntaxNode, SyntaxToken};

impl Lowerer<'_, '_> {
    /// Lowers `e`, recording its instructions at its span.
    pub(super) fn expr(&mut self, e: &Expr) -> Lower<Reg> {
        let range = e.syntax().text_range();
        self.at(range, |l| {
            if let Some(ty) = l.cx.type_of(e) {
                l.check_repr(ty)?;
            }
            l.expr_here(e)
        })
    }

    fn expr_here(&mut self, e: &Expr) -> Lower<Reg> {
        let node = e.syntax().clone();
        match node.kind() {
            SyntaxKind::LiteralExpr => self.literal(e),
            SyntaxKind::PathExpr => self.path(&node),
            SyntaxKind::FieldExpr => self.field(&node),
            SyntaxKind::OptionalFieldExpr => self.optional_field(&node),
            SyntaxKind::RecordExpr => self.record(&node),
            SyntaxKind::CallExpr => self.call(&node),
            SyntaxKind::IndexExpr => {
                let parts = child_exprs(&node);
                let [list, index] = parts.as_slice() else {
                    return self.bail("an index expression needs a receiver and an index");
                };
                if !matches!(self.ty(list)?, Ty::List(_)) {
                    return self.bail("only a `List` can be indexed");
                }
                let regs = self.operands(&parts)?;
                let dst = self.reg();
                self.emit(Inst::Index {
                    dst,
                    list: regs[0],
                    index: regs[1],
                });
                let _ = index;
                Ok(dst)
            }
            SyntaxKind::TryExpr => self.try_value(&node),
            SyntaxKind::RangeExpr => {
                let bounds = child_exprs(&node);
                if bounds.len() != 2 {
                    return self.bail("an open-ended range has no runtime value yet");
                }
                let fields = self.operands(&bounds)?;
                let dst = self.reg();
                self.emit(Inst::Make {
                    dst,
                    tag: 0,
                    fields,
                });
                Ok(dst)
            }
            SyntaxKind::ClosureExpr => self.closure(e),
            SyntaxKind::BlockExpr => match child_of(&node, SyntaxKind::Block) {
                Some(block) => match self.block(&block, true)? {
                    Some(value) => Ok(value),
                    None => Ok(self.unit()),
                },
                None => self.bail("a block expression without a block"),
            },
            SyntaxKind::BinaryExpr => self.binary(&node),
            SyntaxKind::UnaryExpr => self.unary(&node),
            SyntaxKind::CastExpr => self.cast(e),
            SyntaxKind::ParenExpr => match first_child_expr(&node) {
                Some(inner) => self.expr(&inner),
                None => self.bail("empty parentheses"),
            },
            SyntaxKind::TupleExpr => {
                let elems = child_exprs(&node);
                if elems.is_empty() {
                    return Ok(self.unit());
                }
                let fields = self.operands(&elems)?;
                let dst = self.reg();
                self.emit(Inst::Make {
                    dst,
                    tag: 0,
                    fields,
                });
                Ok(dst)
            }
            SyntaxKind::ListExpr => {
                let items = self.operands(&child_exprs(&node))?;
                let dst = self.reg();
                self.emit(Inst::List { dst, items });
                Ok(dst)
            }
            SyntaxKind::IfExpr => match self.if_chain(&node, true)? {
                Some(value) => Ok(value),
                None => Ok(self.unit()),
            },
            SyntaxKind::MatchExpr => match self.match_value(&node, true)? {
                Some(value) => Ok(value),
                None => Ok(self.unit()),
            },
            kind => self.bail(format!("`{kind:?}` expressions are not supported yet")),
        }
    }

    // --- literals ------------------------------------------------------------

    fn literal(&mut self, e: &Expr) -> Lower<Reg> {
        let Some(token) = e
            .syntax()
            .children_with_tokens()
            .into_iter()
            .filter_map(|t| t.as_token().cloned())
            .find(|t| !t.kind().is_trivia())
        else {
            return self.bail("an empty literal");
        };
        let ty = self.cx.type_of(e).cloned().unwrap_or(Ty::Unknown);
        let float = matches!(ty, Ty::F32 | Ty::F64 | Ty::InferFloat) || ty.is_dimensional();
        let text = token.text().to_string();
        let value = match token.kind() {
            SyntaxKind::IntLiteral => match parse_int_literal(&text) {
                Some(v) if float => Const::Float(round(v as f64, &ty)),
                Some(v) => Const::Int(v),
                None => return self.bail("an integer literal out of range"),
            },
            SyntaxKind::FloatLiteral => match parse_float_literal(&text) {
                Some(v) => Const::Float(round(v, &ty)),
                None => return self.bail("a malformed float literal"),
            },
            SyntaxKind::TrueKw => Const::Bool(true),
            SyntaxKind::FalseKw => Const::Bool(false),
            SyntaxKind::CharLiteral => {
                let body = text.trim_start_matches('\'').trim_end_matches('\'');
                let mut chars = unescape(body)
                    .unwrap_or_default()
                    .chars()
                    .collect::<Vec<_>>();
                match (chars.pop(), chars.is_empty()) {
                    (Some(c), true) => Const::Char(c),
                    _ => return self.bail("a malformed character literal"),
                }
            }
            SyntaxKind::StringLiteral | SyntaxKind::RawStringLiteral => {
                match template_literal(e)
                    .and_then(|(text, raw)| if raw { Some(text) } else { unescape(&text) })
                {
                    Some(text) => Const::Str(text),
                    None => return self.bail("a malformed string literal"),
                }
            }
            SyntaxKind::ColorLiteral => match color(&text) {
                Some(rgba) => Const::Color(rgba),
                None => return self.bail("a malformed color literal"),
            },
            SyntaxKind::NoneKw => Const::Nil,
            SyntaxKind::UnitLiteral => match unit_literal(&text) {
                Some(value) => value,
                None => return self.bail("a malformed suffixed literal"),
            },
            _ => return self.bail("an unknown literal"),
        };
        Ok(self.constant(value))
    }

    // --- names ---------------------------------------------------------------

    fn path(&mut self, node: &SyntaxNode) -> Lower<Reg> {
        let Some(path) = PathExpr::cast(node.clone()) else {
            return self.bail("a malformed path");
        };
        let segments: Vec<SyntaxToken> = path.segments().collect();
        let Some(head) = segments.first() else {
            return self.bail("an empty path");
        };
        match self.cx.resolution_at(head.text_range()) {
            Some(Resolution::Local(slot)) if segments.len() == 1 => self.local(slot),
            Some(Resolution::Symbol(id)) if segments.len() >= 2 => {
                let Some((index, variant)) = self.variant(id, &segments[1].text()) else {
                    return self.bail("this path names no enum variant");
                };
                match variant.payload {
                    VariantPayload::Unit => Ok(self.constant(Const::Tag(index))),
                    _ => self.bail("a variant constructor used as a value"),
                }
            }
            Some(Resolution::Symbol(id)) if segments.len() == 1 => {
                self.symbol_value(id, &head.text())
            }
            Some(Resolution::Env) if segments.len() == 1 => self.env_value(),
            Some(Resolution::Theme) if segments.len() == 1 => self.env_field(EnvField::Theme),
            Some(Resolution::Native(id)) => {
                match self.env.natives().and_then(|n| n.variant_by_id(id)) {
                    Some(variant) => Ok(self.constant(Const::Tag(variant.index))),
                    None => self.bail("a native used as a value"),
                }
            }
            None if builtin_variant(&segments) == Some("None") => Ok(self.constant(Const::Nil)),
            _ => self.bail("this path has no runtime value yet"),
        }
    }

    /// Variant `name` of the enum `owner`, with its index.
    fn variant(&self, owner: SymbolId, name: &str) -> Option<(u32, VariantInfo)> {
        let variants = self.env.enum_variants(owner)?;
        let index = variants.iter().position(|v| v.name == name)?;
        Some((index as u32, variants[index].clone()))
    }

    /// The value of the declaration `id`: a state or input slot, the result of a
    /// computed value or constant, or a function value.
    pub(super) fn symbol_value(&mut self, id: SymbolId, name: &str) -> Lower<Reg> {
        if let Some(&(_, place)) = self.b.places.get(&id) {
            let dst = self.reg();
            self.emit(match place {
                Place::State(slot) => Inst::LoadState { dst, slot },
                Place::Input(slot) => Inst::LoadInput { dst, slot },
            });
            return Ok(dst);
        }
        match self.env.symbol_kind(id) {
            Some(SymbolKind::Computed | SymbolKind::Const) => {
                let func = self.b.func_of(id, name);
                let dst = self.reg();
                self.emit(Inst::Call {
                    dst,
                    func,
                    args: Vec::new(),
                });
                Ok(dst)
            }
            Some(SymbolKind::Function | SymbolKind::Action) => {
                let func = self.b.func_of(id, name);
                let dst = self.reg();
                self.emit(Inst::Closure {
                    dst,
                    func,
                    captures: Vec::new(),
                });
                Ok(dst)
            }
            _ => self.bail(format!("`{name}` has no runtime value yet")),
        }
    }

    /// The whole `env`, an `Environment` of every field's slot.
    pub(super) fn env_value(&mut self) -> Lower<Reg> {
        let fields = EnvField::ENVIRONMENT
            .into_iter()
            .map(|field| self.env_field(field))
            .collect::<Lower<Vec<Reg>>>()?;
        let dst = self.reg();
        self.emit(Inst::Make {
            dst,
            tag: 0,
            fields,
        });
        Ok(dst)
    }

    /// The `env` field `field`, read from the slot the runtime fills.
    fn env_field(&mut self, field: EnvField) -> Lower<Reg> {
        let Some(slot) = self.b.env_slot(field) else {
            return self.bail("`env` is read only in a component's view");
        };
        let dst = self.reg();
        self.emit(Inst::LoadState { dst, slot });
        Ok(dst)
    }

    /// The field `name` of `recv` when `recv` is the bare `env`.
    fn env_member(&self, recv: &Expr, name: &str) -> Option<EnvField> {
        let path = PathExpr::cast(recv.syntax().clone())?;
        let mut segments = path.segments();
        let head = segments.next()?;
        if segments.next().is_some()
            || !matches!(
                self.cx.resolution_at(head.text_range()),
                Some(Resolution::Env)
            )
        {
            return None;
        }
        EnvField::named(name)
    }

    // --- members -------------------------------------------------------------

    /// The field index `name` reads on a value of type `recv`.
    pub(super) fn field_index(&self, recv: &Ty, name: &str) -> Lower<u32> {
        let index = match recv {
            Ty::Named(id) => self
                .env
                .record_fields(*id)
                .and_then(|fields| fields.iter().position(|f| f.name == name)),
            Ty::Tuple(elems) => name.parse::<usize>().ok().filter(|i| *i < elems.len()),
            _ => None,
        };
        match index {
            Some(index) => Ok(index as u32),
            None => self.bail(format!("the member `{name}` has no runtime layout yet")),
        }
    }

    fn field(&mut self, node: &SyntaxNode) -> Lower<Reg> {
        let Some(field) = FieldExpr::cast(node.clone()) else {
            return self.bail("a malformed field access");
        };
        let (Some(recv), Some(name)) = (field.receiver(), field.field()) else {
            return self.bail("a field access without a receiver or name");
        };
        let name = name.text();
        let name = name.trim_start_matches("r#");
        if let Some(field) = self.env_member(&recv, name) {
            return self.env_field(field);
        }
        if let Some(native) = self.cx.native_call(node.text_range()) {
            let Some(entry) = self.env.natives().and_then(|n| n.function_by_id(native.id)) else {
                return self.bail("the native this reads is not registered");
            };
            let args = self.operands(&[recv])?;
            let import = self.b.native(entry);
            let dst = self.reg();
            self.emit(Inst::Native { dst, import, args });
            return Ok(dst);
        }
        let ty = self.ty(&recv)?;
        // A resource's slot holds its state.
        if matches!(ty, Ty::Resource(..)) && name == "state" {
            return self.expr(&recv);
        }
        let index = self.field_index(&ty, name)?;
        let src = self.expr(&recv)?;
        let dst = self.reg();
        self.emit(Inst::Field { dst, src, index });
        Ok(dst)
    }

    fn optional_field(&mut self, node: &SyntaxNode) -> Lower<Reg> {
        let Some(recv) = first_child_expr(node) else {
            return self.bail("`?.` without a receiver");
        };
        let Some(name) = node
            .children_with_tokens()
            .into_iter()
            .filter_map(|e| e.as_token().cloned())
            .skip_while(|t| t.kind() != SyntaxKind::QuestionDot)
            .find(|t| t.kind() != SyntaxKind::QuestionDot && !t.kind().is_trivia())
        else {
            return self.bail("`?.` without a member name");
        };
        let name = name.text();
        let name = name.trim_start_matches("r#");
        let (inner, optional) = match self.ty(&recv)? {
            Ty::Option(inner) => (*inner, true),
            other => (other, false),
        };
        let index = self.field_index(&inner, name)?;
        let src = self.expr(&recv)?;
        let dst = self.reg();
        if !optional {
            self.emit(Inst::Field { dst, src, index });
            return Ok(dst);
        }
        let nil = self.reg();
        self.emit(Inst::IsNil { dst: nil, src });
        self.emit(Inst::Move { dst, src });
        let skip = self.jump_if(nil, true);
        self.emit(Inst::Field { dst, src, index });
        self.patch_here(&[skip]);
        Ok(dst)
    }

    /// A record or record-variant literal: fields evaluate in source order, then
    /// the `..base`; missing fields come from the base, else from their defaults.
    fn record(&mut self, node: &SyntaxNode) -> Lower<Reg> {
        let heads: Vec<SyntaxToken> = node
            .children_with_tokens()
            .into_iter()
            .filter_map(|e| e.as_token().cloned())
            .take_while(|t| t.kind() != SyntaxKind::LBrace)
            .filter(|t| matches!(t.kind(), SyntaxKind::Ident | SyntaxKind::RawIdent))
            .collect();
        let Some(last) = heads.last() else {
            return self.bail("a record literal without a type");
        };
        let record = self
            .cx
            .symbol_at(last.text_range())
            .filter(|id| self.env.record_fields(*id).is_some());
        let (owner, tag, fields) = match record {
            Some(id) => (
                Some(id),
                0,
                self.env.record_fields(id).unwrap_or_default().to_vec(),
            ),
            None => {
                let owner = heads
                    .len()
                    .checked_sub(2)
                    .and_then(|i| self.cx.symbol_at(heads[i].text_range()));
                let variant = owner.and_then(|id| self.variant(id, &last.text()));
                match variant {
                    Some((
                        tag,
                        VariantInfo {
                            payload: VariantPayload::Record(fields),
                            ..
                        },
                    )) => (None, tag, fields),
                    _ => return self.bail("this record literal names no record type"),
                }
            }
        };

        let entries: Vec<SyntaxNode> = node
            .children()
            .into_iter()
            .filter(|c| c.kind() == SyntaxKind::RecordExprField && !is_spread(c))
            .collect();
        let mut values: Vec<Option<Reg>> = vec![None; fields.len()];
        for (i, entry) in entries.iter().enumerate() {
            let Some(label) = entry
                .children_with_tokens()
                .into_iter()
                .filter_map(|e| e.as_token().cloned())
                .find(|t| matches!(t.kind(), SyntaxKind::Ident | SyntaxKind::RawIdent))
            else {
                continue;
            };
            let name = label.text();
            let name = name.trim_start_matches("r#");
            let Some(index) = fields.iter().position(|f| f.name == name) else {
                return self.bail(format!("no field `{name}` in this record"));
            };
            let mut reg = match first_child_expr(entry) {
                Some(value) => self.expr(&value)?,
                None => match self.cx.resolution_at(label.text_range()) {
                    Some(Resolution::Local(slot)) => self.local(slot)?,
                    Some(Resolution::Symbol(id)) => self.symbol_value(id, name)?,
                    Some(Resolution::Native(_)) => {
                        return self.bail(format!("the shorthand `{name}` names a native"));
                    }
                    Some(Resolution::Env) => self.env_value()?,
                    Some(Resolution::Theme) => self.env_field(EnvField::Theme)?,
                    None => return self.bail(format!("the shorthand `{name}` names nothing")),
                },
            };
            let later_assigns = entries[i + 1..].iter().any(super::assigns)
                || record_spread(node).is_some_and(|b| super::assigns(b.syntax()));
            if self.is_local(reg) && later_assigns {
                reg = self.copy(reg);
            }
            values[index] = Some(reg);
        }
        let base = match record_spread(node) {
            Some(base) => Some(self.expr(&base)?),
            None => None,
        };
        let mut regs = Vec::with_capacity(fields.len());
        for (index, value) in values.into_iter().enumerate() {
            let reg = match (value, base, owner) {
                (Some(reg), _, _) => reg,
                (None, Some(base), _) => {
                    let dst = self.reg();
                    self.emit(Inst::Field {
                        dst,
                        src: base,
                        index: index as u32,
                    });
                    dst
                }
                (None, None, Some(record)) if fields[index].has_default => {
                    let name = format!("{}.{}", last.text(), fields[index].name);
                    let func = self.b.field_default_slot(record, index as u32, &name);
                    let dst = self.reg();
                    self.emit(Inst::Call {
                        dst,
                        func,
                        args: Vec::new(),
                    });
                    dst
                }
                _ => {
                    let name = &fields[index].name;
                    return self.bail(format!("the field `{name}` has no value"));
                }
            };
            regs.push(reg);
        }
        let dst = self.reg();
        self.emit(Inst::Make {
            dst,
            tag,
            fields: regs,
        });
        Ok(dst)
    }

    // --- calls ---------------------------------------------------------------

    fn call(&mut self, node: &SyntaxNode) -> Lower<Reg> {
        let Some(call) = CallExpr::cast(node.clone()) else {
            return self.bail("a malformed call");
        };
        if let Some(native) = self.cx.native_call(node.text_range()) {
            return self.native(&call, native);
        }
        let Some(callee) = call.callee() else {
            return self.bail("a call without a callee");
        };
        let segments: Vec<SyntaxToken> = match PathExpr::cast(callee.syntax().clone()) {
            Some(path) if callee.syntax().kind() == SyntaxKind::PathExpr => {
                path.segments().collect()
            }
            _ => Vec::new(),
        };
        let head = segments
            .first()
            .and_then(|h| self.cx.resolution_at(h.text_range()));
        if head.is_none() && segments.len() == 1 && segments[0].text() == "format" {
            return self.format(node);
        }
        let args = emit_args(node);
        if head.is_none() && segments.len() == 1 && segments[0].text() == "untracked" {
            return match args.as_slice() {
                [(None, value)] => self.expr(value),
                _ => self.bail("`untracked` takes one value"),
            };
        }
        if args.iter().any(|(label, _)| label.is_some()) {
            return self.bail("named call arguments are not supported yet");
        }
        let args: Vec<Expr> = args.into_iter().map(|(_, e)| e).collect();
        if head.is_none()
            && let Some(ctor) = builtin_variant(&segments)
            && ctor != "None"
        {
            let Some(arg) = args.first() else {
                return self.bail(format!("`{ctor}` needs a value"));
            };
            let value = self.expr(arg)?;
            if ctor == "Some" {
                return Ok(value);
            }
            let dst = self.reg();
            self.emit(Inst::Make {
                dst,
                tag: u32::from(ctor == "Err"),
                fields: vec![value],
            });
            return Ok(dst);
        }
        match (head, segments.len()) {
            (Some(Resolution::Symbol(id)), n) if n >= 2 && self.env.enum_variants(id).is_some() => {
                let Some((tag, _)) = self.variant(id, &segments[1].text()) else {
                    return self.bail("this path names no enum variant");
                };
                let fields = self.operands(&args)?;
                let dst = self.reg();
                self.emit(Inst::Make { dst, tag, fields });
                Ok(dst)
            }
            (Some(Resolution::Symbol(id)), 1)
                if matches!(
                    self.env.symbol_kind(id),
                    Some(SymbolKind::Function | SymbolKind::Action)
                ) =>
            {
                let func = self.b.func_of(id, &segments[0].text());
                let args = self.operands(&args)?;
                let dst = self.reg();
                self.emit(Inst::Call { dst, func, args });
                Ok(dst)
            }
            (Some(Resolution::Symbol(id)), 1)
                if self.env.symbol_kind(id) == Some(SymbolKind::Task) =>
            {
                // A task calls another on its own fiber, suspending with it.
                let func = self.b.func_of(id, &segments[0].text());
                let args = self.operands(&args)?;
                let dst = self.reg();
                self.emit(Inst::Call { dst, func, args });
                Ok(dst)
            }
            _ if matches!(
                callee.syntax().kind(),
                SyntaxKind::FieldExpr | SyntaxKind::OptionalFieldExpr
            ) =>
            {
                self.bail("method calls are not supported yet")
            }
            _ => {
                if !matches!(self.ty(&callee)?, Ty::Fn(..)) {
                    return self.bail("this callee has no runtime value yet");
                }
                let mut all = Vec::with_capacity(args.len() + 1);
                all.push(callee);
                all.extend(args);
                let regs = self.operands(&all)?;
                let dst = self.reg();
                self.emit(Inst::CallValue {
                    dst,
                    callee: regs[0],
                    args: regs[1..].to_vec(),
                });
                Ok(dst)
            }
        }
    }

    /// A call bound to a native function: the receiver of a method call, then
    /// the arguments, evaluate in source order.
    fn native(&mut self, call: &CallExpr, native: NativeCall) -> Lower<Reg> {
        let Some((import, args)) = self.native_operands(call, native)? else {
            // Debug draw is removed from release builds, arguments and all.
            return Ok(self.unit());
        };
        let dst = self.reg();
        self.emit(Inst::Native { dst, import, args });
        Ok(dst)
    }

    /// The import and the evaluated arguments of a call bound to a native,
    /// `None` for a debug draw a release build strips.
    fn native_operands(
        &mut self,
        call: &CallExpr,
        native: NativeCall,
    ) -> Lower<Option<(u32, Vec<Reg>)>> {
        let Some(entry) = self.env.natives().and_then(|n| n.function_by_id(native.id)) else {
            return self.bail("the native this calls is not registered");
        };
        if entry.function.debug_draw && self.b.strips_debug_draw() {
            return Ok(None);
        }
        let args = emit_args(call.syntax());
        if args.iter().any(|(label, _)| label.is_some()) {
            return self.bail("named native arguments are not supported");
        }
        let mut exprs = Vec::with_capacity(args.len() + 1);
        if native.receiver {
            let Some(receiver) = call
                .callee()
                .and_then(|c| FieldExpr::cast(c.syntax().clone()))
                .and_then(|f| f.receiver())
            else {
                return self.bail("a method call without a receiver");
            };
            exprs.push(receiver);
        }
        exprs.extend(args.into_iter().map(|(_, e)| e));
        let params = entry.function.params;
        let mut args = Vec::with_capacity(exprs.len());
        for (i, e) in exprs.iter().enumerate() {
            let mut reg = if params.get(i).is_some_and(|p| p.ty == SchemaTy::Ticks) {
                // A tick duration was converted to whole ticks while typing.
                let Some(ticks) = self.cx.ticks_at(e.syntax().text_range()) else {
                    return self.bail("a tick duration that is not a compile-time constant");
                };
                self.constant(Const::Int(i128::from(ticks)))
            } else {
                self.expr(e)?
            };
            if self.is_local(reg) && exprs[i + 1..].iter().any(|l| assigns(l.syntax())) {
                reg = self.copy(reg);
            }
            args.push(reg);
        }
        let import = self.b.native(entry);
        Ok(Some((import, args)))
    }

    /// The task a `start` runs and its evaluated arguments: a `task`, or a
    /// native task through the task that awaits it.
    pub(super) fn task_call(&mut self, call: &Expr) -> Lower<(FuncId, Vec<Reg>)> {
        let Some(c) = CallExpr::cast(call.syntax().clone()) else {
            return self.bail("`start` takes a task call");
        };
        if let Some(native) = self.cx.native_call(call.syntax().text_range()) {
            let Some((import, args)) = self.native_operands(&c, native)? else {
                return self.bail("`start` takes a task call");
            };
            let name = self.name.clone();
            let task = self.b.native_task(import, args.len(), &name, self.module);
            return Ok((task, args));
        }
        let head = c
            .callee()
            .and_then(|callee| PathExpr::cast(callee.syntax().clone()))
            .and_then(|path| {
                let segments: Vec<SyntaxToken> = path.segments().collect();
                match &segments[..] {
                    [one] => Some(one.clone()),
                    _ => None,
                }
            });
        let symbol = head
            .as_ref()
            .and_then(|h| match self.cx.resolution_at(h.text_range()) {
                Some(Resolution::Symbol(id))
                    if self.env.symbol_kind(id) == Some(SymbolKind::Task) =>
                {
                    Some(id)
                }
                _ => None,
            });
        let (Some(symbol), Some(head)) = (symbol, head) else {
            return self.bail("`start` takes a task call");
        };
        let args = emit_args(call.syntax());
        if args.iter().any(|(label, _)| label.is_some()) {
            return self.bail("named call arguments are not supported yet");
        }
        let args: Vec<Expr> = args.into_iter().map(|(_, e)| e).collect();
        let task = self.b.func_of(symbol, &head.text());
        let args = self.operands(&args)?;
        Ok((task, args))
    }

    /// `format(template, args..)`: the arguments evaluate in source order, each
    /// non-`String` one is displayed, and the pieces concatenate.
    fn format(&mut self, node: &SyntaxNode) -> Lower<Reg> {
        let args = emit_args(node);
        let Some((_, template)) = args.first() else {
            return self.bail("`format` needs a template");
        };
        let Some(pieces) =
            template_literal(template).and_then(|(text, raw)| template_pieces(&text, raw))
        else {
            return self.bail("a malformed `format` template");
        };
        let exprs: Vec<Expr> = args[1..].iter().map(|(_, e)| e.clone()).collect();
        let regs = self.operands(&exprs)?;
        let mut positional = Vec::new();
        let mut named = Vec::new();
        for ((label, e), reg) in args[1..].iter().zip(regs) {
            let ty = self.ty(e)?;
            let shown = self.display(reg, &ty)?;
            match label {
                Some(label) => {
                    named.push((label.text().trim_start_matches("r#").to_string(), shown))
                }
                None => positional.push(shown),
            }
        }
        let mut positional = positional.into_iter();
        let mut parts = Vec::with_capacity(pieces.len());
        for piece in pieces {
            let reg = match piece {
                Piece::Text(text) => self.constant(Const::Str(text)),
                Piece::Hole(Hole::Positional) => match positional.next() {
                    Some(reg) => reg,
                    None => return self.bail("too few `format` arguments"),
                },
                Piece::Hole(Hole::Named(name)) => match named.iter().find(|(n, _)| *n == name) {
                    Some((_, reg)) => *reg,
                    None => return self.bail(format!("no `format` argument `{name}:`")),
                },
            };
            parts.push(reg);
        }
        if let [only] = parts.as_slice() {
            return Ok(*only);
        }
        if parts.is_empty() {
            return Ok(self.constant(Const::Str(String::new())));
        }
        let dst = self.reg();
        self.emit(Inst::Concat { dst, parts });
        Ok(dst)
    }

    /// The text of `src`, a value of type `ty`.
    fn display(&mut self, src: Reg, ty: &Ty) -> Lower<Reg> {
        let kind = match ty {
            Ty::String => return Ok(src),
            Ty::Bool => DisplayKind::Bool,
            Ty::I8 | Ty::I16 | Ty::I32 | Ty::I64 | Ty::InferInt => DisplayKind::Signed,
            Ty::U8 | Ty::U16 | Ty::U32 | Ty::U64 => DisplayKind::Unsigned,
            Ty::F32 => DisplayKind::F32,
            Ty::F64 | Ty::InferFloat => DisplayKind::F64,
            Ty::Char => DisplayKind::Char,
            Ty::Dp => DisplayKind::Dimension("dp"),
            Ty::Px => DisplayKind::Dimension("px"),
            Ty::Sp => DisplayKind::Dimension("sp"),
            Ty::Em => DisplayKind::Dimension("em"),
            Ty::Percent => DisplayKind::Dimension("%"),
            Ty::Duration => DisplayKind::Dimension("s"),
            Ty::Angle => DisplayKind::Dimension("deg"),
            Ty::Frequency => DisplayKind::Dimension("hz"),
            _ => return self.bail("this value cannot be displayed yet"),
        };
        let dst = self.reg();
        self.emit(Inst::Display { dst, src, kind });
        Ok(dst)
    }

    /// `value?`: an absent `Option` or an `Err` returns from the function as is;
    /// otherwise the present value.
    fn try_value(&mut self, node: &SyntaxNode) -> Lower<Reg> {
        let Some(inner) = first_child_expr(node) else {
            return self.bail("`?` without an operand");
        };
        let ty = self.ty(&inner)?;
        let src = self.expr(&inner)?;
        let flag = self.reg();
        match ty {
            Ty::Option(_) => {
                self.emit(Inst::IsNil { dst: flag, src });
                let ok = self.jump_if(flag, false);
                self.emit(Inst::Return { src });
                self.patch_here(&[ok]);
                Ok(src)
            }
            Ty::Result(..) => {
                self.emit(Inst::Tag { dst: flag, src });
                let ok = self.jump_if(flag, false);
                self.emit(Inst::Return { src });
                self.patch_here(&[ok]);
                let dst = self.reg();
                self.emit(Inst::Field { dst, src, index: 0 });
                Ok(dst)
            }
            _ => self.bail("`?` needs an `Option` or a `Result`"),
        }
    }

    // --- operators -----------------------------------------------------------

    fn binary(&mut self, node: &SyntaxNode) -> Lower<Reg> {
        let operands = child_exprs(node);
        let [lhs, rhs] = operands.as_slice() else {
            return self.bail("a binary expression needs two operands");
        };
        let Some(op) = binary_op_kind(node) else {
            return self.bail("a binary expression without an operator");
        };
        if matches!(op, SyntaxKind::AmpAmp | SyntaxKind::PipePipe) {
            let dst = self.reg();
            let l = self.expr(lhs)?;
            self.emit(Inst::Move { dst, src: l });
            let done = self.jump_if(dst, op == SyntaxKind::PipePipe);
            let r = self.expr(rhs)?;
            self.emit(Inst::Move { dst, src: r });
            self.patch_here(&[done]);
            return Ok(dst);
        }
        let (lt, rt) = (self.ty(lhs)?, self.ty(rhs)?);
        let regs = self.operands(&operands)?;
        self.operate(op, regs[0], &lt, regs[1], &rt)
    }

    /// Applies the binary operator `op` to `lhs: lt` and `rhs: rt`.
    pub(super) fn operate(
        &mut self,
        op: SyntaxKind,
        lhs: Reg,
        lt: &Ty,
        rhs: Reg,
        rt: &Ty,
    ) -> Lower<Reg> {
        let dst = self.reg();
        let op = match op {
            SyntaxKind::EqEq | SyntaxKind::Neq => {
                let mixed = num_of(lt)
                    .zip(num_of(rt))
                    .is_some_and(|(a, b)| a.is_float() != b.is_float());
                let (lhs, rhs) = if mixed {
                    (
                        self.coerce(lhs, lt, Num::F64),
                        self.coerce(rhs, rt, Num::F64),
                    )
                } else {
                    (lhs, rhs)
                };
                let op = if op == SyntaxKind::EqEq {
                    BinaryOp::Eq
                } else {
                    BinaryOp::Ne
                };
                self.emit(Inst::Binary { dst, op, lhs, rhs });
                return Ok(dst);
            }
            SyntaxKind::Lt | SyntaxKind::Le | SyntaxKind::Gt | SyntaxKind::Ge => {
                let num = match (lt, rt) {
                    _ if lt.is_dimensional() || rt.is_dimensional() => Num::F64,
                    (Ty::Char, Ty::Char) => Num::U32,
                    _ => match unify_numeric(lt, rt).as_ref().and_then(num_of) {
                        Some(num) => num,
                        None => return self.bail("only numbers and characters compare by order"),
                    },
                };
                let (lhs, rhs) = (self.coerce(lhs, lt, num), self.coerce(rhs, rt, num));
                let op = match op {
                    SyntaxKind::Lt => BinaryOp::Lt(num),
                    SyntaxKind::Le => BinaryOp::Le(num),
                    SyntaxKind::Gt => BinaryOp::Gt(num),
                    _ => BinaryOp::Ge(num),
                };
                self.emit(Inst::Binary { dst, op, lhs, rhs });
                return Ok(dst);
            }
            SyntaxKind::Plus if *lt == Ty::String => BinaryOp::Concat,
            op => {
                let num = match (lt, rt) {
                    (Ty::Bool, Ty::Bool) => Some(Num::U64),
                    _ if lt.is_dimensional() || rt.is_dimensional() => Some(Num::F64),
                    _ if matches!(op, SyntaxKind::Shl | SyntaxKind::Shr) => num_of(lt),
                    _ => unify_numeric(lt, rt).as_ref().and_then(num_of),
                };
                let Some(num) = num else {
                    return self.bail("this operator has no runtime meaning for these operands");
                };
                let (lhs, rhs) = if matches!(op, SyntaxKind::Shl | SyntaxKind::Shr) {
                    (lhs, rhs)
                } else {
                    (self.coerce(lhs, lt, num), self.coerce(rhs, rt, num))
                };
                let op = match op {
                    SyntaxKind::Plus => BinaryOp::Add(num),
                    SyntaxKind::Minus => BinaryOp::Sub(num),
                    SyntaxKind::Star => BinaryOp::Mul(num),
                    SyntaxKind::Slash => BinaryOp::Div(num),
                    SyntaxKind::Percent => BinaryOp::Rem(num),
                    SyntaxKind::Amp => BinaryOp::BitAnd(num),
                    SyntaxKind::Pipe => BinaryOp::BitOr(num),
                    SyntaxKind::Caret => BinaryOp::BitXor(num),
                    SyntaxKind::Shl => BinaryOp::Shl(num),
                    SyntaxKind::Shr => BinaryOp::Shr(num),
                    _ => return self.bail("an unknown binary operator"),
                };
                self.emit(Inst::Binary { dst, op, lhs, rhs });
                return Ok(dst);
            }
        };
        self.emit(Inst::Binary { dst, op, lhs, rhs });
        Ok(dst)
    }

    /// `src: ty` as an operand of a `num` operation: an integer converts to a
    /// float operation's type; every other value already has its representation.
    fn coerce(&mut self, src: Reg, ty: &Ty, num: Num) -> Reg {
        match num_of(ty) {
            Some(from) if num.is_float() && !from.is_float() => {
                let dst = self.reg();
                self.emit(Inst::Cast {
                    dst,
                    src,
                    from,
                    to: num,
                });
                dst
            }
            _ => src,
        }
    }

    fn unary(&mut self, node: &SyntaxNode) -> Lower<Reg> {
        let Some(operand) = first_child_expr(node) else {
            return self.bail("a unary expression without an operand");
        };
        if unary_op_kind(node) == Some(SyntaxKind::AwaitKw) {
            // A suspension point only: the task suspends inside the call.
            return self.expr(&operand);
        }
        let ty = self.ty(&operand)?;
        let src = self.expr(&operand)?;
        let num = num_of(&ty);
        let op = match (unary_op_kind(node), num) {
            (Some(SyntaxKind::Minus), Some(num)) => UnaryOp::Neg(num),
            (Some(SyntaxKind::Bang), _) if ty == Ty::Bool => UnaryOp::Not,
            (Some(SyntaxKind::Bang | SyntaxKind::Tilde), Some(num)) if !num.is_float() => {
                UnaryOp::BitNot(num)
            }
            _ => return self.bail("this unary operator has no runtime meaning here"),
        };
        let dst = self.reg();
        self.emit(Inst::Unary { dst, op, src });
        Ok(dst)
    }

    fn cast(&mut self, e: &Expr) -> Lower<Reg> {
        let Some(operand) = CastExpr::cast(e.syntax().clone()).and_then(|c| c.operand()) else {
            return self.bail("a cast without an operand");
        };
        let from_ty = self.ty(&operand)?;
        let to_ty = self.ty(e)?;
        let src = self.expr(&operand)?;
        let repr = |ty: &Ty| match ty {
            Ty::Bool => Some(Num::U64),
            Ty::Char => Some(Num::U32),
            other => num_of(other),
        };
        if to_ty == Ty::Char {
            return match from_ty {
                Ty::U8 | Ty::Char => Ok(src),
                _ => self.bail("only a `U8` casts to `Char`"),
            };
        }
        if to_ty == Ty::Bool && from_ty != Ty::Bool {
            return self.bail("nothing casts to `Bool`");
        }
        let (Some(from), Some(to)) = (repr(&from_ty), repr(&to_ty)) else {
            return self.bail("this cast has no runtime meaning yet");
        };
        if from == to {
            return Ok(src);
        }
        let dst = self.reg();
        self.emit(Inst::Cast { dst, src, from, to });
        Ok(dst)
    }

    // --- closures ------------------------------------------------------------

    /// A closure: its body lowers as a function of its parameters, and the locals
    /// it reads from enclosing functions are captured by value when it is made.
    fn closure(&mut self, e: &Expr) -> Lower<Reg> {
        let Ty::Fn(_, ret) = self.ty(e)? else {
            return self.bail("a closure without a function type");
        };
        let node = e.syntax().clone();
        let params: Vec<SyntaxNode> = child_of(&node, SyntaxKind::ClosureParams)
            .map(|list| {
                list.children()
                    .into_iter()
                    .filter(|c| c.kind() == SyntaxKind::ClosureParam)
                    .collect()
            })
            .unwrap_or_default();
        self.frames.push(Frame::default());
        let lowered = self.closure_body(&node, &params, *ret != Ty::Unit);
        let frame = self.frames.pop().expect("the closure frame");
        lowered?;
        let mut sources = Vec::with_capacity(frame.captures.len());
        for (slot, _) in &frame.captures {
            sources.push(self.local(*slot)?);
        }
        let func = self.b.push(Function {
            name: format!("{}/closure", self.name),
            kind: FunctionKind::Closure,
            symbol: None,
            module: self.module,
            params: params.len() as u32,
            captures: frame.captures.iter().map(|(_, reg)| *reg).collect(),
            body: Ok(frame.body),
        });
        let dst = self.reg();
        self.emit(Inst::Closure {
            dst,
            func,
            captures: sources,
        });
        Ok(dst)
    }

    /// An effect's `cleanup` block as a closure of no parameters, capturing
    /// the body's locals it reads.
    pub(super) fn cleanup_closure(&mut self, block: &SyntaxNode) -> Lower<Reg> {
        self.frames.push(Frame::default());
        let lowered = self.block(block, false).map(|_| {
            let src = self.unit();
            self.emit(Inst::Return { src });
        });
        let frame = self.frames.pop().expect("the cleanup frame");
        lowered?;
        let mut sources = Vec::with_capacity(frame.captures.len());
        for (slot, _) in &frame.captures {
            sources.push(self.local(*slot)?);
        }
        let func = self.b.push(Function {
            name: format!("{}/cleanup", self.name),
            kind: FunctionKind::Closure,
            symbol: None,
            module: self.module,
            params: 0,
            captures: frame.captures.iter().map(|(_, reg)| *reg).collect(),
            body: Ok(frame.body),
        });
        let dst = self.reg();
        self.emit(Inst::Closure {
            dst,
            func,
            captures: sources,
        });
        Ok(dst)
    }

    /// The closure a started task's value runs: the `success` handler with
    /// the value, or its `Ok` payload and the `error` handler with the `Err`
    /// payload of a task returning `result`. `None` without either handler.
    pub(super) fn done_closure(
        &mut self,
        success: Option<(SyntaxNode, SyntaxNode)>,
        error: Option<(SyntaxNode, SyntaxNode)>,
        result: &Ty,
    ) -> Lower<Option<Reg>> {
        if success.is_none() && error.is_none() {
            return Ok(None);
        }
        self.frames.push(Frame::default());
        let lowered = self.done_body(success, error, result);
        let frame = self.frames.pop().expect("the handler frame");
        lowered?;
        self.handler_closure(frame, 1, "done").map(Some)
    }

    fn done_body(
        &mut self,
        success: Option<(SyntaxNode, SyntaxNode)>,
        error: Option<(SyntaxNode, SyntaxNode)>,
        result: &Ty,
    ) -> Lower<()> {
        let value = self.reg();
        let handler = |l: &mut Self, (pattern, block): (SyntaxNode, SyntaxNode), src, ty: &Ty| {
            l.destructure(&pattern, src, ty)?;
            l.block(&block, false).map(|_| ())
        };
        let Ty::Result(ok, err) = result else {
            if let Some(success) = success {
                handler(self, success, value, result)?;
            }
            let src = self.unit();
            self.emit(Inst::Return { src });
            return Ok(());
        };
        let tag = self.reg();
        self.emit(Inst::Tag {
            dst: tag,
            src: value,
        });
        let to_error = self.jump_if(tag, true);
        if let Some(success) = success {
            let payload = self.reg();
            self.emit(Inst::Field {
                dst: payload,
                src: value,
                index: 0,
            });
            handler(self, success, payload, ok)?;
        }
        let src = self.unit();
        self.emit(Inst::Return { src });
        self.patch_here(&[to_error]);
        if let Some(error) = error {
            let payload = self.reg();
            self.emit(Inst::Field {
                dst: payload,
                src: value,
                index: 0,
            });
            handler(self, error, payload, err)?;
        }
        let src = self.unit();
        self.emit(Inst::Return { src });
        Ok(())
    }

    /// The `cancelled` handler `block` as a closure of no parameters.
    pub(super) fn cancelled_closure(&mut self, block: &SyntaxNode) -> Lower<Reg> {
        self.frames.push(Frame::default());
        let lowered = self.block(block, false).map(|_| {
            let src = self.unit();
            self.emit(Inst::Return { src });
        });
        let frame = self.frames.pop().expect("the handler frame");
        lowered?;
        self.handler_closure(frame, 0, "cancelled")
    }

    /// The closure of the lowered handler `frame`, capturing the locals it
    /// reads.
    fn handler_closure(&mut self, frame: Frame, params: u32, what: &str) -> Lower<Reg> {
        let mut sources = Vec::with_capacity(frame.captures.len());
        for (slot, _) in &frame.captures {
            sources.push(self.local(*slot)?);
        }
        let func = self.b.push(Function {
            name: format!("{}/{what}", self.name),
            kind: FunctionKind::Closure,
            symbol: None,
            module: self.module,
            params,
            captures: frame.captures.iter().map(|(_, reg)| *reg).collect(),
            body: Ok(frame.body),
        });
        let dst = self.reg();
        self.emit(Inst::Closure {
            dst,
            func,
            captures: sources,
        });
        Ok(dst)
    }

    fn closure_body(
        &mut self,
        node: &SyntaxNode,
        params: &[SyntaxNode],
        returns_value: bool,
    ) -> Lower<()> {
        let regs: Vec<Reg> = params.iter().map(|_| self.reg()).collect();
        for (param, reg) in params.iter().zip(regs) {
            let Some(pattern) = child_of(param, SyntaxKind::Pattern) else {
                continue;
            };
            let ty = self
                .cx
                .type_of(&Expr::cast(node.clone()).expect("a closure expression"))
                .and_then(|t| match t {
                    Ty::Fn(ps, _) => ps.get(params.iter().position(|p| p == param)?).cloned(),
                    _ => None,
                })
                .unwrap_or(Ty::Unknown);
            self.destructure(&pattern, reg, &ty)?;
        }
        let value = match child_of(node, SyntaxKind::Block) {
            Some(block) => self.block(&block, returns_value)?,
            None => match node.children().into_iter().rev().find_map(Expr::cast) {
                Some(value) => Some(self.expr(&value)?),
                None => None,
            },
        };
        let src = match value {
            Some(value) => value,
            None => self.unit(),
        };
        self.emit(Inst::Return { src });
        Ok(())
    }
}

/// `value` rounded to the precision of `ty`.
fn round(value: f64, ty: &Ty) -> f64 {
    if *ty == Ty::F32 {
        f64::from(value as f32)
    } else {
        value
    }
}

/// A suffixed literal's value: an integer for an integer type suffix, else the
/// number scaled to its dimension's base unit (seconds, degrees, hertz).
fn unit_literal(text: &str) -> Option<Const> {
    let (body, ty) = split_unit_literal(text)?;
    let suffix = &text[body.len()..];
    if is_integer_ty(&ty) {
        return parse_int_literal(body).map(Const::Int);
    }
    let value = match parse_int_literal(body) {
        Some(v) => v as f64,
        None => parse_float_literal(body)?,
    };
    Some(Const::Float(round(in_base_unit(value, suffix), &ty)))
}

/// A `#rgb`, `#rgba`, `#rrggbb` or `#rrggbbaa` color as `0xRRGGBBAA`.
fn color(text: &str) -> Option<u32> {
    let hex = text.strip_prefix('#')?;
    let digits: Vec<u32> = hex.chars().map(|c| c.to_digit(16)).collect::<Option<_>>()?;
    let channels: Vec<u32> = match digits.len() {
        3 | 4 => digits.iter().map(|d| d * 17).collect(),
        6 | 8 => digits.chunks(2).map(|p| p[0] * 16 + p[1]).collect(),
        _ => return None,
    };
    let alpha = channels.get(3).copied().unwrap_or(255);
    Some((channels[0] << 24) | (channels[1] << 16) | (channels[2] << 8) | alpha)
}
