//! The filesystem package loader: every `.vs` file under a package's source root,
//! each given the module path its file path derives (section 22), parsed, then
//! resolved and lowered together as one module graph so imports between files
//! resolve.
//!
//! The manifest is read by the tooling (`tools/project`), which hands the package
//! name and pinned language version in as plain data: the compiler never parses
//! `Viso.toml`, and the tooling never depends on the compiler.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use viso_behavior::native::Natives;

use crate::ast::{AstNode, CompilationUnit};
use crate::diag::Diagnostic;
use crate::frontend::{
    check_language_version, component_decls, module_path, mounted_component, unit_library,
    view_diagnostics,
};
use crate::hir::{HirComponent, LoweredPackage, TargetProfile, lower};
use crate::ir::lower_component_view;
use crate::resolve::{ModuleGraph, ModulePath, NameInterner, ResolvedModule, SourceUnit, resolve};
use crate::syntax::grammar::{Entry, Parse, parse_entry};
use crate::syntax::{SyntaxNode, TextRange, tokenize};

/// The directory below a package root that holds its `.vs` sources.
pub const SOURCE_ROOT: &str = "src";

/// The extension of a Viso DSL source file.
pub const SOURCE_EXTENSION: &str = "vs";

/// What the package manifest says, as far as the compiler is concerned.
#[derive(Debug, Clone)]
pub struct PackageManifest<'a> {
    /// The package identity (`[package] name`).
    pub name: &'a str,
    /// The pinned language version and its span in the manifest, if any.
    pub language: Option<(&'a str, TextRange)>,
    /// What its targets are and how it is built.
    pub profile: TargetProfile,
}

/// One loaded source file.
#[derive(Debug)]
pub struct LoadedFile {
    /// The file's path.
    pub path: PathBuf,
    /// The module path its file path derives.
    pub module: Vec<String>,
    /// The file's text.
    pub source: String,
    /// Its parse, resolution and lowering diagnostics, spans relative to `source`.
    pub diagnostics: Vec<Diagnostic>,
}

/// A loaded, resolved and lowered package.
#[derive(Debug)]
pub struct LoadedPackage {
    /// The manifest's diagnostics (`E1001`), spanned in `Viso.toml`.
    pub manifest_diagnostics: Vec<Diagnostic>,
    /// The module graph's diagnostics (a duplicate module path, a cycle member):
    /// facts about the whole package, with no source position.
    pub graph_diagnostics: Vec<Diagnostic>,
    /// Every source file, in path order.
    pub files: Vec<LoadedFile>,
    /// Files and directories that could not be read.
    pub unreadable: Vec<(PathBuf, io::Error)>,
    /// The package's typed HIR.
    pub hir: LoweredPackage,
    /// What resolution and lowering worked from, for the tools that read past
    /// the HIR: the schema of each component, the IR dumps.
    pub(crate) model: PackageModel,
}

/// The parsed units of a package, its module graph and each module's
/// resolution, as the HIR was lowered from them.
pub(crate) struct PackageModel {
    /// The package identity.
    pub name: String,
    pub interner: NameInterner,
    /// Each file's unit, index-parallel to [`LoadedPackage::files`].
    pub units: Vec<SourceUnit>,
    pub graph: ModuleGraph,
    /// Each graph module's resolution, index-parallel to its modules.
    pub resolved: Vec<ResolvedModule>,
}

impl PackageModel {
    /// The resolution of the module the file `file` declares.
    pub fn module_of(&self, file: usize) -> Option<&ResolvedModule> {
        let text = self.units.get(file)?.path.display(&self.interner);
        let index = self
            .graph
            .modules()
            .iter()
            .position(|m| m.path.display(&self.interner) == text)?;
        self.resolved.get(index)
    }
}

impl std::fmt::Debug for PackageModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PackageModel")
            .field("name", &self.name)
            .field("units", &self.units.len())
            .finish_non_exhaustive()
    }
}

impl LoadedPackage {
    /// Whether any file, the manifest or the module graph raised an error, or a
    /// source could not be read.
    pub fn has_errors(&self) -> bool {
        let error = |d: &Diagnostic| d.severity == crate::diag::Severity::Error;
        !self.unreadable.is_empty()
            || self.manifest_diagnostics.iter().any(error)
            || self.graph_diagnostics.iter().any(error)
            || self.files.iter().flat_map(|f| &f.diagnostics).any(error)
    }
}

