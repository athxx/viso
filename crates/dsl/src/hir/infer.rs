//! Expression type inference.
//!
//! This walks a resolved expression tree and assigns each expression a [`Ty`],
//! emitting the numeric diagnostics (`E2101`/`E2102`/`E2103`) as it goes. It is the
//! piece that discharges the doc's "no untyped numeric literal after HIR" contract:
//! an integer/float literal is not a runtime type until inference pins it, either
//! from an expected type flowing down or, failing that, from the host default
//! (`1` -> `I64`, `1.0` -> `F64`).
//!
//! The rules implemented here:
//!
//! - **Literal typing.** With an expected numeric type, a literal *instantiates* at
//!   that type directly (a float literal at `F32` is a normal `F32` instantiation, not
//!   an `F64 -> F32` narrowing), after a range/precision check. Without an expected
//!   type, a numeric literal takes the host default.
//! - **Numeric widening.** Where a value of one numeric type meets an expected type,
//!   the only implicit moves allowed are the safe widenings ([`Ty::check_implicit_widen`]);
//!   anything else is `E2102`. A non-numeric mismatch is `E2103`.
//! - **Unification.** `if`/`match` result types unify their branch types (equal, or a
//!   single common widening target); a non-unifiable set is `E2103` on the expression.
//!
//! Inference is deliberately decoupled from the HIR node types (built in a later
//! section) through the [`TypeEnv`] trait: everything inference needs to know about
//! *what a resolved name's type is* and *what a callee's signature is* is answered by
//! the environment, so this module is testable against a stub environment without the
//! component-lowering machinery existing yet.

pub(crate) mod body;
mod carry;
pub(crate) mod format;
mod lens;
mod native;
pub(crate) mod pattern;
mod record;

pub use native::NativeCall;

pub(crate) use pattern::MatchCheck;

use std::collections::{HashMap, HashSet};

use crate::ast::{AstNode, CallExpr, CastExpr, Expr, PathExpr, TypePath};
use crate::diag::Diagnostic;
use crate::hir::ty::{PackageTypes, Ty, TypeError, WidenError};
use crate::resolve::{LocalSlot, Resolution, ResolvedRef, SymbolId, SymbolKind};
use crate::syntax::{SyntaxKind, SyntaxNode, SyntaxToken, TextRange};
use viso_behavior::native::{NativeId, Natives};

/// One field of a record type or of a record-payload enum variant.
#[derive(Debug, Clone, PartialEq)]
pub struct FieldInfo {
    /// The field's name.
    pub name: String,
    /// The field's declared type.
    pub ty: Ty,
    /// Whether the declaration gives the field a default, so an initializer may omit it.
    pub has_default: bool,
    /// The span of the field's name in its declaration.
    pub declared_at: TextRange,
}

/// One event a component declares.
#[derive(Debug, Clone, PartialEq)]
pub struct EventInfo {
    /// The event's name.
    pub name: String,
    /// The event's symbol, whose record fields are its payload.
    pub symbol: SymbolId,
    /// The span of the event's name in its declaration.
    pub declared_at: TextRange,
}

/// What an enum variant carries.
#[derive(Debug, Clone, PartialEq)]
pub enum VariantPayload {
    /// `idle;`
    Unit,
    /// `ready(T);`
    Tuple(Vec<Ty>),
    /// `failed { code: I64; }`
    Record(Vec<FieldInfo>),
}

/// One variant of an enum type.
#[derive(Debug, Clone, PartialEq)]
pub struct VariantInfo {
    /// The variant's name.
    pub name: String,
    /// What the variant carries.
    pub payload: VariantPayload,
    /// The span of the variant's name in its declaration.
    pub declared_at: TextRange,
}

/// The record and enum declarations of a package, by symbol: what a hot reload
/// compares a state's old and new type against.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TypeSchemas {
    /// The fields of every record, and the payload of every event.
    pub records: HashMap<SymbolId, Vec<FieldInfo>>,
    /// The variants of every enum.
    pub enums: HashMap<SymbolId, Vec<VariantInfo>>,
    /// The declared name of every record, enum, event and component.
    pub names: HashMap<SymbolId, String>,
}

/// What inference needs to know about the surrounding program: the type of a resolved
/// name and the signature of a callable it resolves to. Supplying this as a trait keeps
/// inference independent of the HIR node types (which a later section builds) — the
/// component-lowering layer implements it over the real symbol/HIR tables, and tests
/// implement it over a stub.
pub trait TypeEnv {
    /// The type of a resolved name use (a `let`/param local, or a symbol — a `state`,
    /// `input`, `computed`, `const`, or record/enum type). `None` when the environment
    /// has no type for it (inference records `Ty::Unknown` and moves on; the missing
    /// binding is a resolver-level fact, not typed here).
    fn resolution_ty(&self, to: &Resolution) -> Option<Ty>;

    /// The signature `(params, ret)` of a callable a name resolves to, when the name is
    /// used in callee position. `None` if the resolution is not a callable (a plain
    /// value call is then an `E2103`).
    fn callee_signature(&self, to: &Resolution) -> Option<(Vec<Ty>, Ty)>;

    /// The fields of the record type `ty` names, when it is a record the environment
    /// knows.
    fn record_fields(&self, ty: SymbolId) -> Option<&[FieldInfo]> {
        let _ = ty;
        None
    }

    /// The variants of the enum type `ty` names, when it is an enum the environment
    /// knows.
    fn enum_variants(&self, ty: SymbolId) -> Option<&[VariantInfo]> {
        let _ = ty;
        None
    }

    /// The declared name of the nominal type `ty`, for diagnostics.
    fn type_name(&self, ty: SymbolId) -> Option<&str> {
        let _ = ty;
        None
    }

    /// What kind of declaration the symbol `id` is, when the environment knows it.
    fn symbol_kind(&self, id: SymbolId) -> Option<SymbolKind> {
        let _ = id;
        None
    }

    /// The events of the component `component`, when the environment knows it.
    fn component_events(&self, component: SymbolId) -> Option<&[EventInfo]> {
        let _ = component;
        None
    }

    /// The component whose body is being typed, when there is one.
    fn enclosing_component(&self) -> Option<SymbolId> {
        None
    }

    /// The native registry paths resolved against, when the environment has one.
    fn natives(&self) -> Option<&Natives> {
        None
    }

    /// The ticks a second of the game's fixed step, which a tick duration
    /// argument is converted with.
    fn tick_rate(&self) -> u32 {
        viso_behavior::DEFAULT_TICK_RATE
    }

    /// The package's types of input actions and game tags: the enum its
    /// `InputMap` maps, or `viso::game::InputAction`, and its
    /// `@derive(GameTag)` enum, or `viso::game::GameTag`.
    fn package_types(&self) -> PackageTypes {
        PackageTypes {
            action: Ty::Native(NativeId::of(viso_behavior::game::InputAction::PATH)),
            tag: Ty::Native(NativeId::of(viso_behavior::game::GameTag::PATH)),
        }
    }

    /// Notes that the call at `call` is bound to the native function `id`.
    fn record_native(&self, call: TextRange, id: NativeId) {
        let _ = (call, id);
    }

    /// Where the declaration at `range` inside `owner` (one of its fields, variants,
    /// events or inputs) can be shown: the declaring module's `::`-joined path, or
    /// `None` for the module being typed, and the range. `None` when `owner` has no
    /// source file in the package, as the prelude's declarations do not.
    fn declaration_site(
        &self,
        owner: SymbolId,
        range: TextRange,
    ) -> Option<(Option<&str>, TextRange)> {
        let _ = owner;
        Some((None, range))
    }
}

/// One enclosing loop during a body walk.
struct LoopFrame {
    /// Whether this is a `loop` (the only loop a `break` may carry a value out of).
    carries_value: bool,
    /// The type the loop's `break` values unify to so far.
    break_ty: Option<Ty>,
}

/// The inference context for one expression walk: the resolved-reference index (so a
/// path use can be looked up by the span of its head segment), the environment, and the
/// diagnostics sink.
pub struct InferCx<'a> {
    /// Every resolved name use keyed by its token span, so a `PathExpr` head resolves in
    /// O(1).
    refs: HashMap<TextRange, Resolution>,
    /// The surrounding-program type oracle.
    env: &'a dyn TypeEnv,
    /// Diagnostics accumulated during the walk.
    diagnostics: Vec<Diagnostic>,
    /// The type each local binding was given, keyed by its slot.
    locals: HashMap<LocalSlot, Ty>,
    /// The return type of each enclosing callable or closure, innermost last; `None`
    /// when a closure's return type is still unknown.
    returns: Vec<Option<Ty>>,
    /// The enclosing loops, innermost last.
    loops: Vec<LoopFrame>,
    /// Every expression whose own type is `Percent`.
    percent_typed: HashSet<TextRange>,
    /// What the walk has defined names as, for the module's percent flow.
    percent_defs: Vec<(Resolution, crate::hir::percent::Carry)>,
    /// The locals declared `mut`, which alone may be assigned.
    mutable: HashSet<LocalSlot>,
    /// The type each expression was given, keyed by its span and kind (a span alone
    /// is shared by a wrapper and its only child). A re-typed expression keeps its
    /// last type, the one its context settled on.
    types: HashMap<(TextRange, SyntaxKind), Ty>,
    /// Every call bound to a native function, keyed by the call's span.
    native_calls: HashMap<TextRange, NativeCall>,
    /// The whole ticks each tick duration argument of a native call converts
    /// to, keyed by the argument's span.
    ticks: HashMap<TextRange, i64>,
}

impl<'a> InferCx<'a> {
    /// Builds a context from a module's resolved references and a type environment.
    pub fn new(refs: &[ResolvedRef], env: &'a dyn TypeEnv) -> Self {
        let mut index = HashMap::with_capacity(refs.len());
        for r in refs {
            index.insert(r.range, r.to);
        }
        InferCx {
            refs: index,
            env,
            diagnostics: Vec::new(),
            locals: HashMap::new(),
            returns: Vec::new(),
            loops: Vec::new(),
            percent_typed: HashSet::new(),
            percent_defs: Vec::new(),
            mutable: HashSet::new(),
            types: HashMap::new(),
            native_calls: HashMap::new(),
            ticks: HashMap::new(),
        }
    }

