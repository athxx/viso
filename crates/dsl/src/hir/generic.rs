//! User traits, impls and generics (§26, §30, §31, §78, §79): what a package
//! declares, collected once before any body is typed, and the questions typing
//! asks of it — whether a type meets a bound, which callable a method or an
//! associated name is on a type, and what an associated type or an alias
//! stands for.
//!
//! A generic callable is typed once with its parameters opaque
//! ([`Ty::Param`], meeting only their bounds) and lowered once per
//! instantiation with them substituted; a call records the instantiation it
//! makes as a [`CallTarget`]. Resolution is deterministic and independent of
//! import order: an inherent member first, then a member of a visible trait the
//! type implements, and two candidates of either kind are ambiguous (`E2202`).

use std::collections::HashMap;

use crate::resolve::SymbolId;
use crate::syntax::TextRange;

use super::ty::Ty;

/// How deep a bound check may recurse through impl bounds before it gives up.
const BOUND_DEPTH: u32 = 16;

/// A trait applied to its arguments, `TwoWayConverter<F64, String>`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct TraitRef {
    pub(crate) symbol: SymbolId,
    pub(crate) args: Vec<Ty>,
}

impl TraitRef {
    /// `self` with each argument rewritten by `f`.
    pub(crate) fn map(&self, f: &dyn Fn(&Ty) -> Ty) -> TraitRef {
        TraitRef {
            symbol: self.symbol,
            args: self.args.iter().map(f).collect(),
        }
    }
}

/// Generic parameters solved to types, in solving order.
pub(crate) type Subst = Vec<(SymbolId, Ty)>;

/// `ty` with the parameters `subst` solves replaced.
pub(crate) fn apply(ty: &Ty, subst: &[(SymbolId, Ty)]) -> Ty {
    if subst.is_empty() {
        return ty.clone();
    }
    ty.subst(&|p| subst.iter().find(|(q, _)| *q == p).map(|(_, t)| t.clone()))
}

/// What a generic parameter takes: a type, or a constant of a type.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ParamKind {
    Type,
    Const(Ty),
}

/// One generic parameter.
#[derive(Debug, Clone)]
pub(crate) struct GenericParam {
    pub(crate) symbol: SymbolId,
    pub(crate) name: String,
    pub(crate) kind: ParamKind,
}

/// A bound a declaration requires: `subject: Trait<..>`.
#[derive(Debug, Clone)]
pub(crate) struct Bound {
    pub(crate) subject: Ty,
    pub(crate) tr: TraitRef,
}

/// The generic parameters a declaration binds — for a member of an impl or a
/// trait, the enclosing ones (`Self` and the trait's first for a trait member)
/// followed by its own — and the bounds on them.
#[derive(Debug, Clone, Default)]
pub(crate) struct Generics {
    pub(crate) params: Vec<GenericParam>,
    pub(crate) bounds: Vec<Bound>,
}

impl Generics {
    /// Whether `p` is one of these parameters.
    pub(crate) fn binds(&self, p: SymbolId) -> bool {
        self.params.iter().any(|g| g.symbol == p)
    }

    /// The parameters' symbols, in order.
    pub(crate) fn symbols(&self) -> Vec<SymbolId> {
        self.params.iter().map(|g| g.symbol).collect()
    }
}

/// What a member of a trait or an impl is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ItemKind {
    Fn,
    Action,
    Task,
    Const,
    Type,
}

/// One member of a trait or an impl.
#[derive(Debug, Clone)]
pub(crate) struct Item {
    pub(crate) name: String,
    pub(crate) symbol: SymbolId,
    pub(crate) kind: ItemKind,
    /// Whether a callable takes `self`.
    pub(crate) receiver: bool,
    /// Whether it has a body or a value: a default in a trait.
    pub(crate) has_body: bool,
    /// An associated const's type, or an associated type's value in an impl.
    pub(crate) ty: Option<Ty>,
    /// The bounds a trait's associated type declares.
    pub(crate) bounds: Vec<TraitRef>,
    pub(crate) at: TextRange,
}

