//! The one frontend behind the three DSL source forms (AGENTS section 21.5).
//!
//! `ui! { … }` enters at the ViewFragment production, `component! { … }` at the
//! ComponentDecl entry, and `view!("x.vs")` at a whole compilation unit. That entry
//! choice is the only thing the forms differ in: from the parse on, all three run
//! the same resolver, the same view lowering to UI IR, the same Binding IR and key
//! passes, and — for the two forms that declare a component — the same typed HIR
//! lowering. A `component!` body is a one-component compilation unit, so its parse
//! is handed to the module frontend exactly as a `.vs` file's is.
//!
//! Every result is a [`Compiled`]: the view template and its compiled bindings,
//! the reactive sources the bindings read, the component's typed HIR when there is
//! one, and every diagnostic any stage raised (spans relative to the source text the
//! caller passed in). The proc-macros and the hot-reload planner are thin callers
//! over these three functions; none re-implements a stage.

use std::collections::BTreeSet;
use std::path::{Component, Path};
use std::rc::Rc;
use std::sync::Arc;

use viso_behavior::native::Natives;

use crate::ast::{
    AstNode, CompilationUnit, ComponentDecl, Expr, Item, LiteralExpr, Member, PathExpr, UnaryExpr,
    ViewFragment,
};
use crate::behavior::{Program, hidden_state, inline_instances};
use crate::diag::{Diagnostic, Severity};
use crate::hir::{ConstValue, DerivedReads, HirComponent, SourceSet, Ty, TypeSchemas, write_backs};
use crate::ir::{
    BindingIr, ComponentLibrary, InstanceSources, KeyIr, LibraryComponent, UiTree, analyze_keys,
    lower_bindings, lower_component_view, lower_fragment_items, lower_view_bindings,
};
use crate::resolve::{
    ModuleGraph, ModulePath, NameInterner, SourceUnit, SymbolId, SymbolIdentity, SymbolKind,
    fingerprint, resolve, resolve_fragment,
};
use crate::syntax::grammar::{Entry, Parse, parse_entry};
use crate::syntax::{GreenNode, SyntaxKind, SyntaxNode, TextRange, TextSize, tokenize};

/// The language version this compiler implements; a package's `Viso.toml` may pin
/// it, and a file never restates it.
pub const LANGUAGE_VERSION: &str = "1.0";

/// Checks a language version a package pins (its `Viso.toml` or lockfile) against
/// the one this compiler implements: `E1001` at `at`, the span of the version in
/// the manifest when the caller has one, when they differ.
pub fn check_language_version(version: &str, at: TextRange) -> Option<Diagnostic> {
    (version != LANGUAGE_VERSION).then(|| {
        let mut error = Diagnostic::error(
            "E1001",
            at,
            format!(
                "unknown or unsupported language version `{version}`; this compiler \
                 implements `{LANGUAGE_VERSION}`"
            ),
        );
        error.notes.push(format!(
            "set `language = \"{LANGUAGE_VERSION}\"` in `Viso.toml`"
        ));
        error
    })
}

/// The package a bare fragment's reactive sources are minted under. The fragment
/// has no module of its own, so every `ui!` and every fragment hot reload shares
/// this anchor, and a source name keeps one identity across both.
pub const FRAGMENT_PACKAGE: &str = "<ui!>";

/// Where a component-declaring source lives: its package and the module path its
/// source path derives (there is no in-file `module` header, section 21.5.3), plus
/// the language version the package pins, if any.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Origin {
    /// The package identity.
    pub package: String,
    /// The module path segments, derived from the source path.
    pub module: Vec<String>,
    /// The language version the package pins; `None` takes [`LANGUAGE_VERSION`].
    pub language: Option<String>,
}

impl Origin {
    /// The origin of the source file at `file` in the package rooted at `root`:
    /// its module path is the file's path below the package's `src/` directory (or
    /// below the root when it lies outside `src/`), extension dropped, with a final
    /// `mod`, `lib` or `main` naming its directory's module rather than one of its
    /// own.
    pub fn for_file(package: &str, root: &Path, file: &Path, language: Option<String>) -> Self {
        Self {
            package: package.to_owned(),
            module: module_path(root, file),
            language,
        }
    }
}

