//! IR dumps for compiler tests, Studio and AI debugging (`viso dump`): the
//! syntax tree, the typed HIR, the UI IR, the reactive IR, the Behavior IR,
//! the shader IR, the system IR and the module graph of a loaded package,
//! each as text and as JSON through one [`Dump`] trait.
//!
//! The text is for people and golden tests and may change between versions;
//! the JSON's keys are versioned by [`DUMP_VERSION`]. Spans are byte ranges
//! of the section's file, with the 1-based line and column of their start in
//! the text. Cold path: one dump per tool request.

mod ast;
mod behavior;
mod graph;
mod hir;
mod shader;
mod system;
mod view;

use std::path::Path;
use std::sync::Arc;

use viso_ende::JsonWriter;

use crate::ast::{AstNode, CompilationUnit, Item};
use crate::diag::Diagnostic;
use crate::frontend::{Compiled, Origin, compile_file_for};
use crate::hir::TargetProfile;
use crate::package::LoadedPackage;
use crate::resolve::SymbolId;
use crate::resolve::suggest::{Candidate, nearest};
use crate::syntax::{LineIndex, SyntaxNode, TextRange};

/// The version of the dumps' JSON keys: a key removed or changing meaning
/// raises it.
pub const DUMP_VERSION: u32 = 1;

/// Something a dump prints.
pub trait Dump {
    /// Appends the text form.
    fn write_text(&self, out: &mut String);
    /// Writes the JSON value.
    fn write_json(&self, w: &mut JsonWriter);
}

/// What a dump shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DumpKind {
    /// The syntax tree of each file, trivia aside.
    Ast,
    /// The typed HIR of each file's components and functions.
    Hir,
    /// The retained-tree template of the view each file mounts.
    UiIr,
    /// The binding edges of the view each file mounts and what each reactive
    /// source invalidates.
    ReactiveIr,
    /// The Behavior IR each file compiles to.
    BehaviorIr,
    /// The checked shader programs.
    ShaderIr,
    /// The systems a scheduler runs, in order, with their hooks and states.
    SystemIr,
    /// The package's modules, their files and imports.
    ModuleGraph,
}

impl DumpKind {
    /// Every kind, in the order tools list them.
    pub const ALL: [DumpKind; 8] = [
        DumpKind::Ast,
        DumpKind::Hir,
        DumpKind::UiIr,
        DumpKind::ReactiveIr,
        DumpKind::BehaviorIr,
        DumpKind::ShaderIr,
        DumpKind::SystemIr,
        DumpKind::ModuleGraph,
    ];

    /// The name a command line gives it.
    pub fn name(self) -> &'static str {
        match self {
            DumpKind::Ast => "ast",
            DumpKind::Hir => "hir",
            DumpKind::UiIr => "ui-ir",
            DumpKind::ReactiveIr => "reactive-ir",
            DumpKind::BehaviorIr => "behavior-ir",
            DumpKind::ShaderIr => "shader-ir",
            DumpKind::SystemIr => "system-ir",
            DumpKind::ModuleGraph => "module-graph",
        }
    }

    /// The kind named `name`.
    pub fn from_name(name: &str) -> Option<DumpKind> {
        DumpKind::ALL.into_iter().find(|kind| kind.name() == name)
    }
}

/// Which part of a package a dump covers.
#[derive(Debug, Clone, Copy, Default)]
pub struct Selection<'a> {
    /// One file, by its index in [`LoadedPackage::files`]; every file when
    /// `None`.
    pub file: Option<usize>,
    /// One declaration, by its path or a `::`-suffix of it, optionally with
    /// `.member` (`Counter`, `app::counter::Counter`, `Counter.increment`):
    /// the file declaring it, narrowed to it where the kind can be.
    pub symbol: Option<&'a str>,
}

/// A declaration a selection names.
#[derive(Debug, Clone)]
pub(crate) struct Focus {
    /// The declaration.
    pub symbol: SymbolId,
    /// Its name.
    pub name: String,
    /// The member the selection narrows to.
    pub member: Option<String>,
    /// The declaration's syntax range.
    pub range: TextRange,
}

/// What one file contributes to a dump, and where it is.
pub(crate) struct FileCx<'p> {
    pub package: &'p LoadedPackage,
    /// The file's index.
    pub index: usize,
    /// The file's path, relative to the package root, `/`-separated.
    pub name: String,
    /// Its module path, `::`-joined.
    pub module: String,
    pub source: &'p str,
    pub lines: LineIndex,
    /// The compilation unit's syntax root.
    pub root: SyntaxNode,
    /// The declaration the selection names, in this file.
    pub focus: Option<Focus>,
}

