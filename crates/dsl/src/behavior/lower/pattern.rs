//! Patterns and `match`: a pattern lowers to a test that jumps away when the
//! value does not match and binds the pattern's names when it does. A `match`
//! whose arms each name a different variant of one enum, or a different small
//! integer, dispatches through one `Switch` instead of testing arm by arm.

use std::collections::HashMap;

use super::super::ir::{BinaryOp, Const, Inst, Num, Reg};
use super::{Lower, Lowerer, num_of};
use crate::ast::{AstNode, Expr};
use crate::hir::Ty;
use crate::hir::infer::pattern::{Ctor, Lit, is_rest, literal, name_token, range_bounds};
use crate::hir::infer::{FieldInfo, VariantPayload};
use crate::resolve::Resolution;
use crate::syntax::{SyntaxKind, SyntaxNode, TextRange};

/// The widest key range a dense integer `Switch` spans.
const DENSE_SPAN: i128 = 256;

/// One arm of a `match`.
struct Arm {
    pattern: SyntaxNode,
    guard: Option<Expr>,
    block: Option<SyntaxNode>,
    value: Option<Expr>,
    at: TextRange,
}

impl Lowerer<'_, '_> {
    /// Binds an irrefutable pattern (`let`, `for`, a closure parameter) to `src`.
    pub(super) fn destructure(&mut self, pattern: &SyntaxNode, src: Reg, ty: &Ty) -> Lower<()> {
        let mut fails = Vec::new();
        self.test(pattern, src, ty, &mut fails)?;
        if fails.is_empty() {
            Ok(())
        } else {
            self.bail("a refutable pattern where only an irrefutable one is allowed")
        }
    }

    /// A `match`. With `value`, each arm's value lands in one register.
    pub(super) fn match_value(&mut self, node: &SyntaxNode, value: bool) -> Lower<Option<Reg>> {
        let Some(scrutinee) = crate::hir::infer::first_child_expr(node) else {
            return self.bail("a `match` without a scrutinee");
        };
        let ty = self.ty(&scrutinee)?;
        let src = self.expr(&scrutinee)?;
        let arms = arms(node);
        let dst = if value { Some(self.reg()) } else { None };
        let mut ends = Vec::new();
        if !self.switch_on_tag(&arms, src, &ty, dst, &mut ends)?
            && !self.switch_on_int(&arms, src, &ty, dst, &mut ends)?
        {
            for arm in &arms {
                let range = arm.at;
                self.at(range, |l| {
                    let mut fails = Vec::new();
                    l.test(&arm.pattern, src, &ty, &mut fails)?;
                    if let Some(guard) = &arm.guard {
                        let pass = l.expr(guard)?;
                        fails.push(l.jump_if(pass, false));
                    }
                    l.arm_body(arm, dst)?;
                    ends.push(l.jump());
                    l.patch_here(&fails);
                    Ok(())
                })?;
            }
            self.emit(Inst::Unreachable);
        }
        self.patch_here(&ends);
        Ok(dst)
    }

    /// An arm's block or value, moved into `dst` when one is wanted.
    fn arm_body(&mut self, arm: &Arm, dst: Option<Reg>) -> Lower<()> {
        match (&arm.block, &arm.value) {
            (Some(block), _) => self.branch(block, dst),
            (None, Some(value)) => {
                let src = self.expr(value)?;
                if let Some(dst) = dst {
                    self.emit(Inst::Move { dst, src });
                }
                Ok(())
            }
            (None, None) => Ok(()),
        }
    }

    /// Dispatches on an enum's tag when every arm but an optional trailing
    /// catch-all names a different variant, with no guard and no refutable
    /// sub-pattern. Returns whether it did.
    fn switch_on_tag(
        &mut self,
        arms: &[Arm],
        src: Reg,
        ty: &Ty,
        dst: Option<Reg>,
        ends: &mut Vec<usize>,
    ) -> Lower<bool> {
        let Ty::Named(owner) = ty else {
            return Ok(false);
        };
        let Some(count) = self.env.enum_variants(*owner).map(<[_]>::len) else {
            return Ok(false);
        };
        let (cases, default) = split_default(arms);
        if cases.len() < 2 || arms.iter().any(|a| a.guard.is_some()) {
            return Ok(false);
        }
        let mut targets: Vec<Option<usize>> = vec![None; count];
        for (i, arm) in cases.iter().enumerate() {
            let node = unwrap(&arm.pattern);
            if !matches!(
                node.kind(),
                SyntaxKind::ConstructorPattern | SyntaxKind::QualifiedVariantPattern
            ) {
                return Ok(false);
            }
            let Some(Ctor::Variant(o, index)) = self.cx.pattern_ctor(&node) else {
                return Ok(false);
            };
            if o != *owner
                || targets.get(index).is_none_or(Option::is_some)
                || !node.children().iter().all(|c| self.irrefutable(c))
            {
                return Ok(false);
            }
            targets[index] = Some(i);
        }
        let tag = self.reg();
        self.emit(Inst::Tag { dst: tag, src });
        self.dispatch(cases, default, &targets, 0, tag, src, ty, dst, ends)?;
        Ok(true)
    }

