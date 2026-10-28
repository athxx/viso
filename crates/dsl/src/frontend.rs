//! The one frontend behind the three DSL source forms.
//!
//! `ui! { … }` enters at the ViewFragment production, `component! { … }` at the
//! ComponentDecl entry, and `view!("x.vs")` at a whole compilation unit. That entry
//! choice is the only thing the forms differ in: from the parse on, all three run
//! the same resolver, typed HIR, view lowering to UI IR, Binding IR and key passes.
//! A `component!` body is a one-component compilation unit, so its parse is handed
//! to the module frontend exactly as a `.vs` file's is; a fragment compiles as the
//! view of a component that declares nothing else.
//!
//! Every result is a [`Compiled`]: the view template and its compiled bindings,
//! the reactive sources the bindings read, the component's typed HIR when there is
//! one, and every diagnostic any stage raised (spans relative to the source text the
//! caller passed in). The proc-macros and the hot-reload planner are thin callers
//! over these three functions; none re-implements a stage.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use viso_behavior::native::Natives;

use crate::ast::{
    AstNode, CompilationUnit, ComponentDecl, Expr, Item, LiteralExpr, Member, UnaryExpr,
};
use crate::behavior::{Program, hidden_state, inline_instances};
use crate::diag::{Diagnostic, Severity};
use crate::hir::style::{StyleBook, StyleBooks};
use crate::hir::{
    CheckedShader, ConstValue, DerivedReads, HirComponent, Migrator, ShaderPaint, SourceSet,
    TargetProfile, Ty, TypeSchemas, persists, write_backs,
};
use crate::ir::{
    BindingIr, ComponentLibrary, Consts, DirtyClass, InstanceSources, KeyIr, LibraryComponent,
    LibraryUnit, LoweredView, UiTree, ViewUnits, analyze_keys, lower_component_view,
    lower_view_reactivity, source_invalidations,
};
use crate::resolve::{
    ModuleGraph, ModulePath, NameInterner, ResolvedModule, SourceUnit, SymbolId, SymbolIdentity,
    SymbolKind, fingerprint,
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

/// The package a fragment compiles under: it has no module of its own.
const FRAGMENT_PACKAGE: &str = "<ui!>";

/// Where a component-declaring source lives: its package and the module path its
/// source path derives (there is no in-file `module` header), plus
/// the language version the package pins, if any, and where the package's other
/// modules are read from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Origin {
    /// The package identity.
    pub package: String,
    /// The module path segments, derived from the source path.
    pub module: Vec<String>,
    /// The language version the package pins; `None` takes [`LANGUAGE_VERSION`].
    pub language: Option<String>,
    /// Where the modules the source imports are read from.
    pub siblings: Siblings,
}

/// Where a one-file compile reads the package's other modules from: the source
/// imports them, so each it imports, directly or through another, compiles with
/// it, while the rest of the package is never read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Siblings {
    /// Nowhere: an import of another of the package's modules is unresolved.
    #[default]
    None,
    /// The `.vs` files of the package rooted at the directory.
    Dir(PathBuf),
    /// Each module's path and source.
    Memory(Arc<[(Vec<String>, String)]>),
}

impl Siblings {
    /// The file and source of the package module `module`, if there is one.
    fn source(&self, module: &[String]) -> Option<(Option<PathBuf>, String)> {
        match self {
            Siblings::None => None,
            Siblings::Dir(root) => module_files(root, module).into_iter().find_map(|path| {
                let source = std::fs::read_to_string(&path).ok()?;
                Some((Some(path), source))
            }),
            Siblings::Memory(modules) => modules
                .iter()
                .find(|(path, _)| path == module)
                .map(|(_, source)| (None, source.clone())),
        }
    }
}

/// The files that would declare `module` in the package rooted at `root`, in
/// the order [`module_path`] prefers none of: `a/b.vs`, then `a/b/mod.vs`;
/// the root module's `lib.vs`, `main.vs` or `mod.vs`.
fn module_files(root: &Path, module: &[String]) -> Vec<PathBuf> {
    let mut dir = root.join(crate::package::SOURCE_ROOT);
    let ext = crate::package::SOURCE_EXTENSION;
    match module.split_last() {
        None => ["lib", "main", "mod"]
            .iter()
            .map(|name| dir.join(format!("{name}.{ext}")))
            .collect(),
        Some((last, parents)) => {
            dir.extend(parents);
            vec![
                dir.join(format!("{last}.{ext}")),
                dir.join(last).join(format!("mod.{ext}")),
            ]
        }
    }
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
            siblings: Siblings::Dir(root.to_path_buf()),
        }
    }
}