impl FileCx<'_> {
    /// The text of `range`.
    pub fn text(&self, range: TextRange) -> &str {
        self.source
            .get(range.start().to_usize()..range.end().to_usize())
            .unwrap_or("")
    }

    /// The 1-based line and UTF-16 column `range` starts at.
    pub fn position(&self, range: TextRange) -> (u32, u32) {
        let at = self.lines.line_col_utf16(range.start());
        (at.line + 1, at.column + 1)
    }

    /// Whether the selection keeps the declaration `symbol`.
    pub fn keeps(&self, symbol: Option<SymbolId>) -> bool {
        self.focus
            .as_ref()
            .is_none_or(|focus| symbol == Some(focus.symbol))
    }

    /// The file compiled as the view it mounts and the behavior it runs,
    /// as a hot reload and a release build compile it.
    pub fn compile(&self, profile: &TargetProfile) -> Compiled {
        let file = &self.package.files[self.index];
        let origin = Origin {
            package: self.package.model.name.clone(),
            module: file.module.clone(),
            language: None,
        };
        compile_file_for(
            self.source,
            &origin,
            Arc::clone(self.package.model.graph.natives()),
            profile.clone(),
        )
    }
}

/// One file's part of a dump.
struct Section {
    file: String,
    module: String,
    body: Box<dyn Dump>,
}

/// A dump of one kind over a package.
pub struct PackageDump {
    kind: DumpKind,
    sections: Vec<Section>,
    /// What compiling the selected files raised, for the caller to report.
    pub diagnostics: Vec<(usize, Diagnostic)>,
}

impl PackageDump {
    /// The kind it dumps.
    pub fn kind(&self) -> DumpKind {
        self.kind
    }
}

impl Dump for PackageDump {
    fn write_text(&self, out: &mut String) {
        out.push_str(&format!("{} {DUMP_VERSION}\n", self.kind.name()));
        for section in &self.sections {
            match (section.file.is_empty(), section.module.is_empty()) {
                (true, _) => {}
                (false, true) => out.push_str(&format!("\n== {}\n", section.file)),
                (false, false) => out.push_str(&format!(
                    "\n== {} (module {})\n",
                    section.file, section.module
                )),
            }
            section.body.write_text(out);
        }
    }

    fn write_json(&self, w: &mut JsonWriter) {
        w.begin_object();
        w.name("dump_version");
        w.uint(u64::from(DUMP_VERSION));
        w.name("sections");
        w.begin_array();
        for section in &self.sections {
            w.begin_object();
            if !section.file.is_empty() {
                w.name("file");
                w.string(&section.file);
                w.name("module");
                w.string(&section.module);
            }
            w.name("ir");
            section.body.write_json(w);
            w.end_object();
        }
        w.end_array();
        w.end_object();
    }
}

/// The dump of `kind` over the part of `package`, rooted at `root`, that
/// `selection` names; `profile` is the build the files compile for.
///
/// # Errors
///
/// An `E2001` diagnostic, with the nearest names as notes, when the
/// selection's symbol names no declaration, or more than one.
pub fn dump(
    package: &LoadedPackage,
    root: &Path,
    profile: &TargetProfile,
    kind: DumpKind,
    selection: Selection<'_>,
) -> Result<PackageDump, Box<Diagnostic>> {
    let focus = match selection.symbol {
        Some(symbol) => Some(find(package, symbol)?),
        None => None,
    };
    let files: Vec<usize> = match (&focus, selection.file) {
        (Some((file, _)), _) => vec![*file],
        (None, Some(file)) => vec![file],
        (None, None) => (0..package.files.len()).collect(),
    };
    let focus = focus.map(|(_, focus)| focus);
    let mut out = PackageDump {
        kind,
        sections: Vec::new(),
        diagnostics: Vec::new(),
    };
    if kind == DumpKind::ModuleGraph {
        let body = graph::graph(package, root, &files);
        out.sections.push(Section {
            file: String::new(),
            module: String::new(),
            body: Box::new(body),
        });
        return Ok(out);
    }
    for index in files {
        let file = &package.files[index];
        let Some(unit) = package.model.units.get(index) else {
            continue;
        };
        let cx = FileCx {
            package,
            index,
            name: relative(root, &file.path),
            module: file.module.join("::"),
            source: &file.source,
            lines: LineIndex::new(&file.source),
            root: SyntaxNode::new_root(unit.parse.root.clone()),
            focus: focus.clone(),
        };
        let body: Box<dyn Dump> = match kind {
            DumpKind::Ast => Box::new(ast::tree(&cx)),
            DumpKind::Hir => Box::new(hir::file(&cx)),
            DumpKind::ModuleGraph => unreachable!("dumped above"),
            _ => {
                let compiled = cx.compile(profile);
                out.diagnostics
                    .extend(compiled.diagnostics.iter().cloned().map(|d| (index, d)));
                match kind {
                    DumpKind::UiIr => Box::new(view::ui(&cx, &compiled)),
                    DumpKind::ReactiveIr => Box::new(view::reactive(&cx, &compiled)),
                    DumpKind::BehaviorIr => Box::new(behavior::program(&cx, &compiled)),
                    DumpKind::ShaderIr => Box::new(shader::shaders(&cx, &compiled)),
                    _ => Box::new(system::systems(&cx, &compiled)),
                }
            }
        };
        out.sections.push(Section {
            file: cx.name,
            module: cx.module,
            body,
        });
    }
    Ok(out)
}