/// The module path of `file` in the package rooted at `root`; see
/// [`Origin::for_file`].
pub fn module_path(root: &Path, file: &Path) -> Vec<String> {
    let relative = file.strip_prefix(root).unwrap_or(file);
    let relative = relative.strip_prefix("src").unwrap_or(relative);
    let mut module: Vec<String> = relative
        .with_extension("")
        .components()
        .filter_map(|part| match part {
            Component::Normal(name) => Some(name.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();
    if module
        .last()
        .is_some_and(|last| matches!(last.as_str(), "mod" | "lib" | "main"))
    {
        module.pop();
    }
    module
}

/// What a reactive source is, as far as mounting it is concerned.
#[derive(Debug, Clone, PartialEq)]
pub enum SourceKind {
    /// A fragment read of a name the surrounding Rust scope supplies.
    External,
    /// A component `state`, with its initializer when it folds to a constant.
    State {
        initial: Option<ConstValue>,
        /// The state's type, declared or inferred.
        ty: Ty,
        /// The span of the state's declaration.
        declared: TextRange,
    },
    /// A component `input`.
    Input,
    /// A component `computed`.
    Computed,
}

/// One reactive source the view may read.
#[derive(Debug, Clone, PartialEq)]
pub struct Source {
    /// The source's name in the DSL text.
    pub name: String,
    /// Its compile-stable identity, the key binding edges refer to it by.
    pub symbol: SymbolId,
    pub kind: SourceKind,
}

/// A compiled source form.
#[derive(Debug, Clone)]
pub struct Compiled {
    /// The typed HIR of the component the source declares; `None` for a fragment,
    /// which declares none.
    pub component: Option<HirComponent>,
    /// The view's static template.
    pub tree: UiTree,
    /// The view's compiled binding edges.
    pub bindings: BindingIr,
    /// The view's keyed-list analysis.
    pub keys: KeyIr,
    /// Every reactive source in scope of the view, in declaration (or, for a
    /// fragment, first-read) order.
    pub sources: Vec<Source>,
    /// The hidden states of the instances a control-flow region mounts, which
    /// each mount of the region content keeps for itself instead of a state
    /// cell of the view.
    pub regional: Vec<Source>,
    /// The type name and span of each node of a fragment that no widget
    /// declares, a component of the surrounding Rust scope, in source order.
    pub rust_components: Vec<(String, TextRange)>,
    /// Every diagnostic, of every severity, from every stage, in stage order.
    pub diagnostics: Vec<Diagnostic>,
    /// Every body of the unit lowered to the Behavior IR.
    pub behavior: Program,
    /// The unit's record and enum declarations.
    pub types: TypeSchemas,
}

impl Compiled {
    /// The diagnostics that make the source uncompilable.
    pub fn errors(&self) -> impl Iterator<Item = &Diagnostic> {
        self.diagnostics
            .iter()
            .filter(|d| d.severity == Severity::Error)
    }

    /// Whether any stage raised an error.
    pub fn has_errors(&self) -> bool {
        self.errors().next().is_some()
    }

    /// The source a binding edge's symbol names, if it is in scope.
    pub fn source(&self, symbol: SymbolId) -> Option<&Source> {
        self.sources.iter().find(|s| s.symbol == symbol)
    }
}

/// Compiles a `ui!` view fragment. Its reactive sources are the value-position
/// path heads, supplied by the surrounding Rust scope; a head that turns out to be
/// a node or loop-local simply yields no edge. A node type no widget declares is
/// a [`NodeKind::Component`](crate::ir::ui_ir::NodeKind::Component) of that
/// scope; a target that has no Rust scope rejects it.
pub fn compile_fragment(source: &str) -> Compiled {
    let parse = parse_entry(&tokenize(source), source, Entry::ViewFragment);
    let root = SyntaxNode::new_root(parse.root.clone());
    let mut diagnostics = parse.errors.clone();
    let fragment =
        ViewFragment::cast(root.clone()).expect("the view-fragment entry roots a ViewFragment");

    let names = path_heads(&root);
    let name_refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let mut interner = NameInterner::new();
    let resolved = resolve_fragment(&fragment, &name_refs, &mut interner, FRAGMENT_PACKAGE);
    diagnostics.extend(resolved.errors.iter().cloned());

    let lowered = lower_fragment_items(fragment.items(), &Natives::standard());
    for (at, reason) in &lowered.unmounted {
        diagnostics.push(Diagnostic::error(
            "E3711",
            *at,
            format!("the component cannot be mounted here: {reason}"),
        ));
    }
    let tree = lowered.tree;
    let env = SourceSet::new(resolved.sources.iter().copied());
    let bindings = lower_bindings(&tree, &root, &resolved.refs, &env);
    let keys = analyze_keys(&tree, &root, &resolved.refs, &env);
    diagnostics.extend(keys.diagnostics.iter().cloned());

    let sources = names
        .into_iter()
        .zip(resolved.sources)
        .map(|(name, symbol)| Source {
            name,
            symbol,
            kind: SourceKind::External,
        })
        .collect();
    Compiled {
        component: None,
        tree,
        bindings,
        keys,
        sources,
        regional: Vec::new(),
        rust_components: lowered.unknown,
        diagnostics,
        behavior: Program::default(),
        types: TypeSchemas::default(),
    }
}

/// Compiles a `component!` body: optional imports, then exactly one component,
/// its `component` keyword optional.
pub fn compile_component(source: &str, origin: &Origin) -> Compiled {
    let parse = parse_entry(&tokenize(source), source, Entry::ComponentEntry);
    // The entry production only restricts what may appear; its tree has the shape
    // of a compilation unit, so the module frontend takes it as one.
    let unit = Parse {
        root: Rc::new(GreenNode::new(
            SyntaxKind::CompilationUnit,
            parse.root.children().to_vec(),
        )),
        errors: parse.errors,
    };
    compile_unit(source, unit, origin, Natives::standard())
}

/// Compiles a `.vs` file for `view!`: the unit's exported component, or its only
/// component when it exports none.
pub fn compile_file(source: &str, origin: &Origin) -> Compiled {
    compile_file_in(source, origin, Natives::standard())
}

/// [`compile_file`] with native paths resolved against `natives` instead of
/// the standard libraries alone.
pub fn compile_file_in(source: &str, origin: &Origin, natives: Arc<Natives>) -> Compiled {
    let parse = parse_entry(&tokenize(source), source, Entry::CompilationUnit);
    compile_unit(source, parse, origin, natives)
}

/// The module frontend over one parsed unit: resolve, lower to typed HIR, pick the
/// component that is mounted, and lower its view.
fn compile_unit(source: &str, parse: Parse, origin: &Origin, natives: Arc<Natives>) -> Compiled {
    let mut diagnostics = parse.errors.clone();
    if let Some(error) = origin
        .language
        .as_deref()
        .and_then(|language| check_language_version(language, TextRange::empty(TextSize::ZERO)))
    {
        diagnostics.push(error);
    }
    let root = SyntaxNode::new_root(parse.root.clone());
    let cu =
        CompilationUnit::cast(root.clone()).expect("every module entry roots a CompilationUnit");

    let mut interner = NameInterner::new();
    let segments: Vec<&str> = origin.module.iter().map(String::as_str).collect();
    let path = ModulePath::intern(&mut interner, &segments);
    let units = vec![SourceUnit::new(path.clone(), parse)];
    let graph = ModuleGraph::build_with(&units, &interner, natives);
    diagnostics.extend(graph.errors().iter().cloned());
    let mut resolved = resolve(&graph, &units, &mut interner, &origin.package);
    let lowered = crate::hir::lower(&graph, &units, &resolved, &mut interner, &origin.package);
    let mut behavior = lowered.behavior;
    let Some(module) = resolved.pop() else {
        return Compiled::empty(diagnostics, behavior);
    };
    diagnostics.extend(module.errors.iter().cloned());
    diagnostics.extend(lowered.diagnostics.iter().cloned());

    let Some(decl) = mounted_component(&cu, source, &mut diagnostics) else {
        return Compiled::empty(diagnostics, behavior);
    };
    let range = decl.syntax().text_range();
    let mut components = lowered.components;
    let Some(mounted) = components.iter().position(|c| c.source_origin == range) else {
        return Compiled::empty(diagnostics, behavior);
    };

    // Every component of the unit is one the mounted view may inline.
    let decls: Vec<ComponentDecl> = component_decls(&cu);
    let decl_of = |c: &HirComponent| {
        decls
            .iter()
            .find(|d| d.syntax().text_range() == c.source_origin)
            .cloned()
    };
    let library = ComponentLibrary::new(
        &module.refs,
        components
            .iter()
            .filter_map(|c| {
                let decl = decl_of(c)?;
                Some(LibraryComponent {
                    schema: &c.schema,
                    write_backs: write_backs(&decl),
                    decl,
                })
            })
            .collect(),
    );
    let root_symbol = components[mounted].schema.symbol;
    let lowered_view = decl
        .view()
        .and_then(|view| view.block())
        .map(|block| lower_component_view(&block, graph.natives(), &library, root_symbol));
    drop(library);
    // The view checker has already reported every unregistered node type.
    let tree = match lowered_view {
        Some(view) => {
            for (at, reason) in view.unmounted {
                diagnostics.push(Diagnostic::error(
                    "E3711",
                    at,
                    format!("the component cannot be mounted here: {reason}"),
                ));
            }
            view.tree
        }
        None => UiTree::default(),
    };
    inline_instances(&mut behavior, root_symbol, &tree);

    let mut sources = component_sources(&components[mounted], &decl);
    let own = sources.len();
    let mut regional = Vec::new();
    let mut instances = Vec::with_capacity(tree.instances.len());
    for instance in &tree.instances {
        let mut inlined = InstanceSources::default();
        let child = components
            .iter()
            .find(|c| c.schema.symbol == instance.component);
        let child_decl = child.and_then(decl_of);
        let (Some(child), Some(child_decl)) = (child, child_decl) else {
            instances.push(inlined);
            continue;
        };
        inlined.inputs = child
            .schema
            .inputs
            .iter()
            .map(|i| i.meta.resolved_symbol)
            .collect();
        let module_path = path.display(&interner);
        for source in component_sources(child, &child_decl) {
            if !matches!(source.kind, SourceKind::State { .. }) {
                continue;
            }
            let name = hidden_state(&instance.identity, &source.name);
            let decl_path = format!("{}.{name}", components[mounted].schema.name);
            let symbol = fingerprint(SymbolIdentity {
                package: &origin.package,
                module_path: &module_path,
                kind: SymbolKind::State,
                decl_path: &decl_path,
            });
            inlined.states.push((source.symbol, symbol));
            let hidden = Source {
                name,
                symbol,
                kind: source.kind,
            };
            if instance.regional {
                regional.push(hidden);
            } else {
                sources.push(hidden);
            }
        }
        instances.push(inlined);
    }
    let hidden = sources.split_off(own);
    let env = SourceSet::new(
        sources
            .iter()
            .chain(&hidden)
            .chain(&regional)
            .map(|s| s.symbol)
            .chain(tree.instances.iter().flat_map(|i| {
                components
                    .iter()
                    .find(|c| c.schema.symbol == i.component)
                    .map(instance_symbols)
                    .unwrap_or_default()
            })),
    );
    let derived = DerivedReads::merged(components.iter().map(|c| &c.schema.derived));
    let bindings = lower_view_bindings(&tree, &root, &module.refs, &env, &derived, &instances);
    let keys = analyze_keys(&tree, &root, &module.refs, &env);
    diagnostics.extend(keys.diagnostics.iter().cloned());
    sources.extend(hidden);

    Compiled {
        component: Some(components.swap_remove(mounted)),
        tree,
        bindings,
        keys,
        sources,
        regional,
        rust_components: Vec::new(),
        diagnostics,
        behavior,
        types: lowered.types,
    }
}

/// Every component the unit declares, exported or not.
fn component_decls(cu: &CompilationUnit) -> Vec<ComponentDecl> {
    cu.items()
        .filter_map(|item| match item {
            Item::Component(decl) => Some(decl),
            Item::Export(export) => match export.declaration() {
                Some(Item::Component(decl)) => Some(decl),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

/// The states and inputs of `component`, the reactive sources an inlined
/// instance's view reads.
fn instance_symbols(component: &HirComponent) -> Vec<SymbolId> {
    let schema = &component.schema;
    schema
        .states
        .iter()
        .map(|s| s.meta.resolved_symbol)
        .chain(schema.inputs.iter().map(|i| i.meta.resolved_symbol))
        .flatten()
        .collect()
}

impl Compiled {
    fn empty(diagnostics: Vec<Diagnostic>, behavior: Program) -> Self {
        Self {
            component: None,
            tree: UiTree::default(),
            bindings: BindingIr::default(),
            keys: KeyIr::default(),
            sources: Vec::new(),
            regional: Vec::new(),
            rust_components: Vec::new(),
            diagnostics,
            behavior,
            types: TypeSchemas::default(),
        }
    }
}

/// The component a unit mounts: its single exported component, else its single
/// component. Anything else is ambiguous or empty and reported.
fn mounted_component(
    cu: &CompilationUnit,
    source: &str,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<ComponentDecl> {
    let mut exported = Vec::new();
    let mut all = Vec::new();
    for item in cu.items() {
        match item {
            Item::Component(decl) => all.push(decl),
            Item::Export(export) => {
                if let Some(Item::Component(decl)) = export.declaration() {
                    exported.push(decl.clone());
                    all.push(decl);
                }
            }
            _ => {}
        }
    }
    let candidates = if exported.is_empty() { all } else { exported };
    match candidates.len() {
        1 => candidates.into_iter().next(),
        0 => {
            diagnostics.push(Diagnostic::error(
                "E2005",
                whole(source),
                "the source declares no component to mount",
            ));
            None
        }
        _ => {
            diagnostics.push(Diagnostic::error(
                "E2006",
                candidates[1].syntax().text_range(),
                "the source declares several components; export the one to mount",
            ));
            None
        }
    }
}

/// The component's reactive sources in declaration order: states (with their
/// constant initializers), inputs, then computeds.
fn component_sources(component: &HirComponent, decl: &ComponentDecl) -> Vec<Source> {
    let schema = &component.schema;
    let initializers: Vec<(String, Option<Expr>)> = decl
        .members()
        .filter_map(|member| match member {
            Member::State(state) => Some((state.name()?.text(), state.initializer())),
            _ => None,
        })
        .collect();
    let mut sources = Vec::new();
    for state in &schema.states {
        let Some(symbol) = state.meta.resolved_symbol else {
            continue;
        };
        let initial = initializers
            .iter()
            .find(|(name, _)| *name == state.name)
            .and_then(|(_, init)| init.as_ref())
            .and_then(|init| fold_constant(init, &state.meta.inferred_type));
        sources.push(Source {
            name: state.name.clone(),
            symbol,
            kind: SourceKind::State {
                initial,
                ty: state.meta.inferred_type.clone(),
                declared: state.meta.source_origin,
            },
        });
    }
    for input in &schema.inputs {
        if let Some(symbol) = input.meta.resolved_symbol {
            sources.push(Source {
                name: input.name.clone(),
                symbol,
                kind: SourceKind::Input,
            });
        }
    }
    for computed in &schema.computeds {
        if let Some(symbol) = computed.meta.resolved_symbol {
            sources.push(Source {
                name: computed.name.clone(),
                symbol,
                kind: SourceKind::Computed,
            });
        }
    }
    sources
}

/// A literal (or negated numeric literal) initializer as a constant of `ty`.
fn fold_constant(expr: &Expr, ty: &Ty) -> Option<ConstValue> {
    let node = expr.syntax();
    match node.kind() {
        SyntaxKind::LiteralExpr => literal_constant(&LiteralExpr::cast(node.clone())?, ty, false),
        SyntaxKind::UnaryExpr => {
            let unary = UnaryExpr::cast(node.clone())?;
            if unary.op()?.kind() != SyntaxKind::Minus {
                return None;
            }
            let operand = unary.operand()?;
            literal_constant(&LiteralExpr::cast(operand.syntax().clone())?, ty, true)
        }
        SyntaxKind::ParenExpr => {
            fold_constant(&node.children().into_iter().find_map(Expr::cast)?, ty)
        }
        _ => None,
    }
}

fn literal_constant(lit: &LiteralExpr, ty: &Ty, negated: bool) -> Option<ConstValue> {
    let token = lit.token()?;
    let text: String = token.text().chars().filter(|&c| c != '_').collect();
    let sign: i128 = if negated { -1 } else { 1 };
    match token.kind() {
        SyntaxKind::IntLiteral => {
            let value = parse_int(&text)?;
            if matches!(ty, Ty::F32 | Ty::F64) {
                Some(ConstValue::Float((sign * value) as f64, ty.clone()))
            } else {
                Some(ConstValue::Int(sign * value, ty.clone()))
            }
        }
        SyntaxKind::FloatLiteral => {
            let value: f64 = text.parse().ok()?;
            Some(ConstValue::Float(
                if negated { -value } else { value },
                ty.clone(),
            ))
        }
        SyntaxKind::TrueKw if !negated => Some(ConstValue::Bool(true)),
        SyntaxKind::FalseKw if !negated => Some(ConstValue::Bool(false)),
        _ => None,
    }
}

fn parse_int(text: &str) -> Option<i128> {
    let (digits, radix) = match text.get(..2) {
        Some("0x" | "0X") => (&text[2..], 16),
        Some("0o" | "0O") => (&text[2..], 8),
        Some("0b" | "0B") => (&text[2..], 2),
        _ => (text, 10),
    };
    i128::from_str_radix(digits, radix).ok()
}

/// Every value-position path head in the tree, first-appearance order,
/// deduplicated: the names a fragment's surrounding scope must supply. A callee
/// (`format(..)`) is not one: a captured state is a value, never a function.
fn path_heads(root: &SyntaxNode) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut names = Vec::new();
    for node in root.descendants() {
        let callee = node.parent().is_some_and(|parent| {
            parent.kind() == SyntaxKind::CallExpr
                && parent.first_child().is_some_and(|first| first == node)
        });
        let Some(path) = PathExpr::cast(node).filter(|_| !callee) else {
            continue;
        };
        if let Some(head) = path.segments().next() {
            let text = head.text();
            if seen.insert(text.clone()) {
                names.push(text);
            }
        }
    }
    names
}

fn whole(source: &str) -> TextRange {
    TextRange::new(TextSize::ZERO, TextSize::new(source.len() as u32))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn origin(module: &[&str]) -> Origin {
        Origin {
            package: "app".to_owned(),
            module: module.iter().map(|s| (*s).to_owned()).collect(),
            language: None,
        }
    }

    fn codes(compiled: &Compiled) -> Vec<&'static str> {
        compiled.errors().map(|d| d.code).collect()
    }

    #[test]
    fn module_paths_derive_from_the_source_path() {
        let root = PathBuf::from("/pkg");
        let path = |file: &str| module_path(&root, &root.join(file));
        assert_eq!(
            path("src/features/home/view.vs"),
            ["features", "home", "view"]
        );
        assert_eq!(path("src/features/home/mod.vs"), ["features", "home"]);
        assert_eq!(path("src/lib.rs"), Vec::<String>::new());
        assert_eq!(path("tests/counter.vs"), ["tests", "counter"]);
        assert_eq!(
            module_path(&root, Path::new("/elsewhere/x.vs")),
            ["elsewhere", "x"]
        );
    }

    #[test]
    fn a_headerless_file_compiles_its_only_component() {
        let src = "component Counter {
  state count = 3;
  view { Text { text: format(\"{}\", count); } }\n}\n";
        let compiled = compile_file(src, &origin(&["counter"]));
        assert!(!compiled.has_errors(), "{:?}", compiled.diagnostics);
        let component = compiled.component.as_ref().unwrap();
        assert_eq!(component.schema.name, "Counter");
        assert_eq!(compiled.sources.len(), 1);
        let SourceKind::State { initial, ty, .. } = &compiled.sources[0].kind else {
            panic!("{:?}", compiled.sources[0].kind);
        };
        assert_eq!(*initial, Some(ConstValue::Int(3, Ty::I64)));
        assert_eq!(*ty, Ty::I64);
        assert_eq!(compiled.bindings.static_edges().count(), 1);
    }

    #[test]
    fn a_binding_through_a_computed_or_a_function_binds_its_states() {
        let src = "component C {
  state a = 1;
  state b = 2;
  state c = 3;
  computed sum: I64 = a + total();
  computed twice: I64 = sum * 2;
  fn total() -> I64 { b }
  view { Text { text: format(\"{}\", twice); } Text { text: format(\"{}\", c); } }\n}\n";
        let compiled = compile_file(src, &origin(&[]));
        assert!(!compiled.has_errors(), "{:?}", compiled.diagnostics);
        let names = |node: u32| {
            let mut names: Vec<&str> = compiled
                .bindings
                .static_edges()
                .filter(|e| e.node.0 == node)
                .map(|e| compiled.source(e.source).unwrap().name.as_str())
                .collect();
            names.sort_unstable();
            names
        };
        let root = compiled.bindings.static_edges().next().unwrap().node.0;
        assert_eq!(names(root), ["a", "b"]);
        assert_eq!(names(root + 1), ["c"]);
        assert!(
            compiled.bindings.static_edges().all(|e| matches!(
                compiled.source(e.source).unwrap().kind,
                SourceKind::State { .. }
            )),
            "every edge names a state"
        );
    }

    #[test]
    fn the_component_keyword_is_optional_inline() {
        let compiled = compile_component("Tiny { view { Text {} } }", &origin(&[]));
        assert!(!compiled.has_errors(), "{:?}", compiled.diagnostics);
        assert_eq!(compiled.component.unwrap().schema.name, "Tiny");
    }

    #[test]
    fn negated_and_float_initializers_fold() {
        let src = "component C {\n  state a = -2;\n  state b = 1.5;\n  state c = true;\n}";
        let compiled = compile_file(src, &origin(&[]));
        let initial: Vec<_> = compiled
            .sources
            .iter()
            .map(|source| match &source.kind {
                SourceKind::State { initial, .. } => initial.clone(),
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(
            initial,
            [
                Some(ConstValue::Int(-2, Ty::I64)),
                Some(ConstValue::Float(1.5, Ty::F64)),
                Some(ConstValue::Bool(true)),
            ]
        );
    }

    #[test]
    fn a_file_without_a_component_is_reported() {
        let compiled = compile_file("record P { x: F32; }", &origin(&[]));
        assert_eq!(codes(&compiled), ["E2005"]);
        assert!(compiled.component.is_none());
    }

    #[test]
    fn several_components_need_one_exported() {
        let src = "component A { }\ncomponent B { }";
        assert_eq!(codes(&compile_file(src, &origin(&[]))), ["E2006"]);
        let exported = "component A { }\nexport component B { }";
        let compiled = compile_file(exported, &origin(&[]));
        assert!(!compiled.has_errors(), "{:?}", compiled.diagnostics);
        assert_eq!(compiled.component.unwrap().schema.name, "B");
    }

    #[test]
    fn a_pinned_language_version_must_match() {
        let mut pinned = origin(&[]);
        pinned.language = Some("2.0".to_owned());
        assert_eq!(codes(&compile_file("component A { }", &pinned)), ["E1001"]);
        pinned.language = Some(LANGUAGE_VERSION.to_owned());
        assert!(!compile_file("component A { }", &pinned).has_errors());
    }

    #[test]
    fn an_unknown_language_version_points_at_the_manifest_span() {
        let at = TextRange::new(TextSize::from(11), TextSize::from(16));
        let error = check_language_version("0.9", at).expect("unsupported");
        assert_eq!((error.code, error.primary), ("E1001", at));
        assert!(error.message.contains("`0.9`"), "{error:?}");
        assert!(check_language_version(LANGUAGE_VERSION, at).is_none());
    }

    #[test]
    fn diagnostics_are_relative_to_the_source_given() {
        let src = "component C {\n  state a = 1;\n  state a = 2;\n}";
        let compiled = compile_file(src, &origin(&[]));
        let error = compiled.errors().next().expect("a duplicate state");
        let at = error.primary.start().to_u32() as usize;
        assert_eq!(&src[at..at + "a = 2".len()], "a = 2", "{error:?}");
    }
}