/// The root of the package whose source `file` declares the module `module`:
/// the directory whose `src/` holds it with that module path, else the first
/// ancestor that derives the path.
pub fn package_root(file: &Path, module: &[String]) -> Option<PathBuf> {
    let derives = |root: &Path| module_path(root, file) == module;
    let sources = file
        .ancestors()
        .skip(1)
        .find(|root| file.starts_with(root.join(crate::package::SOURCE_ROOT)) && derives(root));
    sources
        .or_else(|| file.ancestors().skip(1).find(|root| derives(root)))
        .map(Path::to_path_buf)
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
    /// The typed HIR of the component the source declares, for a fragment the
    /// stateless one it compiles as; `None` when the source does not compile
    /// that far.
    pub component: Option<HirComponent>,
    /// The view's static template.
    pub tree: UiTree,
    /// The view's compiled binding edges.
    pub bindings: BindingIr,
    /// What a write to each reactive source the view reads invalidates, in
    /// [`Compiled::sources`] order then first-read order: the classes of the
    /// properties it binds, and `STRUCTURE` where it decides a region.
    pub invalidations: Vec<(SymbolId, DirtyClass)>,
    /// The view's keyed-list analysis.
    pub keys: KeyIr,
    /// Every reactive source in scope of the view, in declaration order.
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
    /// The unit's `@migrate` functions.
    pub migrators: Vec<Migrator>,
    /// The unit's shaders, each with its program when it checked cleanly.
    pub shaders: Vec<CheckedShader>,
    /// Every value the unit's views paint a node with a shader by.
    pub paints: Vec<ShaderPaint>,
    /// The assets the unit names, by identity, once each.
    pub assets: Vec<crate::assets::AssetUse>,
    /// Where the caller's source starts in the text it compiled as: `0` but
    /// for a fragment, which compiles inside the component it is the view of.
    pub base: TextSize,
    /// The package's other modules the source imports, directly or through
    /// another, in the order they were read: what a change to recompiles it.
    pub imported: Vec<Imported>,
    /// The native libraries its native paths resolved against, which its
    /// behavior links to.
    pub natives: Arc<Natives>,
}

/// A module of the package a compiled source imports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Imported {
    /// Its module path.
    pub module: Vec<String>,
    /// The file it was read from, `None` for a source held in memory.
    pub path: Option<PathBuf>,
}