/// Loads the package rooted at `root`: every `.vs` file below its source root,
/// resolved and lowered as one module graph, and the view each file mounts
/// lowered as its `view!` lowers it.
pub fn load_package(root: &Path, manifest: PackageManifest<'_>) -> LoadedPackage {
    load_package_in(root, manifest, Natives::standard())
}

/// [`load_package`] with native paths resolved against `natives` instead of
/// the standard libraries alone.
pub fn load_package_in(
    root: &Path,
    manifest: PackageManifest<'_>,
    natives: Arc<Natives>,
) -> LoadedPackage {
    let mut unreadable = Vec::new();
    let mut paths = Vec::new();
    collect_sources(&root.join(SOURCE_ROOT), &mut paths, &mut unreadable);

    let mut interner = NameInterner::new();
    let mut files = Vec::with_capacity(paths.len());
    let mut units = Vec::with_capacity(paths.len());
    for path in paths {
        let source = match fs::read_to_string(&path) {
            Ok(source) => source,
            Err(error) => {
                unreadable.push((path, error));
                continue;
            }
        };
        let module = module_path(root, &path);
        let parse = parse_entry(&tokenize(&source), &source, Entry::CompilationUnit);
        let segments: Vec<&str> = module.iter().map(String::as_str).collect();
        units.push(SourceUnit::new(
            ModulePath::intern(&mut interner, &segments),
            parse.clone(),
        ));
        files.push(LoadedFile {
            path,
            module,
            source,
            diagnostics: parse.diagnostics(),
        });
    }

    let graph = ModuleGraph::build_with(&units, &interner, natives, manifest.name);
    let manifest_diagnostics: Vec<Diagnostic> = manifest
        .language
        .and_then(|(version, at)| check_language_version(version, at))
        .into_iter()
        .collect();
    let graph_diagnostics: Vec<Diagnostic> = graph.graph_errors().cloned().collect();

    let resolved = resolve(&graph, &units, &mut interner, manifest.name);
    let hir = lower(
        &graph,
        &units,
        &resolved,
        &mut interner,
        manifest.name,
        manifest.profile,
    );
    for (i, module) in graph.modules().iter().enumerate() {
        let text = module.path.display(&interner);
        let Some(file) = units
            .iter()
            .position(|unit| unit.path.display(&interner) == text)
        else {
            continue;
        };
        let view = resolved.get(i).map(|module| {
            mounted_view_diagnostics(
                &units[file].parse,
                &files[file].source,
                module,
                &hir,
                graph.natives(),
            )
        });
        let diagnostics = &mut files[file].diagnostics;
        if let Some(index) = graph.index_of(&text, &interner) {
            diagnostics.extend(graph.module_errors(index).cloned());
        }
        if let Some(module) = resolved.get(i) {
            diagnostics.extend(module.errors.iter().cloned());
        }
        if let Some(range) = hir.module_diagnostics.get(i) {
            diagnostics.extend(hir.diagnostics[range.clone()].iter().cloned());
        }
        diagnostics.extend(view.into_iter().flatten());
    }

    LoadedPackage {
        manifest_diagnostics,
        graph_diagnostics,
        files,
        unreadable,
        hir,
        model: PackageModel {
            name: manifest.name.to_owned(),
            interner,
            units,
            graph,
            resolved,
        },
    }
}

/// What lowering the view the file `parse` of `source` mounts raises, as its
/// `view!` would: nothing for a file that mounts no component (a module of
/// declarations, or of systems).
fn mounted_view_diagnostics(
    parse: &Parse,
    source: &str,
    module: &ResolvedModule,
    hir: &LoweredPackage,
    natives: &Natives,
) -> Vec<Diagnostic> {
    let root = SyntaxNode::new_root(parse.root.clone());
    let Some(cu) = CompilationUnit::cast(root) else {
        return Vec::new();
    };
    let Some(decl) = mounted_component(&cu, source, None, &mut Vec::new()) else {
        return Vec::new();
    };
    let Some(block) = decl.view().and_then(|view| view.block()) else {
        return Vec::new();
    };
    let own = |c: &&HirComponent| module.decls.iter().any(|d| d.id == c.schema.symbol);
    let range = decl.syntax().text_range();
    let Some(mounted) = hir
        .components
        .iter()
        .filter(own)
        .find(|c| c.source_origin == range)
    else {
        return Vec::new();
    };
    let decls = component_decls(&cu);
    let library = unit_library(&cu, module, &decls, hir.components.iter().filter(own));
    let view = lower_component_view(&block, natives, &library, mounted.schema.symbol);
    view_diagnostics(&view).collect()
}