/// A `trait` declaration.
#[derive(Debug, Clone)]
pub(crate) struct TraitInfo {
    pub(crate) name: String,
    pub(crate) home: Option<usize>,
    /// The trait's `Self`.
    pub(crate) self_param: SymbolId,
    /// Its own parameters (not `Self`) and its `where` bounds.
    pub(crate) generics: Generics,
    /// Its supertraits, over `Self` and its parameters.
    pub(crate) supertraits: Vec<TraitRef>,
    pub(crate) items: Vec<Item>,
    pub(crate) at: TextRange,
}

/// An `impl` declaration, inherent or of a trait.
#[derive(Debug, Clone)]
pub(crate) struct ImplInfo {
    pub(crate) home: Option<usize>,
    /// The `impl` keyword.
    pub(crate) at: TextRange,
    /// The impl's `Self`, which stands for its target.
    pub(crate) self_param: SymbolId,
    pub(crate) generics: Generics,
    pub(crate) target: Ty,
    pub(crate) trait_ref: Option<TraitRef>,
    pub(crate) items: Vec<Item>,
}

impl ImplInfo {
    /// Its member `name`.
    pub(crate) fn item(&self, name: &str) -> Option<&Item> {
        self.items.iter().find(|i| i.name == name)
    }
}

/// What declares a member.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Owner {
    Impl(usize),
    Trait(SymbolId),
}

/// A trait the compiler implements for every type its rule admits (§79).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AutoTrait {
    /// Every value can be copied.
    Clone,
    /// Every value but a function and a trait object can be compared.
    Eq,
    /// What is `Eq` and holds no float.
    Hash,
}

/// A `type` alias: its parameters and what it stands for.
#[derive(Debug, Clone)]
pub(crate) struct Alias {
    pub(crate) params: Vec<SymbolId>,
    pub(crate) body: Ty,
}

/// How a type meets a bound.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Witness {
    /// A generic parameter's (or an undetermined type's) bound says so.
    Param,
    /// The compiler's rule for an auto trait admits it.
    Auto,
    /// A trait object of the trait or a subtrait.
    Dyn,
    /// The impl at this index, with its parameters solved so.
    Impl(usize, Subst),
}

/// The callable or constant a method or an associated name is, on a type.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Found {
    pub(crate) symbol: SymbolId,
    pub(crate) kind: ItemKind,
    pub(crate) receiver: bool,
    /// The enclosing parameters solved: the impl's and its `Self`, or a
    /// trait's `Self` and parameters.
    pub(crate) subst: Subst,
    /// Its slot in a trait object's vtable, for a call through one.
    pub(crate) slot: Option<u32>,
    /// The trait it is a member of, when it is one.
    pub(crate) via: Option<SymbolId>,
}

/// The outcome of looking a name up on a type.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Lookup {
    None,
    Found(Found),
    /// Several candidates, named for the diagnostic.
    Ambiguous(Vec<String>),
}

/// What a call resolved to: the callable and the arguments of each of its
/// generic parameters, in declaration order (the instantiation it lowers to).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CallTarget {
    pub(crate) symbol: SymbolId,
    pub(crate) args: Vec<Ty>,
    /// Whether the receiver of a method call is its first argument.
    pub(crate) receiver: bool,
    /// The vtable slot of a call through a trait object.
    pub(crate) slot: Option<u32>,
}

/// What a `bind .. using C` converts through: `C`'s `to_view` and
/// `to_model`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Converter {
    pub(crate) to_view: CallTarget,
    pub(crate) to_model: CallTarget,
}

/// A value converted to a trait object: the trait, and the method each
/// vtable slot calls for the value's type.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Coercion {
    pub(crate) tr: TraitRef,
    pub(crate) methods: Vec<CallTarget>,
}