    /// Dispatches on an integer or `Char` when at least three arms name distinct
    /// literals in a compact range, with no guard and an optional trailing
    /// catch-all. Returns whether it did.
    fn switch_on_int(
        &mut self,
        arms: &[Arm],
        src: Reg,
        ty: &Ty,
        dst: Option<Reg>,
        ends: &mut Vec<usize>,
    ) -> Lower<bool> {
        if !(num_of(ty).is_some_and(|n| !n.is_float()) || *ty == Ty::Char) {
            return Ok(false);
        }
        let (cases, default) = split_default(arms);
        if cases.len() < 3 || arms.iter().any(|a| a.guard.is_some()) {
            return Ok(false);
        }
        let mut keys = Vec::with_capacity(cases.len());
        for arm in cases {
            let key = match literal(&unwrap(&arm.pattern)) {
                Some(Lit::Int(Some(v))) => v,
                Some(Lit::Char(Some(c))) => i128::from(u32::from(c)),
                _ => return Ok(false),
            };
            if keys.contains(&key) || i64::try_from(key).is_err() {
                return Ok(false);
            }
            keys.push(key);
        }
        let (lo, hi) = (
            *keys.iter().min().expect("three keys"),
            *keys.iter().max().expect("three keys"),
        );
        let span = hi - lo + 1;
        if span > DENSE_SPAN || span > (2 * keys.len() as i128).max(8) {
            return Ok(false);
        }
        let mut targets: Vec<Option<usize>> = vec![None; span as usize];
        for (i, key) in keys.iter().enumerate() {
            targets[(key - lo) as usize] = Some(i);
        }
        self.dispatch(cases, default, &targets, lo as i64, src, src, ty, dst, ends)?;
        Ok(true)
    }

    /// Emits a `Switch` on `key` to each case's arm (by `targets`), the rest to
    /// `default`, or to an unreachable trap when there is none.
    #[allow(clippy::too_many_arguments)]
    fn dispatch(
        &mut self,
        cases: &[Arm],
        default: Option<&Arm>,
        targets: &[Option<usize>],
        base: i64,
        key: Reg,
        src: Reg,
        ty: &Ty,
        dst: Option<Reg>,
        ends: &mut Vec<usize>,
    ) -> Lower<()> {
        let switch = self.emit(Inst::Switch {
            src: key,
            base,
            targets: Vec::new(),
            default: 0,
        });
        let mut starts = Vec::with_capacity(cases.len());
        for arm in cases {
            starts.push(self.here());
            let range = arm.at;
            self.at(range, |l| {
                let mut fails = Vec::new();
                if matches!(ty, Ty::Named(_)) {
                    l.bind_variant(&unwrap(&arm.pattern), src, &mut fails)?;
                }
                if !fails.is_empty() {
                    return l.bail("a switch arm has a refutable pattern");
                }
                l.arm_body(arm, dst)?;
                ends.push(l.jump());
                Ok(())
            })?;
        }
        let fallback = self.here();
        match default {
            Some(arm) => {
                let range = arm.at;
                self.at(range, |l| {
                    let mut fails = Vec::new();
                    l.test(&arm.pattern, src, ty, &mut fails)?;
                    l.arm_body(arm, dst)?;
                    ends.push(l.jump());
                    Ok(())
                })?;
            }
            None => {
                self.emit(Inst::Unreachable);
            }
        }
        let resolved: Vec<u32> = targets
            .iter()
            .map(|t| t.map_or(fallback, |i| starts[i]))
            .collect();
        if let Inst::Switch {
            targets, default, ..
        } = &mut self.frame().body.insts[switch]
        {
            *targets = resolved;
            *default = fallback;
        }
        Ok(())
    }

