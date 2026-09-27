//! Patterns: binding their names at the scrutinee's type, and the usefulness
//! analysis behind match exhaustiveness (`E2301`) and unreachable arms (`E2302`).
//!
//! The analysis is the classic pattern-matrix usefulness check over the finite
//! constructor sets the types give: `Bool`, `Option`, `Result`, enums, tuples and
//! records. Literals of infinite types need a catch-all; list and range patterns
//! are not analyzed, so a match using them is never reported non-exhaustive.

use super::{FieldInfo, InferCx, VariantPayload, first_child_expr};
use crate::ast::{AstNode, Expr};
use crate::diag::Diagnostic;
use crate::hir::ty::Ty;
use crate::resolve::SymbolId;
use crate::syntax::{SyntaxKind, SyntaxNode, SyntaxToken};

/// A pattern reduced to what usefulness needs.
#[derive(Debug, Clone)]
enum Pat {
    /// Matches every value: `_`, a binding, `..`.
    Wild,
    /// A constructor applied to sub-patterns, one per constructor field.
    Ctor(Ctor, Vec<Pat>),
    /// Alternatives.
    Or(Vec<Pat>),
}

/// A value constructor.
#[derive(Debug, Clone, PartialEq)]
enum Ctor {
    Bool(bool),
    Some,
    None,
    Ok,
    Err,
    /// Variant `index` of the enum `owner`.
    Variant(SymbolId, usize),
    /// The one constructor of a tuple or record, with its field count.
    Single(usize),
    /// A literal of an infinite type, by its spelling.
    Literal(String),
    /// A pattern the analysis does not look into (a list or range pattern); only
    /// equal to itself.
    Opaque(usize),
}

impl Ctor {
    /// The number of sub-patterns this constructor carries.
    fn arity(&self, cx: &InferCx<'_>) -> usize {
        match self {
            Ctor::Some | Ctor::Ok | Ctor::Err => 1,
            Ctor::Single(n) => *n,
            Ctor::Variant(owner, index) => cx
                .env
                .enum_variants(*owner)
                .and_then(|vs| vs.get(*index))
                .map_or(0, |v| match &v.payload {
                    VariantPayload::Unit => 0,
                    VariantPayload::Tuple(tys) => tys.len(),
                    VariantPayload::Record(fields) => fields.len(),
                }),
            Ctor::Bool(_) | Ctor::None | Ctor::Literal(_) | Ctor::Opaque(_) => 0,
        }
    }

    /// The types of this constructor's fields in a value of type `ty`.
    fn field_tys(&self, ty: &Ty, cx: &InferCx<'_>) -> Vec<Ty> {
        let n = self.arity(cx);
        let tys = match (self, ty) {
            (Ctor::Some, Ty::Option(t)) | (Ctor::Ok, Ty::Result(t, _)) => vec![(**t).clone()],
            (Ctor::Err, Ty::Result(_, e)) => vec![(**e).clone()],
            (Ctor::Single(_), Ty::Tuple(tys)) => tys.clone(),
            (Ctor::Single(_), Ty::Named(id)) => cx
                .env
                .record_fields(*id)
                .map(|fs| fs.iter().map(|f| f.ty.clone()).collect())
                .unwrap_or_default(),
            (Ctor::Variant(owner, index), _) => cx
                .env
                .enum_variants(*owner)
                .and_then(|vs| vs.get(*index))
                .map(|v| match &v.payload {
                    VariantPayload::Unit => Vec::new(),
                    VariantPayload::Tuple(tys) => tys.clone(),
                    VariantPayload::Record(fields) => fields.iter().map(|f| f.ty.clone()).collect(),
                })
                .unwrap_or_default(),
            _ => Vec::new(),
        };
        if tys.len() == n {
            tys
        } else {
            vec![Ty::Unknown; n]
        }
    }

