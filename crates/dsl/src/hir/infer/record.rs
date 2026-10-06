//! Record literals, field access and enum variant values: the nominal lookups
//! that go through the environment's record and enum tables.

use std::collections::HashSet;

use super::generic::mentions;
use super::{
    FieldInfo, InferCx, TypeEnv, VariantPayload, compatible, first_child_expr, record_spread,
};
use crate::ast::{AstNode, Expr, FieldExpr};
use crate::diag::{Applicability, Diagnostic, Fix, TextEdit};
use crate::hir::generic::{Subst, apply};
use crate::hir::ty::Ty;
use crate::resolve::SymbolId;
use crate::resolve::suggest::{Candidate, attach, nearest};
use crate::syntax::{SyntaxKind, SyntaxNode, SyntaxToken, TextRange};

/// What a record literal's head names.
enum RecordHead {
    /// A record type, by symbol.
    Record(SymbolId),
    /// A record-payload variant of an enum type.
    Variant(SymbolId, usize),
}

impl InferCx<'_> {
    /// Types a path naming variant `name` of enum `owner`: a unit or record variant
    /// is a value of the enum, a tuple variant is its constructor function. An
    /// unknown variant is `E2001`.
    pub(super) fn variant_value(&mut self, owner: SymbolId, name: &SyntaxToken) -> Ty {
        let Some(variants) = self.env.enum_variants(owner) else {
            return Ty::Unknown;
        };
        let text = name.text();
        let this = self.applied(owner, Ty::Param);
        match variants.iter().find(|v| v.name == text) {
            Some(variant) => match &variant.payload {
                VariantPayload::Tuple(tys) => Ty::Fn(tys.clone(), Box::new(this)),
                VariantPayload::Unit | VariantPayload::Record(_) => this,
            },
            None => {
                let candidates: Vec<(String, TextRange)> = variants
                    .iter()
                    .map(|v| (v.name.clone(), v.declared_at))
                    .collect();
                let owner_name = self.describe(&Ty::named(owner));
                let message = format!("no variant `{text}` on `{owner_name}`");
                self.unknown_member(owner, name, message, &candidates, &[]);
                Ty::Unknown
            }
        }
    }

    /// Types a call of a tuple variant of `owner`, solving the enum's
    /// generic parameters from the arguments and the expected type.
    pub(super) fn variant_call(
        &mut self,
        owner: SymbolId,
        sig: (Vec<Ty>, Ty),
        args: &[Expr],
        expected: Option<&Ty>,
        node: &SyntaxNode,
    ) -> Ty {
        let open = self.type_params(owner);
        if open.is_empty() {
            return self.apply_signature(args, sig, expected, node);
        }
        let explicit = self.explicit_args(node);
        let (ret, _) = self.solve(
            &open,
            &mut Vec::new(),
            explicit,
            sig,
            None,
            args,
            expected,
            node.text_range(),
        );
        self.check_against(ret, expected, node)
    }

    /// A payload-free or record variant of `owner` named as a value: a
    /// generic enum's arguments are the expected type's, else undetermined.
    pub(super) fn generic_variant_value(
        &mut self,
        owner: SymbolId,
        ty: Ty,
        expected: Option<&Ty>,
        _node: &SyntaxNode,
    ) -> Ty {
        let open = self.type_params(owner);
        if open.is_empty() {
            return ty;
        }
        let solved: crate::hir::generic::Subst = match expected {
            Some(Ty::Named(id, args)) if *id == owner => {
                open.iter().copied().zip(args.iter().cloned()).collect()
            }
            _ => open.iter().map(|p| (*p, Ty::Unknown)).collect(),
        };
        crate::hir::generic::apply(&ty, &solved)
    }

    /// Types a record literal `P { .. }` or `S::variant { .. }` against its
    /// declaration: every field is known (`E2001`), initialized once and typed
    /// against its declared type (`E2103`), and every field without a default is
    /// given unless a `..base` supplies the rest (`E2103`).
    pub(super) fn infer_record(&mut self, node: &SyntaxNode, expected: Option<&Ty>) -> Ty {
        let heads: Vec<SyntaxToken> = node
            .children_with_tokens()
            .into_iter()
            .filter_map(|e| e.as_token().cloned())
            .take_while(|t| t.kind() != SyntaxKind::LBrace)
            .filter(|t| matches!(t.kind(), SyntaxKind::Ident | SyntaxKind::RawIdent))
            .collect();
        let head = self.record_head(&heads);
        let (owner, fields, owner_name) = match head {
            Some(RecordHead::Record(id)) => {
                let fields = self.env.record_fields(id).map(<[FieldInfo]>::to_vec);
                (Some(id), fields, self.describe(&Ty::named(id)))
            }
            Some(RecordHead::Variant(id, index)) => {
                let variant = self.env.enum_variants(id).and_then(|vs| vs.get(index));
                let fields = variant.and_then(|v| match &v.payload {
                    VariantPayload::Record(fields) => Some(fields.clone()),
                    _ => None,
                });
                let name = format!(
                    "{}::{}",
                    self.describe(&Ty::named(id)),
                    variant.map_or("", |v| v.name.as_str())
                );
                if fields.is_none() {
                    let message = format!("the variant `{name}` has no fields");
                    self.diagnostics
                        .push(Diagnostic::error("E2103", node.text_range(), message));
                }
                (Some(id), fields, name)
            }
            None => (None, None, String::new()),
        };

        // A generic record's (or enum's) arguments: given, expected, or solved
        // from the fields.
        let open = owner.map(|id| self.type_params(id)).unwrap_or_default();
        let mut solved: Subst = Vec::new();
        if let Some(id) = owner
            && !open.is_empty()
        {
            let explicit = self.explicit_args(node);
            if explicit.len() == open.len() {
                solved.extend(open.iter().copied().zip(explicit));
            } else if let Some(Ty::Named(e, args)) = expected
                && *e == id
            {
                solved.extend(
                    open.iter()
                        .copied()
                        .zip(args.iter().cloned())
                        .filter(|(_, t)| *t != Ty::Unknown),
                );
            }
        }
        let mut seen: HashSet<String> = HashSet::new();
        for field in node
            .children()
            .into_iter()
            .filter(|c| c.kind() == SyntaxKind::RecordExprField)
        {
            let Some(label) = field
                .children_with_tokens()
                .into_iter()
                .filter_map(|e| e.as_token().cloned())
                .find(|t| matches!(t.kind(), SyntaxKind::Ident | SyntaxKind::RawIdent))
            else {
                continue;
            };
            let value = first_child_expr(&field);
            let name = label.text().to_string();
            let Some(fields) = &fields else {
                if let Some(value) = &value {
                    let _ = self.infer_expr(value, None);
                }
                continue;
            };
            if !seen.insert(name.clone()) {
                let message = format!("the field `{name}` is initialized twice");
                self.diagnostics
                    .push(Diagnostic::error("E2103", label.text_range(), message));
            }
            match fields.iter().find(|f| f.name == name) {
                Some(info) => {
                    let want = self.settle(&apply(&info.ty, &solved));
                    let open_here = open
                        .iter()
                        .any(|p| !solved.iter().any(|(q, _)| q == p) && mentions(&want, *p));
                    let is_var = |p: SymbolId| open.contains(&p);
                    match &value {
                        Some(value) if open_here => {
                            let got = self.infer_expr(value, None);
                            if !want.bind(&got, &is_var, &mut solved) {
                                self.emit_mismatch(&got, &want, value.syntax().text_range());
                            }
                        }
                        Some(value) => {
                            let _ = self.infer_promoted(value, &want);
                        }
                        None => {
                            let ty = match self.refs.get(&label.text_range()).copied() {
                                Some(to) => self.resolution_ty(&to),
                                None => Ty::Unknown,
                            };
                            if open_here {
                                let _ = want.bind(&ty, &is_var, &mut solved);
                            } else {
                                self.check_promoted(ty, &want, &field);
                            }
                        }
                    }
                }
                None => {
                    let found = match &value {
                        Some(value) => self.infer_expr(value, None),
                        None => Ty::Unknown,
                    };
                    let message = format!("no field `{name}` on `{owner_name}`");
                    let candidates = field_candidates(fields);
                    let fitting = fitting_names(fields, &found);
                    if let Some(owner) = owner {
                        self.unknown_member(owner, &label, message, &candidates, &fitting);
                    }
                }
            }
        }

        let this = owner.map(|id| {
            self.applied(id, |p| {
                solved
                    .iter()
                    .find(|(q, _)| *q == p)
                    .map_or(Ty::Unknown, |(_, t)| t.clone())
            })
        });
        let base = record_spread(node);
        if let Some(base) = &base {
            let want = this.clone();
            let _ = self.infer_expr(base, want.as_ref());
        }
        if let Some(fields) = &fields
            && base.is_none()
        {
            let missing: Vec<String> = fields
                .iter()
                .filter(|f| !f.has_default && !seen.contains(&f.name))
                .map(|f| format!("`{}`", f.name))
                .collect();
            if !missing.is_empty() {
                let message = format!("missing field {} in `{owner_name}`", missing.join(", "));
                self.diagnostics
                    .push(Diagnostic::error("E2103", node.text_range(), message));
            }
        }
        match this {
            Some(this) => self.check_against(this, expected, node),
            None => Ty::Unknown,
        }
    }

    /// What a record literal's head tokens name: the record at the last token, or
    /// the enum at the next-to-last token with its variant at the last.
    fn record_head(&mut self, heads: &[SyntaxToken]) -> Option<RecordHead> {
        let last = heads.last()?;
        if let Some(id) = self.symbol_at(last.text_range())
            && self.env.record_fields(id).is_some()
        {
            return Some(RecordHead::Record(id));
        }
        let owner = heads.len().checked_sub(2).map(|i| &heads[i])?;
        let id = self.symbol_at(owner.text_range())?;
        let variants = self.env.enum_variants(id)?;
        match variants.iter().position(|v| v.name == last.text()) {
            Some(index) => Some(RecordHead::Variant(id, index)),
            None => {
                let candidates: Vec<(String, TextRange)> = variants
                    .iter()
                    .map(|v| (v.name.clone(), v.declared_at))
                    .collect();
                let message = format!(
                    "no variant `{}` on `{}`",
                    last.text(),
                    self.describe(&Ty::named(id))
                );
                self.unknown_member(id, last, message, &candidates, &[]);
                None
            }
        }
    }

    /// Types `recv.field`: a record field by name or a tuple element by index. A
    /// name the record does not declare is `E2001`, suggesting the declared fields
    /// whose type fits the expected type first. A receiver of an undetermined or
    /// native type leaves the result undetermined.
    pub(super) fn infer_field(&mut self, node: &SyntaxNode, expected: Option<&Ty>) -> Ty {
        let Some(field) = FieldExpr::cast(node.clone()) else {
            return Ty::Unknown;
        };
        let recv = match field.receiver() {
            Some(recv) => self.infer_expr(&recv, None),
            None => Ty::Unknown,
        };
        let Some(name) = field.field() else {
            return Ty::Unknown;
        };
        if let Ty::Native(ty) = recv {
            let ty = self.native_property(ty, &name, node);
            return self.check_against(ty, expected, node);
        }
        match self.member_ty(&recv, &name, expected) {
            Some(ty) => self.check_against(ty, expected, node),
            None => Ty::Unknown,
        }
    }

    /// Types `recv?.field`: the field of an `Option` receiver's value, itself
    /// optional (`Option<Option<T>>` flattens to `Option<T>`). A receiver that is no
    /// `Option` is reported and its field reads as with `.`.
    pub(super) fn infer_optional_field(&mut self, node: &SyntaxNode, expected: Option<&Ty>) -> Ty {
        let (recv, through_option) = self.infer_optional_receiver(node);
        let Some(name) = node
            .children_with_tokens()
            .into_iter()
            .filter_map(|e| e.as_token().cloned())
            .skip_while(|t| t.kind() != SyntaxKind::QuestionDot)
            .find(|t| t.kind() != SyntaxKind::QuestionDot && !t.kind().is_trivia())
        else {
            return Ty::Unknown;
        };
        let want = match expected {
            Some(Ty::Option(t)) if through_option => Some(t.as_ref()),
            Some(t) if !through_option => Some(t),
            _ => None,
        };
        let Some(ty) = self.member_ty(&recv, &name, want) else {
            return Ty::Unknown;
        };
        let ty = match ty {
            Ty::Option(t) => Ty::Option(t),
            other if through_option => Ty::Option(Box::new(other)),
            other => other,
        };
        self.check_against(ty, expected, node)
    }

    /// Types the receiver of `value?.member`, yielding the type the member is looked
    /// up on and whether the receiver is an `Option`. A receiver of a known type that
    /// is no `Option` is `E2103`, with a fix to `.`; the member then reads as with `.`.
    pub(super) fn infer_optional_receiver(&mut self, node: &SyntaxNode) -> (Ty, bool) {
        let recv = match first_child_expr(node) {
            Some(recv) => self.infer_expr(&recv, None),
            None => Ty::Unknown,
        };
        match recv {
            Ty::Option(inner) => (*inner, true),
            Ty::Unknown | Ty::Never => (recv, true),
            other => {
                let op = node
                    .children_with_tokens()
                    .into_iter()
                    .filter_map(|e| e.as_token().cloned())
                    .find(|t| t.kind() == SyntaxKind::QuestionDot);
                if let Some(op) = op {
                    let actual = self.describe(&other);
                    let message =
                        format!("`?.` reads through an `Option`, but the receiver is `{actual}`");
                    let mut diagnostic = Diagnostic::error("E2103", op.text_range(), message)
                        .expecting(["Option<T>"], actual);
                    diagnostic.fixes.push(Fix {
                        title: "use `.`".to_string(),
                        applicability: Applicability::MachineApplicable,
                        edits: vec![TextEdit::new(op.text_range(), ".")],
                    });
                    self.diagnostics.push(diagnostic);
                }
                (other, false)
            }
        }
    }

    /// The type of member `name` on a value of type `recv`, or `None` when the
    /// receiver's type does not say (reporting `E2001` for a name a known record or
    /// tuple lacks).
    pub(super) fn member_ty(
        &mut self,
        recv: &Ty,
        name: &SyntaxToken,
        expected: Option<&Ty>,
    ) -> Option<Ty> {
        let text = name.text();
        match recv {
            Ty::Named(id, ..) => {
                let fields = self.env.record_fields(*id)?.to_vec();
                if let Some(info) = fields.iter().find(|f| f.name == text) {
                    let subst = self.args_subst(recv);
                    return Some(self.settle(&crate::hir::generic::apply(&info.ty, &subst)));
                }
                let message = format!("no field `{text}` on `{}`", self.describe(&Ty::named(*id)));
                let fitting = match expected {
                    Some(want) => fitting_names(&fields, want),
                    None => Vec::new(),
                };
                self.unknown_member(*id, name, message, &field_candidates(&fields), &fitting);
                None
            }
            Ty::Resource(value, error) => {
                if text == "state" {
                    return Some(Ty::ResourceState(value.clone(), error.clone()));
                }
                let message = format!("no member `{text}` on a resource; it has `.state`");
                self.diagnostics
                    .push(Diagnostic::error("E2001", name.text_range(), message));
                None
            }
            Ty::Tuple(tys) => match text.parse::<usize>().ok().and_then(|i| tys.get(i)) {
                Some(ty) => Some(ty.clone()),
                None => {
                    let message =
                        format!("no field `{text}` on the tuple `{}`", self.describe(recv));
                    self.diagnostics
                        .push(Diagnostic::error("E2001", name.text_range(), message));
                    None
                }
            },
            _ => None,
        }
    }

    /// Reports an `E2001` unknown member at `name`, suggesting the nearest of
    /// `fitting` (the candidates whose type fits the use) and, when none of those
    /// is near, the nearest of all `candidates`. The candidates are declared in
    /// `owner`.
    fn unknown_member(
        &mut self,
        owner: SymbolId,
        name: &SyntaxToken,
        message: String,
        candidates: &[(String, TextRange)],
        fitting: &[String],
    ) {
        let text = name.text();
        let env = self.env;
        fn candidate<'c>(
            env: &'c dyn TypeEnv,
            owner: SymbolId,
            (n, at): &'c (String, TextRange),
        ) -> Candidate<'c> {
            Candidate {
                name: n.as_str(),
                declared_at: env.declaration_site(owner, *at),
            }
        }
        let to_candidate = |c| candidate(env, owner, c);
        let mut suggestions = nearest(
            &text,
            candidates
                .iter()
                .filter(|(n, _)| fitting.contains(n))
                .map(to_candidate),
        );
        if suggestions.is_empty() {
            suggestions = nearest(&text, candidates.iter().map(to_candidate));
        }
        let mut diagnostic = Diagnostic::error("E2001", name.text_range(), message);
        attach(&mut diagnostic, name.text_range(), &suggestions);
        self.diagnostics.push(diagnostic);
    }

    /// Types a record-field initializer against its field type, with the one
    /// implicit promotion a field slot allows: a `T` initializes an `Option<T>`.
    pub(crate) fn infer_promoted(&mut self, value: &Expr, target: &Ty) -> Ty {
        let Ty::Option(inner) = target else {
            return self.infer_expr(value, Some(target));
        };
        let mark = self.diagnostics.len();
        let ty = self.infer_expr(value, Some(target));
        if self.diagnostics.len() == mark {
            return ty;
        }
        self.diagnostics.truncate(mark);
        let _ = self.infer_expr(value, Some(inner));
        target.clone()
    }

    /// [`Self::infer_promoted`] for a value whose type is already known.
    fn check_promoted(&mut self, ty: Ty, target: &Ty, node: &SyntaxNode) {
        if let Ty::Option(inner) = target
            && !matches!(ty, Ty::Option(_))
            && compatible(&ty, inner)
        {
            return;
        }
        let _ = self.check_against(ty, Some(target), node);
    }
}

/// The suggestion candidates for a record's fields.
fn field_candidates(fields: &[FieldInfo]) -> Vec<(String, TextRange)> {
    fields
        .iter()
        .map(|f| (f.name.clone(), f.declared_at))
        .collect()
}

/// The fields a value of type `ty` could initialize or stand for.
fn fitting_names(fields: &[FieldInfo], ty: &Ty) -> Vec<String> {
    if *ty == Ty::Unknown {
        return Vec::new();
    }
    fields
        .iter()
        .filter(|f| compatible(ty, &f.ty) || ty.widens_to(&f.ty))
        .map(|f| f.name.clone())
        .collect()
}
