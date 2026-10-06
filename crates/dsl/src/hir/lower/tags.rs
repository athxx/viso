//! Game tags: the package's one `@derive(GameTag)` enum is the type of every
//! tag a `viso::game` native takes (`SchemaTy::Tag`), as in
//! `SpawnDesc::player().tag(..)` and `world.query(..)`; a package deriving
//! none uses `viso::game::GameTag`. A second deriving enum is `E2202`, and a
//! tag enum has at most 64 variants, one bit each (`E2201`).

use viso_behavior::game::{GameTag, Tag};
use viso_behavior::native::NativeId;

use crate::diag::{Diagnostic, Related};
use crate::hir::infer::TypeEnv;
use crate::resolve::SymbolId;
use crate::syntax::TextRange;

use super::{Declarations, ModuleEnv, ModuleScope, Ty};

/// One enum of the package deriving `GameTag`.
pub(super) struct TagDecl {
    symbol: SymbolId,
    /// The module declaring it.
    module: Option<usize>,
    /// The derive's name.
    at: TextRange,
}

/// Records `symbol`, whose `@derive` names `GameTag` at `at`.
pub(super) fn collect(
    symbol: SymbolId,
    at: TextRange,
    scope: &ModuleScope,
    decls: &mut Declarations,
) {
    decls.tag_derives.push(TagDecl {
        symbol,
        module: scope.home,
        at,
    });
}

/// The type of the package's game tags: its first `@derive(GameTag)` enum,
/// or `viso::game::GameTag`.
pub(super) fn tag_type(decls: &Declarations) -> Ty {
    decls
        .tag_derives
        .first()
        .map_or(Ty::Native(NativeId::of(GameTag::PATH)), |t| {
            Ty::named(t.symbol)
        })
}

/// Checks the `GameTag` derive of `symbol` at `at`: only the package's first
/// tag enum derives it (`E2202`), and it has at most [`Tag::MAX`] variants
/// (`E2201`).
pub(super) fn check(
    symbol: SymbolId,
    at: TextRange,
    env: &ModuleEnv<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let tags = &env.decls.tag_derives;
    let Some(index) = tags.iter().position(|t| t.symbol == symbol && t.at == at) else {
        return;
    };
    if index > 0 {
        let first = &tags[0];
        let mut diagnostic = Diagnostic::error(
            "E2202",
            at,
            "the package already derives its game tags: one `GameTag` enum names every tag",
        );
        if first.module == tags[index].module {
            diagnostic.related.push(Related::new(
                first.at,
                "the tag enum derives `GameTag` here",
            ));
        }
        diagnostics.push(diagnostic);
        return;
    }
    let variants = env.enum_variants(symbol).map_or(0, <[_]>::len);
    if variants > Tag::MAX as usize {
        diagnostics.push(Diagnostic::error(
            "E2201",
            at,
            format!(
                "`GameTag` derives for at most {} variants, one bit each; this enum has {variants}",
                Tag::MAX
            ),
        ));
    }
}
