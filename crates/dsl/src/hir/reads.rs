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

use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::ast::{AstNode, Expr, PathExpr};
use crate::resolve::{Resolution, ResolvedRef, SymbolId};
use crate::syntax::{SyntaxKind, SyntaxNode, SyntaxToken, TextRange};

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

/// A [`ReadEnv`] backed by an explicit set of reactive-source symbols: a resolution
/// is a reactive read exactly when it names a symbol in the set.
///
/// A component's read environment answers `reactive_source` from its schema, but a
/// bare `ui!` fragment has no schema — its reactive sources are the Rust `state`
/// names the macro captures, minted into symbols by
/// [`crate::resolve::resolve_fragment`]. Feeding those ids here gives the Binding IR
/// and keys passes the same [`ReadEnv`] shape the component path uses, with no
/// hand-rolled environment on the macro side.
#[derive(Debug, Clone, Default)]
pub struct SourceSet {
    sources: BTreeSet<SymbolId>,
}

impl SourceSet {
    /// A source set over the given reactive-source symbols.
    pub fn new(sources: impl IntoIterator<Item = SymbolId>) -> Self {
        Self {
            sources: sources.into_iter().collect(),
        }
    }
}

impl ReadEnv for SourceSet {
    fn reactive_source(&self, to: &Resolution) -> Option<SymbolId> {
        match to {
            Resolution::Symbol(id) if self.sources.contains(id) => Some(*id),
            _ => None,
        }
    }
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

/// [`collect_reads`] over any syntax node — a whole declaration rather than one
/// expression, such as a function member's parameters and body.
pub fn collect_reads_in(
    refs: &[ResolvedRef],
    env: &dyn ReadEnv,
    node: &SyntaxNode,
) -> BTreeSet<SymbolId> {
    let mut index: HashMap<TextRange, Resolution> = HashMap::with_capacity(refs.len());
    for r in refs {
        index.insert(r.range, r.to);
    }
    let mut reads = BTreeSet::new();
    walk(&index, env, node, &mut reads);
    reads
}

/// What each *derived* member of a component — a `computed` or a `fn` — reads once
/// every call is followed: the states and inputs beneath it.
///
/// A binding or a computed that calls a function or reads another computed observes
/// whatever that callee reads, so its invalidation edges must name those states, not
/// the callee (spec reactive-graph and `ComputedNode` sections). The table maps each
/// derived symbol to its transitive base reads; [`DerivedReads::flatten`] rewrites a
/// read set through it. Cycles are harmless here: the closure is a fixpoint, and a
/// computed cycle is reported by the topology pass as `E2105`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DerivedReads {
    of: BTreeMap<SymbolId, BTreeSet<SymbolId>>,
}

impl DerivedReads {
    /// The closure of `direct`, each derived member's direct reads: base sources and
    /// the derived members it mentions.
    pub fn new(direct: BTreeMap<SymbolId, BTreeSet<SymbolId>>) -> Self {
        let mut of: BTreeMap<SymbolId, BTreeSet<SymbolId>> = direct
            .iter()
            .map(|(key, reads)| {
                let base = reads.iter().copied().filter(|r| !direct.contains_key(r));
                (*key, base.collect())
            })
            .collect();
        // Monotone growth over a finite set: each pass adds a callee's base reads to
        // its callers until nothing is added.
        loop {
            let mut changed = false;
            for (key, reads) in &direct {
                for callee in reads.iter().filter(|r| *r != key && direct.contains_key(r)) {
                    let beneath = of[callee].clone();
                    let entry = of.get_mut(key).expect("every key has an entry");
                    for read in beneath {
                        changed |= entry.insert(read);
                    }
                }
            }
            if !changed {
                return Self { of };
            }
        }
    }