/// Every trait, impl, generic declaration and alias of the package.
#[derive(Debug, Default)]
pub struct TraitTable {
    pub(crate) traits: HashMap<SymbolId, TraitInfo>,
    /// In module then source order.
    pub(crate) impls: Vec<ImplInfo>,
    /// Every declaration with generic parameters, a callable's including its
    /// enclosing impl's or trait's.
    pub(crate) generics: HashMap<SymbolId, Generics>,
    /// Every generic parameter.
    pub(crate) params: HashMap<SymbolId, GenericParam>,
    /// The bounds on each generic parameter, inline and from `where`.
    pub(crate) param_bounds: HashMap<SymbolId, Vec<TraitRef>>,
    /// The impl or trait each member belongs to.
    pub(crate) owners: HashMap<SymbolId, Owner>,
    pub(crate) aliases: HashMap<SymbolId, Alias>,
    pub(crate) auto: HashMap<SymbolId, AutoTrait>,
    /// The prelude's `TwoWayConverter`.
    pub(crate) converter: Option<SymbolId>,
}

impl TraitTable {
    /// The generics of `symbol`, empty when it has none.
    pub(crate) fn generics_of(&self, symbol: SymbolId) -> Option<&Generics> {
        self.generics.get(&symbol).filter(|g| !g.params.is_empty())
    }

    /// `tr` and every trait it requires, transitively, in breadth-first
    /// declaration order.
    pub(crate) fn with_supertraits(&self, tr: &TraitRef) -> Vec<TraitRef> {
        let mut out = vec![tr.clone()];
        let mut at = 0;
        while at < out.len() && out.len() < 64 {
            let current = out[at].clone();
            at += 1;
            let Some(info) = self.traits.get(&current.symbol) else {
                continue;
            };
            let subst: Subst = info
                .generics
                .symbols()
                .into_iter()
                .zip(current.args.iter().cloned())
                .collect();
            for sup in &info.supertraits {
                let sup = sup.map(&|t| apply(t, &subst));
                if !out.iter().any(|t| t.symbol == sup.symbol) {
                    out.push(sup);
                }
            }
        }
        out
    }

    /// The bounds on the generic parameter `p`, supertraits included.
    pub(crate) fn bounds_of(&self, p: SymbolId) -> Vec<TraitRef> {
        let mut out: Vec<TraitRef> = Vec::new();
        for b in self.param_bounds.get(&p).into_iter().flatten() {
            for t in self.with_supertraits(b) {
                if !out.contains(&t) {
                    out.push(t);
                }
            }
        }
        out
    }

    /// Whether `ty` meets `tr`.
    pub(crate) fn satisfies(&self, ty: &Ty, tr: &TraitRef) -> bool {
        self.implementation(ty, tr, 0).is_some()
    }

    /// How `ty` meets `tr`, `None` when it does not.
    pub(crate) fn implementation(&self, ty: &Ty, tr: &TraitRef, depth: u32) -> Option<Witness> {
        if depth > BOUND_DEPTH {
            return None;
        }
        let same = |t: &TraitRef| {
            t.symbol == tr.symbol
                && t.args.len() == tr.args.len()
                && t.args
                    .iter()
                    .zip(&tr.args)
                    .all(|(a, b)| super::infer::compatible(a, b))
        };
        match ty {
            Ty::Unknown | Ty::Never => return Some(Witness::Param),
            Ty::Param(p) => {
                return self
                    .bounds_of(*p)
                    .iter()
                    .any(same)
                    .then_some(Witness::Param);
            }
            Ty::Assoc(base, name) => {
                return self
                    .assoc_bounds(base, name)
                    .iter()
                    .flat_map(|b| self.with_supertraits(b))
                    .any(|t| same(&t))
                    .then_some(Witness::Param);
            }
            Ty::Dyn(t, args) => {
                let object = TraitRef {
                    symbol: *t,
                    args: args.clone(),
                };
                if self.with_supertraits(&object).iter().any(same) {
                    return Some(Witness::Dyn);
                }
            }
            _ => {}
        }
        if let Some(auto) = self.auto.get(&tr.symbol) {
            return self.auto_admits(*auto, ty, depth).then_some(Witness::Auto);
        }
        self.impls_of(ty, tr, depth)
            .into_iter()
            .next()
            .map(|(i, s)| Witness::Impl(i, s))
    }