impl Compiled {
    /// `range`, a span of the text the source compiled as, in the offsets of
    /// the source the caller passed in.
    pub fn authored(&self, range: TextRange) -> TextRange {
        shift_back(range, self.base)
    }

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

/// The component a `ui!` fragment compiles as, the fragment its view: the
/// text before the fragment's own.
const FRAGMENT_HEAD: &str = "component UiFragment__ {\nview {\n";
/// The text after the fragment's own.
const FRAGMENT_TAIL: &str = "\n}\n}\n";

/// Compiles a `ui!` view fragment as the view of a component that declares
/// nothing else, through the module frontend a `component!` body runs: it
/// resolves, types and lowers exactly as that view would. It has no state, so a
/// value name it does not declare itself is unresolved. A node type no widget
/// declares is a
/// [`NodeKind::Component`](crate::ir::ui_ir::NodeKind::Component) of the
/// surrounding Rust scope; a target that has no Rust scope rejects it.
///
/// Its diagnostics and [`Compiled::rust_components`] span the fragment's own
/// text; every other span is one [`Compiled::authored`] maps there.
pub fn compile_fragment(source: &str) -> Compiled {
    let parse = parse_entry(&tokenize(source), source, Entry::ViewFragment);
    let diagnostics = parse.diagnostics();
    if diagnostics.iter().any(|d| d.severity == Severity::Error) {
        return Compiled::empty(diagnostics, Program::default(), Vec::new());
    }
    let text = format!("{FRAGMENT_HEAD}{source}{FRAGMENT_TAIL}");
    let parse = parse_entry(&tokenize(&text), &text, Entry::CompilationUnit);
    let origin = Origin {
        package: FRAGMENT_PACKAGE.to_owned(),
        ..Origin::default()
    };
    let profile = TargetProfile {
        rust_scope: true,
        ..TargetProfile::default()
    };
    let mut compiled = compile_unit(&text, parse, &origin, Natives::standard(), profile, None);
    let base = TextSize::new(FRAGMENT_HEAD.len() as u32);
    compiled.base = base;
    let authored = |range: TextRange| shift_back(range, base);
    for diagnostic in &mut compiled.diagnostics {
        diagnostic.primary = authored(diagnostic.primary);
        for related in &mut diagnostic.related {
            related.range = authored(related.range);
        }
        for edit in diagnostic.fixes.iter_mut().flat_map(|fix| &mut fix.edits) {
            edit.range = authored(edit.range);
        }
    }
    for (_, at) in &mut compiled.rust_components {
        *at = authored(*at);
    }
    compiled
}

/// `range`, which spans text from `base` on, in that text's own offsets; a
/// span before `base` is the empty one at its start.
fn shift_back(range: TextRange, base: TextSize) -> TextRange {
    let back = |at: TextSize| at.saturating_sub(base);
    TextRange::new(back(range.start()), back(range.end()))
}

/// Compiles a `component!` body: optional imports, then exactly one component,
/// its `component` keyword optional.
pub fn compile_component(source: &str, origin: &Origin) -> Compiled {
    compile_component_for(
        source,
        origin,
        Natives::standard(),
        TargetProfile::default(),
    )
}

/// [`compile_component`] with native paths resolved against `natives`, for
/// the targets and build `profile` describes.
pub fn compile_component_for(
    source: &str,
    origin: &Origin,
    natives: Arc<Natives>,
    profile: TargetProfile,
) -> Compiled {
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
    compile_unit(source, unit, origin, natives, profile, None)
}

/// Compiles a `.vs` file for `view!`: the unit's exported component, or its only
/// component when it exports none.
pub fn compile_file(source: &str, origin: &Origin) -> Compiled {
    compile_file_in(source, origin, Natives::standard())
}

/// [`compile_file`] with native paths resolved against `natives` instead of
/// the standard libraries alone.
pub fn compile_file_in(source: &str, origin: &Origin, natives: Arc<Natives>) -> Compiled {
    compile_file_for(source, origin, natives, TargetProfile::default())
}

/// [`compile_file_in`] over `parse`, the parse of `source` the caller
/// already holds.
pub fn compile_parsed_in(
    source: &str,
    parse: Parse,
    origin: &Origin,
    natives: Arc<Natives>,
) -> Compiled {
    compile_unit(
        source,
        parse,
        origin,
        natives,
        TargetProfile::default(),
        None,
    )
}

/// [`compile_file_in`] for the targets and build `profile` describes.
pub fn compile_file_for(
    source: &str,
    origin: &Origin,
    natives: Arc<Natives>,
    profile: TargetProfile,
) -> Compiled {
    let parse = parse_entry(&tokenize(source), source, Entry::CompilationUnit);
    compile_unit(source, parse, origin, natives, profile, None)
}

/// [`compile_file_for`] mounting the component named `component`, exported
/// or not, whatever else the file declares.
pub fn compile_mount_for(
    source: &str,
    origin: &Origin,
    natives: Arc<Natives>,
    profile: TargetProfile,
    component: &str,
) -> Compiled {
    let parse = parse_entry(&tokenize(source), source, Entry::CompilationUnit);
    compile_unit(source, parse, origin, natives, profile, Some(component))
}

/// A component a `.vs` file declares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclaredComponent {
    pub name: String,
    pub exported: bool,
    /// Its declaration's span.
    pub range: TextRange,
    /// The inputs it declares without a default, which a use must give.
    pub required: Vec<String>,
}

