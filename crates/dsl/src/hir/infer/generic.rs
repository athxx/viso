//! Typing through generics and traits: the arguments a call to a generic
//! callable or constructor gives its parameters (turbofish, else solved from
//! the arguments and the expected type), the callable a method call or an
//! associated path (`T::f`, `Self::C`) resolves to, the bounds each
//! instantiation meets (`E2201`), and the conversion of a value to a trait
//! object.

use super::InferCx;
use crate::ast::{AstNode, Expr, FieldExpr};
use crate::diag::Diagnostic;
use crate::hir::generic::{CallTarget, Coercion, Found, ItemKind, Lookup, Subst, TraitRef, apply};
use crate::hir::ty::Ty;
use crate::resolve::{Resolution, SymbolId, SymbolKind};
use crate::syntax::{SyntaxKind, SyntaxNode, SyntaxToken, TextRange};

impl InferCx<'_> {
    /// Types the body of an instantiation: each generic parameter `subst`
    /// solves reads as its argument.
    pub(crate) fn instantiate(&mut self, subst: Subst) {
        self.subst = subst;
    }

    /// The argument each generic parameter of the instantiation being typed
    /// has, when it is one.
    pub(crate) fn instance_arg(&self, p: SymbolId) -> Option<&Ty> {
        self.subst.iter().find(|(q, _)| *q == p).map(|(_, t)| t)
    }

    /// `ty` as the instantiation being typed has it, aliases expanded and
    /// associated types of known types resolved.
    pub(crate) fn settle(&self, ty: &Ty) -> Ty {
        let ty = apply(ty, &self.subst);
        match self.env.traits() {
            Some(t) => t.normalize(&ty),
            None => ty,
        }
    }

    /// `ty`, a callee's type over its own parameters, with aliases expanded
    /// and associated types of known types resolved — not substituted by the
    /// instantiation being typed, whose parameters a recursive callee shares.
    pub(crate) fn normalized(&self, ty: &Ty) -> Ty {
        match self.env.traits() {
            Some(t) => t.normalize(ty),
            None => ty.clone(),
        }
    }

    /// The parameters of the nominal type `ty` solved to its arguments.
    pub(crate) fn args_subst(&self, ty: &Ty) -> Subst {
        let Ty::Named(id, args) = ty else {
            return Vec::new();
        };
        match self.env.traits().and_then(|t| t.generics_of(*id)) {
            Some(g) => g.symbols().into_iter().zip(args.iter().cloned()).collect(),
            None => Vec::new(),
        }
    }

    /// The generic parameters of the nominal type `id`, in order.
    pub(super) fn type_params(&self, id: SymbolId) -> Vec<SymbolId> {
        self.env
            .traits()
            .and_then(|t| t.generics_of(id))
            .map(|g| g.symbols())
            .unwrap_or_default()
    }

    /// `Named(id)` with each of its parameters as `fill`.
    pub(super) fn applied(&self, id: SymbolId, fill: impl Fn(SymbolId) -> Ty) -> Ty {
        Ty::Named(id, self.type_params(id).into_iter().map(fill).collect())
    }

    /// The `::<..>` arguments under `node`, a call or a record literal.
    pub(super) fn explicit_args(&mut self, node: &SyntaxNode) -> Vec<Ty> {
        let Some(list) = node
            .children()
            .into_iter()
            .find(|c| c.kind() == SyntaxKind::GenericCallArgs)
        else {
            return Vec::new();
        };
        list.children()
            .into_iter()
            .filter(|c| c.kind().is_type() || c.kind() == SyntaxKind::ConstGenericArg)
            .map(|c| {
                let at = c.text_range();
                self.annotation_ty(&c, at)
            })
            .collect()
    }

    /// The type a path head names, for an associated path `Head::name`.
    pub(super) fn head_type(&self, head: &SyntaxToken) -> Option<Ty> {
        let at = head.text_range();
        let ty = self
            .env
            .nominal(at)
            .or_else(|| self.refs.get(&at).and_then(|r| r.nominal()))?;
        let ty = match ty {
            Ty::Named(id, args) if args.is_empty() => self.applied(id, |_| Ty::Unknown),
            other => other,
        };
        Some(self.settle(&ty))
    }

    /// Whether the trait `t` is in scope.
    fn visible(&self, t: SymbolId) -> bool {
        self.env.trait_visible(t)
    }

    /// Looks `name` up on `ty`; a trait named as the head (`Trait::f`) looks
    /// up its own member, `Self` left to solve.
    fn lookup(&self, ty: &Ty, name: &str) -> Lookup {
        let Some(table) = self.env.traits() else {
            return Lookup::None;
        };
        if let Ty::Named(id, args) = ty
            && let Some(info) = table.traits.get(id)
        {
            let Some(item) = info.items.iter().find(|i| i.name == name) else {
                return Lookup::None;
            };
            let subst = info
                .generics
                .symbols()
                .into_iter()
                .zip(args.iter().cloned())
                .collect();
            return Lookup::Found(Found {
                symbol: item.symbol,
                kind: item.kind,
                receiver: item.receiver,
                subst,
                slot: None,
                via: Some(*id),
            });
        }
        table.lookup(ty, name, &|t| self.visible(t))
    }

    /// Types `Head::name(args)`: an associated function of the type `Head`
    /// names. `None` when `Head` names no type with members.
    pub(super) fn assoc_call(
        &mut self,
        head: &SyntaxToken,
        name: &SyntaxToken,
        args: &[Expr],
        expected: Option<&Ty>,
        node: &SyntaxNode,
    ) -> Option<Ty> {
        let ty = self.head_type(head)?;
        match self.lookup(&ty, &name.text()) {
            Lookup::Found(found)
                if found.kind != ItemKind::Const && found.kind != ItemKind::Type =>
            {
                let explicit = self.explicit_args(node);
                Some(self.call_found(found, None, explicit, args, expected, node))
            }
            Lookup::Found(_) => None,
            Lookup::Ambiguous(names) => {
                self.ambiguous(name, &names);
                self.infer_args_alone(args);
                Some(Ty::Unknown)
            }
            Lookup::None => None,
        }
    }

    /// Types `Head::NAME`: an associated const of the type `Head` names.
    pub(super) fn assoc_value(
        &mut self,
        head: &SyntaxToken,
        name: &SyntaxToken,
        node: &SyntaxNode,
    ) -> Option<Ty> {
        let ty = self.head_type(head)?;
        match self.lookup(&ty, &name.text()) {
            Lookup::Found(found) if found.kind == ItemKind::Const => {
                let declared = self
                    .env
                    .resolution_ty(&Resolution::Symbol(found.symbol))
                    .unwrap_or(Ty::Unknown);
                self.targets.insert(
                    node.text_range(),
                    CallTarget {
                        symbol: found.symbol,
                        args: Vec::new(),
                        receiver: false,
                        slot: None,
                    },
                );
                Some(self.settle(&apply(&declared, &found.subst)))
            }
            Lookup::Ambiguous(names) => {
                self.ambiguous(name, &names);
                Some(Ty::Unknown)
            }
            _ => None,
        }
    }

    /// The type of a path naming a const generic parameter.
    pub(super) fn const_param_ty(&self, p: SymbolId) -> Option<Ty> {
        match &self.env.traits()?.params.get(&p)?.kind {
            crate::hir::generic::ParamKind::Const(ty) => Some(ty.clone()),
            crate::hir::generic::ParamKind::Type => None,
        }
    }

    /// Types `recv.name(args)` on a receiver of a user type, a generic
    /// parameter or a trait object.
    pub(super) fn method_call(
        &mut self,
        callee: &SyntaxNode,
        recv: Ty,
        args: &[Expr],
        expected: Option<&Ty>,
        node: &SyntaxNode,
    ) -> Ty {
        let Some(name) = FieldExpr::cast(callee.clone()).and_then(|f| f.field()) else {
            return Ty::Unknown;
        };
        if matches!(recv, Ty::Unknown | Ty::Never) {
            self.infer_args_alone(args);
            return Ty::Unknown;
        }
        match self.lookup(&recv, &name.text()) {
            Lookup::Found(found) if found.receiver => {
                let explicit = self.explicit_args(node);
                self.call_found(found, Some(recv), explicit, args, expected, node)
            }
            Lookup::Found(_) => {
                let message = format!(
                    "`{}` takes no `self`; call it as `{}::{}(..)`",
                    name.text(),
                    self.describe(&recv),
                    name.text()
                );
                self.diagnostics
                    .push(Diagnostic::error("E2103", name.text_range(), message));
                self.infer_args_alone(args);
                Ty::Unknown
            }
            Lookup::Ambiguous(names) => {
                self.ambiguous(&name, &names);
                self.infer_args_alone(args);
                Ty::Unknown
            }
            Lookup::None if let Ty::List(elem) = &recv => {
                let Some(receiver) = FieldExpr::cast(callee.clone()).and_then(|f| f.receiver())
                else {
                    return Ty::Unknown;
                };
                self.list_method(&name, &receiver, elem, args, expected, node)
            }
            Lookup::None => {
                // A field holding a function is called as one.
                if let Some(Ty::Fn(params, ret)) = self.member_ty(&recv, &name, None) {
                    self.types.insert(
                        (callee.text_range(), callee.kind()),
                        Ty::Fn(params.clone(), ret.clone()),
                    );
                    return self.apply_signature(args, (params, *ret), expected, node);
                }
                if matches!(recv, Ty::Named(..) | Ty::Param(_) | Ty::Dyn(..)) {
                    let message =
                        format!("no method `{}` on `{}`", name.text(), self.describe(&recv));
                    self.diagnostics
                        .push(Diagnostic::error("E2001", name.text_range(), message));
                }
                self.infer_args_alone(args);
                Ty::Unknown
            }
        }
    }

    /// Infers each argument of a call whose callee is unknown, for their own
    /// diagnostics.
    pub(super) fn infer_args_alone(&mut self, args: &[Expr]) {
        for arg in args {
            let _ = self.infer_expr(arg, None);
        }
    }

    fn ambiguous(&mut self, name: &SyntaxToken, names: &[String]) {
        let message = format!(
            "`{}` is ambiguous: it may be {}; call it through its trait, as `Trait::{}(..)`",
            name.text(),
            names.join(" or "),
            name.text()
        );
        self.diagnostics
            .push(Diagnostic::error("E2202", name.text_range(), message));
    }

    /// Types a call to a single-name generic callable `symbol`.
    pub(super) fn generic_call(
        &mut self,
        symbol: SymbolId,
        args: &[Expr],
        expected: Option<&Ty>,
        node: &SyntaxNode,
    ) -> Ty {
        let kind = match self.env.symbol_kind(symbol) {
            Some(SymbolKind::Action) => ItemKind::Action,
            Some(SymbolKind::Task) => ItemKind::Task,
            _ => ItemKind::Fn,
        };
        let found = Found {
            symbol,
            kind,
            receiver: false,
            subst: Vec::new(),
            slot: None,
            via: None,
        };
        let explicit = self.explicit_args(node);
        self.call_found(found, None, explicit, args, expected, node)
    }

    /// Whether `symbol` has generic parameters of its own or its owner's.
    pub(super) fn is_generic(&self, symbol: SymbolId) -> bool {
        self.env
            .traits()
            .is_some_and(|t| t.generics_of(symbol).is_some())
    }

    /// Types a call of the callable `found` (with `recv` its receiver, for a
    /// method call): solves its generic parameters, checks its bounds and its
    /// arguments, and records the call's target.
    pub(super) fn call_found(
        &mut self,
        found: Found,
        recv: Option<Ty>,
        explicit: Vec<Ty>,
        args: &[Expr],
        expected: Option<&Ty>,
        node: &SyntaxNode,
    ) -> Ty {
        let Some((params, ret)) = self.env.callee_signature(&Resolution::Symbol(found.symbol))
        else {
            self.infer_args_alone(args);
            return Ty::Unknown;
        };
        let generics = self
            .env
            .traits()
            .and_then(|t| t.generics_of(found.symbol))
            .cloned()
            .unwrap_or_default();
        let mut solved: Subst = found
            .subst
            .iter()
            .filter(|(_, t)| *t != Ty::Unknown)
            .cloned()
            .collect();
        let open: Vec<SymbolId> = generics
            .symbols()
            .into_iter()
            .filter(|p| !solved.iter().any(|(q, _)| q == p))
            .collect();
        let range = node.text_range();
        let (ret, solved) = self.solve(
            &open,
            &mut solved,
            explicit,
            (params, ret),
            recv.as_ref(),
            args,
            expected,
            range,
        );
        self.check_bounds(&generics.bounds, &solved, range);
        let (symbol, generics, solved) = self.through_impl(found.symbol, generics, solved);
        let target_args = generics
            .symbols()
            .into_iter()
            .map(|p| {
                solved
                    .iter()
                    .find(|(q, _)| *q == p)
                    .map_or(Ty::Unknown, |(_, t)| t.clone())
            })
            .collect();
        self.targets.insert(
            range,
            CallTarget {
                symbol,
                args: target_args,
                receiver: recv.is_some(),
                slot: found.slot,
            },
        );
        self.env.record_call(range, symbol);
        if ret == Ty::Unknown {
            return ret;
        }
        self.check_against(ret, expected, node)
    }

    /// A trait's member called with its `Self` solved to a known type (as
    /// `Trait::f(x)` is) as the member of that type's impl: the callable, its
    /// generics and their solution. Anything else is itself.
    fn through_impl(
        &self,
        symbol: SymbolId,
        generics: crate::hir::generic::Generics,
        solved: Subst,
    ) -> (SymbolId, crate::hir::generic::Generics, Subst) {
        let unchanged = (symbol, generics.clone(), solved.clone());
        let Some(table) = self.env.traits() else {
            return unchanged;
        };
        let Some(crate::hir::generic::Owner::Trait(t)) = table.owners.get(&symbol) else {
            return unchanged;
        };
        let Some(info) = table.traits.get(t) else {
            return unchanged;
        };
        let arg = |p: SymbolId| {
            solved
                .iter()
                .find(|(q, _)| *q == p)
                .map_or(Ty::Unknown, |(_, t)| t.clone())
        };
        let self_ty = arg(info.self_param);
        if self_ty.has_param() || self_ty.has_unknown() || matches!(self_ty, Ty::Dyn(..)) {
            return unchanged;
        }
        let tr = TraitRef {
            symbol: *t,
            args: info.generics.symbols().into_iter().map(arg).collect(),
        };
        let Some(name) = info
            .items
            .iter()
            .find(|i| i.symbol == symbol)
            .map(|i| i.name.clone())
        else {
            return unchanged;
        };
        let Some(found) = table.trait_method(&self_ty, &tr, &name) else {
            return unchanged;
        };
        if found.symbol == symbol {
            return unchanged;
        }
        let theirs = table.generics_of(found.symbol).cloned().unwrap_or_default();
        // The member's own parameters follow the enclosing ones in both.
        let own_trait: Vec<SymbolId> = generics
            .symbols()
            .into_iter()
            .skip(1 + info.generics.params.len())
            .collect();
        let outer = theirs.params.len().saturating_sub(own_trait.len());
        let mut out: Subst = found.subst.clone();
        for (p, q) in theirs.symbols().into_iter().skip(outer).zip(own_trait) {
            out.push((p, arg(q)));
        }
        (found.symbol, theirs, out)
    }

    /// Solves the parameters `open` of the signature `sig` (some already
    /// solved in `solved`): from `explicit` arguments when given, else from
    /// the receiver, the expected type and each argument in turn. Types each
    /// argument and returns the return type and the solution.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn solve(
        &mut self,
        open: &[SymbolId],
        solved: &mut Subst,
        explicit: Vec<Ty>,
        (params, ret): (Vec<Ty>, Ty),
        recv: Option<&Ty>,
        args: &[Expr],
        expected: Option<&Ty>,
        at: TextRange,
    ) -> (Ty, Subst) {
        if !explicit.is_empty() {
            if explicit.len() == open.len() {
                solved.extend(open.iter().copied().zip(explicit));
            } else {
                let message = format!(
                    "this call takes {} type argument(s), {} given",
                    open.len(),
                    explicit.len()
                );
                self.diagnostics
                    .push(Diagnostic::error("E2103", at, message));
            }
        }
        let is_var = |p: SymbolId| open.contains(&p);
        let mut params = params.into_iter();
        if let Some(recv) = recv
            && let Some(first) = params.next()
        {
            let first = apply(&first, solved);
            if !first.bind(recv, &is_var, solved) {
                let (want, got) = (self.describe(&first), self.describe(recv));
                let message = format!("this method takes `{want}` as `self`, not `{got}`");
                self.diagnostics
                    .push(Diagnostic::error("E2103", at, message));
            }
        }
        let params: Vec<Ty> = params.collect();
        if let Some(want) = expected {
            let mut trial = solved.clone();
            if apply(&ret, solved).bind(want, &is_var, &mut trial) {
                *solved = trial;
            }
        }
        let mut undetermined = false;
        for (i, arg) in args.iter().enumerate() {
            let Some(param) = params.get(i) else {
                let _ = self.infer_expr(arg, None);
                continue;
            };
            let want = self.normalized(&apply(param, solved));
            let open_here = want.has_param() && open.iter().any(|p| mentions(&want, *p));
            if open_here {
                let got = if arg.syntax().kind() == SyntaxKind::ClosureExpr {
                    self.infer_closure(arg.syntax(), None, true)
                } else {
                    self.infer_expr(arg, None)
                };
                undetermined |= got.has_unknown();
                if !want.bind(&got, &is_var, solved) {
                    let (w, g) = (self.describe(&want), self.describe(&got));
                    self.diagnostics.push(
                        Diagnostic::error(
                            "E2103",
                            arg.syntax().text_range(),
                            format!("type mismatch: expected `{w}`, found `{g}`"),
                        )
                        .expecting([w], g),
                    );
                }
            } else {
                let got = self.infer_expr(arg, Some(&want));
                undetermined |= got.has_unknown();
            }
        }
        for p in open {
            if !solved.iter().any(|(q, _)| q == p) {
                if !undetermined {
                    let name = self.describe(&Ty::Param(*p));
                    let message = format!(
                        "the type of `{name}` cannot be inferred here; give it, as `::<..>`"
                    );
                    self.diagnostics
                        .push(Diagnostic::error("E2103", at, message));
                }
                solved.push((*p, Ty::Unknown));
            }
        }
        (self.normalized(&apply(&ret, solved)), solved.clone())
    }

    /// Reports each of `bounds` the solution `solved` leaves unmet (`E2201`).
    pub(super) fn check_bounds(
        &mut self,
        bounds: &[crate::hir::generic::Bound],
        solved: &Subst,
        at: TextRange,
    ) {
        let Some(table) = self.env.traits() else {
            return;
        };
        for b in bounds {
            let subject = self.normalized(&apply(&b.subject, solved));
            let tr = b.tr.map(&|t| self.normalized(&apply(t, solved)));
            if subject.has_unknown() || table.satisfies(&subject, &tr) {
                continue;
            }
            let message = format!(
                "`{}` does not implement `{}`",
                self.describe(&subject),
                self.describe_trait(&tr)
            );
            self.diagnostics
                .push(Diagnostic::error("E2201", at, message));
        }
    }

    /// A trait as source spells it.
    pub(super) fn describe_trait(&self, tr: &TraitRef) -> String {
        self.describe(&Ty::Named(tr.symbol, tr.args.clone()))
    }

    /// Whether a value of `ty` may be compared with `==`: it is not a generic
    /// parameter without an `Eq` bound. Reports `E2201` at `at` otherwise.
    pub(super) fn check_comparable(&mut self, ty: &Ty, at: TextRange) {
        let Some(table) = self.env.traits() else {
            return;
        };
        let Some((&eq, _)) = table
            .auto
            .iter()
            .find(|(_, a)| **a == crate::hir::generic::AutoTrait::Eq)
        else {
            return;
        };
        let tr = TraitRef {
            symbol: eq,
            args: Vec::new(),
        };
        if ty.has_param() && !table.satisfies(ty, &tr) {
            let message = format!(
                "`{}` may not be compared: bound it by `Eq`",
                self.describe(ty)
            );
            self.diagnostics
                .push(Diagnostic::error("E2201", at, message));
        }
    }

    /// The converter `using` names for a `bind` of a `view` property to a
    /// `model` source: its `to_view` and `to_model`. A type that does not
    /// implement `TwoWayConverter<Model, View>` is `E2201`.
    pub(crate) fn converter(
        &mut self,
        using: &crate::ast::TypePath,
        model: &Ty,
        view: &Ty,
    ) -> Option<crate::hir::generic::Converter> {
        let at = using.syntax().text_range();
        let ty = self.annotation_ty(using.syntax(), at);
        let table = self.env.traits()?;
        let symbol = table.converter?;
        if ty.has_unknown() || model.has_unknown() || view.has_unknown() {
            return None;
        }
        let tr = TraitRef {
            symbol,
            args: vec![model.clone(), view.clone()],
        };
        if !table.satisfies(&ty, &tr) {
            let message = format!(
                "`{}` does not implement `{}`, so it cannot convert this binding",
                self.describe(&ty),
                self.describe_trait(&tr)
            );
            self.diagnostics
                .push(Diagnostic::error("E2201", at, message));
            return None;
        }
        let target = |name: &str| {
            let found = table.trait_method(&ty, &tr, name)?;
            let generics = table.generics_of(found.symbol).cloned().unwrap_or_default();
            let args = generics
                .symbols()
                .into_iter()
                .map(|p| {
                    found
                        .subst
                        .iter()
                        .find(|(q, _)| *q == p)
                        .map_or(Ty::Unknown, |(_, t)| t.clone())
                })
                .collect();
            self.env.record_call(at, found.symbol);
            Some(CallTarget {
                symbol: found.symbol,
                args,
                receiver: false,
                slot: None,
            })
        };
        Some(crate::hir::generic::Converter {
            to_view: target("to_view")?,
            to_model: target("to_model")?,
        })
    }

    /// Why no object can be of the trait `t`, if none can: a method its vtable
    /// would hold is generic of its own, or names `Self` beyond its receiver.
    fn dyn_incompatible(&self, t: SymbolId) -> Option<String> {
        let table = self.env.traits()?;
        let root = TraitRef {
            symbol: t,
            args: Vec::new(),
        };
        for tr in table.with_supertraits(&root) {
            let info = table.traits.get(&tr.symbol)?;
            let inherited = 1 + info.generics.params.len();
            for item in info.items.iter().filter(|i| i.receiver) {
                let own = table
                    .generics_of(item.symbol)
                    .map_or(0, |g| g.params.len().saturating_sub(inherited));
                if own > 0 {
                    return Some(format!("`{}` has type parameters of its own", item.name));
                }
                let Some((params, ret)) =
                    self.env.callee_signature(&Resolution::Symbol(item.symbol))
                else {
                    continue;
                };
                if params
                    .iter()
                    .skip(1)
                    .chain([&ret])
                    .any(|ty| mentions(ty, info.self_param))
                {
                    return Some(format!("`{}` names `Self` beyond its receiver", item.name));
                }
            }
        }
        None
    }

    /// Converts a value of `produced` to the trait object `target`, recording
    /// the vtable the conversion builds at `node`; a type that does not
    /// implement the trait, or a trait no object can be of, is `E2201`.
    pub(super) fn coerce(&mut self, produced: Ty, target: &Ty, node: &SyntaxNode) -> Ty {
        let Ty::Dyn(t, args) = target else {
            return produced;
        };
        let Some(table) = self.env.traits() else {
            return target.clone();
        };
        if matches!(produced, Ty::Unknown | Ty::Never) {
            return target.clone();
        }
        let tr = TraitRef {
            symbol: *t,
            args: args.clone(),
        };
        if let Ty::Dyn(s, _) = &produced {
            if s == t {
                return target.clone();
            }
            let message = format!(
                "a `{}` is not a `{}`: a trait object converts only to itself",
                self.describe(&produced),
                self.describe(target)
            );
            self.diagnostics
                .push(Diagnostic::error("E2201", node.text_range(), message));
            return target.clone();
        }
        if let Some(why) = self.dyn_incompatible(*t) {
            let message = format!(
                "`{}` is not usable as `dyn`: {why}",
                self.describe_trait(&tr)
            );
            self.diagnostics
                .push(Diagnostic::error("E2201", node.text_range(), message));
            return target.clone();
        }
        if !table.satisfies(&produced, &tr) {
            let message = format!(
                "`{}` does not implement `{}`",
                self.describe(&produced),
                self.describe_trait(&tr)
            );
            self.diagnostics
                .push(Diagnostic::error("E2201", node.text_range(), message));
            return target.clone();
        }
        let mut methods = Vec::new();
        for (item, name) in table.vtable(*t) {
            let owner = match table.owners.get(&item) {
                Some(crate::hir::generic::Owner::Trait(o)) => *o,
                _ => continue,
            };
            let of = table
                .with_supertraits(&tr)
                .into_iter()
                .find(|x| x.symbol == owner)
                .unwrap_or(TraitRef {
                    symbol: owner,
                    args: Vec::new(),
                });
            let Some(found) = table.trait_method(&produced, &of, &name) else {
                continue;
            };
            let generics = table.generics_of(found.symbol).cloned().unwrap_or_default();
            methods.push(CallTarget {
                symbol: found.symbol,
                args: generics
                    .symbols()
                    .into_iter()
                    .map(|p| {
                        found
                            .subst
                            .iter()
                            .find(|(q, _)| *q == p)
                            .map_or(Ty::Unknown, |(_, t)| self.settle(t))
                    })
                    .collect(),
                receiver: true,
                slot: None,
            });
            self.env.record_call(node.text_range(), found.symbol);
        }
        self.coercions
            .insert((node.text_range(), node.kind()), Coercion { tr, methods });
        target.clone()
    }
}

/// Whether `ty` mentions the parameter `p`.
pub(super) fn mentions(ty: &Ty, p: SymbolId) -> bool {
    matches!(ty, Ty::Param(q) if *q == p) || ty.parts().into_iter().any(|t| mentions(t, p))
}