    /// The type a constructor builds, when it says on its own.
    fn own_ty(&self) -> Option<Ty> {
        match self {
            Ctor::Bool(_) => Some(Ty::Bool),
            Ctor::Some | Ctor::None => Some(Ty::Option(Box::new(Ty::Unknown))),
            Ctor::Ok | Ctor::Err => Some(Ty::Result(Box::new(Ty::Unknown), Box::new(Ty::Unknown))),
            Ctor::Variant(owner, _) => Some(Ty::Named(*owner)),
            _ => None,
        }
    }

    /// How the constructor reads in a "not covered" list.
    fn spell(&self, cx: &InferCx<'_>) -> String {
        let args = |n: usize| vec!["_"; n].join(", ");
        match self {
            Ctor::Bool(b) => b.to_string(),
            Ctor::Some => "Some(_)".into(),
            Ctor::None => "None".into(),
            Ctor::Ok => "Ok(_)".into(),
            Ctor::Err => "Err(_)".into(),
            Ctor::Variant(owner, index) => {
                let owner_name = cx.describe(&Ty::Named(*owner));
                match cx.env.enum_variants(*owner).and_then(|vs| vs.get(*index)) {
                    Some(v) => match &v.payload {
                        VariantPayload::Unit => format!("{owner_name}::{}", v.name),
                        VariantPayload::Tuple(tys) => {
                            format!("{owner_name}::{}({})", v.name, args(tys.len()))
                        }
                        VariantPayload::Record(_) => format!("{owner_name}::{} {{ .. }}", v.name),
                    },
                    None => owner_name,
                }
            }
            Ctor::Single(n) => format!("({})", args(*n)),
            Ctor::Literal(text) => text.clone(),
            Ctor::Opaque(_) => "_".into(),
        }
    }
}