    /// The union of several components' tables; derived symbols are unique per member.
    pub fn merged<'a>(tables: impl IntoIterator<Item = &'a DerivedReads>) -> Self {
        let mut of = BTreeMap::new();
        for table in tables {
            of.extend(table.of.iter().map(|(k, v)| (*k, v.clone())));
        }
        Self { of }
    }

    /// Whether `symbol` is a derived member this table knows.
    pub fn is_derived(&self, symbol: SymbolId) -> bool {
        self.of.contains_key(&symbol)
    }

    /// The base reads of derived member `symbol`.
    pub fn of(&self, symbol: SymbolId) -> Option<&BTreeSet<SymbolId>> {
        self.of.get(&symbol)
    }

    /// `reads` with every derived member replaced by its base reads.
    pub fn flatten(&self, reads: impl IntoIterator<Item = SymbolId>) -> BTreeSet<SymbolId> {
        let mut out = BTreeSet::new();
        for read in reads {
            match self.of.get(&read) {
                Some(beneath) => out.extend(beneath.iter().copied()),
                None => {
                    out.insert(read);
                }
            }
        }
        out
    }
}

/// A [`ReadEnv`] that also counts a mention of a derived member as a read, so a
/// collected set can be flattened through [`DerivedReads`].
pub struct WithDerived<'a> {
    /// The environment classifying base reactive sources.
    pub env: &'a dyn ReadEnv,
    /// The derived members a mention of which is a read.
    pub derived: &'a dyn Fn(SymbolId) -> bool,
}

impl ReadEnv for WithDerived<'_> {
    fn reactive_source(&self, to: &Resolution) -> Option<SymbolId> {
        match to {
            Resolution::Symbol(id) if (self.derived)(*id) => Some(*id),
            _ => self.env.reactive_source(to),
        }
    }
}

/// The reactive reads under `node` an effect body records, each at its name: a
/// read inside `untracked(..)` records nothing, and neither does the target of
/// an assignment, which the body writes rather than reads.
pub fn tracked_reads(
    refs: &[ResolvedRef],
    env: &dyn ReadEnv,
    node: &SyntaxNode,
) -> Vec<(SymbolId, SyntaxToken)> {
    let index: HashMap<TextRange, Resolution> = refs.iter().map(|r| (r.range, r.to)).collect();
    let mut reads = Vec::new();
    tracked(&index, env, node, &mut reads);
    reads
}

fn tracked(
    index: &HashMap<TextRange, Resolution>,
    env: &dyn ReadEnv,
    node: &SyntaxNode,
    reads: &mut Vec<(SymbolId, SyntaxToken)>,
) {
    match node.kind() {
        SyntaxKind::CallExpr if crate::hir::effect::is_untracked(node, index) => return,
        SyntaxKind::PathExpr => {
            if let Some(head) = PathExpr::cast(node.clone()).and_then(|p| p.segments().next())
                && let Some(to) = index.get(&head.text_range())
                && let Some(source) = env.reactive_source(to)
            {
                reads.push((source, head));
            }
        }
        SyntaxKind::AssignStmt => {
            // The target's own index expressions are still reads.
            let mut children = node.children().into_iter();
            if let Some(target) = children.next() {
                for inner in target.children() {
                    tracked(index, env, &inner, reads);
                }
            }
            for child in children {
                tracked(index, env, &child, reads);
            }
            return;
        }
        _ => {}
    }
    for child in node.children() {
        tracked(index, env, &child, reads);
    }
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
    fn derived_reads_follow_calls_to_a_fixpoint() {
        let [a, b, f, g, h] = [1, 2, 3, 4, 5].map(|i| SymbolId::from_parts(i, 0));
        // f reads a and g; g reads b and f (a cycle); h reads f.
        let derived = DerivedReads::new(BTreeMap::from([
            (f, BTreeSet::from([a, g])),
            (g, BTreeSet::from([b, f])),
            (h, BTreeSet::from([f])),
        ]));
        for member in [f, g, h] {
            assert_eq!(derived.of(member), Some(&BTreeSet::from([a, b])));
        }
        assert_eq!(derived.flatten([h, a]), BTreeSet::from([a, b]));
        assert_eq!(derived.flatten([b]), BTreeSet::from([b]));
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