    /// Binds the sub-patterns of a variant pattern whose variant is already
    /// known to match.
    fn bind_variant(&mut self, node: &SyntaxNode, src: Reg, fails: &mut Vec<usize>) -> Lower<()> {
        let Some(Ctor::Variant(owner, index)) = self.cx.pattern_ctor(node) else {
            return self.bail("this pattern names no variant");
        };
        let payload = self
            .env
            .enum_variants(owner)
            .and_then(|vs| vs.get(index))
            .map(|v| v.payload.clone());
        let (fields, names) = match payload {
            Some(VariantPayload::Unit) => return Ok(()),
            Some(VariantPayload::Tuple(tys)) => {
                (tys.into_iter().map(|t| (t, true)).collect(), Vec::new())
            }
            Some(VariantPayload::Record(fields)) => record_fields(&fields),
            None => return self.bail("this pattern names no variant"),
        };
        self.sub_patterns(node, src, &fields, &names, fails)
    }

    /// Whether `pattern` matches every value of its type.
    fn irrefutable(&self, pattern: &SyntaxNode) -> bool {
        match pattern.kind() {
            SyntaxKind::WildcardPattern
            | SyntaxKind::IdentPattern
            | SyntaxKind::RestPattern
            | SyntaxKind::TypePath => true,
            SyntaxKind::Pattern
            | SyntaxKind::ParenPattern
            | SyntaxKind::BindingPattern
            | SyntaxKind::TuplePattern
            | SyntaxKind::RecordPatternField => {
                pattern.children().iter().all(|c| self.irrefutable(c))
            }
            SyntaxKind::ConstructorPattern => {
                matches!(self.cx.pattern_ctor(pattern), Some(Ctor::Single(_)))
                    && pattern.children().iter().all(|c| self.irrefutable(c))
            }
            _ => false,
        }
    }

    /// Tests `src: ty` against `pattern`, pushing the jumps taken on a mismatch
    /// onto `fails` and binding the pattern's names on a match.
    fn test(
        &mut self,
        pattern: &SyntaxNode,
        src: Reg,
        ty: &Ty,
        fails: &mut Vec<usize>,
    ) -> Lower<()> {
        match pattern.kind() {
            SyntaxKind::Pattern | SyntaxKind::ParenPattern => {
                for child in pattern.children() {
                    self.test(&child, src, ty, fails)?;
                }
                Ok(())
            }
            SyntaxKind::WildcardPattern => Ok(()),
            SyntaxKind::IdentPattern => match name_token(pattern) {
                Some(name) => self.bind_at(name.text_range(), src, true),
                None => Ok(()),
            },
            SyntaxKind::BindingPattern => {
                if let Some(name) = name_token(pattern) {
                    self.bind_at(name.text_range(), src, false)?;
                }
                for child in pattern.children() {
                    self.test(&child, src, ty, fails)?;
                }
                Ok(())
            }
            SyntaxKind::OrPattern => self.test_or(pattern, src, ty, fails),
            SyntaxKind::TuplePattern => {
                let elems = pattern.children();
                let tys = match ty {
                    Ty::Tuple(tys) if tys.len() == elems.len() => tys.clone(),
                    Ty::Unit if elems.is_empty() => Vec::new(),
                    _ => return self.bail("a tuple pattern that does not fit its value"),
                };
                for (i, (elem, ty)) in elems.iter().zip(&tys).enumerate() {
                    let field = self.reg();
                    self.emit(Inst::Field {
                        dst: field,
                        src,
                        index: i as u32,
                    });
                    self.test(elem, field, ty, fails)?;
                }
                Ok(())
            }
            SyntaxKind::ListPattern => self.test_list(pattern, src, ty, fails),
            SyntaxKind::LiteralPattern => {
                let value = match literal(pattern) {
                    Some(Lit::Int(Some(v))) => Const::Int(v),
                    Some(Lit::Char(Some(c))) => Const::Char(c),
                    Some(Lit::Str(Some(s))) => Const::Str(s),
                    Some(Lit::Bool(b)) => {
                        fails.push(self.jump_if(src, !b));
                        return Ok(());
                    }
                    Some(Lit::None) => {
                        let nil = self.reg();
                        self.emit(Inst::IsNil { dst: nil, src });
                        fails.push(self.jump_if(nil, false));
                        return Ok(());
                    }
                    _ => return self.bail("a malformed literal pattern"),
                };
                let key = self.constant(value);
                self.compare(BinaryOp::Eq, src, key, fails);
                Ok(())
            }
            SyntaxKind::RangePattern => {
                let Some((lo, hi)) = range_bounds(pattern) else {
                    return self.bail("a malformed range pattern");
                };
                let num = match ty {
                    Ty::Char => Num::U32,
                    other => match num_of(other).filter(|n| !n.is_float()) {
                        Some(num) => num,
                        None => return self.bail("a range pattern on a non-integer"),
                    },
                };
                let lo = self.constant(Const::Int(lo));
                self.compare(BinaryOp::Ge(num), src, lo, fails);
                let hi = self.constant(Const::Int(hi));
                self.compare(BinaryOp::Le(num), src, hi, fails);
                Ok(())
            }
            SyntaxKind::ConstructorPattern | SyntaxKind::QualifiedVariantPattern => {
                self.test_constructor(pattern, src, ty, fails)
            }
            kind => self.bail(format!("`{kind:?}` patterns are not supported yet")),
        }
    }

