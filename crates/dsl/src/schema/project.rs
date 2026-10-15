//! The schema of each component a package declares, read from its typed HIR
//! and its view's binding edges, with where it is declared.

use std::path::Path;

use viso_ende::JsonWriter;

use super::{EventSchema, InputSchema, Schema, SchemaKind, SourceOrigin, dirty_names};
use crate::ast::{AstNode, CompilationUnit, ComponentDecl, Member, PartNode};
use crate::frontend::{component_decls, input_invalidations};
use crate::hir::{HirComponent, LoweredPackage};
use crate::package::LoadedPackage;
use crate::resolve::digest;
use crate::syntax::{LineIndex, SyntaxNode};

/// The schema of every component and template `package` declares, in file
/// then source order, each path led by the package name and its module's
/// path, and each versioned `version`. `root` is the package root its file
/// paths are reported relative to.
pub fn package_components(package: &LoadedPackage, root: &Path, version: &str) -> Vec<Schema> {
    let model = &package.model;
    let hir = &package.hir;
    let natives = model.graph.natives();
    let mut out = Vec::new();
    for (index, file) in package.files.iter().enumerate() {
        let (Some(module), Some(unit)) = (model.module_of(index), model.units.get(index)) else {
            continue;
        };
        let Some(cu) = CompilationUnit::cast(SyntaxNode::new_root(unit.parse.root.clone())) else {
            continue;
        };
        let own: Vec<&HirComponent> = hir
            .components
            .iter()
            .filter(|c| module.decls.iter().any(|d| d.id == c.schema.symbol))
            .collect();
        let decls = component_decls(&cu);
        let lines = LineIndex::new(&file.source);
        let relative = file.path.strip_prefix(root).unwrap_or(&file.path);
        let file_name = relative
            .components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        let mut prefix = vec![model.name.as_str()];
        prefix.extend(file.module.iter().map(String::as_str));
        for component in &own {
            let Some(decl) = decls
                .iter()
                .find(|d| d.syntax().text_range() == component.source_origin)
            else {
                continue;
            };
            let Some(declared) = module
                .decls
                .iter()
                .find(|d| d.id == component.schema.symbol)
            else {
                continue;
            };
            let invalidates = input_invalidations(&cu, module, &own, natives, component);
            let mut schema = component_schema(component, decl, &invalidates, hir, natives);
            schema.symbol = format!("{}::{}", prefix.join("::"), component.schema.name);
            schema.version = version.to_owned();
            let mut w = JsonWriter::new();
            schema.write_json(&mut w);
            let at = declared.name_range;
            let position = |offset| {
                let at = lines.line_col_utf16(offset);
                (at.line + 1, at.column + 1)
            };
            schema.origin = Some(SourceOrigin {
                file: file_name.clone(),
                range: at,
                start: position(at.start()),
                end: position(at.end()),
                symbol_id: component.schema.symbol,
                public: declared.export_at.is_none(),
                revision: digest([w.as_str().as_bytes()]),
            });
            out.push(schema);
        }
    }
    out
}

/// The schema of `component`, its path, version and origin left for the
/// caller.
fn component_schema(
    component: &HirComponent,
    decl: &ComponentDecl,
    invalidates: &[crate::ir::DirtyClass],
    hir: &LoweredPackage,
    natives: &viso_behavior::native::Natives,
) -> Schema {
    let schema = &component.schema;
    let describe = |ty| hir.types.describe_in(ty, natives);
    let defaults: Vec<Option<String>> = decl
        .members()
        .filter_map(|m| match m {
            Member::Input(input) => Some(input.default().map(|d| source_text(d.syntax()))),
            _ => None,
        })
        .collect();
    let two_way = crate::hir::write_backs(decl);
    let inputs = schema
        .inputs
        .iter()
        .enumerate()
        .map(|(i, input)| {
            let default = defaults.get(i).cloned().flatten();
            InputSchema {
                name: input.name.clone(),
                ty: describe(&input.meta.inferred_type),
                required: default.is_none(),
                default,
                invalidates: dirty_names(invalidates.get(i).copied().unwrap_or_default()),
                two_way: two_way.iter().any(|&(slot, _)| slot == i),
            }
        })
        .collect();
    let events = schema
        .events
        .iter()
        .map(|event| {
            let fields = event
                .meta
                .resolved_symbol
                .and_then(|symbol| hir.types.records.get(&symbol))
                .filter(|fields| !fields.is_empty());
            EventSchema {
                name: event.name.clone(),
                payload: fields.map(|fields| {
                    let fields: Vec<String> = fields
                        .iter()
                        .map(|f| format!("{}: {}", f.name, describe(&f.ty)))
                        .collect();
                    format!("({})", fields.join(", "))
                }),
                bubbles: false,
                cancelable: false,
            }
        })
        .collect();
    let mut capabilities: Vec<String> = schema
        .callables
        .iter()
        .flat_map(|c| c.meta.capability_set.iter().map(str::to_owned))
        .collect();
    capabilities.sort();
    capabilities.dedup();
    let mut parts: Vec<String> = decl
        .syntax()
        .descendants()
        .into_iter()
        .filter_map(PartNode::cast)
        .filter_map(|part| Some(part.name()?.text()))
        .collect();
    parts.dedup();
    Schema {
        kind: SchemaKind::Component,
        symbol: schema.name.clone(),
        version: String::new(),
        member: None,
        inputs,
        events,
        slots: schema.slots.iter().map(|s| s.name.clone()).collect(),
        parts,
        capabilities,
        functions: Vec::new(),
        types: Vec::new(),
        handle: None,
        origin: None,
    }
}

/// The text of `node`, each run of whitespace one space.
fn source_text(node: &SyntaxNode) -> String {
    node.text()
        .to_string()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

impl SourceOrigin {
    /// `file:line:column` of the declaration's name.
    pub fn location(&self) -> String {
        format!("{}:{}:{}", self.file, self.start.0, self.start.1)
    }

    /// `public` for an exported declaration, `private` for one only its
    /// module sees.
    pub fn visibility(&self) -> &'static str {
        if self.public { "public" } else { "private" }
    }

    /// Writes the origin object: the location of the declaration's name, its
    /// symbol, its visibility and the schema's revision.
    pub(super) fn write_json(&self, w: &mut JsonWriter) {
        w.begin_object();
        w.name("file");
        w.string(&self.file);
        w.name("byte_start");
        w.uint(u64::from(self.range.start().to_u32()));
        w.name("byte_end");
        w.uint(u64::from(self.range.end().to_u32()));
        w.name("line");
        w.uint(u64::from(self.start.0));
        w.name("column_utf16");
        w.uint(u64::from(self.start.1));
        w.name("end_line");
        w.uint(u64::from(self.end.0));
        w.name("end_column_utf16");
        w.uint(u64::from(self.end.1));
        w.name("symbol_id");
        w.string(&self.symbol_id.to_string());
        w.name("visibility");
        w.string(self.visibility());
        w.name("revision");
        w.string(&format!("{:032x}", self.revision));
        w.end_object();
    }
}
