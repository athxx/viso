//! The module graph — the deterministic set of source units and their import edges.
//!
//! A [`SourceUnit`] pairs a module path with an already-parsed tree; the caller
//! supplies the set in memory (there is no filesystem package loader yet — no
//! `Viso.toml` exists, and the graph must be testable standalone). Module identity
//! is the module path alone; there is no in-file `module` header (doc section
//! 21.5.3).
//!
//! [`ModuleGraph::build`] is **deterministic**: units are keyed and iterated in
//! sorted module-path order, never registration order, so the same input set always
//! yields the same graph and the same diagnostics regardless of how the caller
//! assembled it. It resolves each unit's `import` declarations to edges, reporting a
//! duplicate module path as ambiguity ([`E2002`](ResolveErrorKind::AmbiguousModule)),
//! an import of an unknown module as unresolved
//! ([`E2001`](ResolveErrorKind::UnresolvedModule)), and an import cycle as
//! [`E2003`](ResolveErrorKind::CyclicImport).
//!
//! This is a cold-path structure (built once during resolution), so `HashMap`/`Vec`/
//! owned paths are appropriate (AGENTS section 7.2).

use std::collections::BTreeMap;

use crate::ast::{AstNode, CompilationUnit};
use crate::syntax::SyntaxNode;
use crate::syntax::grammar::Parse;
use crate::syntax::span::TextRange;

use super::name::{NameId, NameInterner};

/// A `::`-separated module path, interned segment by segment.
///
/// Ordering is lexicographic over the raw segment ids; because those ids are
/// assigned by insertion order into the shared [`NameInterner`], graph builds that
/// intern paths in the same order compare identically. [`ModuleGraph::build`]
/// re-sorts by the *text* of each path (via the interner) so the result does not
/// depend on interner insertion order at all.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ModulePath {
    segments: Vec<NameId>,
}

impl ModulePath {
    /// Builds a module path from its already-interned segments.
    pub fn new(segments: Vec<NameId>) -> Self {
        Self { segments }
    }

    /// Interns each `::`-free segment of `text` and builds the path.
    pub fn intern(interner: &mut NameInterner, segments: &[&str]) -> Self {
        Self {
            segments: segments.iter().map(|s| interner.intern(s)).collect(),
        }
    }

    /// The path's segments, root first.
    pub fn segments(&self) -> &[NameId] {
        &self.segments
    }

    /// The `::`-joined text of the path, for diagnostics and fingerprint input.
    pub fn display(&self, interner: &NameInterner) -> String {
        let mut out = String::new();
        for (i, &seg) in self.segments.iter().enumerate() {
            if i > 0 {
                out.push_str("::");
            }
            out.push_str(interner.text(seg).unwrap_or("?"));
        }
        out
    }
}

/// One parsed source unit: a module path plus its parse tree.
///
/// The caller owns parsing; the graph only reads the compilation unit's `import`
/// declarations. `path` is the unit's module identity.
pub struct SourceUnit {
    /// The module this unit defines.
    pub path: ModulePath,
    /// The parsed tree (root must be a [`CompilationUnit`]).
    pub parse: Parse,
}

impl SourceUnit {
    /// A source unit from a module path and a parse.
    pub fn new(path: ModulePath, parse: Parse) -> Self {
        Self { path, parse }
    }

    /// The typed compilation-unit view over this unit's parse, if the root casts.
    fn compilation_unit(&self) -> Option<CompilationUnit> {
        CompilationUnit::cast(SyntaxNode::new_root(self.parse.root.clone()))
    }
}

/// A resolution-level diagnostic (module-graph phase).
///
/// Mirrors the parser's `code()`/`message()` style; the shared `Diagnostic` type
/// (AGENTS section 30) is consolidated in a later commit, at which point these fold
/// into it. Codes match the spec's `E2xxx` resolution namespace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolveError {
    /// The source span the error points at, when one is available.
    pub range: Option<TextRange>,
    /// What went wrong.
    pub kind: ResolveErrorKind,
    /// The module-path text the error concerns, for the rendered message.
    pub subject: String,
}

/// The kind of a module-graph resolution error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ResolveErrorKind {
    /// An `import` named a module not present in the graph.
    UnresolvedModule,
    /// Two source units declared the same module path.
    AmbiguousModule,
    /// The import graph contains a cycle.
    CyclicImport,
}

impl ResolveErrorKind {
    /// The stable diagnostic code (spec section 30).
    pub const fn code(self) -> &'static str {
        match self {
            ResolveErrorKind::UnresolvedModule => "E2001",
            ResolveErrorKind::AmbiguousModule => "E2002",
            ResolveErrorKind::CyclicImport => "E2003",
        }
    }

    /// A short, human-readable description.
    pub const fn message(self) -> &'static str {
        match self {
            ResolveErrorKind::UnresolvedModule => "imported module does not exist",
            ResolveErrorKind::AmbiguousModule => "two source units declare the same module",
            ResolveErrorKind::CyclicImport => "modules form an import cycle",
        }
    }
}

/// A dense index into a [`ModuleGraph`]'s sorted module list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ModuleIndex(u32);