    /// The type inference gave `expr`, when the walk reached it.
    pub(crate) fn type_of(&self, expr: &Expr) -> Option<&Ty> {
        let node = expr.syntax();
        self.types.get(&(node.text_range(), node.kind()))
    }

    /// What the name token at `range` resolves to.
    pub(crate) fn resolution_at(&self, range: TextRange) -> Option<Resolution> {
        self.refs.get(&range).copied()
    }

    /// The surrounding-program type oracle.
    pub(crate) fn env(&self) -> &'a dyn TypeEnv {
        self.env
    }

    /// Consumes the context, returning the diagnostics it gathered.
    pub fn into_diagnostics(self) -> Vec<Diagnostic> {
        self.diagnostics
    }

    /// Borrows the diagnostics gathered so far (for a caller that keeps inferring).
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    /// Infers the type of `expr`, using `expected` to type numeric literals and to check
    /// implicit conversions where the surrounding context demands a specific type. When
    /// `expected` is `None`, numeric literals take the host default and no conversion is
    /// checked here (the caller checks against its own expectation, if any).
    pub fn infer_expr(&mut self, expr: &Expr, expected: Option<&Ty>) -> Ty {
        let node = expr.syntax();
        let ty = match node.kind() {
            SyntaxKind::LiteralExpr => self.infer_literal(node, expected),
            SyntaxKind::PathExpr => {
                let ty = self.infer_path(node, expected);
                self.check_against(ty, expected, node)
            }
            SyntaxKind::FieldExpr => self.infer_field(node, expected),
            SyntaxKind::OptionalFieldExpr => self.infer_optional_field(node, expected),
            SyntaxKind::RecordExpr => self.infer_record(node, expected),
            SyntaxKind::CallExpr => self.infer_call(node, expected),
            SyntaxKind::IndexExpr => self.infer_index(node, expected),
            SyntaxKind::TryExpr => self.infer_try(node, expected),
            SyntaxKind::RangeExpr => self.infer_range(node, expected),
            SyntaxKind::ClosureExpr => self.infer_closure(node, expected, false),
            SyntaxKind::BlockExpr => match node
                .children()
                .into_iter()
                .find(|c| c.kind() == SyntaxKind::Block)
            {
                Some(block) => self.infer_block(&block, expected),
                None => Ty::Unknown,
            },
            SyntaxKind::BinaryExpr => self.infer_binary(node, expected),
            SyntaxKind::UnaryExpr => self.infer_unary(node, expected),
            SyntaxKind::CastExpr => self.infer_cast(node),
            SyntaxKind::ParenExpr => match first_child_expr(node) {
                Some(inner) => self.infer_expr(&inner, expected),
                None => Ty::Unknown,
            },
            SyntaxKind::TupleExpr => self.infer_tuple(node, expected),
            SyntaxKind::ListExpr => self.infer_list(node, expected),
            SyntaxKind::IfExpr => self.infer_if(node, expected),
            SyntaxKind::MatchExpr => self.infer_match(node, expected),
            _ => Ty::Unknown,
        };
        self.note_percent(&ty, node);
        self.types
            .insert((node.text_range(), node.kind()), ty.clone());
        ty
    }

    /// Types a numeric/scalar literal. A numeric literal instantiates at `expected` when
    /// one is given (after range/precision), else takes the host default; non-numeric
    /// literals (bool/char/string/color) have a fixed type checked against `expected`.
    fn infer_literal(&mut self, node: &SyntaxNode, expected: Option<&Ty>) -> Ty {
        let Some(tok) = node
            .children_with_tokens()
            .into_iter()
            .find_map(|e| e.as_token().cloned())
        else {
            return Ty::Unknown;
        };
        let range = node.text_range();
        match tok.kind() {
            SyntaxKind::IntLiteral => self.type_int_literal(&tok.text(), expected, range),
            SyntaxKind::FloatLiteral => self.type_float_literal(&tok.text(), expected, range),
            SyntaxKind::TrueKw | SyntaxKind::FalseKw => {
                self.check_against(Ty::Bool, expected, node)
            }
            SyntaxKind::CharLiteral => self.check_against(Ty::Char, expected, node),
            SyntaxKind::StringLiteral | SyntaxKind::RawStringLiteral => {
                self.check_against(Ty::String, expected, node)
            }
            SyntaxKind::ColorLiteral => self.check_against(Ty::Color, expected, node),
            SyntaxKind::NoneKw => match expected {
                Some(Ty::Option(_) | Ty::Unknown) => expected.cloned().unwrap_or(Ty::Unknown),
                _ => self.check_against(Ty::Option(Box::new(Ty::Unknown)), expected, node),
            },
            SyntaxKind::UnitLiteral => self.type_unit_literal(&tok.text(), expected, node),
            _ => Ty::Unknown,
        }
    }

    /// Types a suffixed literal by its suffix (§19.1): a unit suffix gives its
    /// dimension and a numeric type suffix (`2u8`, `1.5f32`) its scalar, range
    /// checked. An unknown suffix was already reported by the lexer.
    fn type_unit_literal(&mut self, text: &str, expected: Option<&Ty>, node: &SyntaxNode) -> Ty {
        let Some((body, produced)) = split_unit_literal(text) else {
            return expected.cloned().unwrap_or(Ty::Unknown);
        };
        if is_integer_ty(&produced)
            && let Some(v) = parse_int_literal(body)
            && !int_fits(v, &produced)
        {
            self.diagnostics.push(Diagnostic::error(
                "E2103",
                node.text_range(),
                format!("integer literal out of range for `{}`", ty_name(&produced)),
            ));
        }
        self.check_against(produced, expected, node)
    }

    /// Types an integer literal. With an integer `expected` type it instantiates there
    /// after a range check; with a float `expected` it is a mismatch (an int literal is
    /// not a float source implicitly); with no context it is the host default `I64`.
    fn type_int_literal(&mut self, text: &str, expected: Option<&Ty>, range: TextRange) -> Ty {
        let value = parse_int_literal(text);
        match expected {
            Some(target) if is_integer_ty(target) => {
                if let Some(v) = value
                    && !int_fits(v, target)
                {
                    self.diagnostics.push(Diagnostic::error(
                        "E2103",
                        range,
                        format!("integer literal out of range for `{}`", ty_name(target)),
                    ));
                }
                target.clone()
            }
            Some(target) if is_float_ty(target) => {
                // An integer literal in a float slot is a legal literal instantiation
                // (`1` where `F32` is expected is the float one), per the doc's
                // "instantiate the literal, do not convert" rule.
                target.clone()
            }
            Some(target) => {
                self.emit_mismatch(&Ty::I64, target, range);
                target.clone()
            }
            None => Ty::I64,
        }
    }

    /// Types a float literal. With a float `expected` it instantiates there (checking the
    /// value is representable); an integer or other `expected` is a mismatch; no context
    /// gives the host default `F64`.
    fn type_float_literal(&mut self, text: &str, expected: Option<&Ty>, range: TextRange) -> Ty {
        match expected {
            Some(target) if is_float_ty(target) => {
                // Instantiation, not conversion: reject only a value that has no
                // finite `f32` representation at all.
                if target == &Ty::F32
                    && let Some(v) = parse_float_literal(text)
                    && v.is_finite()
                    && (v as f32).is_infinite()
                {
                    self.diagnostics.push(Diagnostic::error(
                        "E2103",
                        range,
                        "float literal out of range for `F32`",
                    ));
                }
                target.clone()
            }
            Some(target) if is_integer_ty(target) => {
                self.emit_mismatch(&Ty::F64, target, range);
                target.clone()
            }
            Some(target) => {
                self.emit_mismatch(&Ty::F64, target, range);
                target.clone()
            }
            None => Ty::F64,
        }
    }

    /// Types a `PathExpr`: a local, a symbol the environment types, an enum variant
    /// (`S::idle`, or the constructor `S::ready`), or `Option::None`.
    fn infer_path(&mut self, node: &SyntaxNode, expected: Option<&Ty>) -> Ty {
        let Some(path) = PathExpr::cast(node.clone()) else {
            return Ty::Unknown;
        };
        let segments: Vec<SyntaxToken> = path.segments().collect();
        let Some(head) = segments.first() else {
            return Ty::Unknown;
        };
        match self.refs.get(&head.text_range()).copied() {
            Some(Resolution::Symbol(id)) if segments.len() >= 2 => {
                match self.env.enum_variants(id) {
                    Some(_) => self.variant_value(id, &segments[1]),
                    None => Ty::Unknown,
                }
            }
            Some(Resolution::Native(id)) => match self.native_variant(id) {
                Some(variant) => self.check_against(Ty::Native(variant.ty), expected, node),
                None => self.native_value(id, node.text_range()),
            },
            Some(to) if segments.len() == 1 => self.resolution_ty(&to),
            Some(_) => Ty::Unknown,
            None => match builtin_variant(&segments) {
                Some("None") => match expected {
                    Some(Ty::Option(_)) => expected.cloned().unwrap_or(Ty::Unknown),
                    _ => Ty::Option(Box::new(Ty::Unknown)),
                },
                _ => Ty::Unknown,
            },
        }
    }

    /// The type of a resolved name: a local's bound type, else the environment's answer.
    pub(crate) fn resolution_ty(&self, to: &Resolution) -> Ty {
        match to {
            Resolution::Local(slot) => self.locals.get(slot).cloned().unwrap_or(Ty::Unknown),
            Resolution::Symbol(_) | Resolution::Env => {
                self.env.resolution_ty(to).unwrap_or(Ty::Unknown)
            }
            Resolution::Native(_) => Ty::Unknown,
        }
    }

    /// The symbol the name token at `range` resolves to, if it is a symbol.
    pub(crate) fn symbol_at(&self, range: TextRange) -> Option<SymbolId> {
        match self.refs.get(&range) {
            Some(Resolution::Symbol(id)) => Some(*id),
            _ => None,
        }
    }

    /// Types a call: resolves the callee's signature, then checks each argument against
    /// its parameter type (widening allowed, `E2102`/`E2103` otherwise) and returns the
    /// declared return type. `Some`/`Ok`/`Err` construct from the expected type, and
    /// `format` is checked against its template.
    fn infer_call(&mut self, node: &SyntaxNode, expected: Option<&Ty>) -> Ty {
        let Some(call) = CallExpr::cast(node.clone()) else {
            return Ty::Unknown;
        };
        let callee = call.callee();
        let args = call_args(node);
        let path = callee
            .as_ref()
            .filter(|c| c.syntax().kind() == SyntaxKind::PathExpr)
            .and_then(|c| PathExpr::cast(c.syntax().clone()));
        let segments: Vec<SyntaxToken> = path
            .as_ref()
            .map(|p| p.segments().collect())
            .unwrap_or_default();
        let head = segments
            .first()
            .and_then(|h| self.refs.get(&h.text_range()).copied());

        if head.is_none() && !segments.is_empty() {
            if segments.len() == 1 && segments[0].text() == "format" {
                let ty = self.check_format(node);
                return self.check_against(ty, expected, node);
            }
            if let Some(ctor) = builtin_variant(&segments)
                && ctor != "None"
            {
                return self.infer_builtin_ctor(ctor, &args, expected, node);
            }
        }

        let sig = match (head, segments.len()) {
            (Some(Resolution::Local(slot)), 1) => match self.locals.get(&slot).cloned() {
                Some(Ty::Fn(params, ret)) => {
                    if let Some(callee) = &callee {
                        let _ = self.infer_expr(callee, None);
                    }
                    Some((params, *ret))
                }
                _ => None,
            },
            (Some(Resolution::Symbol(id)), n) if n >= 2 && self.env.enum_variants(id).is_some() => {
                match self.variant_value(id, &segments[1]) {
                    Ty::Fn(params, ret) => Some((params, *ret)),
                    _ => None,
                }
            }
            (Some(Resolution::Native(id)), _) => {
                let at = callee
                    .as_ref()
                    .map_or(node.text_range(), |c| c.syntax().text_range());
                let sig = self.native_path_call(id, node, at);
                if let Some(sig) = sig {
                    let ty = self.apply_signature(&args, sig, expected, node);
                    self.native_ticks(id, &args, false);
                    return ty;
                }
                None
            }
            (Some(to), 1) => self.env.callee_signature(&to),
            _ => {
                // A method or computed callee: infer its receiver for its own diagnostics.
                if let Some(callee) = &callee {
                    match callee.syntax().kind() {
                        SyntaxKind::PathExpr => {}
                        SyntaxKind::FieldExpr => {
                            if let Some(recv) = first_child_expr(callee.syntax())
                                && let Ty::Native(ty) = self.infer_expr(&recv, None)
                            {
                                return self.native_method_call(
                                    ty,
                                    callee.syntax(),
                                    &args,
                                    expected,
                                    node,
                                );
                            }
                        }
                        SyntaxKind::OptionalFieldExpr => {
                            let _ = self.infer_optional_receiver(callee.syntax());
                        }
                        _ => {
                            if let Ty::Fn(params, ret) = self.infer_expr(callee, None) {
                                return self.apply_signature(&args, (params, *ret), expected, node);
                            }
                        }
                    }
                }
                None
            }
        };

        match sig {
            Some(sig) => self.apply_signature(&args, sig, expected, node),
            None => {
                // Unknown callee signature: still infer arguments so their own
                // diagnostics fire, then leave the result undetermined. A closure here
                // takes its parameter types from a signature this pass cannot see.
                for arg in &args {
                    if arg.syntax().kind() == SyntaxKind::ClosureExpr {
                        let _ = self.infer_closure(arg.syntax(), None, true);
                    } else {
                        let _ = self.infer_expr(arg, None);
                    }
                }
                Ty::Unknown
            }
        }
    }

    /// Checks `args` against a known signature and yields its return type.
    fn apply_signature(
        &mut self,
        args: &[Expr],
        (params, ret): (Vec<Ty>, Ty),
        expected: Option<&Ty>,
        node: &SyntaxNode,
    ) -> Ty {
        for (i, arg) in args.iter().enumerate() {
            let _ = self.infer_expr(arg, params.get(i));
        }
        if ret == Ty::Unknown {
            return ret;
        }
        self.check_against(ret, expected, node)
    }

    /// Types `Some(v)`, `Ok(v)` and `Err(e)` from the expected `Option`/`Result`.
    fn infer_builtin_ctor(
        &mut self,
        ctor: &str,
        args: &[Expr],
        expected: Option<&Ty>,
        node: &SyntaxNode,
    ) -> Ty {
        let (inner_expected, ok_expected, err_expected) = match expected {
            Some(Ty::Option(t)) => (Some(t.as_ref()), None, None),
            Some(Ty::Result(t, e)) => (None, Some(t.as_ref()), Some(e.as_ref())),
            _ => (None, None, None),
        };
        let want = match ctor {
            "Some" => inner_expected,
            "Ok" => ok_expected,
            _ => err_expected,
        };
        let inner = match args.first() {
            Some(arg) => self.infer_expr(arg, want),
            None => Ty::Unknown,
        };
        for extra in args.iter().skip(1) {
            let _ = self.infer_expr(extra, None);
        }
        let produced = match (ctor, expected) {
            ("Some", _) => Ty::Option(Box::new(inner)),
            ("Ok", Some(Ty::Result(_, e))) => Ty::Result(Box::new(inner), e.clone()),
            ("Ok", _) => Ty::Result(Box::new(inner), Box::new(Ty::Unknown)),
            (_, Some(Ty::Result(t, _))) => Ty::Result(t.clone(), Box::new(inner)),
            _ => Ty::Result(Box::new(Ty::Unknown), Box::new(inner)),
        };
        self.check_against(produced, expected, node)
    }

    /// Types `recv[index]`: a `List<T>` element is a `T`, and the index is an integer.
    fn infer_index(&mut self, node: &SyntaxNode, expected: Option<&Ty>) -> Ty {
        let exprs = child_exprs(node);
        let recv = exprs
            .first()
            .map_or(Ty::Unknown, |e| self.infer_expr(e, None));
        if let Some(index) = exprs.get(1) {
            let ty = self.infer_expr(index, None);
            if !matches!(ty, Ty::Unknown | Ty::Never) && !is_integer_ty(&ty) {
                let actual = self.describe(&ty);
                let message = format!("a list index is an integer, found `{actual}`");
                self.diagnostics.push(
                    Diagnostic::error("E2103", index.syntax().text_range(), message)
                        .expecting(INTEGER_TYPES, actual),
                );
            }
        }
        match recv {
            Ty::List(elem) => self.check_against(*elem, expected, node),
            _ => Ty::Unknown,
        }
    }

    /// Types `expr?`: it unwraps an `Option<T>` or `Result<T, E>` to `T`.
    fn infer_try(&mut self, node: &SyntaxNode, expected: Option<&Ty>) -> Ty {
        let Some(inner) = first_child_expr(node) else {
            return Ty::Unknown;
        };
        match self.infer_expr(&inner, None) {
            Ty::Option(t) | Ty::Result(t, _) => self.check_against(*t, expected, node),
            Ty::Unknown | Ty::Never => Ty::Unknown,
            other => {
                let actual = self.describe(&other);
                let message =
                    format!("the `?` operator needs an `Option` or a `Result`, found `{actual}`");
                self.diagnostics.push(
                    Diagnostic::error("E2103", node.text_range(), message)
                        .expecting(["Option<T>", "Result<T, E>"], actual),
                );
                Ty::Unknown
            }
        }
    }

    /// Types `a..b` / `a..=b`: both bounds share one element type.
    fn infer_range(&mut self, node: &SyntaxNode, expected: Option<&Ty>) -> Ty {
        let inclusive = node
            .children_with_tokens()
            .into_iter()
            .filter_map(|e| e.as_token().cloned())
            .any(|t| t.kind() == SyntaxKind::DotDotEq);
        let want = match expected {
            Some(Ty::Range(t) | Ty::RangeInclusive(t)) => Some(t.as_ref().clone()),
            _ => None,
        };
        let bounds = child_exprs(node);
        let mut order: Vec<&Expr> = bounds.iter().collect();
        // A typed bound instantiates a bare one, so the typed side goes first.
        order.sort_by_key(|e| is_bare_number(e));
        let mut elem = want.clone();
        for bound in order {
            let context = want.clone().or_else(|| elem.clone().filter(is_numeric_ty));
            let ty = self.infer_expr(bound, context.as_ref());
            elem = match elem {
                None => Some(ty),
                Some(prev) => match unify(&prev, &ty) {
                    Some(u) => Some(u),
                    None => {
                        let (expected, actual) = (self.describe(&prev), self.describe(&ty));
                        let message = format!(
                            "range bounds have different types: `{expected}` and `{actual}`"
                        );
                        self.diagnostics.push(
                            Diagnostic::error("E2103", node.text_range(), message)
                                .expecting([expected], actual),
                        );
                        Some(prev)
                    }
                },
            };
        }
        let elem = Box::new(elem.unwrap_or(Ty::Unknown));
        let produced = if inclusive {
            Ty::RangeInclusive(elem)
        } else {
            Ty::Range(elem)
        };
        self.check_against(produced, expected, node)
    }

    /// Types a binary expression. Comparison/logical operators yield `Bool`; arithmetic
    /// and bitwise operators yield the operands' common numeric type (widened to fit),
    /// with `E2102` on an illegal implicit mix. Dimensional operands follow §19.3.
    ///
    /// A bare number (an unsuffixed literal, or `-`/`()`/arithmetic over them) takes
    /// its type from the other operand, so the typed side is inferred first.
    fn infer_binary(&mut self, node: &SyntaxNode, expected: Option<&Ty>) -> Ty {
        let mut operands = child_exprs(node);
        let rhs = operands.pop();
        let lhs = operands.pop();
        let op = binary_op_kind(node);
        let lhs_bare = lhs.as_ref().is_some_and(is_bare_number);
        let rhs_bare = rhs.as_ref().is_some_and(is_bare_number);
        let relational = matches!(
            op,
            Some(
                SyntaxKind::EqEq
                    | SyntaxKind::Neq
                    | SyntaxKind::Lt
                    | SyntaxKind::Le
                    | SyntaxKind::Gt
                    | SyntaxKind::Ge
            )
        );
        let logical = matches!(op, Some(SyntaxKind::AmpAmp | SyntaxKind::PipePipe));
        // Only an arithmetic result type can instantiate the operands.
        let context = expected.filter(|t| !relational && !logical && is_numeric_ty(t));

        let (lty, rty) = if lhs_bare && !rhs_bare {
            let rty = self.infer_operand(rhs.as_ref(), None);
            let lty = self.infer_operand(lhs.as_ref(), numeric(&rty));
            (lty, rty)
        } else {
            let lty = self.infer_operand(lhs.as_ref(), if lhs_bare { context } else { None });
            let want = if rhs_bare { numeric(&lty) } else { None };
            let rty = self.infer_operand(rhs.as_ref(), want);
            (lty, rty)
        };

        if relational || logical {
            if relational && (lty.is_dimensional() || rty.is_dimensional()) {
                self.check_dimension_comparison(&lty, &rty, node);
            }
            return self.check_against(Ty::Bool, expected, node);
        }
        if lty != Ty::Unknown
            && rty != Ty::Unknown
            && (lty.is_dimensional() || rty.is_dimensional())
        {
            let scale = |ty: &Ty, bare: bool| ty == &Ty::F32 || (bare && is_numeric_ty(ty));
            let result = self.infer_dimension_arith(
                op,
                (&lty, scale(&lty, lhs_bare)),
                (&rty, scale(&rty, rhs_bare)),
                rhs.as_ref(),
                node,
            );
            return self.check_against(result, expected, node);
        }

        let result = unify_numeric(&lty, &rty).unwrap_or_else(|| {
            // A non-widenable numeric mix is an illegal implicit conversion.
            if is_numeric_ty(&lty) && is_numeric_ty(&rty) && lty != rty {
                self.diagnostics.push(Diagnostic::error(
                    "E2102",
                    node.text_range(),
                    "operands have incompatible numeric types; an explicit cast is required",
                ));
            }
            if lty == Ty::Unknown {
                rty.clone()
            } else {
                lty.clone()
            }
        });
        self.check_against(result, expected, node)
    }

    fn infer_operand(&mut self, expr: Option<&Expr>, expected: Option<&Ty>) -> Ty {
        expr.map_or(Ty::Unknown, |e| self.infer_expr(e, expected))
    }

    /// The §19.3 arithmetic table for a binary expression with a dimensional operand.
    /// Each side carries whether it may act as a scale factor (`F32` or a bare
    /// number). An illegal combination is reported and yields `Unknown`.
    fn infer_dimension_arith(
        &mut self,
        op: Option<SyntaxKind>,
        (l, l_scale): (&Ty, bool),
        (r, r_scale): (&Ty, bool),
        rhs: Option<&Expr>,
        node: &SyntaxNode,
    ) -> Ty {
        let cross = l.is_dimensional() && r.is_dimensional() && !(l.is_length() && r.is_length());
        let (code, message) = match op {
            Some(SyntaxKind::Plus | SyntaxKind::Minus) => {
                if l == r {
                    return l.clone();
                }
                if l.is_length() && r.is_length() {
                    return Ty::MixedLength;
                }
                if cross {
                    (
                        "E2103",
                        format!("cannot combine `{}` with `{}`", ty_name(l), ty_name(r)),
                    )
                } else {
                    (
                        "E2107",
                        format!(
                            "cannot add or subtract `{}` and `{}`; a dimensional value only combines with the same dimension",
                            ty_name(l),
                            ty_name(r)
                        ),
                    )
                }
            }
            Some(SyntaxKind::Star) => {
                if l.is_dimensional() && r_scale {
                    return l.clone();
                }
                if r.is_dimensional() && l_scale {
                    return r.clone();
                }
                (
                    "E2107",
                    "a dimensional value can only be scaled by an `F32` or a bare number"
                        .to_owned(),
                )
            }
            Some(SyntaxKind::Slash) => {
                if l.is_dimensional() && r_scale {
                    if l.is_length() && rhs.and_then(bare_value) == Some(0.0) {
                        ("E2109", "a length divided by constant zero".to_owned())
                    } else {
                        return l.clone();
                    }
                } else if l == r && l != &Ty::MixedLength {
                    return Ty::F64;
                } else if cross {
                    (
                        "E2103",
                        format!("cannot divide `{}` by `{}`", ty_name(l), ty_name(r)),
                    )
                } else {
                    (
                        "E2107",
                        format!("cannot divide `{}` by `{}`", ty_name(l), ty_name(r)),
                    )
                }
            }
            _ => (
                "E2107",
                "this operator is not defined for dimensional values".to_owned(),
            ),
        };
        self.diagnostics
            .push(Diagnostic::error(code, node.text_range(), message));
        Ty::Unknown
    }

    /// A relational comparison with a dimensional operand: only two values of the same
    /// concrete dimension compare; a `MixedLength` is unordered until layout.
    fn check_dimension_comparison(&mut self, l: &Ty, r: &Ty, node: &SyntaxNode) {
        if l == &Ty::Unknown || r == &Ty::Unknown {
            return;
        }
        let (code, message) = if l == &Ty::MixedLength || r == &Ty::MixedLength {
            (
                "E2107",
                "a `MixedLength` cannot be compared before layout".to_owned(),
            )
        } else if l != r {
            (
                "E2103",
                format!("cannot compare `{}` with `{}`", ty_name(l), ty_name(r)),
            )
        } else {
            return;
        };
        self.diagnostics
            .push(Diagnostic::error(code, node.text_range(), message));
    }

    /// Types a unary expression: `!` on `Bool` -> `Bool`; `-`/`~` preserve the operand's
    /// numeric type.
    fn infer_unary(&mut self, node: &SyntaxNode, expected: Option<&Ty>) -> Ty {
        let op = unary_op_kind(node);
        let operand = first_child_expr(node);
        let inner = match &operand {
            Some(e) => {
                let want = if op == Some(SyntaxKind::Bang) {
                    Some(&Ty::Bool)
                } else {
                    expected
                };
                self.infer_expr(e, want)
            }
            None => Ty::Unknown,
        };
        let result = match op {
            Some(SyntaxKind::Bang) => Ty::Bool,
            _ => inner,
        };
        self.check_against(result, expected, node)
    }

    /// Types an `expr as Type` cast. The target type is the result; an unknown target
    /// name is `E2103` and a `Float` target is `E2101`. A cast is the explicit escape
    /// hatch, so the operand's own type is inferred but not conversion-checked here.
    fn infer_cast(&mut self, node: &SyntaxNode) -> Ty {
        let Some(cast) = CastExpr::cast(node.clone()) else {
            return Ty::Unknown;
        };
        if let Some(op) = cast.operand() {
            let _ = self.infer_expr(&op, None);
        }
        match cast.ty() {
            Some(tp) => self.resolve_annotation(&tp, node.text_range()),
            None => Ty::Unknown,
        }
    }

    /// Types a tuple expression element-wise. An `expected` tuple type distributes over
    /// the elements; otherwise elements are inferred without context.
    fn infer_tuple(&mut self, node: &SyntaxNode, expected: Option<&Ty>) -> Ty {
        let elems = child_exprs(node);
        let expected_elems = match expected {
            Some(Ty::Tuple(tys)) if tys.len() == elems.len() => Some(tys),
            _ => None,
        };
        let mut tys = Vec::with_capacity(elems.len());
        for (i, e) in elems.iter().enumerate() {
            let want = expected_elems.and_then(|es| es.get(i));
            tys.push(self.infer_expr(e, want));
        }
        Ty::Tuple(tys)
    }

    /// Types a list expression. An `expected` `List<T>` flows `T` into every element and
    /// is the result; otherwise the element type is unified across the elements.
    fn infer_list(&mut self, node: &SyntaxNode, expected: Option<&Ty>) -> Ty {
        let elems = child_exprs(node);
        let expected_elem = match expected {
            Some(Ty::List(inner)) => Some(inner.as_ref()),
            _ => None,
        };
        let mut elem_ty: Option<Ty> = expected_elem.cloned();
        for e in &elems {
            let t = self.infer_expr(e, elem_ty.as_ref().or(expected_elem));
            elem_ty = match elem_ty {
                None => Some(t),
                Some(prev) => Some(unify_numeric(&prev, &t).unwrap_or(prev)),
            };
        }
        Ty::List(Box::new(elem_ty.unwrap_or(Ty::Unknown)))
    }

    /// Types an `if`/`else` expression: the condition types against `Bool`, and the
    /// branch result types unify (`E2103` if they cannot).
    fn infer_if(&mut self, node: &SyntaxNode, expected: Option<&Ty>) -> Ty {
        // An IfExpr's children are: condition Expr, then Block, optional else (Block or
        // IfExpr). We type the condition against Bool and unify the block types.
        let mut branch_tys = Vec::new();
        let mut saw_cond = false;
        for child in node.children() {
            match child.kind() {
                k if Expr::can_cast(k) && !saw_cond => {
                    saw_cond = true;
                    if let Some(cond) = Expr::cast(child) {
                        let _ = self.infer_expr(&cond, Some(&Ty::Bool));
                    }
                }
                SyntaxKind::Block => {
                    branch_tys.push(self.infer_block(&child, expected));
                }
                k if Expr::can_cast(k) => {
                    // else-if chain.
                    if let Some(inner) = Expr::cast(child) {
                        branch_tys.push(self.infer_expr(&inner, expected));
                    }
                }
                _ => {}
            }
        }
        self.unify_branches(&branch_tys, expected, node)
    }

    /// Types a `match` expression: the arms bind against the scrutinee's type, their
    /// result types unify (`E2103` otherwise), and the arms must be exhaustive.
    fn infer_match(&mut self, node: &SyntaxNode, expected: Option<&Ty>) -> Ty {
        let arm_tys = self.check_match(node, expected);
        self.unify_branches(&arm_tys, expected, node)
    }

    /// Unifies branch result types into one type, emitting `E2103` on the given node when
    /// they are incompatible. A diverging branch (`Never`) takes any type; an empty set
    /// (no branches) is `Unit`.
    fn unify_branches(&mut self, tys: &[Ty], expected: Option<&Ty>, node: &SyntaxNode) -> Ty {
        if !tys.is_empty() && tys.iter().all(|t| t == &Ty::Never) {
            return Ty::Never;
        }
        let mut acc: Option<Ty> = expected.cloned();
        for t in tys {
            acc = match acc {
                None => Some(t.clone()),
                Some(prev) => match unify(&prev, t) {
                    Some(u) => Some(u),
                    None => {
                        let (expected, actual) = (self.describe(&prev), self.describe(t));
                        let message = format!(
                            "incompatible types across branches: `{expected}` and `{actual}`"
                        );
                        self.diagnostics.push(
                            Diagnostic::error("E2103", node.text_range(), message)
                                .expecting([expected], actual),
                        );
                        Some(prev)
                    }
                },
            };
        }
        acc.unwrap_or(Ty::Unit)
    }

    /// Resolves a type annotation to a [`Ty`], emitting `E2101` for `Float` and `E2103`
    /// for an unknown builtin name.
    fn resolve_annotation(&mut self, path: &TypePath, range: TextRange) -> Ty {
        self.annotation_ty(path.syntax(), range)
    }

    /// Lowers a type annotation node (a `TypePath` or `TupleType`), naming nominal
    /// types by the symbols the resolver bound their heads to.
    fn annotation_ty(&mut self, node: &SyntaxNode, range: TextRange) -> Ty {
        let refs = &self.refs;
        let nominal = |at: TextRange| refs.get(&at).and_then(|r| r.nominal());
        match Ty::from_annotation(node, &nominal) {
            Ok(ty) => ty,
            Err(err) => {
                self.diagnostics
                    .push(Diagnostic::error(err.code(), range, err.message()));
                match err {
                    TypeError::FloatRemoved | TypeError::UnknownType => Ty::Unknown,
                }
            }
        }
    }

    /// Checks a produced type against an expected type: identical, structurally equal
    /// modulo undetermined parts, or a legal widening is accepted (returning the
    /// *expected* type so it flows outward), else the appropriate diagnostic (`E2102`
    /// for an illegal numeric widening, `E2103` for any other mismatch) is emitted and
    /// the expected type is returned to bound error cascades. A diverging `Never`
    /// satisfies any expectation.
    fn check_against(&mut self, produced: Ty, expected: Option<&Ty>, node: &SyntaxNode) -> Ty {
        self.note_percent(&produced, node);
        let Some(target) = expected else {
            return produced;
        };
        if produced == Ty::Never {
            return target.clone();
        }
        if compatible(&produced, target) {
            return merge(target, &produced);
        }
        if produced == Ty::MixedLength && target.is_length_family() {
            let message = format!(
                "a `MixedLength` cannot be typed as `{}` before layout",
                self.describe(target)
            );
            self.diagnostics
                .push(Diagnostic::error("E2106", node.text_range(), message));
            return target.clone();
        }
        if produced.widens_to(target) {
            return target.clone();
        }
        if is_numeric_ty(&produced) && is_numeric_ty(target) {
            match produced.check_implicit_widen(target) {
                Ok(()) => target.clone(),
                Err(WidenError::IllegalImplicit) => {
                    let (expected, actual) = (self.describe(target), self.describe(&produced));
                    let message = format!(
                        "illegal implicit conversion from `{actual}` to `{expected}`; an explicit cast is required"
                    );
                    self.diagnostics.push(
                        Diagnostic::error("E2102", node.text_range(), message)
                            .expecting([expected], actual),
                    );
                    target.clone()
                }
            }
        } else {
            self.emit_mismatch(&produced, target, node.text_range());
            target.clone()
        }
    }

    /// Emits an `E2103` type mismatch.
    fn emit_mismatch(&mut self, produced: &Ty, target: &Ty, range: TextRange) {
        let (expected, actual) = (self.describe(target), self.describe(produced));
        let message = format!("type mismatch: expected `{expected}`, found `{actual}`");
        self.diagnostics
            .push(Diagnostic::error("E2103", range, message).expecting([expected], actual));
    }

    /// A type as source spells it, for diagnostic messages.
    pub(crate) fn describe(&self, ty: &Ty) -> String {
        spell(
            ty,
            &|id| self.env.type_name(id).unwrap_or("<named>").to_string(),
            &|id| self.native_type_name(id),
        )
    }
}

