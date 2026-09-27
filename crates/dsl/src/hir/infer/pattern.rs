//! Patterns: binding their names at the scrutinee's type (a pattern whose shape
//! does not fit that type is `E2103`), the irrefutability rule for `let`, `for`
//! and closure parameters (`E2303`), and the usefulness analysis behind match
//! exhaustiveness (`E2301`) and unreachable arms (`E2302`).
//!
//! The analysis is the pattern-matrix usefulness check with constructor
//! splitting. `Bool`, `Option`, `Result`, enums, tuples and records have finite
//! constructor sets; an integer or `Char` is a range over its value domain, and a
//! list is a length (exactly `n`, or at least a prefix plus a suffix). Ranges and
//! lengths are split at the boundaries the arms draw, so every arm matches each
//! piece entirely or not at all. Strings need a catch-all. A pattern the analysis
//! cannot look into (an unknown constructor, a range with a non-literal end) and a
//! pattern with a type error leave the match unreported.

use super::{FieldInfo, InferCx, VariantPayload, compatible, first_child_expr};
use crate::ast::{AstNode, Expr};
use crate::diag::Diagnostic;
use crate::hir::ty::Ty;
use crate::resolve::SymbolId;
use crate::syntax::{SyntaxKind, SyntaxNode, SyntaxToken, TextRange};

/// How many missing constructors an `E2301` message spells before summarizing.
const MISSING_SHOWN: usize = 4;

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
    /// The integers (or `Char` scalar values) `lo..=hi`; empty when `lo > hi`.
    Range(i128, i128),
    /// A string literal, by value.
    Str(String),
    /// A list of some length.
    Slice(Slice),
    /// A pattern the analysis does not look into; only equal to itself.
    Opaque(usize),
}

/// The lengths a list pattern matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Slice {
    /// Exactly `n` elements.
    Fixed(usize),
    /// At least `prefix + suffix` elements, of which the first `prefix` and the
    /// last `suffix` are matched.
    Var(usize, usize),
}

impl Slice {
    fn arity(self) -> usize {
        match self {
            Slice::Fixed(n) => n,
            Slice::Var(prefix, suffix) => prefix + suffix,
        }
    }

    /// Whether every list `other` stands for is one this shape matches.
    fn covers(self, other: Slice) -> bool {
        match (self, other) {
            (Slice::Fixed(n), Slice::Fixed(k)) => n == k,
            (Slice::Fixed(_), Slice::Var(..)) => false,
            (Slice::Var(p, s), Slice::Fixed(k)) => p + s <= k,
            (Slice::Var(p, s), Slice::Var(q, t)) => p <= q && s <= t,
        }
    }
}

impl Ctor {
    /// The number of sub-patterns this constructor carries.
    fn arity(&self, cx: &InferCx<'_>) -> usize {
        match self {
            Ctor::Some | Ctor::Ok | Ctor::Err => 1,
            Ctor::Single(n) => *n,
            Ctor::Slice(slice) => slice.arity(),
            Ctor::Variant(owner, index) => cx
                .env
                .enum_variants(*owner)
                .and_then(|vs| vs.get(*index))
                .map_or(0, |v| match &v.payload {
                    VariantPayload::Unit => 0,
                    VariantPayload::Tuple(tys) => tys.len(),
                    VariantPayload::Record(fields) => fields.len(),
                }),
            Ctor::Bool(_) | Ctor::None | Ctor::Range(..) | Ctor::Str(_) | Ctor::Opaque(_) => 0,
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
            (Ctor::Slice(_), Ty::List(t)) => vec![(**t).clone(); n],
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
            Ctor::Str(_) => Some(Ty::String),
            Ctor::Slice(_) => Some(Ty::List(Box::new(Ty::Unknown))),
            _ => None,
        }
    }

    /// Whether every value `piece` stands for is one this constructor matches.
    fn covers(&self, piece: &Ctor) -> bool {
        match (self, piece) {
            (Ctor::Range(lo, hi), Ctor::Range(from, to)) => lo <= from && to <= hi,
            (Ctor::Slice(slice), Ctor::Slice(other)) => slice.covers(*other),
            _ => self == piece,
        }
    }

