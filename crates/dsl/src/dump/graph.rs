//! The package's module graph: each module with the file declaring it, the
//! modules it imports, the natives its imports bind and its top-level
//! declarations, then the graph's own diagnostics.

use std::fmt::Write as _;
use std::path::Path;

use viso_ende::JsonWriter;

use super::{Declaration, Dump, declarations, relative, strings};
use crate::ast::{AstNode, CompilationUnit};
use crate::package::LoadedPackage;
use crate::syntax::SyntaxNode;

struct Native {
    local: String,
    path: String,
    owner: Option<String>,
}

struct Module {
    path: String,
    file: Option<String>,
    imports: Vec<String>,
    natives: Vec<Native>,
    declarations: Vec<Declaration>,
}

/// The module graph, narrowed to the modules of the selected files.
pub(super) struct GraphDump {
    package: String,
    modules: Vec<Module>,
    /// Each whole-graph diagnostic: its code and message.
    diagnostics: Vec<(&'static str, String)>,
}

pub(super) fn graph(package: &LoadedPackage, root: &Path, files: &[usize]) -> GraphDump {
    let model = &package.model;
    let modules = model.graph.modules();
    let display = |index: usize| modules[index].path.display(&model.interner);
    let mut out = Vec::new();
    for (index, module) in modules.iter().enumerate() {
        let path = display(index);
        let file = model
            .units
            .iter()
            .position(|u| u.path.display(&model.interner) == path);
        if file.is_none_or(|f| !files.contains(&f)) && files.len() != package.files.len() {
            continue;
        }
        let declared = file
            .and_then(|f| model.units.get(f))
            .and_then(|u| CompilationUnit::cast(SyntaxNode::new_root(u.parse.root.clone())))
            .map(|cu| declarations(&cu))
            .unwrap_or_default();
        out.push(Module {
            path,
            file: file.map(|f| relative(root, &package.files[f].path)),
            imports: module
                .imports
                .iter()
                .map(|i| display(i.as_usize()))
                .collect(),
            natives: module
                .natives
                .iter()
                .map(|n| Native {
                    local: n.local.clone(),
                    path: n.path.clone(),
                    owner: n.owner.clone(),
                })
                .collect(),
            declarations: declared,
        });
    }
    GraphDump {
        package: model.name.clone(),
        modules: out,
        diagnostics: package
            .graph_diagnostics
            .iter()
            .map(|d| (d.code, d.message.clone()))
            .collect(),
    }
}

impl Dump for GraphDump {
    fn write_text(&self, out: &mut String) {
        let _ = writeln!(out, "package {}", self.package);
        for module in &self.modules {
            let _ = write!(out, "module {}", module.path);
            if let Some(file) = &module.file {
                let _ = write!(out, " ({file})");
            }
            out.push('\n');
            for import in &module.imports {
                let _ = writeln!(out, "  import {import}");
            }
            for native in &module.natives {
                let _ = write!(out, "  native {} = {}", native.local, native.path);
                if let Some(owner) = &native.owner {
                    let _ = write!(out, " (in {owner})");
                }
                out.push('\n');
            }
            for decl in &module.declarations {
                let export = if decl.exported { "export " } else { "" };
                let _ = writeln!(out, "  {export}{} {}", decl.kind, decl.name);
            }
        }
        for (code, message) in &self.diagnostics {
            let _ = writeln!(out, "{code} {message}");
        }
    }

    fn write_json(&self, w: &mut JsonWriter) {
        w.begin_object();
        w.name("package");
        w.string(&self.package);
        w.name("modules");
        w.begin_array();
        for module in &self.modules {
            w.begin_object();
            w.name("path");
            w.string(&module.path);
            w.name("file");
            match &module.file {
                Some(file) => w.string(file),
                None => w.null(),
            }
            w.name("imports");
            strings(w, module.imports.iter().map(String::as_str));
            w.name("natives");
            w.begin_array();
            for native in &module.natives {
                w.begin_object();
                w.name("local");
                w.string(&native.local);
                w.name("path");
                w.string(&native.path);
                w.name("owner");
                match &native.owner {
                    Some(owner) => w.string(owner),
                    None => w.null(),
                }
                w.end_object();
            }
            w.end_array();
            w.name("declarations");
            w.begin_array();
            for decl in &module.declarations {
                w.begin_object();
                w.name("name");
                w.string(&decl.name);
                w.name("kind");
                w.string(&decl.kind);
                w.name("exported");
                w.bool(decl.exported);
                w.end_object();
            }
            w.end_array();
            w.end_object();
        }
        w.end_array();
        w.name("diagnostics");
        w.begin_array();
        for (code, message) in &self.diagnostics {
            w.begin_object();
            w.name("code");
            w.string(code);
            w.name("message");
            w.string(message);
            w.end_object();
        }
        w.end_array();
        w.end_object();
    }
}