    /// Fails unless `lhs op rhs`.
    fn compare(&mut self, op: BinaryOp, lhs: Reg, rhs: Reg, fails: &mut Vec<usize>) {
        let pass = self.reg();
        self.emit(Inst::Binary {
            dst: pass,
            op,
            lhs,
            rhs,
        });
        fails.push(self.jump_if(pass, false));
    }

    /// `p | q | ..`: each alternative is tried in turn, and every alternative
    /// binds its names into the registers of the first one's.
    fn test_or(
        &mut self,
        pattern: &SyntaxNode,
        src: Reg,
        ty: &Ty,
        fails: &mut Vec<usize>,
    ) -> Lower<()> {
        let alternatives = pattern.children();
        let mut first: HashMap<String, Reg> = HashMap::new();
        let mut matched = Vec::new();
        for (i, alt) in alternatives.iter().enumerate() {
            if i > 0 {
                for at in bindings(alt) {
                    let name = self.text_at(alt, at);
                    if let (Some(Resolution::Local(slot)), Some(reg)) =
                        (self.cx.resolution_at(at), first.get(&name))
                    {
                        let reg = *reg;
                        self.frame().locals.insert(slot, reg);
                    }
                }
            }
            let mut miss = Vec::new();
            self.test(alt, src, ty, &mut miss)?;
            if i == 0 {
                for at in bindings(alt) {
                    if let Some(Resolution::Local(slot)) = self.cx.resolution_at(at)
                        && let Some(reg) = self.frame().locals.get(&slot).copied()
                    {
                        first.insert(self.text_at(alt, at), reg);
                    }
                }
            }
            if i + 1 == alternatives.len() {
                fails.extend(miss);
            } else {
                matched.push(self.jump());
                self.patch_here(&miss);
            }
        }
        self.patch_here(&matched);
        Ok(())
    }

    /// The source text of the token at `at` inside `node`.
    fn text_at(&self, node: &SyntaxNode, at: TextRange) -> String {
        node.descendants_with_tokens()
            .into_iter()
            .filter_map(|e| e.as_token().cloned())
            .find(|t| t.text_range() == at)
            .map(|t| t.text().trim_start_matches("r#").to_string())
            .unwrap_or_default()
    }

    /// `[a, b, .., z]`: a length check, then each element from the front or back.
    fn test_list(
        &mut self,
        pattern: &SyntaxNode,
        src: Reg,
        ty: &Ty,
        fails: &mut Vec<usize>,
    ) -> Lower<()> {
        let Ty::List(elem) = ty else {
            return self.bail("a list pattern on a non-list");
        };
        let children = pattern.children();
        let rest = children.iter().position(is_rest);
        if let Some(i) = rest
            && name_token(&children[i]).is_some()
        {
            return self.bail("a named rest pattern is not supported yet");
        }
        let (prefix, suffix) = match rest {
            Some(i) => (&children[..i], &children[i + 1..]),
            None => (&children[..], &children[..0]),
        };
        let len = self.reg();
        self.emit(Inst::Len { dst: len, src });
        let want = self.constant(Const::Int((prefix.len() + suffix.len()) as i128));
        let op = if rest.is_some() {
            BinaryOp::Ge(Num::I64)
        } else {
            BinaryOp::Eq
        };
        self.compare(op, len, want, fails);
        for (i, sub) in prefix.iter().enumerate() {
            let index = self.constant(Const::Int(i as i128));
            let item = self.reg();
            self.emit(Inst::Index {
                dst: item,
                list: src,
                index,
            });
            self.test(sub, item, elem, fails)?;
        }
        for (j, sub) in suffix.iter().enumerate() {
            let back = self.constant(Const::Int((suffix.len() - j) as i128));
            let index = self.reg();
            self.emit(Inst::Binary {
                dst: index,
                op: BinaryOp::Sub(Num::I64),
                lhs: len,
                rhs: back,
            });
            let item = self.reg();
            self.emit(Inst::Index {
                dst: item,
                list: src,
                index,
            });
            self.test(sub, item, elem, fails)?;
        }
        Ok(())
    }