/// The components `source`, a `.vs` file, declares, in source order.
pub fn declared_components(source: &str) -> Vec<DeclaredComponent> {
    let parse = parse_entry(&tokenize(source), source, Entry::CompilationUnit);
    let Some(cu) = CompilationUnit::cast(SyntaxNode::new_root(parse.root)) else {
        return Vec::new();
    };
    let declared = |decl: ComponentDecl, exported| {
        let required = decl
            .members()
            .filter_map(|member| match member {
                Member::Input(input) if input.default().is_none() => {
                    Some(input.name()?.text().trim_start_matches("r#").to_owned())
                }
                _ => None,
            })
            .collect();
        Some(DeclaredComponent {
            name: decl.name()?.text(),
            exported,
            range: decl.syntax().text_range(),
            required,
        })
    };
    cu.items()
        .filter_map(|item| match item {
            Item::Component(decl) => declared(decl, false),
            Item::Export(export) => match export.declaration()? {
                Item::Component(decl) => declared(decl, true),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

/// [`compile_file_for`] over `parse`, a parse of `source` with the
/// compilation-unit entry kept current across edits
/// ([`IncrementalParse`](crate::syntax::IncrementalParse)):
/// a dev session reparses only the edited unit, and the rest of the pipeline
/// runs as for a fresh parse.
pub fn compile_parsed_for(
    source: &str,
    parse: Parse,
    origin: &Origin,
    natives: Arc<Natives>,
    profile: TargetProfile,
) -> Compiled {
    compile_unit(source, parse, origin, natives, profile, None)
}

/// The module frontend over one parsed unit: resolve, lower to typed HIR, pick the
/// component that is mounted, and lower its view.
fn compile_unit(
    source: &str,
    parse: Parse,
    origin: &Origin,
    natives: Arc<Natives>,
    profile: TargetProfile,
    mount: Option<&str>,
) -> Compiled {
    let mut diagnostics = parse.diagnostics();
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
    let siblings = imported_modules(&cu, &origin.module, &origin.siblings);
    let imported: Vec<Imported> = siblings.iter().map(|s| s.imported.clone()).collect();
    let mut units = vec![SourceUnit::new(path.clone(), parse)];
    for sibling in &siblings {
        let segments: Vec<&str> = sibling.imported.module.iter().map(String::as_str).collect();
        let path = ModulePath::intern(&mut interner, &segments);
        units.push(SourceUnit::new(path, sibling.parse.clone()));
    }
    let graph = ModuleGraph::build_with(&units, &interner, Arc::clone(&natives), &origin.package);
    let Some(index) = graph.index_of(&path.display(&interner), &interner) else {
        diagnostics.extend(graph.errors().iter().cloned());
        let mut compiled = Compiled::empty(diagnostics, Program::default(), Vec::new());
        compiled.imported = imported;
        return compiled;
    };
    diagnostics.extend(graph.module_errors(index).cloned());
    diagnostics.extend(graph.graph_errors().cloned());
    let (resolved, prelude) =
        crate::resolve::resolve_in(&graph, &mut units, &mut interner, &origin.package);
    // Resolution re-tags the unit's module qualifiers; every later pass reads
    // its tree.
    let root = SyntaxNode::new_root(units[0].parse.root.clone());
    let cu =
        CompilationUnit::cast(root.clone()).expect("every module entry roots a CompilationUnit");
    let rust_scope = profile.rust_scope;
    let lowered = crate::hir::lower_with(
        &graph,
        &units,
        &resolved,
        &mut interner,
        &origin.package,
        profile,
        &prelude,
    );
    let mut behavior = lowered.behavior;
    let shaders = lowered.shaders;
    let paints = lowered.paints;
    let assets = lowered.assets;
    // A module the source imports that does not compile fails it too: each of
    // its errors at the import that reads the module, pointing at where it is.
    for (i, module) in graph.modules().iter().enumerate() {
        let text = module.path.display(&interner);
        let Some(sibling) = siblings
            .iter()
            .find(|s| s.imported.module.join("::") == text)
        else {
            continue;
        };
        let lowered_errors = lowered
            .module_diagnostics
            .get(i)
            .map_or(&[][..], |range| &lowered.diagnostics[range.clone()]);
        let errors = sibling
            .parse
            .diagnostics()
            .into_iter()
            .chain(
                graph
                    .index_of(&text, &interner)
                    .into_iter()
                    .flat_map(|at| graph.module_errors(at).cloned()),
            )
            .chain(
                resolved
                    .get(i)
                    .into_iter()
                    .flat_map(|m| m.errors.iter().cloned()),
            )
            .chain(lowered_errors.iter().cloned())
            .filter(|d| d.severity == Severity::Error);
        for error in errors {
            diagnostics.push(imported_error(error, &text, sibling.via));
        }
    }
    let consts = graph_consts(&graph, &units, &resolved, &interner);
    let graph_sources = graph_units(&graph, &units, &resolved, &interner);
    let module = &resolved[index.as_usize()];
    diagnostics.extend(module.errors.iter().cloned());
    if let Some(range) = lowered.module_diagnostics.get(index.as_usize()) {
        diagnostics.extend(lowered.diagnostics[range.clone()].iter().cloned());
    }

    let empty = |diagnostics, behavior, shaders| {
        let mut compiled = Compiled::empty(diagnostics, behavior, shaders);
        compiled.imported = imported.clone();
        compiled
    };
    let Some(decl) = mounted_component(&cu, source, mount, &mut diagnostics) else {
        return empty(diagnostics, behavior, shaders);
    };
    let range = decl.syntax().text_range();
    let mut components = lowered.components;
    let own = |c: &HirComponent| module.decls.iter().any(|d| d.id == c.schema.symbol);
    let Some(mounted) = components
        .iter()
        .position(|c| own(c) && c.source_origin == range)
    else {
        return empty(diagnostics, behavior, shaders);
    };

    // Every component of the graph is one the mounted view may inline.
    let library = graph_library(&graph_sources, components.iter(), consts);
    let decl_of = |c: &HirComponent| {
        graph_sources.iter().flatten().find_map(|u| {
            u.module
                .decls
                .iter()
                .any(|d| d.id == c.schema.symbol)
                .then(|| {
                    component_decls(&u.cu)
                        .into_iter()
                        .find(|d| d.syntax().text_range() == c.source_origin)
                })?
        })
    };
    let root_symbol = components[mounted].schema.symbol;
    let lowered_view = decl.view().and_then(|view| view.block()).map(|block| {
        lower_component_view(&block, graph.natives(), &library, root_symbol, rust_scope)
    });
    drop(library);
    // The view checker has already reported every unregistered node type, but
    // for one of the Rust scope.
    let (tree, rust_components) = match lowered_view {
        Some(view) => {
            diagnostics.extend(view_diagnostics(&view));
            let rust = if rust_scope { view.unknown } else { Vec::new() };
            (view.tree, rust)
        }
        None => (UiTree::default(), Vec::new()),
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
    let units = view_units(&graph_sources);
    let (bindings, by_source) = lower_view_reactivity(&tree, &units, &env, &derived, &instances);
    let keys = analyze_keys(&tree, &units, &env);
    diagnostics.extend(keys.diagnostics.iter().cloned());
    sources.extend(hidden);
    let invalidations = ordered_invalidations(&sources, &regional, by_source);

    Compiled {
        component: Some(components.swap_remove(mounted)),
        tree,
        bindings,
        invalidations,
        keys,
        sources,
        regional,
        rust_components,
        diagnostics,
        behavior,
        types: lowered.types,
        migrators: lowered.migrators,
        shaders,
        paints,
        assets,
        base: TextSize::ZERO,
        imported,
        natives,
    }
}

/// A module of the package a compiled source imports, read and parsed.
struct Sibling {
    imported: Imported,
    parse: Parse,
    /// The import of the compiled source that reads it, directly or through
    /// other modules.
    via: TextRange,
}

/// Every module of the package `siblings` holds that `cu`, the unit of
/// `module`, imports, directly or through another, each read once.
fn imported_modules(cu: &CompilationUnit, module: &[String], siblings: &Siblings) -> Vec<Sibling> {
    if matches!(siblings, Siblings::None) {
        return Vec::new();
    }
    let mut out: Vec<Sibling> = Vec::new();
    let mut tried: Vec<Vec<String>> = vec![module.to_vec()];
    let mut queue: Vec<(Vec<String>, TextRange)> = imports_of(cu).collect();
    queue.reverse();
    while let Some((path, via)) = queue.pop() {
        if tried.contains(&path) {
            continue;
        }
        tried.push(path.clone());
        let Some((file, source)) = siblings.source(&path) else {
            continue;
        };
        let parse = parse_entry(&tokenize(&source), &source, Entry::CompilationUnit);
        let root = SyntaxNode::new_root(parse.root.clone());
        if let Some(unit) = CompilationUnit::cast(root) {
            let mut next: Vec<_> = imports_of(&unit).map(|(path, _)| (path, via)).collect();
            next.reverse();
            queue.extend(next);
        }
        out.push(Sibling {
            imported: Imported {
                module: path,
                path: file,
            },
            parse,
            via,
        });
    }
    out
}

/// The module path each import of `cu` names, with the import's span.
fn imports_of(cu: &CompilationUnit) -> impl Iterator<Item = (Vec<String>, TextRange)> + '_ {
    cu.imports().filter_map(|import| {
        let path = import.path()?;
        let segments = path.segments().map(|s| s.text().to_string()).collect();
        Some((segments, import.syntax().text_range()))
    })
}

/// `error`, raised in the imported module `module`, reported at `via`, the
/// import that reads it.
fn imported_error(mut error: Diagnostic, module: &str, via: TextRange) -> Diagnostic {
    let at = error.primary;
    error.message = format!("in `{module}`: {}", error.message);
    error.primary = via;
    error.related.insert(
        0,
        crate::diag::Related::in_module(module, at, "raised here"),
    );
    error.fixes.clear();
    error
}

/// `by_source` in the order of `sources` then `regional`, any other source
/// it holds after them in symbol order.
fn ordered_invalidations(
    sources: &[Source],
    regional: &[Source],
    mut by_source: HashMap<SymbolId, DirtyClass>,
) -> Vec<(SymbolId, DirtyClass)> {
    let mut out: Vec<(SymbolId, DirtyClass)> = sources
        .iter()
        .chain(regional)
        .filter_map(|s| by_source.remove(&s.symbol).map(|class| (s.symbol, class)))
        .collect();
    let mut rest: Vec<_> = by_source.into_iter().collect();
    rest.sort_by_key(|(symbol, _)| *symbol);
    out.extend(rest);
    out
}

/// The constants a view of any module of `graph` folds: every module's —
/// the units `units` resolved as `resolved` — each read by the references
/// of the file it is read in.
pub(crate) fn graph_consts(
    graph: &ModuleGraph,
    units: &[SourceUnit],
    resolved: &[ResolvedModule],
    interner: &NameInterner,
) -> Rc<Consts> {
    let mut roots = Vec::new();
    let mut values = HashMap::new();
    for (gm, module) in graph.modules().iter().zip(resolved) {
        let text = gm.path.display(interner);
        let Some(unit) = units.iter().find(|u| u.path.display(interner) == text) else {
            continue;
        };
        let root = SyntaxNode::new_root(unit.parse.root.clone());
        let Some(cu) = CompilationUnit::cast(root.clone()) else {
            continue;
        };
        let symbol = |name: TextRange| {
            module
                .decls
                .iter()
                .find(|d| d.name_range == name)
                .map(|d| d.id)
        };
        values.extend(
            cu.items()
                .filter_map(|item| match item {
                    Item::Export(export) => export.declaration(),
                    other => Some(other),
                })
                .filter_map(|item| match item {
                    Item::Const(decl) => Some((symbol(decl.name()?.text_range())?, decl.value()?)),
                    _ => None,
                }),
        );
        roots.push((root, &module.refs[..]));
    }
    Rc::new(Consts::new(
        roots.iter().map(|(root, refs)| (root, *refs)),
        values,
    ))
}

/// One module of a graph as a view lowering reads it: its tree, after
/// resolution, and how it resolved.
pub(crate) struct GraphUnit<'r> {
    pub(crate) cu: CompilationUnit,
    pub(crate) module: &'r ResolvedModule,
}

/// Each module of `graph` with its unit among `units` and its resolution in
/// `resolved`, by the module's index in the graph: `None` for a module no
/// unit holds.
pub(crate) fn graph_units<'r>(
    graph: &ModuleGraph,
    units: &[SourceUnit],
    resolved: &'r [ResolvedModule],
    interner: &NameInterner,
) -> Vec<Option<GraphUnit<'r>>> {
    graph
        .modules()
        .iter()
        .zip(resolved)
        .map(|(gm, module)| {
            let text = gm.path.display(interner);
            let unit = units.iter().find(|u| u.path.display(interner) == text)?;
            let cu = CompilationUnit::cast(SyntaxNode::new_root(unit.parse.root.clone()))?;
            Some(GraphUnit { cu, module })
        })
        .collect()
}

/// The library a view of any of `units` inlines: the components among
/// `components` each unit declares, each unit's styles, and `consts`, the
/// constants of the graph. A unit is numbered by its index in `units`.
pub(crate) fn graph_library<'a>(
    units: &[Option<GraphUnit<'_>>],
    components: impl Iterator<Item = &'a HirComponent>,
    consts: Rc<Consts>,
) -> ComponentLibrary<'a> {
    let decls: Vec<Vec<ComponentDecl>> = units
        .iter()
        .map(|u| {
            u.as_ref()
                .map(|u| component_decls(&u.cu))
                .unwrap_or_default()
        })
        .collect();
    let library = components.filter_map(|c| {
        let unit = units.iter().position(|u| {
            u.as_ref()
                .is_some_and(|u| u.module.decls.iter().any(|d| d.id == c.schema.symbol))
        })?;
        let decl = decls[unit]
            .iter()
            .find(|d| d.syntax().text_range() == c.source_origin)?
            .clone();
        Some(LibraryComponent {
            schema: &c.schema,
            unit: unit as u32,
            write_backs: write_backs(&decl),
            persists: persists(&decl),
            selectors: crate::hir::selectors_of(&decl),
            decl,
        })
    });
    let library: Vec<LibraryComponent<'a>> = library.collect();
    let styles = StyleBooks::new(
        units
            .iter()
            .map(|unit| {
                let Some(GraphUnit { cu, module }) = unit else {
                    return (StyleBook::default(), &[][..]);
                };
                let symbol = |name: TextRange| {
                    module
                        .decls
                        .iter()
                        .find(|d| d.name_range == name)
                        .map(|d| d.id)
                };
                let book = StyleBook::new(cu.items(), |style| symbol(style.name()?.text_range()));
                (book, &module.refs[..])
            })
            .collect(),
    );
    let units = units
        .iter()
        .map(|unit| match unit {
            Some(GraphUnit { module, .. }) => LibraryUnit::new(&module.refs),
            None => LibraryUnit::default(),
        })
        .collect();
    ComponentLibrary::new(units, library, styles).with_consts(consts)
}