    /// How the constructor reads in a "not covered" list, in a value of type `ty`.
    fn spell(&self, ty: &Ty, cx: &InferCx<'_>) -> String {
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
            Ctor::Range(lo, hi) => {
                let value = |v: i128| match ty {
                    Ty::Char => u32::try_from(v)
                        .ok()
                        .and_then(char::from_u32)
                        .map_or_else(|| v.to_string(), |c| format!("{c:?}")),
                    _ => v.to_string(),
                };
                if lo == hi {
                    value(*lo)
                } else {
                    format!("{}..={}", value(*lo), value(*hi))
                }
            }
            Ctor::Str(text) => format!("{text:?}"),
            Ctor::Slice(Slice::Fixed(n)) => format!("[{}]", args(*n)),
            Ctor::Slice(Slice::Var(prefix, suffix)) => {
                let mut parts = vec!["_"; *prefix];
                parts.push("..");
                parts.extend(vec!["_"; *suffix]);
                format!("[{}]", parts.join(", "))
            }
            Ctor::Opaque(_) => "_".into(),
        }
    }
}

/// What a literal pattern spells.
enum Lit {
    /// An integer, `None` when it does not fit any scalar type.
    Int(Option<i128>),
    /// A character, `None` when malformed (already a lexical error).
    Char(Option<char>),
    Str(Option<String>),
    Bool(bool),
    None,
}

impl Lit {
    /// The value an integer or `Char` literal takes in a range, with whether it is
    /// a `Char`.
    fn scalar(&self) -> Option<(i128, bool)> {
        match self {
            Lit::Int(Some(v)) => Some((*v, false)),
            Lit::Char(Some(c)) => Some((i128::from(u32::from(*c)), true)),
            _ => None,
        }
    }
}

/// The usefulness state of one `match` as its arms are added in order.
pub(crate) struct MatchCheck {
    rows: Vec<Pat>,
    /// Turns false on an arm the analysis cannot judge, silencing `E2301`.
    analyzable: bool,
    opaque: usize,
}

impl MatchCheck {
    pub(crate) fn new() -> Self {
        MatchCheck {
            rows: Vec::new(),
            analyzable: true,
            opaque: 0,
        }
    }
}

