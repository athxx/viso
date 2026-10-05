//! Native handle ownership: a [`Borrowed`](Ownership::Borrowed) handle is valid
//! only for the call that receives it, so it may be a parameter or a local but
//! no place that outlives the call — a `state`, `input`, `computed`, event
//! payload, record field or `const`, nor a returned value — may hold it
//! (`E6102`).

use viso_behavior::native::{NativeTypeEntry, Natives, Ownership};

use crate::diag::Diagnostic;
use crate::hir::ty::Ty;
use crate::syntax::TextRange;

/// The borrowed native handle type `ty` holds, directly or inside a list,
/// option, tuple, result, range or function type.
fn borrowed_in<'n>(ty: &Ty, natives: &'n Natives) -> Option<&'n NativeTypeEntry> {
    match ty {
        Ty::Native(id) => natives
            .ty_by_id(*id)
            .filter(|t| t.ty.ownership == Ownership::Borrowed),
        Ty::List(t) | Ty::Option(t) | Ty::Range(t) | Ty::RangeInclusive(t) => {
            borrowed_in(t, natives)
        }
        Ty::Result(ok, err) | Ty::Resource(ok, err) | Ty::ResourceState(ok, err) => {
            borrowed_in(ok, natives).or_else(|| borrowed_in(err, natives))
        }
        Ty::Tuple(ts) => ts.iter().find_map(|t| borrowed_in(t, natives)),
        Ty::Fn(ps, ret) => ps
            .iter()
            .chain(std::iter::once(&**ret))
            .find_map(|t| borrowed_in(t, natives)),
        _ => None,
    }
}

/// Reports `E6102` at `at` when a value of type `ty`, held by `place` (such as
/// "a `state`"), holds a borrowed native handle.
pub(crate) fn check_stored(
    ty: &Ty,
    natives: &Natives,
    place: &str,
    at: TextRange,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Some(handle) = borrowed_in(ty, natives) else {
        return;
    };
    let name = handle.path.rsplit("::").next().unwrap_or(&handle.path);
    let mut diagnostic = Diagnostic::error(
        "E6102",
        at,
        format!("{place} cannot hold the borrowed native handle `{name}`"),
    );
    diagnostic.notes.push(format!(
        "`{}` is valid only for the call that receives it; pass it as a parameter \
         or keep it in a local",
        handle.path
    ));
    diagnostics.push(diagnostic);
}