/// `path` relative to `root`, `/`-separated.
fn relative(root: &Path, path: &Path) -> String {
    let relative = path.strip_prefix(root).unwrap_or(path);
    relative
        .components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// A top-level declaration of a file.
pub(crate) struct Declaration {
    pub name: String,
    /// What it declares: `component`, `record`, `fn`, `type_alias`, ….
    pub kind: String,
    pub range: TextRange,
    pub exported: bool,
}

/// The top-level declarations of `cu`, exported or not.
pub(crate) fn declarations(cu: &CompilationUnit) -> Vec<Declaration> {
    let mut out = Vec::new();
    for item in cu.items() {
        let (item, exported) = match item {
            Item::Export(export) => match export.declaration() {
                Some(item) => (item, true),
                None => continue,
            },
            other => (other, false),
        };
        if let Some(name) = item_name(&item) {
            let node = item.syntax();
            out.push(Declaration {
                name,
                kind: snake(node.kind().name().trim_end_matches("Decl")),
                range: node.text_range(),
                exported,
            });
        }
    }
    out
}

/// `TypeAlias` as `type_alias`.
fn snake(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 2);
    for (i, ch) in name.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

/// The name an item declares.
fn item_name(item: &Item) -> Option<String> {
    let node = item.syntax();
    node.children_with_tokens()
        .into_iter()
        .filter_map(|e| e.as_token().cloned())
        .find(|t| t.kind() == crate::syntax::SyntaxKind::Ident)
        .map(|t| t.text())
}

/// The file and declaration `query` names.
fn find(package: &LoadedPackage, query: &str) -> Result<(usize, Focus), Box<Diagnostic>> {
    let query = query.trim();
    let (path, member) = match query.split_once('.') {
        Some((path, member)) => (path, Some(member.to_owned())),
        None => (query, None),
    };
    let mut all = Vec::new();
    for (index, file) in package.files.iter().enumerate() {
        let (Some(unit), Some(module)) = (
            package.model.units.get(index),
            package.model.module_of(index),
        ) else {
            continue;
        };
        let Some(cu) = CompilationUnit::cast(SyntaxNode::new_root(unit.parse.root.clone())) else {
            continue;
        };
        for Declaration { name, range, .. } in declarations(&cu) {
            let Some(decl) = module
                .decls
                .iter()
                .find(|d| range.contains_range(d.name_range))
            else {
                continue;
            };
            let mut full = vec![package.model.name.as_str()];
            full.extend(file.module.iter().map(String::as_str));
            full.push(&name);
            all.push((
                full.join("::"),
                index,
                Focus {
                    symbol: decl.id,
                    name,
                    member: member.clone(),
                    range,
                },
            ));
        }
    }
    let names = |full: &str| {
        full == path
            || full
                .strip_suffix(path)
                .is_some_and(|head| head.ends_with("::"))
    };
    let mut matches: Vec<_> = all.iter().filter(|(full, ..)| names(full)).collect();
    match matches.len() {
        1 => {
            let (_, index, focus) = matches.remove(0);
            Ok((*index, focus.clone()))
        }
        0 => {
            let last = path.rsplit("::").next().unwrap_or(path);
            let near: Vec<String> = nearest(
                last,
                all.iter().map(|(_, _, f)| Candidate {
                    name: &f.name,
                    declared_at: None,
                }),
            )
            .iter()
            .filter_map(|c| all.iter().find(|(_, _, f)| f.name == c.name))
            .map(|(full, ..)| full.clone())
            .collect();
            Err(unknown(
                format!("no declaration of the package is named `{path}`"),
                &near,
            ))
        }
        _ => {
            let paths: Vec<String> = matches.iter().map(|(full, ..)| full.clone()).collect();
            Err(unknown(
                format!("`{path}` names more than one declaration; give its full path"),
                &paths,
            ))
        }
    }
}

fn unknown(message: String, candidates: &[String]) -> Box<Diagnostic> {
    let mut diagnostic = Diagnostic::error("E2001", TextRange::empty(0.into()), message);
    diagnostic
        .notes
        .extend(candidates.iter().map(|c| format!("did you mean `{c}`?")));
    Box::new(diagnostic)
}

/// Writes `items` as a JSON array of strings.
pub(crate) fn strings<'a>(w: &mut JsonWriter, items: impl IntoIterator<Item = &'a str>) {
    w.begin_array();
    for item in items {
        w.string(item);
    }
    w.end_array();
}

/// `text` in double quotes, its backslashes, quotes and control characters
/// escaped.
pub(crate) fn quote(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for ch in text.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{{{:x}}}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