impl TypeSchemas {
    /// A type as source spells it, its nominal types named by these
    /// declarations.
    pub fn describe(&self, ty: &Ty) -> String {
        spell(
            ty,
            &|id| {
                self.names
                    .get(&id)
                    .map_or("<named>", String::as_str)
                    .to_string()
            },
            &|_| "<native>".to_string(),
        )
    }
}

/// A type as source spells it, each nominal type named by `named` and each
/// native type by `native`.
fn spell(
    ty: &Ty,
    named: &dyn Fn(SymbolId) -> String,
    native: &dyn Fn(NativeId) -> String,
) -> String {
    let one = |t: &Ty| spell(t, named, native);
    let list = |tys: &[Ty]| tys.iter().map(one).collect::<Vec<_>>().join(", ");
    match ty {
        Ty::Named(id) => named(*id),
        Ty::Native(id) => native(*id),
        Ty::Tuple(tys) => format!("({})", list(tys)),
        Ty::Fn(params, ret) => format!("fn({}) -> {}", list(params), one(ret)),
        Ty::List(t) => format!("List<{}>", one(t)),
        Ty::Option(t) => format!("Option<{}>", one(t)),
        Ty::Result(t, e) => format!("Result<{}, {}>", one(t), one(e)),
        Ty::Range(t) => format!("Range<{}>", one(t)),
        Ty::RangeInclusive(t) => format!("RangeInclusive<{}>", one(t)),
        _ => ty_name(ty).to_string(),
    }
}