impl InferCx<'_> {
    /// Binds the names `pattern` introduces at the types its shape takes from
    /// `ty`, reporting a pattern whose shape does not fit `ty` (`E2103`).
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
                let fits = match ty {
                    Ty::Tuple(tys) => tys.len() == elems.len(),
                    Ty::Unit => elems.is_empty(),
                    Ty::Unknown => true,
                    _ => false,
                };
                if !fits {
                    let message = format!(
                        "a tuple pattern of {} elements does not match `{}`",
                        elems.len(),
                        self.describe(ty)
                    );
                    self.diagnostics.push(Diagnostic::error(
                        "E2103",
                        pattern.text_range(),
                        message,
                    ));
                }
                for (i, elem) in elems.iter().enumerate() {
                    let elem_ty = match ty {
                        Ty::Tuple(tys) if fits => tys[i].clone(),
                        _ => Ty::Unknown,
                    };
                    self.bind_pattern(elem, &elem_ty);
                }
            }
            SyntaxKind::ListPattern => {
                let elem = match ty {
                    Ty::List(t) => (**t).clone(),
                    Ty::Unknown => Ty::Unknown,
                    _ => {
                        let message =
                            format!("a list pattern does not match `{}`", self.describe(ty));
                        self.diagnostics.push(Diagnostic::error(
                            "E2103",
                            pattern.text_range(),
                            message,
                        ));
                        Ty::Unknown
                    }
                };
                let rests: Vec<SyntaxNode> =
                    pattern.children().into_iter().filter(is_rest).collect();
                if let [_, extra, ..] = rests.as_slice() {
                    self.diagnostics.push(Diagnostic::error(
                        "E2103",
                        extra.text_range(),
                        "a list pattern takes at most one `..`",
                    ));
                }
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
            SyntaxKind::LiteralPattern => self.check_literal_pattern(pattern, ty),
            SyntaxKind::RangePattern => self.check_range_pattern(pattern, ty),
            SyntaxKind::ConstructorPattern | SyntaxKind::QualifiedVariantPattern => {
                self.bind_constructor(pattern, ty);
            }
            _ => {}
        }
    }

    /// Checks a literal pattern against the scrutinee type: an integer only
    /// matches an integer type and must lie in its range, the others match their
    /// own type. A float is never matched by a pattern.
    fn check_literal_pattern(&mut self, node: &SyntaxNode, ty: &Ty) {
        let Some(lit) = literal(node) else {
            return;
        };
        let ty = defaulted(ty);
        let found = match lit {
            Lit::Int(value) => {
                if super::is_integer_ty(ty) {
                    if !value.is_some_and(|v| super::int_fits(v, ty)) {
                        let message =
                            format!("integer literal out of range for `{}`", self.describe(ty));
                        self.diagnostics.push(Diagnostic::error(
                            "E2103",
                            node.text_range(),
                            message,
                        ));
                    }
                } else if super::is_float_ty(ty) {
                    let message = format!(
                        "a `{}` value cannot be matched by a pattern; compare it with `==`",
                        self.describe(ty)
                    );
                    self.diagnostics
                        .push(Diagnostic::error("E2103", node.text_range(), message));
                } else if *ty != Ty::Unknown {
                    self.emit_mismatch(&Ty::I64, ty, node.text_range());
                }
                return;
            }
            Lit::Char(_) => Ty::Char,
            Lit::Str(_) => Ty::String,
            Lit::Bool(_) => Ty::Bool,
            Lit::None => Ty::Option(Box::new(Ty::Unknown)),
        };
        if !compatible(&found, ty) {
            self.emit_mismatch(&found, ty, node.text_range());
        }
    }

    /// Checks a range pattern: both ends are integer or `Char` literals of the
    /// same kind, each checked against the scrutinee type like a literal pattern.
    fn check_range_pattern(&mut self, node: &SyntaxNode, ty: &Ty) {
        let ends = node.children();
        let kinds: Vec<Option<bool>> = ends
            .iter()
            .map(|end| match literal(end) {
                Some(Lit::Int(_)) => Some(false),
                Some(Lit::Char(_)) => Some(true),
                _ => None,
            })
            .collect();
        let [Some(a), Some(b)] = kinds.as_slice() else {
            return self.range_error(node, "a range pattern's ends must be literals");
        };
        if a != b {
            return self.range_error(
                node,
                "a range pattern's ends must both be integers or both `Char` values",
            );
        }
        let mark = self.diagnostics.len();
        for end in &ends {
            self.check_literal_pattern(end, ty);
        }
        if self.diagnostics.len() == mark
            && let Some((lo, hi)) = range_bounds(node)
            && lo > hi
        {
            self.range_error(node, "this range pattern matches no value");
        }
    }

    fn range_error(&mut self, node: &SyntaxNode, message: &str) {
        self.diagnostics
            .push(Diagnostic::error("E2103", node.text_range(), message));
    }

    /// Binds a constructor pattern's sub-patterns (`Some(p)`, `Ok(p)`, `Err(p)`,
    /// `S::v(p, ..)`, `S::v { f, .. }`, `P { f: p, .. }`) and checks that the
    /// constructor builds the scrutinee type.
    fn bind_constructor(&mut self, pattern: &SyntaxNode, ty: &Ty) {
        let ctor = self.pattern_ctor(pattern);
        if let Some(own) = ctor.as_ref().and_then(Ctor::own_ty)
            && !compatible(&own, ty)
        {
            self.emit_mismatch(&own, ty, pattern.text_range());
        }
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

    /// Reports a refutable pattern where only an irrefutable one is allowed
    /// (`E2303`): a `let`, a `for` or a closure parameter. `place` names it.
    pub(crate) fn check_irrefutable(&mut self, pattern: &SyntaxNode, place: &str) {
        let Some(part) = self.refutable_part(pattern) else {
            return;
        };
        let message = format!(
            "{place} must be an irrefutable pattern, but `{}` matches only some values; use `match`",
            part.text().to_string().trim()
        );
        self.diagnostics
            .push(Diagnostic::error("E2303", part.text_range(), message));
    }

    /// The first refutable sub-pattern of `node`: a literal, range, list, `|` or
    /// enum variant pattern. A tuple or record pattern is refutable only through
    /// its parts; an unknown constructor is not judged.
    fn refutable_part(&self, node: &SyntaxNode) -> Option<SyntaxNode> {
        match node.kind() {
            SyntaxKind::Pattern
            | SyntaxKind::ParenPattern
            | SyntaxKind::BindingPattern
            | SyntaxKind::TuplePattern
            | SyntaxKind::RecordPatternField => node
                .children()
                .iter()
                .find_map(|child| self.refutable_part(child)),
            SyntaxKind::ConstructorPattern => match self.pattern_ctor(node)? {
                Ctor::Single(_) => node
                    .children()
                    .iter()
                    .find_map(|child| self.refutable_part(child)),
                _ => Some(node.clone()),
            },
            SyntaxKind::QualifiedVariantPattern => self.pattern_ctor(node).map(|_| node.clone()),
            SyntaxKind::LiteralPattern
            | SyntaxKind::RangePattern
            | SyntaxKind::ListPattern
            | SyntaxKind::OrPattern => Some(node.clone()),
            _ => None,
        }
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
        let mut check = MatchCheck::new();
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
                self.bind_arm(&mut check, pattern, &scrutinee_ty);
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
            if let Some(pattern) = &pattern {
                self.add_arm(&mut check, pattern, &scrutinee_ty, guard.is_some());
            }
        }
        let at = scrutinee
            .as_ref()
            .map_or(node.text_range(), |s| s.syntax().text_range());
        self.finish_match(check, &scrutinee_ty, at);
        arm_tys
    }

    /// Binds one arm's pattern at the scrutinee type; a pattern with a type error
    /// leaves the match unjudged, so no exhaustiveness error cascades from it.
    pub(crate) fn bind_arm(&mut self, check: &mut MatchCheck, pattern: &SyntaxNode, ty: &Ty) {
        let mark = self.diagnostics.len();
        self.bind_pattern(pattern, ty);
        if self.diagnostics.len() > mark {
            check.analyzable = false;
        }
    }

    /// Adds one arm to the match, after its guard and body are typed: an arm no
    /// value reaches is `E2302`, and only an unguarded arm counts toward
    /// exhaustiveness.
    pub(crate) fn add_arm(
        &mut self,
        check: &mut MatchCheck,
        pattern: &SyntaxNode,
        ty: &Ty,
        guarded: bool,
    ) {
        let pat = self.lower_pat(pattern, &mut check.analyzable, &mut check.opaque);
        if !self.useful(
            &check.rows,
            std::slice::from_ref(&pat),
            std::slice::from_ref(ty),
        ) {
            self.diagnostics.push(Diagnostic::warning(
                "E2302",
                pattern.text_range(),
                "unreachable pattern: earlier arms already match every value it matches",
            ));
        }
        if !guarded {
            check.rows.push(pat);
        }
    }

    /// Reports a match whose unguarded arms miss some value of the scrutinee type
    /// (`E2301`), spelling the missing constructors, at `at`.
    pub(crate) fn finish_match(&mut self, check: MatchCheck, ty: &Ty, at: TextRange) {
        if !check.analyzable
            || *ty == Ty::Unknown
            || !self.useful(&check.rows, &[Pat::Wild], std::slice::from_ref(ty))
        {
            return;
        }
        let missing = self.missing(&check.rows, ty);
        let shown = missing
            .iter()
            .take(MISSING_SHOWN)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        let message = match missing.len().checked_sub(MISSING_SHOWN) {
            Some(more) if more > 0 => {
                format!("non-exhaustive match: {shown} and {more} more not covered")
            }
            _ => format!("non-exhaustive match: {shown} not covered"),
        };
        self.diagnostics
            .push(Diagnostic::error("E2301", at, message));
    }

    /// Reduces a pattern node to a [`Pat`]. `analyzable` turns false on a pattern
    /// the analysis does not look into, which becomes an opaque constructor.
    fn lower_pat(&self, node: &SyntaxNode, analyzable: &mut bool, opaque: &mut usize) -> Pat {
        match self.lower_known(node, analyzable, opaque) {
            Some(pat) => pat,
            None => {
                *analyzable = false;
                *opaque += 1;
                Pat::Ctor(Ctor::Opaque(*opaque), Vec::new())
            }
        }
    }

    /// [`Self::lower_pat`] for the patterns the analysis looks into.
    fn lower_known(
        &self,
        node: &SyntaxNode,
        analyzable: &mut bool,
        opaque: &mut usize,
    ) -> Option<Pat> {
        let mut sub = |n: &SyntaxNode| self.lower_pat(n, analyzable, opaque);
        let pat = match node.kind() {
            SyntaxKind::Pattern | SyntaxKind::ParenPattern | SyntaxKind::BindingPattern => {
                match node.first_child() {
                    Some(inner) => sub(&inner),
                    None => Pat::Wild,
                }
            }
            SyntaxKind::WildcardPattern | SyntaxKind::IdentPattern | SyntaxKind::RestPattern => {
                Pat::Wild
            }
            SyntaxKind::OrPattern => Pat::Or(node.children().iter().map(&mut sub).collect()),
            SyntaxKind::TuplePattern => {
                let elems = node.children();
                if elems.iter().any(is_rest) {
                    return None;
                }
                let subs: Vec<Pat> = elems.iter().map(&mut sub).collect();
                Pat::Ctor(Ctor::Single(subs.len()), subs)
            }
            SyntaxKind::ListPattern => {
                let items = node.children();
                let rests: Vec<usize> = (0..items.len()).filter(|&i| is_rest(&items[i])).collect();
                let subs: Vec<Pat> = items.iter().filter(|n| !is_rest(n)).map(&mut sub).collect();
                let slice = match rests.as_slice() {
                    [] => Slice::Fixed(subs.len()),
                    [at] => Slice::Var(*at, items.len() - at - 1),
                    _ => return None,
                };
                Pat::Ctor(Ctor::Slice(slice), subs)
            }
            SyntaxKind::LiteralPattern => {
                let ctor = match literal(node)? {
                    Lit::Bool(b) => Ctor::Bool(b),
                    Lit::None => Ctor::None,
                    Lit::Str(text) => Ctor::Str(text?),
                    lit => {
                        let (v, _) = lit.scalar()?;
                        Ctor::Range(v, v)
                    }
                };
                Pat::Ctor(ctor, Vec::new())
            }
            SyntaxKind::RangePattern => {
                let (lo, hi) = range_bounds(node)?;
                if lo > hi {
                    return None;
                }
                Pat::Ctor(Ctor::Range(lo, hi), Vec::new())
            }
            SyntaxKind::QualifiedVariantPattern | SyntaxKind::ConstructorPattern => {
                // An unknown constructor (an imported or native type) is opaque.
                let ctor = self.pattern_ctor(node)?;
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
                let mut subs = vec![Pat::Wild; ctor.arity(self)];
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
                                *slot = sub(&p);
                            }
                        }
                        _ => {
                            let lowered = sub(&child);
                            if let Some(slot) = subs.get_mut(positional) {
                                *slot = lowered;
                            }
                            positional += 1;
                        }
                    }
                }
                Pat::Ctor(ctor, subs)
            }
            _ => return None,
        };
        Some(pat)
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
                let heads = head_ctors(&rows);
                split(ctor, &heads).iter().any(|piece| {
                    self.useful_piece(&rows, piece, Some((ctor, args)), &v[1..], &ty, rest_tys)
                })
            }
            Pat::Wild => {
                let ty = self.column_ty(&rows, &ty);
                let heads = head_ctors(&rows);
                match self.pieces(&ty, &heads) {
                    Some(pieces) if pieces.iter().all(|p| heads.iter().any(|h| h.covers(p))) => {
                        pieces.iter().any(|piece| {
                            self.useful_piece(&rows, piece, None, &v[1..], &ty, rest_tys)
                        })
                    }
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

    /// Whether `head` (its constructor and arguments, or a wildcard when `None`)
    /// followed by `rest` is useful among the values `piece` stands for.
    fn useful_piece(
        &self,
        rows: &[Vec<Pat>],
        piece: &Ctor,
        head: Option<(&Ctor, &[Pat])>,
        rest: &[Pat],
        ty: &Ty,
        rest_tys: &[Ty],
    ) -> bool {
        let arity = piece.arity(self);
        let specialized = specialize(rows, piece, arity);
        let mut w = head
            .and_then(|(ctor, args)| spread(ctor, args, piece, arity))
            .unwrap_or_else(|| vec![Pat::Wild; arity]);
        w.extend_from_slice(rest);
        let mut next_tys = piece.field_tys(ty, self);
        next_tys.extend_from_slice(rest_tys);
        self.useful_matrix(&specialized, &w, &next_tys)
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

    /// Every constructor of `ty`, when the set is known: finite for the sum and
    /// product types, one range per value domain for the integers and `Char`, and
    /// one "any length" slice for a list.
    fn all_ctors(&self, ty: &Ty) -> Option<Vec<Ctor>> {
        match defaulted(ty) {
            Ty::Bool => Some(vec![Ctor::Bool(true), Ctor::Bool(false)]),
            Ty::Option(_) => Some(vec![Ctor::None, Ctor::Some]),
            Ty::Result(_, _) => Some(vec![Ctor::Ok, Ctor::Err]),
            Ty::Tuple(tys) => Some(vec![Ctor::Single(tys.len())]),
            Ty::Unit => Some(vec![Ctor::Single(0)]),
            Ty::Char => Some(vec![Ctor::Range(0, 0xD7FF), Ctor::Range(0xE000, 0x10_FFFF)]),
            Ty::List(_) => Some(vec![Ctor::Slice(Slice::Var(0, 0))]),
            Ty::Named(id) => {
                if let Some(variants) = self.env.enum_variants(*id) {
                    Some((0..variants.len()).map(|i| Ctor::Variant(*id, i)).collect())
                } else {
                    self.env
                        .record_fields(*id)
                        .map(|fs| vec![Ctor::Single(fs.len())])
                }
            }
            scalar => super::int_bounds(scalar).map(|(lo, hi)| vec![Ctor::Range(lo, hi)]),
        }
    }

    /// The constructors of `ty` split at the boundaries `heads` draw, so each
    /// head covers every piece entirely or not at all.
    fn pieces(&self, ty: &Ty, heads: &[Ctor]) -> Option<Vec<Ctor>> {
        let all = self.all_ctors(ty)?;
        Some(all.iter().flat_map(|c| split(c, heads)).collect())
    }

    /// The values of `ty` no row covers, spelled for a diagnostic (adjacent
    /// ranges merged); `_` when the type's constructors are not enumerable.
    fn missing(&self, rows: &[Pat], ty: &Ty) -> Vec<String> {
        let matrix: Vec<Vec<Pat>> =
            expand_or(&rows.iter().map(|p| vec![p.clone()]).collect::<Vec<_>>());
        let Some(pieces) = self.pieces(ty, &head_ctors(&matrix)) else {
            return vec!["`_`".into()];
        };
        let mut missing: Vec<Ctor> = Vec::new();
        for piece in pieces {
            if !self.useful_piece(&matrix, &piece, None, &[], ty, &[]) {
                continue;
            }
            if let (Some(Ctor::Range(_, prev_hi)), Ctor::Range(lo, hi)) =
                (missing.last_mut(), &piece)
                && prev_hi.saturating_add(1) == *lo
            {
                *prev_hi = *hi;
                continue;
            }
            missing.push(piece);
        }
        if missing.is_empty() {
            return vec!["`_`".into()];
        }
        let ty = defaulted(ty);
        missing
            .iter()
            .map(|ctor| format!("`{}`", ctor.spell(ty, self)))
            .collect()
    }
}

/// The type a pattern sees for an undetermined integer literal scrutinee.
fn defaulted(ty: &Ty) -> &Ty {
    match ty {
        Ty::InferInt => &Ty::I64,
        Ty::InferFloat => &Ty::F64,
        ty => ty,
    }
}

/// `ctor` split at the range and length boundaries `heads` draw: every head
/// either covers a piece or shares no value with it. An empty range has no
/// pieces.
fn split(ctor: &Ctor, heads: &[Ctor]) -> Vec<Ctor> {
    match ctor {
        Ctor::Range(lo, hi) => {
            if lo > hi {
                return Vec::new();
            }
            let end = hi.saturating_add(1);
            let mut cuts = vec![*lo, end];
            for head in heads {
                if let Ctor::Range(a, b) = head {
                    cuts.extend(
                        [*a, b.saturating_add(1)]
                            .into_iter()
                            .filter(|c| lo < c && *c < end),
                    );
                }
            }
            cuts.sort_unstable();
            cuts.dedup();
            cuts.windows(2)
                .map(|w| Ctor::Range(w[0], w[1] - 1))
                .collect()
        }
        Ctor::Slice(Slice::Var(p, s)) => {
            let (mut prefix, mut suffix, mut fixed_end) = (*p, *s, 0);
            for head in heads {
                match head {
                    Ctor::Slice(Slice::Var(q, t)) => {
                        prefix = prefix.max(*q);
                        suffix = suffix.max(*t);
                    }
                    Ctor::Slice(Slice::Fixed(n)) => fixed_end = fixed_end.max(n + 1),
                    _ => {}
                }
            }
            // Lengths below `len` are told apart one by one; every longer list
            // looks the same to every head.
            let len = (prefix + suffix).max(fixed_end);
            let mut pieces: Vec<Ctor> =
                (p + s..len).map(|n| Ctor::Slice(Slice::Fixed(n))).collect();
            pieces.push(Ctor::Slice(Slice::Var(len - suffix, suffix)));
            pieces
        }
        _ => vec![ctor.clone()],
    }
}

/// The `arity` sub-patterns a row headed by `ctor(args)` puts in the columns
/// of `piece`, or `None` when `ctor` does not cover `piece`. A variable-length
/// list pattern matches the middle elements with wildcards.
fn spread(ctor: &Ctor, args: &[Pat], piece: &Ctor, arity: usize) -> Option<Vec<Pat>> {
    if !ctor.covers(piece) {
        return None;
    }
    let out = match ctor {
        Ctor::Slice(Slice::Var(prefix, _)) => {
            let at = (*prefix).min(args.len());
            let middle = arity.saturating_sub(args.len());
            let mut out = args[..at].to_vec();
            out.extend(std::iter::repeat_n(Pat::Wild, middle));
            out.extend_from_slice(&args[at..]);
            out
        }
        _ => {
            let mut out = args.to_vec();
            out.resize(arity, Pat::Wild);
            out
        }
    };
    Some(out)
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

/// The rows that match every value of `piece`, with its `arity` fields spread
/// into columns.
fn specialize(rows: &[Vec<Pat>], piece: &Ctor, arity: usize) -> Vec<Vec<Pat>> {
    rows.iter()
        .filter_map(|row| {
            let (head, rest) = row.split_first()?;
            let mut out = match head {
                Pat::Wild => vec![Pat::Wild; arity],
                Pat::Ctor(c, args) => spread(c, args, piece, arity)?,
                Pat::Or(_) => return None,
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

/// What a literal pattern spells, or `None` for another node.
fn literal(node: &SyntaxNode) -> Option<Lit> {
    if node.kind() != SyntaxKind::LiteralPattern {
        return None;
    }
    let tokens: Vec<SyntaxToken> = node
        .children_with_tokens()
        .into_iter()
        .filter_map(|e| e.as_token().cloned())
        .filter(|t| !t.kind().is_trivia())
        .collect();
    let (negative, token) = match tokens.as_slice() {
        [minus, token] if minus.kind() == SyntaxKind::Minus => (true, token),
        [token] => (false, token),
        _ => return None,
    };
    let text = token.text();
    let text = text.as_str();
    let lit = match token.kind() {
        SyntaxKind::IntLiteral => {
            let value = super::parse_int_literal(text);
            Lit::Int(if negative { value.map(|v| -v) } else { value })
        }
        SyntaxKind::CharLiteral => {
            let body = text.strip_prefix('\'')?.strip_suffix('\'')?;
            Lit::Char(unescape(body).and_then(|s| {
                let mut chars = s.chars();
                let c = chars.next()?;
                chars.next().is_none().then_some(c)
            }))
        }
        SyntaxKind::StringLiteral => {
            Lit::Str(text.strip_prefix('"')?.strip_suffix('"').and_then(unescape))
        }
        SyntaxKind::RawStringLiteral => {
            let hashes = text.strip_prefix('r')?;
            let marks = hashes.len() - hashes.trim_start_matches('#').len();
            let body = hashes
                .get(marks..hashes.len().checked_sub(marks)?)?
                .strip_prefix('"')?
                .strip_suffix('"')?;
            Lit::Str(Some(body.to_string()))
        }
        SyntaxKind::TrueKw => Lit::Bool(true),
        SyntaxKind::FalseKw => Lit::Bool(false),
        SyntaxKind::NoneKw => Lit::None,
        _ => return None,
    };
    Some(lit)
}

/// The inclusive value bounds of a range pattern whose ends are both integer or
/// both `Char` literals.
fn range_bounds(node: &SyntaxNode) -> Option<(i128, i128)> {
    let ends: Vec<Option<(i128, bool)>> = node
        .children()
        .iter()
        .map(|end| literal(end).and_then(|lit| lit.scalar()))
        .collect();
    let [Some((lo, a)), Some((hi, b))] = ends.as_slice() else {
        return None;
    };
    if a != b {
        return None;
    }
    let inclusive = node
        .children_with_tokens()
        .into_iter()
        .filter_map(|e| e.as_token().cloned())
        .any(|t| t.kind() == SyntaxKind::DotDotEq);
    Some((*lo, if inclusive { *hi } else { hi - 1 }))
}

/// The value a string or char literal body spells, `None` when an escape is
/// malformed (already a lexical error).
fn unescape(body: &str) -> Option<String> {
    let mut out = String::with_capacity(body.len());
    let mut chars = body.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        let escaped = match chars.next()? {
            'n' => '\n',
            'r' => '\r',
            't' => '\t',
            '0' => '\0',
            '\\' => '\\',
            '"' => '"',
            '\'' => '\'',
            'x' => {
                let digits: String = chars.by_ref().take(2).collect();
                if digits.len() != 2 {
                    return None;
                }
                char::from_u32(u32::from_str_radix(&digits, 16).ok()?)?
            }
            'u' => {
                if chars.next()? != '{' {
                    return None;
                }
                let digits: String = chars.by_ref().take_while(|c| *c != '}').collect();
                if digits.is_empty() || digits.len() > 6 {
                    return None;
                }
                char::from_u32(u32::from_str_radix(&digits, 16).ok()?)?
            }
            _ => return None,
        };
        out.push(escaped);
    }
    Some(out)
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

/// Whether `node` is a rest pattern `..`, bare or wrapped in a `Pattern`.
fn is_rest(node: &SyntaxNode) -> bool {
    node.kind() == SyntaxKind::RestPattern
        || (node.kind() == SyntaxKind::Pattern
            && node
                .first_child()
                .is_some_and(|c| c.kind() == SyntaxKind::RestPattern))
}
