//! Typed game input: `@derive` checks on enums, the package's `InputMap`
//! constant evaluated at compile time into the module's input schema, and the
//! target-device coverage of its actions.
//!
//! A package declares at most one `const NAME: InputMap<E> = ...`. Its `E`
//! derives `InputAction` (or is `viso::game::InputAction` itself) and is the
//! type every input action of the package has, so `frame.input.pressed(..)`
//! takes an `E`. A package without a map reads the default `InputAction` set.

use std::collections::HashSet;

use viso_behavior::Value;
use viso_behavior::game::{
    INPUT_ACTION_DERIVE, InputAction, InputMap, InputSchema, PadButton, TouchButton,
};
use viso_behavior::native::{NativeCx, NativeId, NativeObject, Services};

use crate::ast::{AstNode, CallExpr, ConstDecl, EnumDecl, Expr, FieldExpr, PathExpr};
use crate::diag::{Diagnostic, Related, Severity};
use crate::hir::infer::body::emit_args;
use crate::hir::infer::{InferCx, TypeEnv, VariantPayload, parse_float_literal, parse_int_literal};
use crate::resolve::suggest::{Candidate, attach, nearest};
use crate::resolve::{Resolution, ResolvedRef, SymbolId};
use crate::syntax::{SyntaxKind, SyntaxNode, TextRange};

use super::{Declarations, ModuleEnv, ModuleScope, Ty};

/// The input devices the package's targets have, which every action needs a
/// binding for (`E9107`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct InputDevices {
    /// A target has gamepads.
    pub gamepad: bool,
    /// A target has a touch screen.
    pub touch: bool,
}

/// The derives the compiler itself defines.
const STANDARD_DERIVES: [&str; 4] = ["Eq", "Hash", "StableKey", "Snapshot"];

/// One `InputMap` constant of the package.
pub(super) struct MapDecl {
    /// The constant.
    symbol: SymbolId,
    /// The module declaring it.
    module: usize,
    /// Its name.
    at: TextRange,
    /// The action type its annotation names, and where.
    action: Option<(Ty, TextRange)>,
}

/// The identity of `viso::game::InputMap`.
fn input_map() -> NativeId {
    NativeId::of(InputMap::PATH)
}

/// The identity of `viso::game::InputAction`.
fn default_action() -> NativeId {
    NativeId::of(InputAction::PATH)
}

/// Records `decl` if it is an `InputMap` constant, with the action type its
/// annotation names.
pub(super) fn collect_map(
    decl: &ConstDecl,
    symbol: SymbolId,
    ty: &Ty,
    scope: &ModuleScope,
    decls: &mut Declarations,
) {
    if *ty != Ty::Native(input_map()) {
        return;
    }
    let (Some(module), Some(name)) = (scope.home, decl.name()) else {
        return;
    };
    let action = decl
        .syntax()
        .children()
        .into_iter()
        .find(|c| c.kind() == SyntaxKind::TypePath)
        .and_then(|path| {
            path.children()
                .into_iter()
                .rfind(|c| c.kind() == SyntaxKind::TypePathSegment)
        })
        .and_then(|segment| {
            segment
                .children()
                .into_iter()
                .find(|c| c.kind() == SyntaxKind::GenericArgs)
        })
        .and_then(|args| {
            args.children()
                .into_iter()
                .find(|c| c.kind() == SyntaxKind::TypePath)
        })
        .map(|arg| (scope.annotation(&arg), arg.text_range()));
    decls.input_maps.push(MapDecl {
        symbol,
        module,
        at: name.text_range(),
        action,
    });
}

/// Whether `ty` is a type input actions may have: an enum deriving
/// `InputAction`, or `InputAction` itself.
fn is_action_type(ty: &Ty, decls: &Declarations) -> bool {
    match ty {
        Ty::Named(e) => decls.input_derives.contains(e),
        Ty::Native(id) => *id == default_action(),
        _ => false,
    }
}

/// The type of the package's input actions: the action type of its first
/// `InputMap`, unknown when that names no action type (so its uses raise no
/// further errors), or `InputAction` when it declares none.
pub(super) fn action_type(decls: &Declarations) -> Ty {
    match decls.input_maps.first() {
        Some(map) => match &map.action {
            Some((ty, _)) if is_action_type(ty, decls) => ty.clone(),
            _ => Ty::Unknown,
        },
        None => Ty::Native(default_action()),
    }
}