/// Whether two types agree once their undetermined (`Unknown`) parts are ignored.
fn compatible(a: &Ty, b: &Ty) -> bool {
    match (a, b) {
        (Ty::Unknown, _) | (_, Ty::Unknown) => true,
        (Ty::Tuple(xs), Ty::Tuple(ys)) => {
            xs.len() == ys.len() && xs.iter().zip(ys).all(|(x, y)| compatible(x, y))
        }
        (Ty::Fn(xp, xr), Ty::Fn(yp, yr)) => {
            xp.len() == yp.len()
                && xp.iter().zip(yp).all(|(x, y)| compatible(x, y))
                && compatible(xr, yr)
        }
        (Ty::List(x), Ty::List(y))
        | (Ty::Option(x), Ty::Option(y))
        | (Ty::Range(x), Ty::Range(y))
        | (Ty::RangeInclusive(x), Ty::RangeInclusive(y)) => compatible(x, y),
        (Ty::Result(xt, xe), Ty::Result(yt, ye)) => compatible(xt, yt) && compatible(xe, ye),
        _ => a == b,
    }
}

/// `a` with its undetermined parts filled from the compatible `b`.
fn merge(a: &Ty, b: &Ty) -> Ty {
    let boxed = |x: &Ty, y: &Ty| Box::new(merge(x, y));
    match (a, b) {
        (Ty::Unknown, _) => b.clone(),
        (Ty::Tuple(xs), Ty::Tuple(ys)) => {
            Ty::Tuple(xs.iter().zip(ys).map(|(x, y)| merge(x, y)).collect())
        }
        (Ty::Fn(xp, xr), Ty::Fn(yp, yr)) => Ty::Fn(
            xp.iter().zip(yp).map(|(x, y)| merge(x, y)).collect(),
            boxed(xr, yr),
        ),
        (Ty::List(x), Ty::List(y)) => Ty::List(boxed(x, y)),
        (Ty::Option(x), Ty::Option(y)) => Ty::Option(boxed(x, y)),
        (Ty::Range(x), Ty::Range(y)) => Ty::Range(boxed(x, y)),
        (Ty::RangeInclusive(x), Ty::RangeInclusive(y)) => Ty::RangeInclusive(boxed(x, y)),
        (Ty::Result(xt, xe), Ty::Result(yt, ye)) => Ty::Result(boxed(xt, yt), boxed(xe, ye)),
        _ => a.clone(),
    }
}

