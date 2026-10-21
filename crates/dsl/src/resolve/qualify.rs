//! Module-qualified paths: `import model;` then `model::twice(x)`.
//!
//! A path whose leading segments name a package module through a
//! whole-module import reads that module's declaration. Before resolution
//! each such leading segment is re-tagged [`SyntaxKind::ModuleQualifier`], so
//! every later pass reads the path by the declaration's name and what follows
//! it — `model::twice` as `twice`, `model::Shape::circle` as
//! `Shape::circle` — and resolution binds that name in the qualifying module.

use std::collections::HashMap;
use std::rc::Rc;

use crate::ast::{AstNode, CompilationUnit};
use crate::syntax::{
    GreenChild, GreenNode, GreenToken, SyntaxKind, SyntaxNode, TextRange, TextSize,
};

use super::module::{ModuleGraph, SourceUnit};
use super::name::NameInterner;
use super::resolver::type_or_module_path_text;

/// The first segment after a path's module qualifiers: the name the path
/// reads in `module`.
pub(super) struct QualifiedHead {
    /// The name token's span.
    pub at: TextRange,
    /// Its text.
    pub name: String,
    /// The path of the package module the qualifiers name.
    pub module: String,
}

/// Re-tags the module qualifiers of every path of `unit` (a module of
/// `graph`), returning the name each qualified path reads; a unit with no
/// whole-module import of a package module is left as it is.
pub(super) fn qualify(
    unit: &mut SourceUnit,
    graph: &ModuleGraph,
    interner: &NameInterner,
) -> Vec<QualifiedHead> {
    let root = SyntaxNode::new_root(unit.parse.root.clone());
    let Some(cu) = CompilationUnit::cast(root.clone()) else {
        return Vec::new();
    };
    let modules = module_imports(&cu, graph, interner);
    if modules.is_empty() {
        return Vec::new();
    }
    let mut marks = Vec::new();
    let mut heads = Vec::new();
    for node in root.descendants() {
        if !matches!(
            node.kind(),
            SyntaxKind::PathExpr
                | SyntaxKind::TypePath
                | SyntaxKind::QualifiedVariantPattern
                | SyntaxKind::RecordExpr
        ) {
            continue;
        }
        let segments = leading_segments(&node);
        let Some(mut module) = segments
            .first()
            .and_then(|s| modules.get(&s.text()))
            .cloned()
        else {
            continue;
        };
        if segments.len() < 2 {
            continue;
        }
        // `app::model::x` under `import app;` qualifies by both modules.
        let mut qualifiers = 1;
        while qualifiers + 1 < segments.len() {
            let inner = format!("{module}::{}", segments[qualifiers].text());
            if graph.index_of(&inner, interner).is_none() {
                break;
            }
            module = inner;
            qualifiers += 1;
        }
        marks.extend(
            segments[..qualifiers]
                .iter()
                .map(|s| s.text_range().start()),
        );
        let head = &segments[qualifiers];
        heads.push(QualifiedHead {
            at: head.text_range(),
            name: head.text(),
            module,
        });
    }
    if !marks.is_empty() {
        marks.sort_unstable();
        unit.parse.root = Rc::new(retag(&unit.parse.root, TextSize::ZERO, &marks));
    }
    heads
}

/// The local name of each whole-module import of `cu` that names a package
/// module of `graph`, with that module's path.
fn module_imports(
    cu: &CompilationUnit,
    graph: &ModuleGraph,
    interner: &NameInterner,
) -> HashMap<String, String> {
    cu.imports()
        .filter(|import| import.items().next().is_none())
        .filter_map(|import| {
            let target = type_or_module_path_text(import.path()?.syntax());
            graph.index_of(&target, interner)?;
            let local = match import.rename().and_then(|r| r.name()) {
                Some(name) => name.text(),
                None => target.rsplit("::").next()?.to_owned(),
            };
            Some((local, target))
        })
        .collect()
}

/// The name tokens a path begins with: the `::`-separated identifiers a path
/// expression, pattern or record literal starts with, a type path's segment
/// names.
fn leading_segments(node: &SyntaxNode) -> Vec<crate::syntax::SyntaxToken> {
    let is_name = |t: &crate::syntax::SyntaxToken| {
        matches!(t.kind(), SyntaxKind::Ident | SyntaxKind::RawIdent)
    };
    if node.kind() == SyntaxKind::TypePath {
        return node
            .children()
            .into_iter()
            .filter(|n| n.kind() == SyntaxKind::TypePathSegment)
            .map_while(|segment| {
                segment
                    .children_with_tokens()
                    .into_iter()
                    .filter_map(|e| e.as_token().cloned())
                    .find(is_name)
            })
            .collect();
    }
    node.children_with_tokens()
        .into_iter()
        .map_while(|e| e.as_token().cloned())
        .filter(|t| !t.kind().is_trivia())
        .take_while(|t| is_name(t) || t.kind() == SyntaxKind::ColonColon)
        .filter(is_name)
        .collect()
}

/// `node`, which starts at `start`, with each identifier starting at one of
/// `marks` (sorted) re-tagged a module qualifier; a subtree holding none is
/// shared as it is.
fn retag(node: &GreenNode, start: TextSize, marks: &[TextSize]) -> GreenNode {
    let mut at = start;
    let children = node
        .children()
        .iter()
        .map(|child| {
            let from = at;
            at = at + child.text_len();
            let holds = |end: TextSize| {
                let i = marks.partition_point(|m| *m < from);
                marks.get(i).is_some_and(|m| *m < end)
            };
            match child {
                GreenChild::Node(inner) if holds(at) => {
                    GreenChild::Node(Rc::new(retag(inner, from, marks)))
                }
                GreenChild::Token(token)
                    if token.kind() == SyntaxKind::Ident && marks.binary_search(&from).is_ok() =>
                {
                    GreenChild::Token(GreenToken::new(SyntaxKind::ModuleQualifier, token.text()))
                }
                other => other.clone(),
            }
        })
        .collect();
    GreenNode::new(node.kind(), children)
}
