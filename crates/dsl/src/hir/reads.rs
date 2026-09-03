//! Reactive-read collection — which reactive sources an expression observes.
//!
//! A binding's *reactive reads* are the state, input, and computed members it reads while
//! evaluating; they are what the next slice's binding IR turns into precise invalidation
//! edges (`StateId -> Label.text`, spec reactive-binding section), so the frontend must
//! collect them here rather than fall back to runtime dependency tracking. This slice
//! collects the three core reactive kinds — `state`, `input`, `computed` — leaving theme
//! tokens and other sources to their consumer slices.
//!
//! Collection walks the expression's resolved name uses: each path head that resolves to
//! a symbol the environment classifies as a reactive source contributes that symbol. It
//! is decoupled from the HIR node types through the [`ReadEnv`] trait, exactly as
//! [`crate::hir::infer`] and [`crate::hir::effect`] are — the only thing collection needs
//! is *whether a resolved symbol is a reactive source*, which the environment answers, so
//! the collector is testable against a stub before component lowering exists.
//!
//! The result is an ordered [`BTreeSet<SymbolId>`] so a binding's dependency set is
//! deterministic across runs (spec determinism requirement).

use std::collections::{BTreeSet, HashMap};

use crate::ast::{AstNode, Expr, PathExpr};
use crate::resolve::{Resolution, ResolvedRef, SymbolId};
use crate::syntax::{SyntaxKind, SyntaxNode, TextRange};

/// What read-collection needs to know about the surrounding program: whether a resolved
/// symbol is a reactive source (a `state`, `input`, or `computed`). Supplying this as a
/// trait keeps collection independent of the HIR node types, mirroring
/// [`crate::hir::infer::TypeEnv`] and [`crate::hir::effect::EffectEnv`].
pub trait ReadEnv {
    /// The reactive-source symbol a resolution reads, when it names one. `None` for a
    /// local, a callable, a type, or any non-reactive symbol — those contribute no
    /// reactive read.
    fn reactive_source(&self, to: &Resolution) -> Option<SymbolId>;
}

/// Collects the reactive sources an expression reads, given a module's resolved
/// references and a [`ReadEnv`]. Returns the sources in deterministic (symbol) order.
pub fn collect_reads(refs: &[ResolvedRef], env: &dyn ReadEnv, expr: &Expr) -> BTreeSet<SymbolId> {
    let mut index: HashMap<TextRange, Resolution> = HashMap::with_capacity(refs.len());
    for r in refs {
        index.insert(r.range, r.to);
    }
    let mut reads = BTreeSet::new();
    walk(&index, env, expr.syntax(), &mut reads);
    reads
}