/// Checks the `@derive(..)` attributes of `decl`: each names a standard
/// derive or one a native library declares (`E2001` otherwise), and a schema
/// derive applies only to an enum whose variants carry no payload (`E2201`).
pub(super) fn check_derives(
    decl: &EnumDecl,
    env: &ModuleEnv<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let symbol = env.scope.declared.get(&decl.syntax().text_range()).copied();
    for (attr, names) in decl.derives() {
        if names.is_empty() {
            diagnostics.push(Diagnostic::error(
                "E2001",
                attr.text_range(),
                "`@derive` names the traits to derive",
            ));
        }
        for name in names {
            let Some(name) = name else {
                diagnostics.push(Diagnostic::error(
                    "E2001",
                    attr.text_range(),
                    "each argument of `@derive` is the name of a trait",
                ));
                continue;
            };
            let text = name.text();
            if STANDARD_DERIVES.contains(&text.as_str()) {
                continue;
            }
            if env.natives.derive(&text).is_none() {
                let schema = env
                    .natives
                    .libraries()
                    .iter()
                    .flat_map(|l| l.derives.iter());
                let candidates = STANDARD_DERIVES
                    .iter()
                    .chain(schema)
                    .map(|&name| Candidate {
                        name,
                        declared_at: None,
                    });
                let mut diagnostic = Diagnostic::error(
                    "E2001",
                    name.text_range(),
                    format!("there is no derive `{text}`"),
                );
                attach(
                    &mut diagnostic,
                    name.text_range(),
                    &nearest(&text, candidates),
                );
                diagnostics.push(diagnostic);
                continue;
            }
            let payload = symbol
                .and_then(|s| env.enum_variants(s))
                .and_then(|vs| vs.iter().find(|v| v.payload != VariantPayload::Unit));
            if let Some(variant) = payload {
                let mut diagnostic = Diagnostic::error(
                    "E2201",
                    name.text_range(),
                    format!("`{text}` derives only for an enum whose variants carry no payload"),
                );
                diagnostic.related.push(Related::new(
                    variant.declared_at,
                    "this variant carries a payload",
                ));
                diagnostics.push(diagnostic);
            }
        }
    }
}

/// Lowers `decl` if it is an `InputMap` constant. The first of the package
/// becomes its input schema: its action type must derive `InputAction`
/// (`E2201`), its value evaluates at compile time from literals, enum
/// variants and `@const` natives (`E2501` for anything else, `E2112` when a
/// native rejects its arguments), and each action needs a binding on every
/// device the targets have (`E9107`, a warning). Any later one is `E2202`.
pub(super) fn lower_map(
    decl: &ConstDecl,
    refs: &[ResolvedRef],
    env: &ModuleEnv<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Some(symbol) = env.scope.declared.get(&decl.syntax().text_range()).copied() else {
        return;
    };
    let maps = &env.decls.input_maps;
    let Some(index) = maps.iter().position(|m| m.symbol == symbol) else {
        return;
    };
    let map = &maps[index];
    if index > 0 {
        let first = &maps[0];
        let mut diagnostic = Diagnostic::error(
            "E2202",
            map.at,
            "the package already declares its input map: one `InputMap` maps every action",
        );
        if first.module == map.module {
            diagnostic
                .related
                .push(Related::new(first.at, "the input map is declared here"));
        }
        diagnostics.push(diagnostic);
        return;
    }
    let Some((action, action_at)) = &map.action else {
        diagnostics.push(Diagnostic::error(
            "E2201",
            map.at,
            "an `InputMap` names the enum it maps: `InputMap<Action>`",
        ));
        return;
    };
    if !is_action_type(action, env.decls) {
        if !action.has_unknown() {
            let mut diagnostic = Diagnostic::error(
                "E2201",
                *action_at,
                "an `InputMap` maps an enum that derives `InputAction`",
            );
            diagnostic
                .notes
                .push(format!("add `@derive({INPUT_ACTION_DERIVE})` to the enum"));
            diagnostics.push(diagnostic);
        }
        return;
    }
    let (name, actions): (String, Vec<String>) = match action {
        Ty::Named(e) => (
            env.type_name(*e).unwrap_or_default().to_owned(),
            env.enum_variants(*e)
                .unwrap_or_default()
                .iter()
                .map(|v| v.name.clone())
                .collect(),
        ),
        _ => (
            "InputAction".to_owned(),
            InputAction::VARIANTS
                .iter()
                .map(|&v| v.to_owned())
                .collect(),
        ),
    };
    if actions.is_empty() {
        diagnostics.push(Diagnostic::error(
            "E2201",
            *action_at,
            format!("`{name}` declares no action to map"),
        ));
        return;
    }
    let Some(value) = decl.value() else {
        return;
    };
    let mut cx = InferCx::new(refs, env);
    let ty = cx.infer_promoted(&value, &Ty::Native(input_map()));
    if ty.has_unknown()
        || cx
            .diagnostics()
            .iter()
            .any(|d| d.severity == Severity::Error)
    {
        // `check_const` reported the value's errors.
        return;
    }
    let mut services = Services::default();
    let mut eval = ConstEval {
        cx: &cx,
        env,
        services: &mut services,
        diagnostics: Vec::new(),
    };
    let evaluated = eval.expr(&value);
    let errors = eval.diagnostics;
    if !errors.is_empty() {
        diagnostics.extend(errors);
        return;
    }
    let Some(Value::Handle(handle)) = evaluated else {
        return;
    };
    let Some(map_value) = handle.get::<InputMap>() else {
        return;
    };
    let bindings = map_value.bindings.clone();
    check_devices(
        &name,
        &actions,
        &bindings,
        map.at,
        env.decls.profile.devices,
        diagnostics,
    );
    env.behavior.borrow_mut().input(InputSchema {
        name: name.into(),
        actions: actions.into_iter().map(Into::into).collect(),
        bindings,
    });
}