    /// `Some(p)`, `None`, `Ok(p)`, `Err(p)`, `E::v(p, ..)`, `E::v { f: p }`,
    /// `R { f: p }`: the constructor's check, then each named sub-pattern
    /// against its field.
    fn test_constructor(
        &mut self,
        pattern: &SyntaxNode,
        src: Reg,
        ty: &Ty,
        fails: &mut Vec<usize>,
    ) -> Lower<()> {
        let Some(ctor) = self.cx.pattern_ctor(pattern) else {
            return self.bail("this pattern names no constructor");
        };
        let (fields, names): (Vec<(Ty, bool)>, Vec<String>) = match (&ctor, ty) {
            (Ctor::Some, Ty::Option(inner)) => {
                let nil = self.reg();
                self.emit(Inst::IsNil { dst: nil, src });
                fails.push(self.jump_if(nil, true));
                return self.sub_patterns(pattern, src, &[(*inner.clone(), false)], &[], fails);
            }
            (Ctor::None, _) => {
                let nil = self.reg();
                self.emit(Inst::IsNil { dst: nil, src });
                fails.push(self.jump_if(nil, false));
                return Ok(());
            }
            (Ctor::Ok | Ctor::Err, Ty::Result(ok, err)) => {
                let tag = self.reg();
                self.emit(Inst::Tag { dst: tag, src });
                let is_ok = ctor == Ctor::Ok;
                fails.push(self.jump_if(tag, is_ok));
                let inner = if is_ok { ok } else { err };
                (vec![(*inner.clone(), true)], Vec::new())
            }
            (Ctor::Variant(owner, index), _) => {
                let Some(variant) = self
                    .env
                    .enum_variants(*owner)
                    .and_then(|vs| vs.get(*index))
                    .cloned()
                else {
                    return self.bail("this pattern names no variant");
                };
                let tag = self.reg();
                self.emit(Inst::Tag { dst: tag, src });
                let want = self.constant(Const::Int(*index as i128));
                self.compare(BinaryOp::Eq, tag, want, fails);
                match variant.payload {
                    VariantPayload::Unit => return Ok(()),
                    VariantPayload::Tuple(tys) => {
                        (tys.into_iter().map(|t| (t, true)).collect(), Vec::new())
                    }
                    VariantPayload::Record(fields) => record_fields(&fields),
                }
            }
            (Ctor::Single(_), Ty::Tuple(tys)) => {
                (tys.iter().map(|t| (t.clone(), true)).collect(), Vec::new())
            }
            (Ctor::Single(_), Ty::Named(id)) => match self.env.record_fields(*id) {
                Some(fields) => record_fields(fields),
                None => return self.bail("this pattern names no record"),
            },
            _ => return self.bail("this constructor does not fit its value"),
        };
        self.sub_patterns(pattern, src, &fields, &names, fails)
    }

