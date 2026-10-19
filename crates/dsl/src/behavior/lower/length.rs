//! Lengths: a single-unit length is a float in its unit; a `MixedLength` is a
//! record of five such terms, `dp`, `px`, `sp`, `em` and `%` in that order,
//! which arithmetic combines term by term. A value widens where inference
//! noted it does, and a length standing for a `Sizing` or an `EdgeInsets`
//! becomes `fixed` or `all` of it there.

use super::super::ir::{BinaryOp, Const, Inst, Num, Reg, UnaryOp};
use super::{Lower, Lowerer};
use crate::hir::Ty;
use crate::hir::infer::promote::{LengthUnit, StandsFor};
use crate::syntax::{SyntaxKind, SyntaxNode};

/// How many terms a `MixedLength` has.
pub(crate) const TERMS: usize = 5;

/// A length's terms, each in a register or known to be zero.
type Terms = [Option<Reg>; TERMS];

impl Lowerer<'_, '_> {
    /// `value`, the value of `node`, converted as inference noted it converts
    /// where it is typed.
    pub(super) fn promote(&mut self, value: Reg, node: &SyntaxNode) -> Reg {
        let Some(promotion) = self.cx.promotion(node) else {
            return value;
        };
        let mut value = value;
        if let Some(unit) = promotion.unit {
            let mut terms: Terms = [None; TERMS];
            terms[unit.term()] = Some(value);
            value = self.make_length(terms);
        }
        let Some(stands_for) = promotion.stands_for else {
            return value;
        };
        let dst = self.reg();
        self.emit(match stands_for {
            StandsFor::Fixed => Inst::Make {
                dst,
                tag: 0,
                fields: vec![value],
            },
            StandsFor::All => Inst::Make {
                dst,
                tag: 0,
                fields: vec![value; 4],
            },
        });
        dst
    }

    /// The type `e` has before it converts where it is typed: a widened
    /// length's own unit.
    pub(super) fn produced_ty(&self, e: &crate::ast::Expr) -> Lower<Ty> {
        use crate::ast::AstNode;
        match self.cx.promotion(e.syntax()).and_then(|p| p.unit) {
            Some(unit) => Ok(unit.ty()),
            None => self.ty(e),
        }
    }

    /// `lhs op rhs` when either side is a `MixedLength` or the two are
    /// lengths of different units; `None` for any other operands.
    pub(super) fn length_operate(
        &mut self,
        op: SyntaxKind,
        lhs: Reg,
        lt: &Ty,
        rhs: Reg,
        rt: &Ty,
    ) -> Lower<Option<Reg>> {
        let mixed = *lt == Ty::MixedLength || *rt == Ty::MixedLength;
        if !(mixed || lt.is_length() && rt.is_length() && lt != rt) {
            return Ok(None);
        }
        Ok(Some(match op {
            SyntaxKind::Plus | SyntaxKind::Minus if lt.is_length() && rt.is_length() => {
                let (a, b) = (self.terms(lhs, lt), self.terms(rhs, rt));
                let mut sum: Terms = [None; TERMS];
                for (at, slot) in sum.iter_mut().enumerate() {
                    *slot = match (a[at], b[at], op) {
                        (Some(a), Some(b), _) => Some(self.float(
                            if op == SyntaxKind::Plus {
                                BinaryOp::Add(Num::F64)
                            } else {
                                BinaryOp::Sub(Num::F64)
                            },
                            a,
                            b,
                        )),
                        (Some(a), None, _) => Some(a),
                        (None, Some(b), SyntaxKind::Plus) => Some(b),
                        (None, Some(b), _) => Some(self.negate(b)),
                        (None, None, _) => None,
                    };
                }
                self.make_length(sum)
            }
            SyntaxKind::Star if *lt == Ty::MixedLength && !rt.is_length() => {
                let k = self.scale(rhs, rt);
                self.scaled(lhs, BinaryOp::Mul(Num::F64), k)
            }
            SyntaxKind::Star if *rt == Ty::MixedLength && !lt.is_length() => {
                let k = self.scale(lhs, lt);
                self.scaled(rhs, BinaryOp::Mul(Num::F64), k)
            }
            SyntaxKind::Slash if *lt == Ty::MixedLength && !rt.is_length() => {
                let k = self.scale(rhs, rt);
                self.scaled(lhs, BinaryOp::Div(Num::F64), k)
            }
            _ => return self.bail("this operator has no runtime meaning for a `MixedLength`"),
        }))
    }

    /// `-src` of a `MixedLength`.
    pub(super) fn negate_length(&mut self, src: Reg) -> Reg {
        let terms = self.terms(src, &Ty::MixedLength);
        let negated = terms.map(|t| t.map(|t| self.negate(t)));
        self.make_length(negated)
    }

    /// The terms of the length `src` of `ty`.
    fn terms(&mut self, src: Reg, ty: &Ty) -> Terms {
        let mut terms: Terms = [None; TERMS];
        match LengthUnit::of(ty) {
            Some(unit) => terms[unit.term()] = Some(src),
            None => {
                for (at, slot) in terms.iter_mut().enumerate() {
                    let dst = self.reg();
                    self.emit(Inst::Field {
                        dst,
                        src,
                        index: at as u32,
                    });
                    *slot = Some(dst);
                }
            }
        }
        terms
    }

    /// A `MixedLength` of `terms`, an absent one zero.
    fn make_length(&mut self, terms: Terms) -> Reg {
        let mut zero = None;
        let fields = terms
            .into_iter()
            .map(|t| match t {
                Some(reg) => reg,
                None => *zero.get_or_insert_with(|| self.constant(Const::Float(0.0))),
            })
            .collect();
        let dst = self.reg();
        self.emit(Inst::Make {
            dst,
            tag: 0,
            fields,
        });
        dst
    }

    /// Every term of the `MixedLength` `src` combined with the factor `k`.
    fn scaled(&mut self, src: Reg, op: BinaryOp, k: Reg) -> Reg {
        let terms = self.terms(src, &Ty::MixedLength);
        let scaled = terms.map(|t| t.map(|t| self.float(op, t, k)));
        self.make_length(scaled)
    }

    /// The factor `src` of `ty` as a float.
    fn scale(&mut self, src: Reg, ty: &Ty) -> Reg {
        self.coerce(src, ty, Num::F64)
    }

    fn float(&mut self, op: BinaryOp, lhs: Reg, rhs: Reg) -> Reg {
        let dst = self.reg();
        self.emit(Inst::Binary { dst, op, lhs, rhs });
        dst
    }

    fn negate(&mut self, src: Reg) -> Reg {
        let dst = self.reg();
        self.emit(Inst::Unary {
            dst,
            op: UnaryOp::Neg(Num::F64),
            src,
        });
        dst
    }
}