/// `E9107` for each action without a binding on a device the targets have.
fn check_devices(
    name: &str,
    actions: &[String],
    bindings: &viso_behavior::game::InputBindings,
    at: TextRange,
    devices: InputDevices,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let pads: HashSet<u32> = bindings.pads.iter().map(|(_, a)| a.0).collect();
    let touches: HashSet<u32> = bindings.touches.iter().map(|(_, a)| a.0).collect();
    for (i, action) in actions.iter().enumerate() {
        let i = i as u32;
        let missing = [
            (
                devices.gamepad && !pads.contains(&i),
                "gamepad",
                "pad",
                PadButton::PATH,
            ),
            (
                devices.touch && !touches.contains(&i),
                "touch",
                "touch",
                TouchButton::PATH,
            ),
        ];
        for (_, device, method, path) in missing.into_iter().filter(|m| m.0) {
            let button = path.rsplit("::").next().unwrap_or(path);
            let mut diagnostic = Diagnostic::warning(
                "E9107",
                at,
                format!(
                    "`{name}::{action}` has no {device} binding, and a target has {device} input"
                ),
            );
            diagnostic
                .notes
                .push(format!("add `.{method}({button}::..., {name}::{action})`"));
            diagnostics.push(diagnostic);
        }
    }
}

/// A compile-time evaluator of the constant subset an `InputMap` is built
/// from: literals, negated numbers, schema and unit enum variants, and calls
/// to `@const` natives.
struct ConstEval<'c, 'e> {
    cx: &'c InferCx<'c>,
    env: &'c ModuleEnv<'e>,
    services: &'c mut Services,
    diagnostics: Vec<Diagnostic>,
}

impl ConstEval<'_, '_> {
    fn fail(&mut self, at: &SyntaxNode, message: impl Into<String>) -> Option<Value> {
        let mut diagnostic = Diagnostic::error("E2501", at.text_range(), message);
        diagnostic.notes.push(
            "a constant is evaluated while compiling: literals, enum variants and `@const` \
             natives"
                .to_owned(),
        );
        self.diagnostics.push(diagnostic);
        None
    }