/// Appends every `.vs` file below `dir` to `out`, in path order, skipping hidden
/// entries; a directory that cannot be read goes to `unreadable`, one that does
/// not exist nowhere.
pub fn collect_sources(
    dir: &Path,
    out: &mut Vec<PathBuf>,
    unreadable: &mut Vec<(PathBuf, io::Error)>,
) {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return,
        Err(error) => {
            unreadable.push((dir.to_path_buf(), error));
            return;
        }
    };
    let mut entries: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
    entries.sort();
    for path in entries {
        if path
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with('.'))
        {
            continue;
        }
        if path.is_dir() {
            collect_sources(&path, out, unreadable);
        } else if path.extension().is_some_and(|ext| ext == SOURCE_EXTENSION) {
            out.push(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::TextSize;

    /// A scratch package directory, removed on drop.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir()
                .join(format!("viso-dsl-package-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn write(&self, relative: &str, text: &str) {
            let path = self.0.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn manifest(language: Option<(&str, TextRange)>) -> PackageManifest<'_> {
        PackageManifest {
            profile: TargetProfile::default(),
            name: "app",
            language,
        }
    }

    #[test]
    fn files_get_module_paths_and_import_each_other() {
        let s = Scratch::new("imports");
        s.write(
            "src/widgets/badge.vs",
            "export record Badge { text: String; }",
        );
        s.write(
            "src/main.vs",
            "import widgets::badge::{Badge};\ncomponent App { input b: Badge; view { } }",
        );
        s.write("src/.hidden/skip.vs", "this is not parsed");
        s.write("src/notes.txt", "neither is this");
        let package = load_package(&s.0, manifest(None));
        let modules: Vec<_> = package.files.iter().map(|f| f.module.join("::")).collect();
        assert_eq!(modules, ["", "widgets::badge"]);
        assert!(!package.has_errors(), "{package:#?}");
        assert_eq!(package.hir.components.len(), 1);
    }

    #[test]
    fn diagnostics_land_on_the_file_that_raised_them() {
        let s = Scratch::new("attribution");
        s.write("src/a.vs", "import missing::thing;");
        s.write("src/b.vs", "component B { view { } }");
        let package = load_package(&s.0, manifest(None));
        let codes = |i: usize| -> Vec<_> {
            package.files[i]
                .diagnostics
                .iter()
                .map(|d| d.code)
                .collect()
        };
        assert_eq!(codes(0), ["E2001"]);
        assert!(codes(1).is_empty());
    }

    #[test]
    fn an_unsupported_manifest_language_is_a_manifest_diagnostic() {
        let s = Scratch::new("language");
        s.write("src/main.vs", "component App { view { } }");
        let at = TextRange::new(TextSize::from(30), TextSize::from(35));
        let package = load_package(&s.0, manifest(Some(("9.9", at))));
        let error = &package.manifest_diagnostics[0];
        assert_eq!((error.code, error.primary), ("E1001", at));
        assert!(package.files[0].diagnostics.is_empty());
    }

    #[test]
    fn two_files_deriving_one_module_path_are_a_graph_diagnostic() {
        let s = Scratch::new("ambiguous");
        s.write("src/ui.vs", "const A = 1;");
        s.write("src/ui/mod.vs", "const B = 2;");
        let package = load_package(&s.0, manifest(None));
        let codes: Vec<_> = package.graph_diagnostics.iter().map(|d| d.code).collect();
        assert_eq!(codes, ["E2002"]);
    }

    #[test]
    fn a_package_without_a_source_root_is_empty() {
        let s = Scratch::new("empty");
        let package = load_package(&s.0, manifest(None));
        assert!(package.files.is_empty() && !package.has_errors());
    }
}