/// Recursively walks a syntax node, recording a reactive read for every path head that
/// resolves to a reactive source, and descending into every child.
fn walk(
    index: &HashMap<TextRange, Resolution>,
    env: &dyn ReadEnv,
    node: &SyntaxNode,
    reads: &mut BTreeSet<SymbolId>,
) {
    if node.kind() == SyntaxKind::PathExpr
        && let Some(head) = PathExpr::cast(node.clone()).and_then(|p| p.segments().next())
        && let Some(to) = index.get(&head.text_range())
        && let Some(source) = env.reactive_source(to)
    {
        reads.insert(source);
    }
    for child in node.children() {
        walk(index, env, &child, reads);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::{SyntaxNode, tokenize};

    /// A stub read environment: a set of symbols that are reactive sources. Every other
    /// resolution is non-reactive.
    struct StubEnv {
        sources: BTreeSet<SymbolId>,
    }

    impl ReadEnv for StubEnv {
        fn reactive_source(&self, to: &Resolution) -> Option<SymbolId> {
            match to {
                Resolution::Symbol(id) if self.sources.contains(id) => Some(*id),
                _ => None,
            }
        }
    }

    fn parse_fragment(src: &str) -> (SyntaxNode, Expr) {
        let tokens = tokenize(src);
        let parse = crate::syntax::grammar::parse_expr(&tokens, src);
        let root = SyntaxNode::new_root(parse.root);
        let expr = root
            .descendants()
            .into_iter()
            .find_map(Expr::cast)
            .expect("fragment contains an expression");
        (root, expr)
    }

    /// Every `Ident` token's span in `root`, in source order, keyed by text.
    fn ident_spans(root: &SyntaxNode, name: &str) -> Vec<TextRange> {
        root.descendants_with_tokens()
            .into_iter()
            .filter_map(|e| e.as_token().cloned())
            .filter(|t| t.kind() == SyntaxKind::Ident && t.text() == name)
            .map(|t| t.text_range())
            .collect()
    }

    #[test]
    fn a_bare_state_read_is_collected() {
        let (root, expr) = parse_fragment("count");
        let id = SymbolId::from_parts(1, 0);
        let refs: Vec<ResolvedRef> = ident_spans(&root, "count")
            .into_iter()
            .map(|range| ResolvedRef {
                range,
                to: Resolution::Symbol(id),
            })
            .collect();
        let env = StubEnv {
            sources: BTreeSet::from([id]),
        };
        let reads = collect_reads(&refs, &env, &expr);
        assert_eq!(reads, BTreeSet::from([id]));
    }

    #[test]
    fn a_non_reactive_symbol_is_not_collected() {
        let (root, expr) = parse_fragment("helper");
        let id = SymbolId::from_parts(2, 0);
        let refs: Vec<ResolvedRef> = ident_spans(&root, "helper")
            .into_iter()
            .map(|range| ResolvedRef {
                range,
                to: Resolution::Symbol(id),
            })
            .collect();
        // The environment classifies no symbol as a source.
        let env = StubEnv {
            sources: BTreeSet::new(),
        };
        assert!(collect_reads(&refs, &env, &expr).is_empty());
    }

    #[test]
    fn reads_in_a_compound_expression_are_all_collected() {
        // `a + b * c` reads three sources; `d` is present but non-reactive.
        let (root, expr) = parse_fragment("a + b * c + d");
        let a = SymbolId::from_parts(1, 0);
        let b = SymbolId::from_parts(2, 0);
        let c = SymbolId::from_parts(3, 0);
        let d = SymbolId::from_parts(4, 0);
        let mut refs = Vec::new();
        for (name, id) in [("a", a), ("b", b), ("c", c), ("d", d)] {
            for range in ident_spans(&root, name) {
                refs.push(ResolvedRef {
                    range,
                    to: Resolution::Symbol(id),
                });
            }
        }
        let env = StubEnv {
            sources: BTreeSet::from([a, b, c]),
        };
        let reads = collect_reads(&refs, &env, &expr);
        assert_eq!(reads, BTreeSet::from([a, b, c]));
    }

    #[test]
    fn a_repeated_read_is_recorded_once() {
        let (root, expr) = parse_fragment("count + count");
        let id = SymbolId::from_parts(1, 0);
        let refs: Vec<ResolvedRef> = ident_spans(&root, "count")
            .into_iter()
            .map(|range| ResolvedRef {
                range,
                to: Resolution::Symbol(id),
            })
            .collect();
        let env = StubEnv {
            sources: BTreeSet::from([id]),
        };
        let reads = collect_reads(&refs, &env, &expr);
        assert_eq!(reads.len(), 1);
    }

    #[test]
    fn a_local_read_is_not_reactive() {
        use crate::resolve::{NameInterner, ScopeStack};
        let (root, expr) = parse_fragment("x");
        // Mint a real local slot (there is no free-standing `LocalSlot` constructor —
        // slots are bound through a scope stack).
        let mut interner = NameInterner::new();
        let mut scopes = ScopeStack::new();
        scopes.push();
        let slot = scopes.bind(interner.intern("x"));
        // A local resolution never classifies as a reactive source.
        let refs: Vec<ResolvedRef> = ident_spans(&root, "x")
            .into_iter()
            .map(|range| ResolvedRef {
                range,
                to: Resolution::Local(slot),
            })
            .collect();
        let env = StubEnv {
            sources: BTreeSet::new(),
        };
        assert!(collect_reads(&refs, &env, &expr).is_empty());
    }
}
