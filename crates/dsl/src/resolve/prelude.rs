//! The standard prelude: the types every module names without an import — input
//! geometry and state, the payloads of the standard and built-in widget events, and
//! the adaptive environment a view reads as `env`.
//!
//! It is ordinary `.vs` source (`prelude.vs`) resolved like a module of the `viso`
//! package, so its [`SymbolId`](super::SymbolId)s are durable and every consumer that
//! loads it — the resolver for name lookup, lowering for the declarations' fields and
//! variants — agrees on them. A module's own declarations and its imports shadow a
//! prelude name.

use super::resolver::{ResolvedModule, resolve_standalone};
use super::symbol::{SymbolIdentity, SymbolKind, fingerprint};
use super::{NameInterner, Namespace, SymbolId};
use crate::ast::{AstNode, CompilationUnit};
use crate::syntax::grammar::parse;
use crate::syntax::{SyntaxNode, tokenize};

/// The package and module identity of the prelude's declarations.
pub(crate) const PRELUDE_MODULE: &str = "viso";

const SOURCE: &str = include_str!("prelude.vs");

/// The prelude record a view's `env` is typed by.
pub(crate) fn environment() -> SymbolId {
    fingerprint(SymbolIdentity {
        package: PRELUDE_MODULE,
        module_path: PRELUDE_MODULE,
        kind: SymbolKind::Record,
        decl_path: "Environment",
    })
}

/// The parsed and resolved prelude.
pub(crate) struct Prelude {
    pub(crate) unit: CompilationUnit,
    pub(crate) module: ResolvedModule,
}

impl Prelude {
    /// Parses and resolves the prelude, interning its names into `interner`.
    pub(crate) fn load(interner: &mut NameInterner) -> Prelude {
        let parse = parse(&tokenize(SOURCE), SOURCE);
        debug_assert!(parse.errors.is_empty(), "the prelude does not parse");
        let unit = CompilationUnit::cast(SyntaxNode::new_root(parse.root))
            .expect("a parse root is a compilation unit");
        let module = resolve_standalone(&unit, PRELUDE_MODULE, PRELUDE_MODULE, interner);
        debug_assert!(module.errors.is_empty(), "the prelude does not resolve");
        Prelude { unit, module }
    }

    /// Every type the prelude exports, by name.
    pub(crate) fn types<'a>(
        &'a self,
        interner: &'a NameInterner,
    ) -> impl Iterator<Item = (&'a str, SymbolId)> + 'a {
        self.module
            .table
            .names(Namespace::Type)
            .filter(|(_, symbol)| symbol.exported)
            .filter_map(|(name, symbol)| Some((interner.text(name)?, symbol.id)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_prelude_parses_and_resolves_cleanly() {
        let mut interner = NameInterner::new();
        let parse = parse(&tokenize(SOURCE), SOURCE);
        assert!(parse.errors.is_empty(), "{:?}", parse.errors);
        let prelude = Prelude::load(&mut interner);
        assert!(
            prelude.module.errors.is_empty(),
            "{:?}",
            prelude.module.errors
        );
        let names: Vec<_> = prelude.types(&interner).map(|(n, _)| n).collect();
        for expected in [
            "Point",
            "PointerEvent",
            "Key",
            "AnimationEnd",
            "SliderChanged",
            "Rect",
            "Insets",
            "WindowMetrics",
            "LocalConstraints",
            "SizeClass",
            "Orientation",
            "KeyboardInset",
            "DisplayFeature",
            "PointerPrecision",
            "InputCapabilities",
            "LayoutDirection",
            "Locale",
            "Environment",
        ] {
            assert!(names.contains(&expected), "missing {expected}");
        }
        let types: Vec<_> = prelude.types(&interner).collect();
        assert!(types.contains(&("Environment", environment())));
    }
}