/// The common type of two values that meet (branch results, range bounds): a
/// diverging side takes the other, numerics unify, and structural types agree
/// modulo undetermined parts.
fn unify(a: &Ty, b: &Ty) -> Option<Ty> {
    match (a, b) {
        (Ty::Never, _) => Some(b.clone()),
        (_, Ty::Never) => Some(a.clone()),
        _ => unify_numeric(a, b).or_else(|| compatible(a, b).then(|| merge(a, b))),
    }
}

/// The builtin `Option`/`Result` constructor a path names, when it names one:
/// `Some`, `None`, `Ok`, `Err`, or their `Option::`/`Result::` qualified forms.
pub(crate) fn builtin_variant(segments: &[SyntaxToken]) -> Option<&'static str> {
    let texts: Vec<String> = segments.iter().map(|t| t.text().to_string()).collect();
    let texts: Vec<&str> = texts.iter().map(String::as_str).collect();
    match texts.as_slice() {
        ["Some"] | ["Option", "Some"] => Some("Some"),
        ["None"] | ["Option", "None"] => Some("None"),
        ["Ok"] | ["Result", "Ok"] => Some("Ok"),
        ["Err"] | ["Result", "Err"] => Some("Err"),
        _ => None,
    }
}

// --- free helpers ------------------------------------------------------------

/// Whether `ty` is one of the integer scalar types.
/// The integer scalar types, as source spells them.
pub(crate) const INTEGER_TYPES: [&str; 8] = ["I8", "I16", "I32", "I64", "U8", "U16", "U32", "U64"];

pub(crate) fn is_integer_ty(ty: &Ty) -> bool {
    matches!(
        ty,
        Ty::I8 | Ty::I16 | Ty::I32 | Ty::I64 | Ty::U8 | Ty::U16 | Ty::U32 | Ty::U64
    )
}