    fn expr(&mut self, expr: &Expr) -> Option<Value> {
        let node = expr.syntax();
        match node.kind() {
            SyntaxKind::ParenExpr => {
                let inner = node.children().into_iter().find_map(Expr::cast)?;
                self.expr(&inner)
            }
            SyntaxKind::LiteralExpr => self.literal(node),
            SyntaxKind::UnaryExpr => {
                let negated = node
                    .children_with_tokens()
                    .into_iter()
                    .filter_map(|e| e.as_token().cloned())
                    .any(|t| t.kind() == SyntaxKind::Minus);
                let operand = node.children().into_iter().find_map(Expr::cast);
                let Some(operand) = operand.filter(|_| negated) else {
                    return self.fail(node, "this operator is not evaluated at compile time");
                };
                match self.expr(&operand)? {
                    Value::Int(v) => Some(Value::Int(v.checked_neg()?)),
                    Value::Float(v) => Some(Value::Float(-v)),
                    _ => self.fail(node, "only a number is negated at compile time"),
                }
            }
            SyntaxKind::PathExpr => self.path(node),
            SyntaxKind::CallExpr => self.call(node),
            _ => self.fail(node, "this expression is not evaluated at compile time"),
        }
    }

    fn literal(&mut self, node: &SyntaxNode) -> Option<Value> {
        let token = node
            .children_with_tokens()
            .into_iter()
            .filter_map(|e| e.as_token().cloned())
            .find(|t| !t.kind().is_trivia())?;
        let float = Expr::cast(node.clone())
            .and_then(|e| self.cx.type_of(&e).cloned())
            .is_some_and(|ty| matches!(ty, Ty::F32 | Ty::F64 | Ty::InferFloat));
        let text = token.text();
        match token.kind() {
            SyntaxKind::IntLiteral => {
                let v = parse_int_literal(&text)?;
                Some(if float {
                    Value::Float(v as f64)
                } else {
                    Value::Int(i64::try_from(v).ok()?)
                })
            }
            SyntaxKind::FloatLiteral => Some(Value::Float(parse_float_literal(&text)?)),
            SyntaxKind::TrueKw => Some(Value::bool(true)),
            SyntaxKind::FalseKw => Some(Value::bool(false)),
            _ => self.fail(node, "this literal is not evaluated at compile time"),
        }
    }

    fn path(&mut self, node: &SyntaxNode) -> Option<Value> {
        let path = PathExpr::cast(node.clone())?;
        let segments: Vec<_> = path.segments().collect();
        let head = segments.first()?;
        match self.cx.resolution_at(head.text_range()) {
            Some(Resolution::Native(id)) => match self.env.natives.variant_by_id(id) {
                Some(variant) => Some(Value::Int(i64::from(variant.index))),
                None => self.fail(node, "a native is not a constant"),
            },
            Some(Resolution::Symbol(owner)) if segments.len() == 2 => {
                let name = segments[1].text();
                let variants = self.env.enum_variants(owner).unwrap_or_default();
                match variants.iter().position(|v| v.name == name) {
                    Some(i) if variants[i].payload == VariantPayload::Unit => {
                        Some(Value::Int(i as i64))
                    }
                    _ => self.fail(node, "only a variant without a payload is a constant here"),
                }
            }
            _ => self.fail(node, "this name is not evaluated at compile time"),
        }
    }

    fn call(&mut self, node: &SyntaxNode) -> Option<Value> {
        let call = CallExpr::cast(node.clone())?;
        let Some(native) = self.cx.native_call(node.text_range()) else {
            return self.fail(node, "only a `@const` native is called at compile time");
        };
        let entry = self.env.natives.function_by_id(native.id)?;
        if !entry.function.constant {
            let name = entry.path.rsplit("::").next().unwrap_or(&entry.path);
            return self.fail(node, format!("`{name}` is not `@const`"));
        }
        let mut values = Vec::new();
        if native.receiver {
            let receiver = call
                .callee()
                .and_then(|c| FieldExpr::cast(c.syntax().clone()))
                .and_then(|f| f.receiver())?;
            values.push(self.expr(&receiver)?);
        }
        for (label, arg) in emit_args(node) {
            if label.is_some() {
                return self.fail(node, "a `@const` native takes no named arguments");
            }
            values.push(self.expr(&arg)?);
        }
        let mut cx = NativeCx::new(self.services);
        match (entry.function.call)(&mut cx, &values) {
            Ok(value) => Some(value),
            Err(error) => {
                let name = entry.path.rsplit("::").next().unwrap_or(&entry.path);
                self.diagnostics.push(Diagnostic::error(
                    "E2112",
                    node.text_range(),
                    format!("`{name}` rejects its arguments: {error}"),
                ));
                None
            }
        }
    }
}
