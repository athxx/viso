//! Native calls: `text::upper(s)`, `Stopwatch::start()` and `watch.elapsed_ms()`,
//! typed against the registered native schema. Each call bound to a native is
//! recorded, so effect checking, capability inference and lowering see the
//! function it calls without resolving it again.

use viso_behavior::native::{NativeEntry, NativeId, NativeVariant};

use super::InferCx;
use crate::ast::{AstNode, Expr, FieldExpr};
use crate::diag::Diagnostic;
use crate::hir::ty::Ty;
use crate::resolve::suggest::{Candidate, attach, nearest};
use crate::syntax::{SyntaxNode, TextRange};

/// A call bound to a native function.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeCall {
    /// The function or method.
    pub id: NativeId,
    /// Whether the callee is `receiver.method`, the receiver passing as the
    /// first argument.
    pub receiver: bool,
}

impl InferCx<'_> {
    /// A native function's parameter and return types.
    fn signature(&self, entry: &NativeEntry) -> (Vec<Ty>, Ty) {
        let action = self.env.input_action();
        let params = entry
            .function
            .params
            .iter()
            .map(|p| Ty::from_schema(&p.ty, &action))
            .collect();
        (params, Ty::from_schema(&entry.function.ret, &action))
    }

    /// The schema enum variant `id` names, if it names one.
    pub(super) fn native_variant(&self, id: NativeId) -> Option<NativeVariant> {
        self.env.natives()?.variant_by_id(id)
    }

    /// The signature of a call through a path naming the native `id` (a
    /// function, or a handle method called with its receiver first), recording
    /// the call. A handle type is not callable.
    pub(super) fn native_path_call(
        &mut self,
        id: NativeId,
        node: &SyntaxNode,
        callee: TextRange,
    ) -> Option<(Vec<Ty>, Ty)> {
        let natives = self.env.natives()?;
        let Some(entry) = natives.function_by_id(id) else {
            let name = natives.ty_by_id(id).map_or("", |t| &*t.path);
            self.diagnostics.push(Diagnostic::error(
                "E2103",
                callee,
                format!("the native type `{name}` is not callable; call one of its functions"),
            ));
            return None;
        };
        self.record_native(node.text_range(), id, false);
        Some(self.signature(entry))
    }

    /// Types `receiver.method(args)` on a native handle type `ty`: the method's
    /// parameters after its receiver, recording the call. A name the type does
    /// not declare as a method is `E2001`, with the nearest method names.
    pub(super) fn native_method_call(
        &mut self,
        ty: NativeId,
        callee: &SyntaxNode,
        args: &[Expr],
        expected: Option<&Ty>,
        node: &SyntaxNode,
    ) -> Ty {
        let Some(natives) = self.env.natives() else {
            return Ty::Unknown;
        };
        let Some(name) = FieldExpr::cast(callee.clone()).and_then(|f| f.field()) else {
            return Ty::Unknown;
        };
        let text = name.text();
        match natives.method(ty, &text).filter(|m| m.is_method(natives)) {
            Some(entry) if entry.function.property => {
                let short = self.native_type_name(ty);
                self.diagnostics.push(Diagnostic::error(
                    "E2103",
                    name.text_range(),
                    format!("`{short}.{text}` is a property: read it without `()`"),
                ));
                for arg in args {
                    let _ = self.infer_expr(arg, None);
                }
                Ty::Unknown
            }
            Some(entry) => {
                let id = entry.id;
                let (mut params, ret) = self.signature(entry);
                params.remove(0);
                self.record_native(node.text_range(), id, true);
                self.apply_signature(args, (params, ret), expected, node)
            }
            None => {
                let owner = natives.ty_by_id(ty);
                let path = owner.map_or("", |t| &*t.path);
                let short = path.rsplit("::").next().unwrap_or(path);
                let mut diagnostic = Diagnostic::error(
                    "E2001",
                    name.text_range(),
                    format!("`{short}` has no method `{text}`"),
                );
                let methods = owner
                    .into_iter()
                    .flat_map(|t| t.ty.methods.iter())
                    .filter(|m| {
                        natives
                            .method(ty, m.name)
                            .is_some_and(|e| e.is_method(natives))
                    })
                    .map(|m| Candidate {
                        name: m.name,
                        declared_at: None,
                    });
                let suggestions = nearest(&text, methods);
                attach(&mut diagnostic, name.text_range(), &suggestions);
                self.diagnostics.push(diagnostic);
                for arg in args {
                    let _ = self.infer_expr(arg, None);
                }
                Ty::Unknown
            }
        }
    }

    /// Types `receiver.name` on a native handle type `ty`: a property's
    /// value, recording the call. Any other name is `E2001`, with the nearest
    /// property names.
    pub(super) fn native_property(
        &mut self,
        ty: NativeId,
        name: &crate::syntax::SyntaxToken,
        node: &SyntaxNode,
    ) -> Ty {
        let Some(natives) = self.env.natives() else {
            return Ty::Unknown;
        };
        let text = name.text();
        let property = natives
            .method(ty, &text)
            .filter(|m| m.function.property && m.is_method(natives));
        if let Some(entry) = property {
            let id = entry.id;
            let (_, ret) = self.signature(entry);
            self.record_native(node.text_range(), id, true);
            return ret;
        }
        let short = self.native_type_name(ty);
        let mut diagnostic = Diagnostic::error(
            "E2001",
            name.text_range(),
            format!("`{short}` has no property `{text}`"),
        );
        let properties = natives
            .ty_by_id(ty)
            .into_iter()
            .flat_map(|t| t.ty.methods.iter())
            .filter(|m| m.property)
            .map(|m| Candidate {
                name: m.name,
                declared_at: None,
            });
        let suggestions = nearest(&text, properties);
        attach(&mut diagnostic, name.text_range(), &suggestions);
        self.diagnostics.push(diagnostic);
        Ty::Unknown
    }

    /// A path naming a native used as a value, not called: `E2103`.
    pub(super) fn native_value(&mut self, id: NativeId, at: TextRange) -> Ty {
        let path = self.env.natives().and_then(|n| {
            n.function_by_id(id)
                .map(|f| f.path.to_string())
                .or_else(|| n.ty_by_id(id).map(|t| t.path.to_string()))
        });
        let path = path.unwrap_or_default();
        self.diagnostics.push(Diagnostic::error(
            "E2103",
            at,
            format!("the native `{path}` is not a value; call it"),
        ));
        Ty::Unknown
    }

    fn record_native(&mut self, call: TextRange, id: NativeId, receiver: bool) {
        self.native_calls.insert(call, NativeCall { id, receiver });
        self.env.record_native(call, id);
    }

    /// The native function the call at `range` is bound to.
    pub(crate) fn native_call(&self, range: TextRange) -> Option<NativeCall> {
        self.native_calls.get(&range).copied()
    }

    /// The name a native handle type is spelled with, for diagnostics.
    pub(super) fn native_type_name(&self, id: NativeId) -> String {
        self.env.natives().and_then(|n| n.ty_by_id(id)).map_or_else(
            || "<native>".to_string(),
            |t| t.path.rsplit("::").next().unwrap_or(&t.path).to_string(),
        )
    }
}