/// Whether `ty` is one of the float scalar types.
pub(crate) fn is_float_ty(ty: &Ty) -> bool {
    matches!(ty, Ty::F32 | Ty::F64)
}

/// Whether `ty` is any numeric scalar (integer or float).
pub(crate) fn is_numeric_ty(ty: &Ty) -> bool {
    is_integer_ty(ty) || is_float_ty(ty)
}

/// The common numeric type of two operand types: the one the other widens to, if either
/// direction is a legal safe widening; `None` when they are not numerically unifiable.
pub(crate) fn unify_numeric(a: &Ty, b: &Ty) -> Option<Ty> {
    if a == b {
        return Some(a.clone());
    }
    if a == &Ty::Unknown {
        return Some(b.clone());
    }
    if b == &Ty::Unknown {
        return Some(a.clone());
    }
    if a.widens_to(b) {
        Some(b.clone())
    } else if b.widens_to(a) {
        Some(a.clone())
    } else {
        None
    }
}

/// A numeric type as an instantiation context for a bare number.
fn numeric(ty: &Ty) -> Option<&Ty> {
    is_numeric_ty(ty).then_some(ty)
}

/// Whether `expr` is a bare number: an unsuffixed literal, or `-`, parentheses or
/// arithmetic over bare numbers. Such a value has no type of its own until context
/// instantiates it.
fn is_bare_number(expr: &Expr) -> bool {
    let node = expr.syntax();
    match node.kind() {
        SyntaxKind::LiteralExpr => bare_literal_token(node).is_some(),
        SyntaxKind::ParenExpr | SyntaxKind::UnaryExpr => {
            (node.kind() == SyntaxKind::ParenExpr || unary_op_kind(node) == Some(SyntaxKind::Minus))
                && first_child_expr(node).is_some_and(|e| is_bare_number(&e))
        }
        SyntaxKind::BinaryExpr => {
            matches!(
                binary_op_kind(node),
                Some(SyntaxKind::Plus | SyntaxKind::Minus | SyntaxKind::Star | SyntaxKind::Slash)
            ) && child_exprs(node).iter().all(is_bare_number)
        }
        _ => false,
    }
}

/// The value of a bare number, or `None` when it is not one or does not fold
/// (a division by zero inside it).
fn bare_value(expr: &Expr) -> Option<f64> {
    let node = expr.syntax();
    match node.kind() {
        SyntaxKind::LiteralExpr => {
            let tok = bare_literal_token(node)?;
            match tok.kind() {
                SyntaxKind::IntLiteral => parse_int_literal(&tok.text()).map(|v| v as f64),
                _ => parse_float_literal(&tok.text()),
            }
        }
        SyntaxKind::ParenExpr => bare_value(&first_child_expr(node)?),
        SyntaxKind::UnaryExpr if unary_op_kind(node) == Some(SyntaxKind::Minus) => {
            bare_value(&first_child_expr(node)?).map(|v| -v)
        }
        SyntaxKind::BinaryExpr => {
            let operands = child_exprs(node);
            let [lhs, rhs] = operands.as_slice() else {
                return None;
            };
            let (a, b) = (bare_value(lhs)?, bare_value(rhs)?);
            match binary_op_kind(node)? {
                SyntaxKind::Plus => Some(a + b),
                SyntaxKind::Minus => Some(a - b),
                SyntaxKind::Star => Some(a * b),
                SyntaxKind::Slash if b != 0.0 => Some(a / b),
                _ => None,
            }
        }
        _ => None,
    }
}

/// The literal token of an unsuffixed numeric literal expression.
fn bare_literal_token(node: &SyntaxNode) -> Option<SyntaxToken> {
    node.children_with_tokens()
        .into_iter()
        .find_map(|e| e.as_token().cloned())
        .filter(|t| matches!(t.kind(), SyntaxKind::IntLiteral | SyntaxKind::FloatLiteral))
}

/// Splits a suffixed literal into its numeric body and the type its suffix names
/// (§19.1). `None` for a suffix outside the closed set.
pub(crate) fn split_unit_literal(text: &str) -> Option<(&str, Ty)> {
    const SUFFIXES: [(&str, Ty); 25] = [
        ("dp", Ty::Dp),
        ("px", Ty::Px),
        ("sp", Ty::Sp),
        ("em", Ty::Em),
        ("%", Ty::Percent),
        ("ns", Ty::Duration),
        ("us", Ty::Duration),
        ("ms", Ty::Duration),
        ("s", Ty::Duration),
        ("min", Ty::Duration),
        ("deg", Ty::Angle),
        ("rad", Ty::Angle),
        ("turn", Ty::Angle),
        ("hz", Ty::Frequency),
        ("khz", Ty::Frequency),
        ("i8", Ty::I8),
        ("i16", Ty::I16),
        ("i32", Ty::I32),
        ("i64", Ty::I64),
        ("u8", Ty::U8),
        ("u16", Ty::U16),
        ("u32", Ty::U32),
        ("u64", Ty::U64),
        ("f32", Ty::F32),
        ("f64", Ty::F64),
    ];
    // The longest matching suffix wins, so `ms` is not read as `s`.
    SUFFIXES
        .iter()
        .filter(|(suffix, _)| text.len() > suffix.len() && text.ends_with(suffix))
        .max_by_key(|(suffix, _)| suffix.len())
        .map(|(suffix, ty)| (&text[..text.len() - suffix.len()], ty.clone()))
}

/// The factor a dimension suffix scales its number by to the dimension's base
/// unit (seconds, degrees, hertz); 1 for a base unit or a non-dimension suffix.
pub(crate) fn unit_scale(suffix: &str) -> f64 {
    match suffix {
        "ns" => 1e-9,
        "us" => 1e-6,
        "ms" => 1e-3,
        "min" => 60.0,
        "rad" => 180.0 / std::f64::consts::PI,
        "turn" => 360.0,
        "khz" => 1000.0,
        _ => 1.0,
    }
}

/// Whether `value` is representable by `ty`; any value fits a type that is no
/// integer scalar.
fn int_fits(value: i128, ty: &Ty) -> bool {
    int_bounds(ty).is_none_or(|(lo, hi)| value >= lo && value <= hi)
}

/// The inclusive integer range representable by an integer scalar type, as `i128` so both
/// signed and unsigned widths fit.
fn int_bounds(ty: &Ty) -> Option<(i128, i128)> {
    let bounds = match ty {
        Ty::I8 => (i8::MIN as i128, i8::MAX as i128),
        Ty::I16 => (i16::MIN as i128, i16::MAX as i128),
        Ty::I32 => (i32::MIN as i128, i32::MAX as i128),
        Ty::I64 => (i64::MIN as i128, i64::MAX as i128),
        Ty::U8 => (0, u8::MAX as i128),
        Ty::U16 => (0, u16::MAX as i128),
        Ty::U32 => (0, u32::MAX as i128),
        Ty::U64 => (0, u64::MAX as i128),
        _ => return None,
    };
    Some(bounds)
}

/// Parses an integer literal's decimal/hex/octal/binary digits into an `i128`, stripping a
/// trailing type suffix and `_` separators. `None` if it does not fit `i128` (a value that
/// large is out of range for every scalar anyway).
pub(crate) fn parse_int_literal(text: &str) -> Option<i128> {
    // Strip a type suffix: digits/`0x`.. body then optional `I32`/`u8`/... — split at the
    // first ASCII letter that is not part of a radix prefix.
    let body = strip_int_suffix(text);
    let cleaned: String = body.chars().filter(|c| *c != '_').collect();
    let (radix, digits) = if let Some(rest) = cleaned
        .strip_prefix("0x")
        .or_else(|| cleaned.strip_prefix("0X"))
    {
        (16, rest)
    } else if let Some(rest) = cleaned
        .strip_prefix("0o")
        .or_else(|| cleaned.strip_prefix("0O"))
    {
        (8, rest)
    } else if let Some(rest) = cleaned
        .strip_prefix("0b")
        .or_else(|| cleaned.strip_prefix("0B"))
    {
        (2, rest)
    } else {
        (10, cleaned.as_str())
    };
    i128::from_str_radix(digits, radix).ok()
}

/// Splits an integer literal's numeric body from a trailing type suffix (`100i32`).
fn strip_int_suffix(text: &str) -> &str {
    // A suffix begins at the first letter after the digits that is not a radix marker in
    // position 1 (`x`/`o`/`b` right after a leading `0`).
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i] as char;
        let is_radix_marker = i == 1
            && bytes.first() == Some(&b'0')
            && matches!(c, 'x' | 'X' | 'o' | 'O' | 'b' | 'B');
        if c.is_ascii_alphabetic() && !is_radix_marker && c != '_' {
            // For hex, digits a–f are part of the number; only stop at a non-hex letter
            // when we are in hex. Simplify: a suffix always starts with `i`/`u`/`f`.
            if matches!(c, 'i' | 'u' | 'f' | 'I' | 'U' | 'F') && i >= 1 {
                return &text[..i];
            }
        }
        i += 1;
    }
    text
}

/// Parses a float literal's body into an `f64`, stripping a type suffix and `_`.
pub(crate) fn parse_float_literal(text: &str) -> Option<f64> {
    let body = match text.find(['f', 'F']) {
        Some(idx) if idx > 0 => &text[..idx],
        _ => text,
    };
    let cleaned: String = body.chars().filter(|c| *c != '_').collect();
    cleaned.parse::<f64>().ok()
}