impl InferCx<'_> {
    /// Binds the names `pattern` introduces at the types its shape takes from
    /// `ty`.
    pub fn bind_pattern(&mut self, pattern: &SyntaxNode, ty: &Ty) {
        match pattern.kind() {
            SyntaxKind::Pattern | SyntaxKind::ParenPattern => {
                for child in pattern.children() {
                    self.bind_pattern(&child, ty);
                }
            }
            SyntaxKind::IdentPattern => {
                if let Some(name) = name_token(pattern) {
                    self.bind_local(name.text_range(), ty.clone());
                }
            }
            SyntaxKind::BindingPattern => {
                if let Some(name) = name_token(pattern) {
                    self.bind_local(name.text_range(), ty.clone());
                }
                for child in pattern.children() {
                    self.bind_pattern(&child, ty);
                }
            }
            SyntaxKind::OrPattern => {
                for child in pattern.children() {
                    self.bind_pattern(&child, ty);
                }
            }
            SyntaxKind::TuplePattern => {
                let elems = pattern.children();
                for (i, elem) in elems.iter().enumerate() {
                    let elem_ty = match ty {
                        Ty::Tuple(tys) if tys.len() == elems.len() => tys[i].clone(),
                        _ => Ty::Unknown,
                    };
                    self.bind_pattern(elem, &elem_ty);
                }
            }
            SyntaxKind::ListPattern => {
                let elem = match ty {
                    Ty::List(t) => (**t).clone(),
                    _ => Ty::Unknown,
                };
                for child in pattern.children() {
                    self.bind_pattern(&child, &elem);
                }
            }
            SyntaxKind::RestPattern => {
                if let Some(name) = name_token(pattern) {
                    let list = match ty {
                        Ty::Unknown => Ty::Unknown,
                        elem => Ty::List(Box::new(elem.clone())),
                    };
                    self.bind_local(name.text_range(), list);
                }
            }
            SyntaxKind::ConstructorPattern => self.bind_constructor(pattern, ty),
            _ => {}
        }
    }

    /// Binds a constructor pattern's sub-patterns: `Some(p)`, `Ok(p)`, `Err(p)`,
    /// `S::v(p, ..)`, `S::v { f, .. }` and `P { f: p, .. }`.
    fn bind_constructor(&mut self, pattern: &SyntaxNode, ty: &Ty) {
        let ctor = self.pattern_ctor(pattern);
        let (field_tys, fields): (Vec<Ty>, Option<Vec<FieldInfo>>) = match &ctor {
            Some(c @ Ctor::Variant(owner, index)) => {
                let fields = self
                    .env
                    .enum_variants(*owner)
                    .and_then(|vs| vs.get(*index))
                    .and_then(|v| match &v.payload {
                        VariantPayload::Record(fields) => Some(fields.clone()),
                        _ => None,
                    });
                (c.field_tys(ty, self), fields)
            }
            Some(Ctor::Single(_)) => {
                let fields = match ty {
                    Ty::Named(id) => self.env.record_fields(*id).map(<[FieldInfo]>::to_vec),
                    _ => None,
                };
                (Vec::new(), fields)
            }
            Some(c) => (c.field_tys(ty, self), None),
            None => (Vec::new(), None),
        };
        let mut positional = 0;
        for child in pattern.children() {
            match child.kind() {
                SyntaxKind::RecordPatternField => {
                    let Some(label) = name_token(&child) else {
                        continue;
                    };
                    let field_ty = fields
                        .as_ref()
                        .and_then(|fs| fs.iter().find(|f| f.name == label.text()))
                        .map_or(Ty::Unknown, |f| f.ty.clone());
                    if let Some(fs) = &fields
                        && !fs.iter().any(|f| f.name == label.text())
                    {
                        let message = format!("no field `{}` in this pattern's type", label.text());
                        self.diagnostics.push(Diagnostic::error(
                            "E2001",
                            label.text_range(),
                            message,
                        ));
                    }
                    match child.children().into_iter().next() {
                        Some(sub) => self.bind_pattern(&sub, &field_ty),
                        None => self.bind_local(label.text_range(), field_ty),
                    }
                }
                SyntaxKind::TypePath => {}
                _ => {
                    let sub_ty = field_tys.get(positional).cloned().unwrap_or(Ty::Unknown);
                    positional += 1;
                    self.bind_pattern(&child, &sub_ty);
                }
            }
        }
    }

    /// The constructor a constructor or qualified-variant pattern names.
    fn pattern_ctor(&self, pattern: &SyntaxNode) -> Option<Ctor> {
        let segments: Vec<SyntaxToken> = match pattern.kind() {
            SyntaxKind::QualifiedVariantPattern => ident_tokens(pattern),
            _ => {
                let path = pattern
                    .children()
                    .into_iter()
                    .find(|c| c.kind() == SyntaxKind::TypePath)?;
                path.descendants()
                    .into_iter()
                    .filter(|n| n.kind() == SyntaxKind::TypePathSegment)
                    .filter_map(|seg| ident_tokens(&seg).into_iter().next())
                    .collect()
            }
        };
        let texts: Vec<String> = segments.iter().map(|t| t.text().to_string()).collect();
        let texts: Vec<&str> = texts.iter().map(String::as_str).collect();
        match texts.as_slice() {
            ["Some"] | ["Option", "Some"] => return Some(Ctor::Some),
            ["None"] | ["Option", "None"] => return Some(Ctor::None),
            ["Ok"] | ["Result", "Ok"] => return Some(Ctor::Ok),
            ["Err"] | ["Result", "Err"] => return Some(Ctor::Err),
            _ => {}
        }
        let first = segments.first()?;
        let head = self.symbol_at(first.text_range());
        if let Some(id) = head
            && self.env.record_fields(id).is_some()
            && segments.len() == 1
        {
            let n = self.env.record_fields(id).map_or(0, <[FieldInfo]>::len);
            return Some(Ctor::Single(n));
        }
        // `S::v`: the enum is the next-to-last segment, the variant the last.
        let owner_tok = segments.len().checked_sub(2).map(|i| &segments[i])?;
        let owner = self.symbol_at(owner_tok.text_range()).or(head)?;
        let variant = segments.last()?;
        let index = self
            .env
            .enum_variants(owner)?
            .iter()
            .position(|v| v.name == variant.text())?;
        Some(Ctor::Variant(owner, index))
    }

    /// Types a `match` (expression or statement): the scrutinee, each arm's
    /// pattern bound at its type, guards as `Bool`, and each arm's value against
    /// `expected`. Reports unreachable arms (`E2302`) and a non-exhaustive match
    /// (`E2301`). Returns the arm value types.
    pub(super) fn check_match(&mut self, node: &SyntaxNode, expected: Option<&Ty>) -> Vec<Ty> {
        let scrutinee = first_child_expr(node);
        let scrutinee_ty = match &scrutinee {
            Some(s) => self.infer_expr(s, None),
            None => Ty::Unknown,
        };
        let mut arm_tys = Vec::new();
        let mut rows: Vec<Pat> = Vec::new();
        let mut analyzable = true;
        let mut opaque = 0;
        for arm in node
            .children()
            .into_iter()
            .filter(|c| c.kind() == SyntaxKind::MatchArm)
        {
            let pattern = arm
                .children()
                .into_iter()
                .find(|c| c.kind() == SyntaxKind::Pattern);
            if let Some(pattern) = &pattern {
                self.bind_pattern(pattern, &scrutinee_ty);
            }
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
            if let Some(guard) = &guard {
                let _ = self.infer_expr(guard, Some(&Ty::Bool));
            }
            match (&block, &value) {
                (Some(block), _) => arm_tys.push(self.infer_block(block, expected)),
                (None, Some(value)) => arm_tys.push(self.infer_expr(value, expected)),
                (None, None) => {}
            }

            let Some(pattern) = pattern else {
                continue;
            };
            let pat = self.lower_pat(&pattern, &mut analyzable, &mut opaque);
            if !self.useful(
                &rows,
                std::slice::from_ref(&pat),
                std::slice::from_ref(&scrutinee_ty),
            ) {
                self.diagnostics.push(Diagnostic::warning(
                    "E2302",
                    pattern.text_range(),
                    "unreachable pattern: earlier arms already match every value it matches",
                ));
            }
            if guard.is_none() {
                rows.push(pat);
            }
        }
        if analyzable
            && scrutinee_ty != Ty::Unknown
            && self.useful(&rows, &[Pat::Wild], std::slice::from_ref(&scrutinee_ty))
        {
            let missing = self.missing(&rows, &scrutinee_ty);
            let at = scrutinee
                .as_ref()
                .map_or(node.text_range(), |s| s.syntax().text_range());
            let message = format!("non-exhaustive match: {} not covered", missing.join(", "));
            self.diagnostics
                .push(Diagnostic::error("E2301", at, message));
        }
        arm_tys
    }

    /// Reports a `let` pattern that does not match every value of `ty` (`E2301`).
    pub(super) fn check_irrefutable(&mut self, pattern: &SyntaxNode, ty: &Ty) {
        let mut analyzable = true;
        let mut opaque = 0;
        let pat = self.lower_pat(pattern, &mut analyzable, &mut opaque);
        if matches!(pat, Pat::Wild) || !analyzable || *ty == Ty::Unknown {
            return;
        }
        if self.useful(&[pat], &[Pat::Wild], std::slice::from_ref(ty)) {
            let missing = self.missing(&[], ty);
            let message = format!(
                "a `let` pattern must match every value; {} not covered",
                missing.join(", ")
            );
            self.diagnostics
                .push(Diagnostic::error("E2301", pattern.text_range(), message));
        }
    }

    /// Reduces a pattern node to a [`Pat`]. `analyzable` turns false on a pattern
    /// the analysis does not look into.
    fn lower_pat(&self, node: &SyntaxNode, analyzable: &mut bool, opaque: &mut usize) -> Pat {
        let mut sub = |cx: &Self, n: &SyntaxNode| cx.lower_pat(n, analyzable, opaque);
        match node.kind() {
            SyntaxKind::Pattern | SyntaxKind::ParenPattern => match node.first_child() {
                Some(inner) => sub(self, &inner),
                None => Pat::Wild,
            },
            SyntaxKind::WildcardPattern | SyntaxKind::IdentPattern | SyntaxKind::RestPattern => {
                Pat::Wild
            }
            SyntaxKind::BindingPattern => match node.first_child() {
                Some(inner) => sub(self, &inner),
                None => Pat::Wild,
            },
            SyntaxKind::OrPattern => {
                let alts = node.children();
                Pat::Or(alts.iter().map(|a| sub(self, a)).collect())
            }
            SyntaxKind::TuplePattern => {
                let elems = node.children();
                if elems
                    .iter()
                    .any(|e| e.kind() == SyntaxKind::RestPattern || is_rest_wrapper(e))
                {
                    *analyzable = false;
                    *opaque += 1;
                    return Pat::Ctor(Ctor::Opaque(*opaque), Vec::new());
                }
                let subs = elems.iter().map(|e| sub(self, e)).collect::<Vec<_>>();
                Pat::Ctor(Ctor::Single(subs.len()), subs)
            }
            SyntaxKind::LiteralPattern => {
                let text: String = node
                    .children_with_tokens()
                    .into_iter()
                    .filter_map(|e| e.as_token().cloned())
                    .filter(|t| !t.kind().is_trivia())
                    .map(|t| t.text().to_string())
                    .collect();
                let ctor = match text.as_str() {
                    "true" => Ctor::Bool(true),
                    "false" => Ctor::Bool(false),
                    "None" => Ctor::None,
                    _ => Ctor::Literal(text),
                };
                Pat::Ctor(ctor, Vec::new())
            }
            SyntaxKind::QualifiedVariantPattern | SyntaxKind::ConstructorPattern => {
                let Some(ctor) = self.pattern_ctor(node) else {
                    // An unknown constructor (an imported or native type): stay
                    // silent about the whole match.
                    *analyzable = false;
                    *opaque += 1;
                    return Pat::Ctor(Ctor::Opaque(*opaque), Vec::new());
                };
                let arity = ctor.arity(self);
                let record_fields = match &ctor {
                    Ctor::Variant(owner, index) => self
                        .env
                        .enum_variants(*owner)
                        .and_then(|vs| vs.get(*index))
                        .and_then(|v| match &v.payload {
                            VariantPayload::Record(fs) => Some(fs.clone()),
                            _ => None,
                        }),
                    Ctor::Single(_) => node
                        .children()
                        .into_iter()
                        .find(|c| c.kind() == SyntaxKind::TypePath)
                        .and_then(|p| {
                            p.descendants()
                                .into_iter()
                                .find_map(|n| ident_tokens(&n).into_iter().next())
                        })
                        .and_then(|t| self.symbol_at(t.text_range()))
                        .and_then(|id| self.env.record_fields(id))
                        .map(<[FieldInfo]>::to_vec),
                    _ => None,
                };
                let mut subs = vec![Pat::Wild; arity];
                let mut positional = 0;
                for child in node.children() {
                    match child.kind() {
                        SyntaxKind::TypePath => {}
                        SyntaxKind::RecordPatternField => {
                            let (Some(label), Some(fields)) = (name_token(&child), &record_fields)
                            else {
                                continue;
                            };
                            let Some(index) = fields.iter().position(|f| f.name == label.text())
                            else {
                                continue;
                            };
                            if let (Some(p), Some(slot)) =
                                (child.children().into_iter().next(), subs.get_mut(index))
                            {
                                *slot = sub(self, &p);
                            }
                        }
                        _ => {
                            let lowered = sub(self, &child);
                            if let Some(slot) = subs.get_mut(positional) {
                                *slot = lowered;
                            }
                            positional += 1;
                        }
                    }
                }
                Pat::Ctor(ctor, subs)
            }
            _ => {
                // List and range patterns.
                *analyzable = false;
                *opaque += 1;
                Pat::Ctor(Ctor::Opaque(*opaque), Vec::new())
            }
        }
    }

    /// Whether the pattern vector `v` matches some value no row of `rows` matches.
    /// `tys` are the types of the columns.
    fn useful(&self, rows: &[Pat], v: &[Pat], tys: &[Ty]) -> bool {
        let rows: Vec<Vec<Pat>> = rows.iter().map(|p| vec![p.clone()]).collect();
        self.useful_matrix(&rows, v, tys)
    }

    fn useful_matrix(&self, rows: &[Vec<Pat>], v: &[Pat], tys: &[Ty]) -> bool {
        let Some(head) = v.first() else {
            return rows.is_empty();
        };
        let ty = tys.first().cloned().unwrap_or(Ty::Unknown);
        let rest_tys = tys.get(1..).unwrap_or(&[]);
        let rows = expand_or(rows);
        match head {
            Pat::Or(alts) => alts.iter().any(|alt| {
                let mut w = vec![alt.clone()];
                w.extend_from_slice(&v[1..]);
                self.useful_matrix(&rows, &w, tys)
            }),
            Pat::Ctor(ctor, args) => {
                let field_tys = ctor.field_tys(&ty, self);
                let specialized = specialize(&rows, ctor, args.len());
                let mut w = args.clone();
                w.extend_from_slice(&v[1..]);
                let mut next_tys = field_tys;
                next_tys.extend_from_slice(rest_tys);
                self.useful_matrix(&specialized, &w, &next_tys)
            }
            Pat::Wild => {
                let ty = self.column_ty(&rows, &ty);
                let seen = head_ctors(&rows);
                match self.all_ctors(&ty) {
                    Some(all) if all.iter().all(|c| seen.contains(c)) => all.iter().any(|ctor| {
                        let arity = ctor.arity(self);
                        let specialized = specialize(&rows, ctor, arity);
                        let mut w = vec![Pat::Wild; arity];
                        w.extend_from_slice(&v[1..]);
                        let mut next_tys = ctor.field_tys(&ty, self);
                        next_tys.extend_from_slice(rest_tys);
                        self.useful_matrix(&specialized, &w, &next_tys)
                    }),
                    _ => {
                        let defaults: Vec<Vec<Pat>> = rows
                            .iter()
                            .filter(|r| matches!(r.first(), Some(Pat::Wild)))
                            .map(|r| r[1..].to_vec())
                            .collect();
                        self.useful_matrix(&defaults, &v[1..], rest_tys)
                    }
                }
            }
        }
    }

    /// A column's type: the scrutinee's when known, else the type its first
    /// self-describing constructor builds.
    fn column_ty(&self, rows: &[Vec<Pat>], ty: &Ty) -> Ty {
        if *ty != Ty::Unknown {
            return ty.clone();
        }
        head_ctors(rows)
            .iter()
            .find_map(Ctor::own_ty)
            .unwrap_or(Ty::Unknown)
    }

    /// Every constructor of `ty`, when the set is finite and known.
    fn all_ctors(&self, ty: &Ty) -> Option<Vec<Ctor>> {
        match ty {
            Ty::Bool => Some(vec![Ctor::Bool(true), Ctor::Bool(false)]),
            Ty::Option(_) => Some(vec![Ctor::None, Ctor::Some]),
            Ty::Result(_, _) => Some(vec![Ctor::Ok, Ctor::Err]),
            Ty::Tuple(tys) => Some(vec![Ctor::Single(tys.len())]),
            Ty::Unit => Some(vec![Ctor::Single(0)]),
            Ty::Named(id) => {
                if let Some(variants) = self.env.enum_variants(*id) {
                    Some((0..variants.len()).map(|i| Ctor::Variant(*id, i)).collect())
                } else {
                    self.env
                        .record_fields(*id)
                        .map(|fs| vec![Ctor::Single(fs.len())])
                }
            }
            _ => None,
        }
    }

    /// The top-level constructors of `ty` no row covers, spelled for a
    /// diagnostic; `_` when the type's constructors are not enumerable.
    fn missing(&self, rows: &[Pat], ty: &Ty) -> Vec<String> {
        let matrix: Vec<Vec<Pat>> =
            expand_or(&rows.iter().map(|p| vec![p.clone()]).collect::<Vec<_>>());
        let Some(all) = self.all_ctors(ty) else {
            return vec!["`_`".into()];
        };
        let missing: Vec<String> = all
            .iter()
            .filter(|ctor| {
                let arity = ctor.arity(self);
                let specialized = specialize(&matrix, ctor, arity);
                self.useful_matrix(
                    &specialized,
                    &vec![Pat::Wild; arity],
                    &ctor.field_tys(ty, self),
                )
            })
            .map(|ctor| format!("`{}`", ctor.spell(self)))
            .collect();
        if missing.is_empty() {
            vec!["`_`".into()]
        } else {
            missing
        }
    }
}