    /// The impls of `tr` that apply to `ty`, with their parameters solved.
    fn impls_of(&self, ty: &Ty, tr: &TraitRef, depth: u32) -> Vec<(usize, Subst)> {
        let mut found = Vec::new();
        for (i, imp) in self.impls.iter().enumerate() {
            let Some(of) = imp.trait_ref.as_ref().filter(|t| t.symbol == tr.symbol) else {
                continue;
            };
            let Some(subst) = self.applies(imp, ty, depth) else {
                continue;
            };
            let mut subst = subst;
            let vars = |p: SymbolId| imp.generics.binds(p);
            if of.args.len() == tr.args.len()
                && of
                    .args
                    .iter()
                    .zip(&tr.args)
                    .all(|(x, y)| apply(x, &subst).bind(y, &vars, &mut subst))
            {
                found.push((i, subst));
            }
        }
        found
    }

    /// The impl's parameters solved for its target to be `ty` and its bounds
    /// met, `None` when it does not apply. A parameter nothing solves is
    /// undetermined.
    pub(crate) fn applies(&self, imp: &ImplInfo, ty: &Ty, depth: u32) -> Option<Subst> {
        let mut subst = Vec::new();
        if !imp.target.bind(ty, &|p| imp.generics.binds(p), &mut subst) {
            return None;
        }
        for p in imp.generics.symbols() {
            if !subst.iter().any(|(q, _)| *q == p) {
                subst.push((p, Ty::Unknown));
            }
        }
        let met = imp.generics.bounds.iter().all(|b| {
            self.implementation(
                &apply(&b.subject, &subst),
                &b.tr.map(&|t| apply(t, &subst)),
                depth + 1,
            )
            .is_some()
        });
        met.then_some(subst)
    }

    fn auto_admits(&self, auto: AutoTrait, ty: &Ty, depth: u32) -> bool {
        let tr = |symbol| TraitRef {
            symbol,
            args: Vec::new(),
        };
        let symbol = self.auto.iter().find(|(_, a)| **a == auto).map(|(s, _)| *s);
        let parts_admit = || {
            ty.parts().into_iter().all(|t| match symbol {
                Some(s) => self.implementation(t, &tr(s), depth + 1).is_some(),
                None => true,
            })
        };
        match auto {
            AutoTrait::Clone => true,
            AutoTrait::Eq => !matches!(ty, Ty::Fn(..) | Ty::Dyn(..)) && parts_admit(),
            AutoTrait::Hash => {
                !matches!(ty, Ty::Fn(..) | Ty::Dyn(..) | Ty::F32 | Ty::F64) && parts_admit()
            }
        }
    }

    /// The bounds the trait declaring the associated type `name` of `base`
    /// puts on it.
    fn assoc_bounds(&self, base: &Ty, name: &str) -> Vec<TraitRef> {
        let Ty::Param(p) = base else {
            return Vec::new();
        };
        self.bounds_of(*p)
            .iter()
            .filter_map(|t| self.traits.get(&t.symbol))
            .flat_map(|info| info.items.iter())
            .filter(|i| i.kind == ItemKind::Type && i.name == name)
            .flat_map(|i| i.bounds.iter().cloned())
            .collect()
    }