    /// Tests a constructor pattern's sub-patterns against the fields of `src`:
    /// positional ones in order, `f: p` and shorthand `f` by name. A field whose
    /// flag is false is `src` itself (the payload of `Some`).
    fn sub_patterns(
        &mut self,
        pattern: &SyntaxNode,
        src: Reg,
        fields: &[(Ty, bool)],
        names: &[String],
        fails: &mut Vec<usize>,
    ) -> Lower<()> {
        let mut positional = 0;
        for child in pattern.children() {
            let (index, sub) = match child.kind() {
                SyntaxKind::TypePath => continue,
                SyntaxKind::RecordPatternField => {
                    let Some(label) = name_token(&child) else {
                        continue;
                    };
                    let text = label.text();
                    let name = text.trim_start_matches("r#");
                    let Some(index) = names.iter().position(|n| n == name) else {
                        return self.bail(format!("no field `{name}` in this pattern's type"));
                    };
                    match child.children().into_iter().next() {
                        Some(sub) => (index, Some(sub)),
                        None => {
                            let field = self.field_reg(src, index, fields)?;
                            self.bind_at(label.text_range(), field, true)?;
                            continue;
                        }
                    }
                }
                _ if is_rest(&child) => {
                    positional = usize::MAX;
                    continue;
                }
                _ => {
                    if positional == usize::MAX {
                        return self.bail("a positional pattern after `..`");
                    }
                    positional += 1;
                    (positional - 1, Some(child.clone()))
                }
            };
            let Some(sub) = sub else {
                continue;
            };
            let field = self.field_reg(src, index, fields)?;
            let ty = fields[index].0.clone();
            self.test(&sub, field, &ty, fails)?;
        }
        Ok(())
    }

    /// The register holding field `index` of `src`.
    fn field_reg(&mut self, src: Reg, index: usize, fields: &[(Ty, bool)]) -> Lower<Reg> {
        match fields.get(index) {
            Some((_, false)) => Ok(src),
            Some((_, true)) => {
                let dst = self.reg();
                self.emit(Inst::Field {
                    dst,
                    src,
                    index: index as u32,
                });
                Ok(dst)
            }
            None => self.bail("more sub-patterns than fields"),
        }
    }
}

/// A record's field types (each a real field) and names.
fn record_fields(fields: &[FieldInfo]) -> (Vec<(Ty, bool)>, Vec<String>) {
    (
        fields.iter().map(|f| (f.ty.clone(), true)).collect(),
        fields.iter().map(|f| f.name.clone()).collect(),
    )
}

/// The arms of a `match`.
fn arms(node: &SyntaxNode) -> Vec<Arm> {
    node.children()
        .into_iter()
        .filter(|c| c.kind() == SyntaxKind::MatchArm)
        .filter_map(|arm| {
            let pattern = arm
                .children()
                .into_iter()
                .find(|c| c.kind() == SyntaxKind::Pattern)?;
            let exprs: Vec<Expr> = arm.children().into_iter().filter_map(Expr::cast).collect();
            let block = arm
                .children()
                .into_iter()
                .find(|c| c.kind() == SyntaxKind::Block);
            let (guard, value) = match (&block, exprs.len()) {
                (Some(_), _) => (exprs.first().cloned(), None),
                (None, 2) => (exprs.first().cloned(), exprs.get(1).cloned()),
                (None, _) => (None, exprs.first().cloned()),
            };
            Some(Arm {
                pattern,
                guard,
                block,
                value,
                at: arm.text_range(),
            })
        })
        .collect()
}

/// The arms before a trailing catch-all (`_` or a name), and the catch-all.
fn split_default(arms: &[Arm]) -> (&[Arm], Option<&Arm>) {
    match arms.split_last() {
        Some((last, rest))
            if matches!(
                unwrap(&last.pattern).kind(),
                SyntaxKind::WildcardPattern | SyntaxKind::IdentPattern
            ) =>
        {
            (rest, Some(last))
        }
        _ => (arms, None),
    }
}

/// `pattern` without its `Pattern`/parenthesis wrappers.
fn unwrap(pattern: &SyntaxNode) -> SyntaxNode {
    let mut node = pattern.clone();
    while matches!(node.kind(), SyntaxKind::Pattern | SyntaxKind::ParenPattern) {
        match node.children().into_iter().next() {
            Some(inner) => node = inner,
            None => break,
        }
    }
    node
}

/// The name tokens that bind locals in `pattern`.
fn bindings(pattern: &SyntaxNode) -> Vec<TextRange> {
    let mut out = Vec::new();
    for node in std::iter::once(pattern.clone()).chain(pattern.descendants()) {
        match node.kind() {
            SyntaxKind::IdentPattern | SyntaxKind::BindingPattern | SyntaxKind::RestPattern => {
                out.extend(name_token(&node).map(|t| t.text_range()));
            }
            SyntaxKind::RecordPatternField if node.children().is_empty() => {
                out.extend(name_token(&node).map(|t| t.text_range()));
            }
            _ => {}
        }
    }
    out
}