impl ModuleIndex {
    /// The raw index into [`ModuleGraph::modules`].
    #[inline]
    pub fn as_usize(self) -> usize {
        self.0 as usize
    }
}

/// A resolved node in the module graph: its identity and its outgoing import edges.
pub struct GraphModule {
    /// The module's path.
    pub path: ModulePath,
    /// The units this module imports, as indices into the graph (unresolved imports
    /// are dropped after emitting [`ResolveErrorKind::UnresolvedModule`]).
    pub imports: Vec<ModuleIndex>,
}

/// The deterministic import graph over a set of [`SourceUnit`]s.
pub struct ModuleGraph {
    /// Modules in sorted module-path order; [`ModuleIndex`] indexes this.
    modules: Vec<GraphModule>,
    /// Diagnostics gathered during the build, in a deterministic order.
    errors: Vec<ResolveError>,
}

impl ModuleGraph {
    /// Builds the graph from an in-memory unit set, deterministically.
    ///
    /// The build is independent of the order `units` arrives in: units are sorted by
    /// module-path text first, duplicates reported as
    /// [`ResolveErrorKind::AmbiguousModule`], import edges resolved by path lookup
    /// (unknown targets → [`ResolveErrorKind::UnresolvedModule`]), then any cycle in
    /// the resulting edge set reported as [`ResolveErrorKind::CyclicImport`].
    pub fn build(units: &[SourceUnit], interner: &NameInterner) -> Self {
        let mut errors = Vec::new();

        // Deterministic order: sort units by module-path text (borrowing, so the
        // caller keeps the units). Ties (duplicate module paths) are ambiguities,
        // reported once per extra unit.
        let mut sorted: Vec<&SourceUnit> = units.iter().collect();
        sorted.sort_by_cached_key(|u| u.path.display(interner));

        // Assign each distinct module path an index; a repeat is an ambiguity.
        let mut index_of: BTreeMap<String, ModuleIndex> = BTreeMap::new();
        let mut kept: Vec<&SourceUnit> = Vec::with_capacity(sorted.len());
        for unit in sorted {
            let key = unit.path.display(interner);
            if index_of.contains_key(&key) {
                errors.push(ResolveError {
                    range: None,
                    kind: ResolveErrorKind::AmbiguousModule,
                    subject: key,
                });
                continue;
            }
            index_of.insert(key, ModuleIndex(kept.len() as u32));
            kept.push(unit);
        }

        // Resolve each unit's import declarations to edges.
        let mut modules: Vec<GraphModule> = Vec::with_capacity(kept.len());
        for unit in &kept {
            let mut imports = Vec::new();
            if let Some(cu) = unit.compilation_unit() {
                for import in cu.imports() {
                    let Some(path_node) = import.path() else {
                        continue;
                    };
                    let target = module_path_text(&path_node);
                    match index_of.get(&target) {
                        Some(&idx) => imports.push(idx),
                        None => errors.push(ResolveError {
                            range: Some(path_node.syntax().text_range()),
                            kind: ResolveErrorKind::UnresolvedModule,
                            subject: target,
                        }),
                    }
                }
            }
            // Edge order deterministic and duplicate-free for stable cycle output.
            imports.sort();
            imports.dedup();
            modules.push(GraphModule {
                path: unit.path.clone(),
                imports,
            });
        }

        let graph = Self { modules, errors };
        graph.detect_cycles(interner)
    }

    /// Runs cycle detection and appends [`ResolveErrorKind::CyclicImport`] for each
    /// module found on a back edge, in sorted index order for determinism.
    fn detect_cycles(mut self, interner: &NameInterner) -> Self {
        #[derive(Clone, Copy, PartialEq)]
        enum Mark {
            Unvisited,
            OnStack,
            Done,
        }
        let n = self.modules.len();
        let mut mark = vec![Mark::Unvisited; n];
        let mut in_cycle = vec![false; n];

        // Iterative DFS from each root in index order (already sorted by path).
        for root in 0..n {
            if mark[root] != Mark::Unvisited {
                continue;
            }
            // Stack of (node, next-child-cursor).
            let mut stack: Vec<(usize, usize)> = vec![(root, 0)];
            mark[root] = Mark::OnStack;
            while let Some(&(node, cursor)) = stack.last() {
                if cursor < self.modules[node].imports.len() {
                    stack.last_mut().unwrap().1 += 1;
                    let next = self.modules[node].imports[cursor].as_usize();
                    match mark[next] {
                        Mark::Unvisited => {
                            mark[next] = Mark::OnStack;
                            stack.push((next, 0));
                        }
                        Mark::OnStack => {
                            // Back edge: every node currently on the stack from
                            // `next` upward is on the cycle.
                            let start = stack.iter().position(|&(m, _)| m == next).unwrap();
                            for &(m, _) in &stack[start..] {
                                in_cycle[m] = true;
                            }
                        }
                        Mark::Done => {}
                    }
                } else {
                    mark[node] = Mark::Done;
                    stack.pop();
                }
            }
        }

        for (i, &on_cycle) in in_cycle.iter().enumerate() {
            if on_cycle {
                self.errors.push(ResolveError {
                    range: None,
                    kind: ResolveErrorKind::CyclicImport,
                    subject: self.modules[i].path.display(interner),
                });
            }
        }
        self
    }