    /// The member `name` of a value or a type `recv`: a method when `methods`,
    /// else any associated callable or const. `visible` says whether a trait
    /// is in scope where the name is used.
    pub(crate) fn lookup(
        &self,
        recv: &Ty,
        name: &str,
        visible: &dyn Fn(SymbolId) -> bool,
    ) -> Lookup {
        let mut found: Vec<Found> = Vec::new();
        let push = |found: &mut Vec<Found>, f: Found| {
            if !found.iter().any(|g| g.symbol == f.symbol) {
                found.push(f);
            }
        };
        match recv {
            Ty::Param(p) => {
                for tr in self.bounds_of(*p) {
                    if let Some(f) = self.trait_item(&tr, recv, name, None) {
                        push(&mut found, f);
                    }
                }
            }
            Ty::Dyn(t, args) => {
                let object = TraitRef {
                    symbol: *t,
                    args: args.clone(),
                };
                let table = self.vtable(*t);
                for tr in self.with_supertraits(&object) {
                    if let Some(mut f) = self.trait_item(&tr, recv, name, None) {
                        f.slot = table
                            .iter()
                            .position(|(s, _)| *s == f.symbol)
                            .map(|i| i as u32);
                        if f.slot.is_some() {
                            push(&mut found, f);
                        }
                    }
                }
            }
            Ty::Unknown | Ty::Assoc(..) => {}
            _ => {
                for imp in &self.impls {
                    if imp.trait_ref.is_some() {
                        continue;
                    }
                    let Some(item) = imp.item(name) else {
                        continue;
                    };
                    if let Some(mut subst) = self.applies(imp, recv, 0) {
                        subst.push((imp.self_param, recv.clone()));
                        push(&mut found, self.found(item, subst, None));
                    }
                }
                if found.is_empty() {
                    for imp in &self.impls {
                        let Some(of) = &imp.trait_ref else {
                            continue;
                        };
                        if !visible(of.symbol) {
                            continue;
                        }
                        let Some(subst) = self.applies(imp, recv, 0) else {
                            continue;
                        };
                        let of = of.map(&|t| apply(t, &subst));
                        if let Some(item) = imp.item(name) {
                            let mut subst = subst;
                            subst.push((imp.self_param, recv.clone()));
                            push(&mut found, self.found(item, subst, Some(of.symbol)));
                        } else if let Some(f) = self.trait_item(&of, recv, name, None) {
                            push(&mut found, f);
                        }
                    }
                }
            }
        }
        match found.len() {
            0 => Lookup::None,
            1 => Lookup::Found(found.remove(0)),
            _ => Lookup::Ambiguous(
                found
                    .iter()
                    .map(|f| match f.via.and_then(|t| self.traits.get(&t)) {
                        Some(t) => format!("`{}::{name}`", t.name),
                        None => format!("an inherent `{name}`"),
                    })
                    .collect(),
            ),
        }
    }

    /// The method `name` of the trait `tr` as `ty`, which implements it, has
    /// it: its impl's, else the trait's default.
    pub(crate) fn trait_method(&self, ty: &Ty, tr: &TraitRef, name: &str) -> Option<Found> {
        if matches!(ty, Ty::Param(_) | Ty::Assoc(..)) {
            return self.trait_item(tr, ty, name, None);
        }
        let (index, subst) = self.impls_of(ty, tr, 0).into_iter().next()?;
        let imp = &self.impls[index];
        match imp.item(name) {
            Some(item) => {
                let mut subst = subst;
                subst.push((imp.self_param, ty.clone()));
                Some(self.found(item, subst, Some(tr.symbol)))
            }
            None => self.trait_item(tr, ty, name, None),
        }
    }

    /// The member `name` of the trait `tr` as `recv` has it.
    fn trait_item(&self, tr: &TraitRef, recv: &Ty, name: &str, slot: Option<u32>) -> Option<Found> {
        let info = self.traits.get(&tr.symbol)?;
        let item = info.items.iter().find(|i| i.name == name)?;
        let mut subst: Subst = vec![(info.self_param, recv.clone())];
        subst.extend(
            info.generics
                .symbols()
                .into_iter()
                .zip(tr.args.iter().cloned()),
        );
        let mut f = self.found(item, subst, Some(tr.symbol));
        f.slot = slot;
        Some(f)
    }

    fn found(&self, item: &Item, subst: Subst, via: Option<SymbolId>) -> Found {
        Found {
            symbol: item.symbol,
            kind: item.kind,
            receiver: item.receiver,
            subst,
            slot: None,
            via,
        }
    }

    /// The methods a trait object of `t` dispatches through its vtable, in
    /// slot order: the receiver `fn`s and `action`s of the trait, then of each
    /// supertrait.
    pub(crate) fn vtable(&self, t: SymbolId) -> Vec<(SymbolId, String)> {
        let root = TraitRef {
            symbol: t,
            args: self
                .traits
                .get(&t)
                .map(|i| i.generics.symbols().into_iter().map(Ty::Param).collect())
                .unwrap_or_default(),
        };
        self.with_supertraits(&root)
            .iter()
            .filter_map(|tr| self.traits.get(&tr.symbol))
            .flat_map(|info| info.items.iter())
            .filter(|i| i.receiver && matches!(i.kind, ItemKind::Fn | ItemKind::Action))
            .map(|i| (i.symbol, i.name.clone()))
            .collect()
    }