/// The expressions and references of each of `units`, as a view lowered
/// with their [`graph_library`] finds its spans' expressions.
pub(crate) fn view_units<'r>(units: &[Option<GraphUnit<'r>>]) -> ViewUnits<'r> {
    ViewUnits::new(units.iter().map(|unit| {
        unit.as_ref()
            .map(|u| (u.cu.syntax().clone(), &u.module.refs[..]))
    }))
}

/// What lowering a view raises: what it cannot mount (`E3711`), each grid
/// template or cell that names no cells (`E3108`) and each component with
/// `@persist` state it inlines (`E9106`).
pub(crate) fn view_diagnostics(view: &LoweredView) -> impl Iterator<Item = Diagnostic> + '_ {
    let persisting = view.persisting.iter().map(|(at, name)| {
        Diagnostic::error(
            "E9106",
            *at,
            format!(
                "`{name}` persists a state under one key, so it mounts only as a view's own \
                 component, not inside another's view"
            ),
        )
    });
    persisting
        .chain(view.unmounted.iter().map(unmounted_diagnostic))
        .chain(view.misplaced.iter().map(misplaced_diagnostic))
}

/// The `E3108` of a grid template or child cell that names no cells,
/// related to the node or `use` placing the view it is in.
fn misplaced_diagnostic(misplaced: &crate::ir::Unmounted) -> Diagnostic {
    let mut diagnostic = Diagnostic::error("E3108", misplaced.at, misplaced.reason.clone());
    if let Some(placed) = misplaced.placed_at {
        diagnostic
            .related
            .push(crate::diag::Related::new(placed, "placed here"));
    }
    diagnostic
}

