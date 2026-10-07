//! `StableKey` (§80.1): which types may identify a resource load or a keyed
//! view row. Integers, `Bool`, `Char`, `String` and `Unit` are stable keys, and
//! so is a tuple, record, `Option` or enum all of whose members are; a float,
//! a list, a map, a handle or a function is not.

use std::collections::BTreeSet;

use crate::hir::infer::{TypeEnv, VariantPayload};
use crate::hir::ty::Ty;
use crate::resolve::SymbolId;

/// Why `ty` is not a `StableKey`: the member that breaks it, spelled from the
/// key's type (`K.b`, `.1`, `E::B.0`), and its reason. `Ok` when it is one.
pub(crate) fn stable_key(env: &dyn TypeEnv, ty: &Ty) -> Result<(), String> {
    let mut path = String::new();
    check(env, ty, &mut BTreeSet::new(), &mut path).map_err(|why| {
        if path.is_empty() {
            format!("it {why}")
        } else {
            format!("its member `{path}` {why}")
        }
    })
}

fn check(
    env: &dyn TypeEnv,
    ty: &Ty,
    visiting: &mut BTreeSet<SymbolId>,
    path: &mut String,
) -> Result<(), &'static str> {
    match ty {
        Ty::Bool
        | Ty::I8
        | Ty::I16
        | Ty::I32
        | Ty::I64
        | Ty::U8
        | Ty::U16
        | Ty::U32
        | Ty::U64
        | Ty::Char
        | Ty::String
        | Ty::Unit
        | Ty::InferInt
        | Ty::Unknown
        | Ty::Never => Ok(()),
        Ty::F32 | Ty::F64 | Ty::InferFloat => {
            Err("is a float, whose equality is not an identity (`NaN`, `-0.0`)")
        }
        Ty::Tuple(items) => items.iter().enumerate().try_for_each(|(i, t)| {
            let len = path.len();
            path.push_str(&format!(".{i}"));
            check(env, t, visiting, path)?;
            path.truncate(len);
            Ok(())
        }),
        Ty::Option(t) => check(env, t, visiting, path),
        Ty::Named(id, ..) => {
            if !visiting.insert(*id) {
                return Ok(());
            }
            let name = env.type_name(*id).unwrap_or("<named>").to_string();
            let mut members: Vec<(String, Ty)> = Vec::new();
            if let Some(fields) = env.record_fields(*id) {
                members.extend(
                    fields
                        .iter()
                        .map(|f| (format!("{name}.{}", f.name), f.ty.clone())),
                );
            } else if let Some(variants) = env.enum_variants(*id) {
                for v in variants {
                    let at = format!("{name}::{}", v.name);
                    match &v.payload {
                        VariantPayload::Unit => {}
                        VariantPayload::Tuple(tys) => members.extend(
                            tys.iter()
                                .enumerate()
                                .map(|(i, t)| (format!("{at}.{i}"), t.clone())),
                        ),
                        VariantPayload::Record(fields) => members.extend(
                            fields
                                .iter()
                                .map(|f| (format!("{at}.{}", f.name), f.ty.clone())),
                        ),
                    }
                }
            } else {
                return Err("is no record or enum the package declares");
            }
            for (member, ty) in members {
                let len = path.len();
                path.clear();
                path.push_str(&member);
                check(env, &ty, visiting, path)?;
                path.truncate(len);
            }
            visiting.remove(id);
            Ok(())
        }
        _ => Err("is no integer, `Bool`, `Char`, `String`, or tuple, record or enum of them"),
    }
}
