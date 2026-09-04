//! Reverse def→use index and declaration spans (net-new pieces 1 and 2).
//!
//! The `viso-dsl` resolver produces [`ResolvedModule::refs`]: one
//! [`ResolvedRef`] per name *use*, each pointing at what it resolves to. That is
//! the forward direction (use → def). Goto-definition, find-references, and rename
//! all need the *reverse*: given a resolution target, every span that touches it,
//! plus the target's own declaration span.
//!
//! This module builds both from one `ResolvedModule`:
//!
//! - [`ReferenceIndex::def_to_uses`] maps each [`Resolution`] to every source span
//!   that resolves to it, in source order. For a [`Resolution::Local`] the binding
//!   site is included — the resolver records the binding occurrence as a self-ref
//!   (a `let`/parameter/`for`/`node` name resolves to its own slot), so a local's
//!   *first* range is its declaration.
//! - [`ReferenceIndex::decl_span`] maps each [`SymbolId`] to its declaration's
//!   name-token span, taken straight from [`ResolvedModule::decls`] (the resolver
//!   records these at each mint site). A [`SymbolId`] is a position-independent
//!   fingerprint, so this map is the only way back to the definition in source.
//!
//! Cold-path tooling (AGENTS 7.2): plain `HashMap`s over an already-resolved
//! module, built once per open/edit.

use std::collections::HashMap;

use viso_dsl::TextRange;
use viso_dsl::TextSize;
use viso_dsl::resolve::{Resolution, ResolvedModule, SymbolId};

/// A reverse index over one resolved module: every use of each resolution target,
/// plus each declared symbol's definition span.
#[derive(Debug, Default)]
pub struct ReferenceIndex {
    /// Every source span that resolves to a given target, in source order.
    def_to_uses: HashMap<Resolution, Vec<TextRange>>,
    /// Each module symbol's declaration name-token span.
    decl_span: HashMap<SymbolId, TextRange>,
}

impl ReferenceIndex {
    /// Builds the reverse index from a resolved module.
    ///
    /// `refs` is already in source order, so each target's use list comes out
    /// source-ordered without a sort — which is what makes the first range of a
    /// local its binding site.
    pub fn build(resolved: &ResolvedModule) -> Self {
        let mut def_to_uses: HashMap<Resolution, Vec<TextRange>> = HashMap::new();
        for r in &resolved.refs {
            def_to_uses.entry(r.to).or_default().push(r.range);
        }
        let mut decl_span: HashMap<SymbolId, TextRange> =
            HashMap::with_capacity(resolved.decls.len());
        for d in &resolved.decls {
            decl_span.insert(d.id, d.name_range);
        }
        Self {
            def_to_uses,
            decl_span,
        }
    }

    /// The declaration name-token span of a module symbol, if this module declares
    /// it. Returns `None` for a symbol imported from another module (its declaration
    /// lives in a different source).
    pub fn decl_span_of(&self, id: SymbolId) -> Option<TextRange> {
        self.decl_span.get(&id).copied()
    }

    /// The symbol whose declaration name-token span contains `offset`, if any.
    ///
    /// A module symbol's declaration name (e.g. the `count` in `state count = 0;`) is
    /// not itself recorded as a use in `refs` — the resolver records uses, not the
    /// binding occurrence of a module-level symbol. This reverse lookup lets a cursor
    /// resting on the declaration name still resolve to its symbol, so goto/references/
    /// rename work from the definition site as well as from a use.
    pub fn symbol_at(&self, offset: TextSize) -> Option<SymbolId> {
        self.decl_span
            .iter()
            .find(|(_, span)| span.contains(offset))
            .map(|(id, _)| *id)
    }

    /// Every use span of a resolution target, in source order, or an empty slice if
    /// the target is never used.
    pub fn uses_of(&self, res: Resolution) -> &[TextRange] {
        self.def_to_uses.get(&res).map_or(&[], Vec::as_slice)
    }

    /// The declaration span of a resolution target.
    ///
    /// For a [`Resolution::Symbol`] this is the recorded declaration name-token span
    /// (present only when the symbol is declared in this module). For a
    /// [`Resolution::Local`] it is the first recorded span — the binding site, since
    /// the resolver records the binding occurrence before any use.
    pub fn decl_of(&self, res: Resolution) -> Option<TextRange> {
        match res {
            Resolution::Symbol(id) => self.decl_span_of(id),
            Resolution::Local(_) => self.uses_of(res).first().copied(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use viso_dsl::resolve::{ModuleGraph, ModulePath, NameInterner, SourceUnit, resolve};
    use viso_dsl::syntax::grammar::parse;
    use viso_dsl::tokenize;

    /// Resolves a single-module source and returns its `ReferenceIndex`, keeping the
    /// resolved module alive alongside it for span lookups in the test.
    fn index_of(src: &str) -> (ResolvedModule, ReferenceIndex) {
        let parse_result = parse(&tokenize(src), src);
        let mut interner = NameInterner::new();
        let path = ModulePath::intern(&mut interner, &["test"]);
        let units = vec![SourceUnit::new(path, parse_result)];
        let graph = ModuleGraph::build(&units, &interner);
        let mut resolved = resolve(&graph, &units, &mut interner, "test");
        let module = resolved.pop().expect("one module");
        let index = ReferenceIndex::build(&module);
        (module, index)
    }

    #[test]
    fn intra_component_member_reference_maps_back_to_declaration() {
        // `count` is declared as state and referenced by the computed. The index
        // must resolve the computed's use back to the state's declaration span, and
        // list that use under the state symbol.
        let src = "component C {\n  state count = 0;\n  computed doubled = count;\n}\n";
        let (module, index) = index_of(src);
        // Find the resolution of the `count` use inside `doubled`.
        let use_range = {
            // The second occurrence of `count` in source is the use.
            let first = src.find("count").unwrap();
            let second = src[first + 1..].find("count").unwrap() + first + 1;
            second as u32
        };
        let use_res = module
            .refs
            .iter()
            .find(|r| r.range.start().to_u32() == use_range)
            .map(|r| r.to)
            .expect("the `count` use resolves");
        let decl = index.decl_of(use_res).expect("declaration span");
        // The declaration span covers the first `count` (the state name).
        assert_eq!(decl.start().to_u32(), src.find("count").unwrap() as u32);
        // And that use appears in the symbol's use list.
        assert!(
            index
                .uses_of(use_res)
                .iter()
                .any(|r| r.start().to_u32() == use_range)
        );
    }

    #[test]
    fn unknown_target_has_no_uses() {
        let src = "component C {\n  state count = 0;\n}\n";
        let (_module, index) = index_of(src);
        // A local slot that was never bound resolves to nothing.
        let bogus = Resolution::Symbol(SymbolId::from_parts(0xdead, 0xbeef));
        assert!(index.uses_of(bogus).is_empty());
        assert!(index.decl_of(bogus).is_none());
    }
}