/// The `E3711` of what a view cannot mount, related to the node or `use`
/// placing the view it is in.
fn unmounted_diagnostic(unmounted: &crate::ir::Unmounted) -> Diagnostic {
    let mut diagnostic = Diagnostic::error(
        "E3711",
        unmounted.at,
        format!("the component cannot be mounted here: {}", unmounted.reason),
    );
    if let Some(placed) = unmounted.placed_at {
        diagnostic
            .related
            .push(crate::diag::Related::new(placed, "placed here"));
    }
    diagnostic
}

/// What a write to each input of `component`, one of `components`, the
/// components of the graph `units` hold, invalidates in the view it mounts,
/// by input: nothing for an input the view never reads.
pub(crate) fn input_invalidations(
    units: &[Option<GraphUnit<'_>>],
    consts: &Rc<Consts>,
    components: &[&HirComponent],
    natives: &Natives,
    component: &HirComponent,
) -> Vec<DirtyClass> {
    let inputs = &component.schema.inputs;
    let library = graph_library(units, components.iter().copied(), consts.clone());
    let block = library
        .component(component.schema.symbol)
        .and_then(|c| c.decl.view()?.block());
    let Some(block) = block else {
        return vec![DirtyClass::EMPTY; inputs.len()];
    };
    let tree = lower_component_view(&block, natives, &library, component.schema.symbol, false).tree;
    drop(library);
    let child = |symbol: SymbolId| components.iter().find(|c| c.schema.symbol == symbol);
    let instances: Vec<InstanceSources> = tree
        .instances
        .iter()
        .map(|i| InstanceSources {
            states: Vec::new(),
            inputs: child(i.component)
                .map(|c| {
                    c.schema
                        .inputs
                        .iter()
                        .map(|i| i.meta.resolved_symbol)
                        .collect()
                })
                .unwrap_or_default(),
        })
        .collect();
    let env = SourceSet::new(
        instance_symbols(component).into_iter().chain(
            tree.instances
                .iter()
                .filter_map(|i| child(i.component))
                .flat_map(|c| instance_symbols(c)),
        ),
    );
    let derived = DerivedReads::merged(components.iter().map(|c| &c.schema.derived));
    let by_source = source_invalidations(&tree, &view_units(units), &env, &derived, &instances);
    inputs
        .iter()
        .map(|input| {
            input
                .meta
                .resolved_symbol
                .and_then(|symbol| by_source.get(&symbol).copied())
                .unwrap_or_default()
        })
        .collect()
}

/// Every component and template the unit declares, exported or not.
pub(crate) fn component_decls(cu: &CompilationUnit) -> Vec<ComponentDecl> {
    cu.items()
        .filter_map(|item| match item {
            Item::Export(export) => export.declaration(),
            other => Some(other),
        })
        .filter_map(|item| match item {
            Item::Component(decl) => Some(decl),
            Item::Template(decl) => Some(decl.as_component()),
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
    fn empty(diagnostics: Vec<Diagnostic>, behavior: Program, shaders: Vec<CheckedShader>) -> Self {
        Self {
            component: None,
            tree: UiTree::default(),
            bindings: BindingIr::default(),
            invalidations: Vec::new(),
            keys: KeyIr::default(),
            sources: Vec::new(),
            regional: Vec::new(),
            rust_components: Vec::new(),
            diagnostics,
            behavior,
            types: TypeSchemas::default(),
            migrators: Vec::new(),
            shaders,
            paints: Vec::new(),
            assets: Vec::new(),
            base: TextSize::ZERO,
            imported: Vec::new(),
            natives: Natives::standard(),
        }
    }
}

/// Whether the unit declares a `system` or a `shader`, exported or not.
fn declares_system(cu: &CompilationUnit) -> bool {
    let runs = |item: Option<Item>| matches!(item, Some(Item::System(_) | Item::Shader(_)));
    cu.items().any(|item| match item {
        Item::Export(export) => runs(export.declaration()),
        other => runs(Some(other)),
    })
}

/// The component a unit mounts: the one named `mount`, else its single
/// exported component, else its single component. A unit of systems or
/// shaders without a component mounts nothing. Anything else is ambiguous or
/// empty and reported.
pub(crate) fn mounted_component(
    cu: &CompilationUnit,
    source: &str,
    mount: Option<&str>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<ComponentDecl> {
    if let Some(name) = mount {
        let found = component_decls(cu)
            .into_iter()
            .find(|decl| decl.name().is_some_and(|n| n.text() == name));
        if found.is_none() {
            diagnostics.push(Diagnostic::error(
                "E2005",
                whole(source),
                format!("the source declares no component `{name}` to mount"),
            ));
        }
        return found;
    }
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
        // A source of systems runs under a scheduler and mounts nothing.
        0 if declares_system(cu) => None,
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
            ..Default::default()
        }
    }

    fn codes(compiled: &Compiled) -> Vec<&'static str> {
        compiled.errors().map(|d| d.code).collect()
    }

    /// A lexical error the parse recovers from is still an error of the file.
    #[test]
    fn a_lexical_error_is_reported_even_when_the_file_parses() {
        let source = "component App {\n    view {\n        Text { text: \"a\\q\"; }\n    }\n}\n";
        let compiled = compile_file(source, &origin(&["app"]));
        let escape = crate::syntax::LexError::InvalidEscape.code();
        assert_eq!(codes(&compiled), [escape]);
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
    fn a_component_is_mounted_by_name_whatever_the_file_exports() {
        let src = "export component Page { view { Badge {} } }
component Badge {
  input label: String = \"new\";
  view { Text { text: label; } }
}
";
        let declared = declared_components(src);
        let names: Vec<(&str, bool)> = declared
            .iter()
            .map(|c| (c.name.as_str(), c.exported))
            .collect();
        assert_eq!(names, [("Page", true), ("Badge", false)]);
        assert!(declared.iter().all(|c| c.required.is_empty()));
        let needy = declared_components(
            "component Tag { input text: String; view { Text { text: text; } } }",
        );
        assert_eq!(needy[0].required, ["text"]);
        assert_eq!(
            compile_file(src, &origin(&["page"]))
                .component
                .unwrap()
                .schema
                .name,
            "Page"
        );
        let badge = compile_mount_for(
            src,
            &origin(&["page"]),
            Natives::standard(),
            TargetProfile::default(),
            "Badge",
        );
        assert!(!badge.has_errors(), "{:?}", badge.diagnostics);
        assert_eq!(badge.component.unwrap().schema.name, "Badge");
        let missing = compile_mount_for(
            src,
            &origin(&["page"]),
            Natives::standard(),
            TargetProfile::default(),
            "Card",
        );
        assert_eq!(codes(&missing), ["E2005"]);
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