    /// `ty` with each alias expanded and each associated type of a known type
    /// replaced by what its impl says it is.
    pub(crate) fn normalize(&self, ty: &Ty) -> Ty {
        self.normalize_at(ty, 0)
    }

    fn normalize_at(&self, ty: &Ty, depth: u32) -> Ty {
        if depth > 32 {
            return Ty::Unknown;
        }
        ty.map(&mut |t| match t {
            Ty::Assoc(base, name) => {
                let base = self.normalize_at(base, depth + 1);
                Some(match self.assoc_value(&base, name) {
                    Some(value) => self.normalize_at(&value, depth + 1),
                    None => Ty::Assoc(Box::new(base), name.clone()),
                })
            }
            Ty::Named(id, args) => {
                let alias = self.aliases.get(id)?;
                let subst: Subst = alias
                    .params
                    .iter()
                    .copied()
                    .zip(args.iter().map(|a| self.normalize_at(a, depth + 1)))
                    .collect();
                Some(self.normalize_at(&apply(&alias.body, &subst), depth + 1))
            }
            _ => None,
        })
    }

    /// The associated type `name` of the known type `base`, from the impl
    /// that gives it.
    fn assoc_value(&self, base: &Ty, name: &str) -> Option<Ty> {
        if base.has_param() || base.has_unknown() || matches!(base, Ty::Assoc(..)) {
            return None;
        }
        self.impls.iter().find_map(|imp| {
            imp.trait_ref.as_ref()?;
            let item = imp
                .items
                .iter()
                .find(|i| i.kind == ItemKind::Type && i.name == name)?;
            let subst = self.applies(imp, base, 0)?;
            Some(apply(item.ty.as_ref()?, &subst))
        })
    }

    /// Whether the impls `a` and `b` could both apply to one type: they are
    /// both inherent, or of one trait with arguments that agree, and their
    /// targets unify.
    pub(crate) fn overlap(&self, a: &ImplInfo, b: &ImplInfo) -> bool {
        let vars = |p: SymbolId| a.generics.binds(p) || b.generics.binds(p);
        let mut subst = Vec::new();
        let traits_agree = match (&a.trait_ref, &b.trait_ref) {
            (None, None) => true,
            (Some(x), Some(y)) if x.symbol == y.symbol => x
                .args
                .iter()
                .zip(&y.args)
                .all(|(p, q)| unify(p, q, &vars, &mut subst)),
            _ => false,
        };
        traits_agree && unify(&a.target, &b.target, &vars, &mut subst)
    }
}

/// Whether `a` and `b` are one type once the parameters `is_var` names are
/// solved, solving them into `subst`.
fn unify(a: &Ty, b: &Ty, is_var: &dyn Fn(SymbolId) -> bool, subst: &mut Subst) -> bool {
    let a = apply(a, subst);
    let b = apply(b, subst);
    match (&a, &b) {
        (Ty::Param(p), Ty::Param(q)) if p == q => true,
        (Ty::Param(p), _) if is_var(*p) => {
            subst.push((*p, b.clone()));
            true
        }
        (_, Ty::Param(q)) if is_var(*q) => {
            subst.push((*q, a.clone()));
            true
        }
        (Ty::Param(_), _) | (_, Ty::Param(_)) => false,
        _ => {
            let (xs, ys) = (a.parts(), b.parts());
            std::mem::discriminant(&a) == std::mem::discriminant(&b)
                && match (&a, &b) {
                    (Ty::Named(x, _), Ty::Named(y, _)) | (Ty::Dyn(x, _), Ty::Dyn(y, _)) => x == y,
                    (Ty::Native(x), Ty::Native(y)) => x == y,
                    (Ty::Const(x), Ty::Const(y)) => x == y,
                    (Ty::Assoc(_, x), Ty::Assoc(_, y)) => x == y,
                    _ => true,
                }
                && xs.len() == ys.len()
                && xs
                    .into_iter()
                    .zip(ys)
                    .all(|(x, y)| unify(x, y, is_var, subst))
        }
    }
}