/// The rows with every top-level `|` pattern split into one row per alternative.
fn expand_or(rows: &[Vec<Pat>]) -> Vec<Vec<Pat>> {
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        match row.first() {
            Some(Pat::Or(alts)) => {
                let split: Vec<Vec<Pat>> = alts
                    .iter()
                    .map(|alt| {
                        let mut r = vec![alt.clone()];
                        r.extend_from_slice(&row[1..]);
                        r
                    })
                    .collect();
                out.extend(expand_or(&split));
            }
            _ => out.push(row.clone()),
        }
    }
    out
}

/// The rows that match constructor `ctor`, with its `arity` fields spread into
/// columns.
fn specialize(rows: &[Vec<Pat>], ctor: &Ctor, arity: usize) -> Vec<Vec<Pat>> {
    rows.iter()
        .filter_map(|row| {
            let (head, rest) = row.split_first()?;
            let mut out = match head {
                Pat::Wild => vec![Pat::Wild; arity],
                Pat::Ctor(c, args) if c == ctor => {
                    let mut args = args.clone();
                    args.resize(arity, Pat::Wild);
                    args
                }
                _ => return None,
            };
            out.extend_from_slice(rest);
            Some(out)
        })
        .collect()
}

/// The distinct constructors heading the rows.
fn head_ctors(rows: &[Vec<Pat>]) -> Vec<Ctor> {
    let mut out: Vec<Ctor> = Vec::new();
    for row in rows {
        if let Some(Pat::Ctor(c, _)) = row.first()
            && !out.contains(c)
        {
            out.push(c.clone());
        }
    }
    out
}

/// The identifier tokens directly in `node`.
fn ident_tokens(node: &SyntaxNode) -> Vec<SyntaxToken> {
    node.children_with_tokens()
        .into_iter()
        .filter_map(|e| e.as_token().cloned())
        .filter(|t| matches!(t.kind(), SyntaxKind::Ident | SyntaxKind::RawIdent))
        .collect()
}

/// The first identifier token directly in `node`.
fn name_token(node: &SyntaxNode) -> Option<SyntaxToken> {
    ident_tokens(node).into_iter().next()
}

/// Whether `node` is a `Pattern` wrapping a rest pattern.
fn is_rest_wrapper(node: &SyntaxNode) -> bool {
    node.kind() == SyntaxKind::Pattern
        && node
            .first_child()
            .is_some_and(|c| c.kind() == SyntaxKind::RestPattern)
}