    /// The graph's modules, in sorted module-path order.
    pub fn modules(&self) -> &[GraphModule] {
        &self.modules
    }

    /// The build diagnostics, in deterministic order.
    pub fn errors(&self) -> &[ResolveError] {
        &self.errors
    }

    /// The index of a module by its `::`-joined path text, if present.
    pub fn index_of(&self, path_text: &str, interner: &NameInterner) -> Option<ModuleIndex> {
        self.modules
            .iter()
            .position(|m| m.path.display(interner) == path_text)
            .map(|i| ModuleIndex(i as u32))
    }
}

/// The `::`-joined text of a syntax `ModulePath` node (from an `import` decl).
fn module_path_text(path: &crate::ast::ModulePath) -> String {
    let mut out = String::new();
    for (i, seg) in path.segments().enumerate() {
        if i > 0 {
            out.push_str("::");
        }
        out.push_str(&seg.text());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::{grammar::parse, tokenize};

    fn parse_unit(src: &str) -> Parse {
        parse(&tokenize(src), src)
    }

    fn unit(interner: &mut NameInterner, path: &[&str], src: &str) -> SourceUnit {
        SourceUnit::new(ModulePath::intern(interner, path), parse_unit(src))
    }

    #[test]
    fn build_is_deterministic_regardless_of_unit_order() {
        // Two orderings of the same three units must produce identical module lists.
        let build_order = |order: &[usize]| {
            let mut interner = NameInterner::new();
            let sources = [
                (vec!["a"], "component A { }"),
                (vec!["b"], "import a; component B { }"),
                (vec!["c"], "import b; component C { }"),
            ];
            let units: Vec<SourceUnit> = order
                .iter()
                .map(|&i| unit(&mut interner, &sources[i].0, sources[i].1))
                .collect();
            let graph = ModuleGraph::build(&units, &interner);
            let paths: Vec<String> = graph
                .modules()
                .iter()
                .map(|m| m.path.display(&interner))
                .collect();
            (paths, graph.errors().to_vec())
        };
        let (paths1, errs1) = build_order(&[0, 1, 2]);
        let (paths2, errs2) = build_order(&[2, 1, 0]);
        assert_eq!(paths1, vec!["a", "b", "c"], "sorted by module path");
        assert_eq!(
            paths1, paths2,
            "build order does not change the module list"
        );
        assert!(
            errs1.is_empty() && errs2.is_empty(),
            "acyclic, no diagnostics"
        );
    }

    #[test]
    fn a_missing_import_target_is_e2001() {
        let mut interner = NameInterner::new();
        let units = vec![unit(&mut interner, &["b"], "import a; component B { }")];
        let graph = ModuleGraph::build(&units, &interner);
        assert!(
            graph
                .errors()
                .iter()
                .any(|e| e.kind == ResolveErrorKind::UnresolvedModule && e.subject == "a"),
            "importing an absent module is E2001"
        );
    }

    #[test]
    fn a_duplicate_module_path_is_e2002() {
        let mut interner = NameInterner::new();
        let units = vec![
            unit(&mut interner, &["dup"], "component A { }"),
            unit(&mut interner, &["dup"], "component B { }"),
        ];
        let graph = ModuleGraph::build(&units, &interner);
        assert_eq!(graph.modules().len(), 1, "the duplicate is dropped");
        assert!(
            graph
                .errors()
                .iter()
                .any(|e| e.kind == ResolveErrorKind::AmbiguousModule && e.subject == "dup"),
            "a repeated module path is E2002"
        );
    }

    #[test]
    fn an_import_cycle_is_e2003() {
        let mut interner = NameInterner::new();
        let units = vec![
            unit(&mut interner, &["a"], "import b; component A { }"),
            unit(&mut interner, &["b"], "import a; component B { }"),
        ];
        let graph = ModuleGraph::build(&units, &interner);
        let cyclic: Vec<&str> = graph
            .errors()
            .iter()
            .filter(|e| e.kind == ResolveErrorKind::CyclicImport)
            .map(|e| e.subject.as_str())
            .collect();
        assert!(
            cyclic.contains(&"a") && cyclic.contains(&"b"),
            "both modules on the a<->b cycle are reported as E2003, got {cyclic:?}"
        );
    }

    #[test]
    fn an_acyclic_diamond_reports_no_cycle() {
        // a -> b, a -> c, b -> d, c -> d: a diamond, no cycle.
        let mut interner = NameInterner::new();
        let units = vec![
            unit(&mut interner, &["a"], "import b; import c; component A { }"),
            unit(&mut interner, &["b"], "import d; component B { }"),
            unit(&mut interner, &["c"], "import d; component C { }"),
            unit(&mut interner, &["d"], "component D { }"),
        ];
        let graph = ModuleGraph::build(&units, &interner);
        assert!(
            !graph
                .errors()
                .iter()
                .any(|e| e.kind == ResolveErrorKind::CyclicImport),
            "a diamond is acyclic"
        );
    }
}