/// A one-word name for a type, for diagnostic messages.
fn ty_name(ty: &Ty) -> &'static str {
    match ty {
        Ty::Bool => "Bool",
        Ty::I8 => "I8",
        Ty::I16 => "I16",
        Ty::I32 => "I32",
        Ty::I64 => "I64",
        Ty::U8 => "U8",
        Ty::U16 => "U16",
        Ty::U32 => "U32",
        Ty::U64 => "U64",
        Ty::F32 => "F32",
        Ty::F64 => "F64",
        Ty::Char => "Char",
        Ty::String => "String",
        Ty::Bytes => "Bytes",
        Ty::Unit => "Unit",
        Ty::Never => "Never",
        Ty::Color => "Color",
        Ty::Dp => "Dp",
        Ty::Px => "Px",
        Ty::Sp => "Sp",
        Ty::Em => "Em",
        Ty::Percent => "Percent",
        Ty::MixedLength => "MixedLength",
        Ty::Duration => "Duration",
        Ty::Angle => "Angle",
        Ty::Frequency => "Frequency",
        Ty::Named(_) => "<named>",
        Ty::Native(_) => "<native>",
        Ty::Tuple(_) => "<tuple>",
        Ty::Fn(_, _) => "<fn>",
        Ty::List(_) => "<list>",
        Ty::Option(_) => "<option>",
        Ty::Result(_, _) => "<result>",
        Ty::Range(_) => "<range>",
        Ty::RangeInclusive(_) => "<range-inclusive>",
        Ty::InferInt => "<int>",
        Ty::InferFloat => "<float>",
        Ty::Unknown => "<unknown>",
    }
}

/// The direct `Expr` children of a node, in order.
pub(crate) fn child_exprs(node: &SyntaxNode) -> Vec<Expr> {
    node.children().into_iter().filter_map(Expr::cast).collect()
}

/// The first direct `Expr` child of a node.
pub(crate) fn first_child_expr(node: &SyntaxNode) -> Option<Expr> {
    node.children().into_iter().find_map(Expr::cast)
}

/// The `..base` of a record literal: the value of its spread field.
pub(crate) fn record_spread(node: &SyntaxNode) -> Option<Expr> {
    node.children()
        .into_iter()
        .filter(is_spread)
        .find_map(|f| first_child_expr(&f))
}

/// Whether a `RecordExprField` is the literal's `..base` spread.
pub(crate) fn is_spread(field: &SyntaxNode) -> bool {
    field.kind() == SyntaxKind::RecordExprField
        && field
            .children_with_tokens()
            .into_iter()
            .any(|e| e.as_token().is_some_and(|t| t.kind() == SyntaxKind::DotDot))
}

/// The argument expressions of a call. Arguments live in the call's `ArgumentList`
/// child, each wrapped in an `Argument` node (which, for a named argument, holds a
/// leading `ident :` before the value expression); we project each `Argument`'s value
/// expression in order.
pub(crate) fn call_args(node: &SyntaxNode) -> Vec<Expr> {
    let mut args = Vec::new();
    for child in node.children() {
        if child.kind() == SyntaxKind::ArgumentList {
            for arg in child.children() {
                if arg.kind() == SyntaxKind::Argument
                    && let Some(e) = first_child_expr(&arg)
                {
                    args.push(e);
                }
            }
        }
    }
    args
}

/// The operator token kind of a binary expression (the operator token between operands).
pub(crate) fn binary_op_kind(node: &SyntaxNode) -> Option<SyntaxKind> {
    node.children_with_tokens()
        .into_iter()
        .filter_map(|e| e.as_token().map(|t| t.kind()))
        .find(|k| is_binary_op(*k))
}

/// The operator token kind of a unary expression.
pub(crate) fn unary_op_kind(node: &SyntaxNode) -> Option<SyntaxKind> {
    node.children_with_tokens()
        .into_iter()
        .filter_map(|e| e.as_token().map(|t| t.kind()))
        .find(|k| matches!(k, SyntaxKind::Minus | SyntaxKind::Bang | SyntaxKind::Tilde))
}

/// Whether a token kind is an assignment operator (`=` or an augmenting one).
pub(crate) fn is_assign_op(k: SyntaxKind) -> bool {
    matches!(
        k,
        SyntaxKind::Eq
            | SyntaxKind::PlusEq
            | SyntaxKind::MinusEq
            | SyntaxKind::StarEq
            | SyntaxKind::SlashEq
            | SyntaxKind::PercentEq
            | SyntaxKind::AmpEq
            | SyntaxKind::PipeEq
            | SyntaxKind::CaretEq
            | SyntaxKind::ShlEq
            | SyntaxKind::ShrEq
    )
}

/// Whether a token kind is a binary operator.
fn is_binary_op(k: SyntaxKind) -> bool {
    matches!(
        k,
        SyntaxKind::Plus
            | SyntaxKind::Minus
            | SyntaxKind::Star
            | SyntaxKind::Slash
            | SyntaxKind::Percent
            | SyntaxKind::Amp
            | SyntaxKind::Pipe
            | SyntaxKind::Caret
            | SyntaxKind::Shl
            | SyntaxKind::Shr
            | SyntaxKind::EqEq
            | SyntaxKind::Neq
            | SyntaxKind::Lt
            | SyntaxKind::Le
            | SyntaxKind::Gt
            | SyntaxKind::Ge
            | SyntaxKind::AmpAmp
            | SyntaxKind::PipePipe
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolve::SymbolId;
    use crate::syntax::{SyntaxNode, tokenize};
    use std::collections::HashMap;

    /// A stub environment: it answers `resolution_ty` / `callee_signature` from two
    /// maps keyed by `SymbolId`, so inference can be exercised without the real
    /// symbol/HIR tables. Path tests wire a name's head-token span to a `SymbolId`
    /// through the `ResolvedRef` index, and register that id's type/signature here.
    #[derive(Default)]
    struct StubEnv {
        tys: HashMap<SymbolId, Ty>,
        sigs: HashMap<SymbolId, (Vec<Ty>, Ty)>,
    }

    impl TypeEnv for StubEnv {
        fn resolution_ty(&self, to: &Resolution) -> Option<Ty> {
            match to {
                Resolution::Symbol(id) => self.tys.get(id).cloned(),
                Resolution::Local(_) | Resolution::Native(_) | Resolution::Env => None,
            }
        }

        fn callee_signature(&self, to: &Resolution) -> Option<(Vec<Ty>, Ty)> {
            match to {
                Resolution::Symbol(id) => self.sigs.get(id).cloned(),
                Resolution::Local(_) | Resolution::Native(_) | Resolution::Env => None,
            }
        }
    }

    /// Parses a bare expression fragment and returns its typed `Expr` view plus the
    /// syntax root (kept alive for the borrow). The fragment entry roots at an
    /// `ExprStmt` whose sole child expression is the one under test.
    fn parse_fragment(src: &str) -> (SyntaxNode, Expr) {
        let parse = crate::syntax::grammar::parse_expr(&tokenize(src), src);
        let root = SyntaxNode::new_root(parse.root);
        // The `Expr` entry roots at `ExprStmt`; the value is its first `Expr` child.
        let expr = root
            .descendants()
            .into_iter()
            .find_map(Expr::cast)
            .expect("fragment parses to an expression");
        (root, expr)
    }

    /// The span of the first identifier token whose text equals `name`, used to wire a
    /// path use to a stub resolution.
    fn ident_range(root: &SyntaxNode, name: &str) -> TextRange {
        root.descendants_with_tokens()
            .into_iter()
            .filter_map(|e| e.as_token().cloned())
            .find(|t| t.kind() == SyntaxKind::Ident && t.text() == name)
            .map(|t| t.text_range())
            .unwrap_or_else(|| panic!("no identifier token `{name}`"))
    }

    /// Infers a fragment with no environment bindings and no expected type.
    fn infer_bare(src: &str) -> (Ty, Vec<Diagnostic>) {
        let (_root, expr) = parse_fragment(src);
        let env = StubEnv::default();
        let mut cx = InferCx::new(&[], &env);
        let ty = cx.infer_expr(&expr, None);
        (ty, cx.into_diagnostics())
    }

    /// Infers a fragment against an expected type, with no environment bindings.
    fn infer_expecting(src: &str, expected: &Ty) -> (Ty, Vec<Diagnostic>) {
        let (_root, expr) = parse_fragment(src);
        let env = StubEnv::default();
        let mut cx = InferCx::new(&[], &env);
        let ty = cx.infer_expr(&expr, Some(expected));
        (ty, cx.into_diagnostics())
    }

    fn codes(diags: &[Diagnostic]) -> Vec<&str> {
        diags.iter().map(|d| d.code).collect()
    }

    // --- literal typing (host default vs expected) --------------------------

    #[test]
    fn integer_literal_defaults_to_i64_without_context() {
        let (ty, diags) = infer_bare("1");
        assert_eq!(ty, Ty::I64);
        assert!(diags.is_empty());
    }

    #[test]
    fn float_literal_defaults_to_f64_without_context() {
        let (ty, diags) = infer_bare("1.0");
        assert_eq!(ty, Ty::F64);
        assert!(diags.is_empty());
    }

    #[test]
    fn integer_literal_instantiates_at_expected_int() {
        let (ty, diags) = infer_expecting("1", &Ty::U8);
        assert_eq!(ty, Ty::U8);
        assert!(diags.is_empty());
    }

    #[test]
    fn integer_literal_instantiates_at_expected_float() {
        // `1` where `F32` is expected is a legal literal instantiation, not a
        // conversion — no diagnostic.
        let (ty, diags) = infer_expecting("1", &Ty::F32);
        assert_eq!(ty, Ty::F32);
        assert!(diags.is_empty());
    }

    #[test]
    fn integer_literal_out_of_range_is_e2103() {
        let (ty, diags) = infer_expecting("300", &Ty::U8);
        assert_eq!(ty, Ty::U8);
        assert_eq!(codes(&diags), ["E2103"]);
    }

    #[test]
    fn float_literal_instantiates_at_expected_f32() {
        let (ty, diags) = infer_expecting("1.5", &Ty::F32);
        assert_eq!(ty, Ty::F32);
        assert!(diags.is_empty());
    }

    #[test]
    fn float_literal_in_integer_slot_is_e2103() {
        let (ty, diags) = infer_expecting("1.5", &Ty::I32);
        assert_eq!(ty, Ty::I32);
        assert_eq!(codes(&diags), ["E2103"]);
    }

    #[test]
    fn bool_and_string_literals_type_directly() {
        assert_eq!(infer_bare("true").0, Ty::Bool);
        assert_eq!(infer_bare("\"x\"").0, Ty::String);
    }

    // --- widening / conversion ---------------------------------------------

    #[test]
    fn safe_widening_is_accepted() {
        // An `I8`-typed name widens to an `I64` slot without a diagnostic.
        let (root, expr) = parse_fragment("small");
        let mut env = StubEnv::default();
        let id = SymbolId::from_parts(1, 0);
        env.tys.insert(id, Ty::I8);
        let refs = [ResolvedRef {
            range: ident_range(&root, "small"),
            to: Resolution::Symbol(id),
        }];
        let mut cx = InferCx::new(&refs, &env);
        let ty = cx.infer_expr(&expr, Some(&Ty::I64));
        assert_eq!(ty, Ty::I64);
        assert!(cx.into_diagnostics().is_empty());
    }

    #[test]
    fn illegal_implicit_conversion_is_e2102() {
        // A `U32`-typed name in an `I64` slot crosses the signed/unsigned boundary.
        let (root, expr) = parse_fragment("n");
        let mut env = StubEnv::default();
        let id = SymbolId::from_parts(2, 0);
        env.tys.insert(id, Ty::U32);
        let refs = [ResolvedRef {
            range: ident_range(&root, "n"),
            to: Resolution::Symbol(id),
        }];
        let mut cx = InferCx::new(&refs, &env);
        let ty = cx.infer_expr(&expr, Some(&Ty::I64));
        assert_eq!(ty, Ty::I64);
        assert_eq!(codes(cx.diagnostics()), ["E2102"]);
    }

    #[test]
    fn non_numeric_mismatch_is_e2103() {
        let (root, expr) = parse_fragment("flag");
        let mut env = StubEnv::default();
        let id = SymbolId::from_parts(3, 0);
        env.tys.insert(id, Ty::Bool);
        let refs = [ResolvedRef {
            range: ident_range(&root, "flag"),
            to: Resolution::Symbol(id),
        }];
        let mut cx = InferCx::new(&refs, &env);
        let ty = cx.infer_expr(&expr, Some(&Ty::String));
        assert_eq!(ty, Ty::String);
        assert_eq!(codes(cx.diagnostics()), ["E2103"]);
    }

    // --- annotations --------------------------------------------------------

    #[test]
    fn float_annotation_on_cast_is_e2101() {
        let (ty, diags) = infer_bare("1 as Float");
        assert_eq!(ty, Ty::Unknown);
        assert_eq!(codes(&diags), ["E2101"]);
    }

    #[test]
    fn cast_target_is_the_result_type() {
        let (ty, diags) = infer_bare("x as I32");
        assert_eq!(ty, Ty::I32);
        assert!(diags.is_empty());
    }

    // --- operators ----------------------------------------------------------

    #[test]
    fn comparison_yields_bool() {
        assert_eq!(infer_bare("1 < 2").0, Ty::Bool);
        assert_eq!(infer_bare("true && false").0, Ty::Bool);
    }

    #[test]
    fn arithmetic_unifies_operand_widths() {
        // Two default-int literals stay `I64`.
        assert_eq!(infer_bare("1 + 2").0, Ty::I64);
    }

    #[test]
    fn logical_not_yields_bool() {
        assert_eq!(infer_bare("!true").0, Ty::Bool);
    }

    // --- if / match unification --------------------------------------------

    #[test]
    fn if_branches_unify_to_common_type() {
        let (ty, diags) = infer_bare("if true { 1 } else { 2 }");
        assert_eq!(ty, Ty::I64);
        assert!(diags.is_empty());
    }

    #[test]
    fn if_branches_of_incompatible_types_are_e2103() {
        let (_ty, diags) = infer_bare("if true { 1 } else { \"x\" }");
        assert!(codes(&diags).contains(&"E2103"));
    }

    // --- calls --------------------------------------------------------------

    #[test]
    fn call_returns_declared_return_and_checks_args() {
        // `f(1)` with signature `(I32) -> Bool`: the literal instantiates at `I32`,
        // the call yields `Bool`, and no diagnostic fires.
        let (root, expr) = parse_fragment("f(1)");
        let mut env = StubEnv::default();
        let id = SymbolId::from_parts(4, 0);
        env.sigs.insert(id, (vec![Ty::I32], Ty::Bool));
        let refs = [ResolvedRef {
            range: ident_range(&root, "f"),
            to: Resolution::Symbol(id),
        }];
        let mut cx = InferCx::new(&refs, &env);
        let ty = cx.infer_expr(&expr, None);
        assert_eq!(ty, Ty::Bool);
        assert!(cx.into_diagnostics().is_empty());
    }

    #[test]
    fn call_argument_type_mismatch_is_reported() {
        // `g(true)` with signature `(I32) -> Unit`: a `Bool` in an `I32` slot.
        let (root, expr) = parse_fragment("g(true)");
        let mut env = StubEnv::default();
        let id = SymbolId::from_parts(5, 0);
        env.sigs.insert(id, (vec![Ty::I32], Ty::Unit));
        let refs = [ResolvedRef {
            range: ident_range(&root, "g"),
            to: Resolution::Symbol(id),
        }];
        let mut cx = InferCx::new(&refs, &env);
        let ty = cx.infer_expr(&expr, None);
        assert_eq!(ty, Ty::Unit);
        assert_eq!(codes(cx.diagnostics()), ["E2103"]);
    }

    // --- dimensional arithmetic (§19.3) -------------------------------------

    /// Infers a fragment where `name` is bound to a symbol of type `ty`.
    fn infer_bound(src: &str, name: &str, ty: Ty, expected: Option<&Ty>) -> (Ty, Vec<Diagnostic>) {
        let (root, expr) = parse_fragment(src);
        let mut env = StubEnv::default();
        let id = SymbolId::from_parts(9, 0);
        env.tys.insert(id, ty);
        let refs = [ResolvedRef {
            range: ident_range(&root, name),
            to: Resolution::Symbol(id),
        }];
        let mut cx = InferCx::new(&refs, &env);
        let ty = cx.infer_expr(&expr, expected);
        (ty, cx.into_diagnostics())
    }

    #[test]
    fn unit_literals_type_by_suffix() {
        for (src, ty) in [
            ("16dp", Ty::Dp),
            ("1px", Ty::Px),
            ("14sp", Ty::Sp),
            ("1.5em", Ty::Em),
            ("50%", Ty::Percent),
            ("250ms", Ty::Duration),
            ("5min", Ty::Duration),
            ("90deg", Ty::Angle),
            ("60hz", Ty::Frequency),
            ("2khz", Ty::Frequency),
            ("2u8", Ty::U8),
            ("1.5f32", Ty::F32),
        ] {
            let (got, diags) = infer_bare(src);
            assert_eq!(got, ty, "{src}");
            assert!(diags.is_empty(), "{src}: {diags:?}");
        }
    }

    #[test]
    fn a_suffixed_integer_is_range_checked() {
        let (ty, diags) = infer_bare("300u8");
        assert_eq!(ty, Ty::U8);
        assert_eq!(codes(&diags), ["E2103"]);
    }

    #[test]
    fn same_dimension_arithmetic_keeps_the_dimension() {
        for (src, ty) in [
            ("1s + 250ms", Ty::Duration),
            ("16dp * 2", Ty::Dp),
            ("2 * 16dp", Ty::Dp),
            ("16dp / 2", Ty::Dp),
            ("-(8dp)", Ty::Dp),
            ("100dp / 50dp", Ty::F64),
        ] {
            let (got, diags) = infer_bare(src);
            assert_eq!(got, ty, "{src}");
            assert!(diags.is_empty(), "{src}: {diags:?}");
        }
    }

    #[test]
    fn mixing_length_units_gives_mixed_length() {
        let (ty, diags) = infer_bound("100% - 2 * inset", "inset", Ty::Dp, None);
        assert_eq!(ty, Ty::MixedLength);
        assert!(diags.is_empty(), "{diags:?}");
        let (ty, diags) = infer_bare("(100% - 8dp) / 2");
        assert_eq!(ty, Ty::MixedLength);
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn a_length_widens_to_mixed_length() {
        let (ty, diags) = infer_expecting("8dp", &Ty::MixedLength);
        assert_eq!(ty, Ty::MixedLength);
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn a_mixed_length_cannot_take_a_concrete_unit() {
        let (_, diags) = infer_expecting("10dp + 5px", &Ty::Dp);
        assert_eq!(codes(&diags), ["E2106"]);
    }

    #[test]
    fn illegal_dimensional_operations_are_reported() {
        for (src, code) in [
            ("1s + 2dp", "E2103"),
            ("10dp < 5px", "E2103"),
            ("(100% - 8dp) < 200dp", "E2107"),
            ("50% + 0.5", "E2107"),
            ("2dp * 3dp", "E2107"),
            ("10dp % 3dp", "E2107"),
            ("1 / 2dp", "E2107"),
            ("8dp / 0", "E2109"),
            ("8dp / (1 - 1)", "E2109"),
        ] {
            let (_, diags) = infer_bare(src);
            assert_eq!(codes(&diags), [code], "{src}");
        }
    }

    #[test]
    fn a_typed_integer_is_not_a_scale_factor() {
        let (_, diags) = infer_bound("16dp * k", "k", Ty::I64, None);
        assert_eq!(codes(&diags), ["E2107"]);
        let (ty, diags) = infer_bound("16dp * k", "k", Ty::F32, None);
        assert_eq!(ty, Ty::Dp);
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn a_bare_number_instantiates_at_the_typed_operand() {
        let (ty, diags) = infer_bound("1 + small", "small", Ty::I8, None);
        assert_eq!(ty, Ty::I8);
        assert!(diags.is_empty(), "{diags:?}");
        let (ty, diags) = infer_expecting("1 + 2", &Ty::U8);
        assert_eq!(ty, Ty::U8);
        assert!(diags.is_empty(), "{diags:?}");
    }

    // --- malformed input does not panic ------------------------------------

    #[test]
    fn malformed_input_does_not_panic() {
        // A truncated expression parses with recovery; inference must not panic.
        for src in ["1 +", "if", "f(", "(", "1 as"] {
            let _ = infer_bare(src);
        }
    }
}
