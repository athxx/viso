//! Completion: the names that fit where the cursor stands, from the widget
//! schema, the native registry, the document's own declarations and the
//! grammar's keywords.
//!
//! The position is read from the token before the word being typed, so a
//! half-written word, which the parser cannot place yet, never decides it.

use viso_dsl::schema::{self, Catalog};
use viso_dsl::syntax::{SyntaxNode, SyntaxToken};
use viso_dsl::{SyntaxKind, TextSize, Ty};

use crate::source_map::OpenDoc;

/// What a completion names, as the protocol's `CompletionItemKind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum CompletionKind {
    Method = 2,
    Function = 3,
    Variable = 6,
    Class = 7,
    Module = 9,
    Property = 10,
    Enum = 13,
    Keyword = 14,
    EnumMember = 20,
    Constant = 21,
    Struct = 22,
    Event = 23,
}

/// One completion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Completion {
    /// The name offered.
    pub label: String,
    /// What it names.
    pub kind: CompletionKind,
    /// Its type or signature.
    pub detail: Option<String>,
    /// A snippet inserted instead of the label, `$0` where the cursor lands.
    pub snippet: Option<String>,
}

impl Completion {
    fn new(label: impl Into<String>, kind: CompletionKind) -> Completion {
        Completion {
            label: label.into(),
            kind,
            detail: None,
            snippet: None,
        }
    }

    fn detail(mut self, detail: impl Into<String>) -> Completion {
        self.detail = Some(detail.into());
        self
    }

    fn snippet(mut self, snippet: impl Into<String>) -> Completion {
        self.snippet = Some(snippet.into());
        self
    }
}

const TOP_LEVEL: &[&str] = &[
    "import",
    "export",
    "component",
    "record",
    "enum",
    "fn",
    "const",
    "type",
    "theme",
    "style",
    "shader",
    "system",
];
const MEMBERS: &[&str] = &[
    "input", "state", "computed", "event", "slot", "effect", "resource", "action", "fn", "task",
    "const", "view", "style",
];
const VIEW_ITEMS: &[&str] = &["node", "if", "for", "match"];
const NODE_MEMBERS: &[&str] = &["on", "node", "if", "for", "match"];
const STATEMENTS: &[&str] = &[
    "let",
    "if",
    "match",
    "for",
    "while",
    "loop",
    "return",
    "break",
    "continue",
    "emit",
    "transaction",
    "start",
];
const VALUES: &[&str] = &[
    "true", "false", "None", "self", "if", "match", "theme", "env",
];

/// The completions at `offset` of `doc`.
pub fn complete(doc: &OpenDoc, offset: TextSize) -> Vec<Completion> {
    let source = doc.source.as_str();
    let end = offset.to_usize().min(source.len());
    let start = source[..end]
        .char_indices()
        .rev()
        .take_while(|&(_, c)| c.is_alphanumeric() || c == '_')
        .last()
        .map_or(end, |(i, _)| i);
    let root = SyntaxNode::new_root(doc.parse.root.clone());
    let tokens: Vec<SyntaxToken> = root
        .descendants_with_tokens()
        .into_iter()
        .filter_map(|element| element.as_token().cloned())
        .filter(|t| !t.kind().is_trivia() && t.kind() != SyntaxKind::Eof)
        .collect();
    let before = tokens.partition_point(|t| t.text_range().end().to_usize() <= start);
    let Some(prev) = before.checked_sub(1).map(|i| &tokens[i]) else {
        return keywords(TOP_LEVEL);
    };
    let mut out = Vec::new();
    let parent = prev.parent();
    // A word that starts a line after a complete item starts the next item.
    let after_item =
        source[prev.text_range().end().to_usize()..start].contains('\n') && ends_item(prev.kind());
    match prev.kind() {
        SyntaxKind::ColonColon => {
            let path = path_before(&tokens[..before - 1]);
            let import = parent.kind() == SyntaxKind::ModulePath;
            path_members(doc, &root, &path, import, &mut out);
        }
        SyntaxKind::Dot => {}
        SyntaxKind::ImportKw => roots(doc, &mut out),
        _ if prev.text() == "on" && node_body(&parent).is_some() => {
            if let Some(body) = node_body(&parent) {
                events(doc, &root, &body, &mut out);
            }
        }
        SyntaxKind::Colon if is_typed(parent.kind()) => types(&root, &mut out),
        SyntaxKind::Colon
            if matches!(
                parent.kind(),
                SyntaxKind::PropertyBinding | SyntaxKind::RecordExprField
            ) =>
        {
            expected_variants(doc, &root, &parent, &mut out);
            values(doc, &root, &parent, start, &mut out);
        }
        SyntaxKind::Arrow | SyntaxKind::Lt => types(&root, &mut out),
        SyntaxKind::Comma if parent.kind() == SyntaxKind::GenericArgs => types(&root, &mut out),
        SyntaxKind::LBrace | SyntaxKind::Semi | SyntaxKind::RBrace => {
            let from = if prev.kind() == SyntaxKind::LBrace {
                Some(parent)
            } else {
                parent.parent()
            };
            match from.and_then(|n| container(&n)) {
                Some(c) => members_of(doc, &root, &c, start, &mut out),
                None => out.extend(keywords(TOP_LEVEL)),
            }
        }
        _ if after_item => match container(&parent) {
            Some(c) => members_of(doc, &root, &c, start, &mut out),
            None => out.extend(keywords(TOP_LEVEL)),
        },
        _ => values(doc, &root, &parent, start, &mut out),
    }
    out
}

/// Whether a token of `kind` can end an item, so a line break after it ends
/// the item.
fn ends_item(kind: SyntaxKind) -> bool {
    use SyntaxKind as K;
    matches!(
        kind,
        K::Ident
            | K::RawIdent
            | K::IntLiteral
            | K::FloatLiteral
            | K::UnitLiteral
            | K::StringLiteral
            | K::StringEnd
            | K::RawStringLiteral
            | K::CharLiteral
            | K::ColorLiteral
            | K::TrueKw
            | K::FalseKw
            | K::NoneKw
            | K::SelfValueKw
            | K::SelfTypeKw
            | K::RParen
            | K::RBracket
            | K::Question
            | K::Gt
    )
}

/// Whether a `:` under `kind` is followed by a type.
fn is_typed(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::InputDecl
            | SyntaxKind::StateDecl
            | SyntaxKind::ComputedDecl
            | SyntaxKind::ConstDecl
            | SyntaxKind::SlotDecl
            | SyntaxKind::ResourceDecl
            | SyntaxKind::Param
            | SyntaxKind::EventParam
            | SyntaxKind::RecordField
            | SyntaxKind::LetStmt
            | SyntaxKind::ClosureParam
            | SyntaxKind::GenericParam
            | SyntaxKind::ShaderUniform
            | SyntaxKind::ShaderInstance
            | SyntaxKind::ShaderVarying
    )
}

/// The nearest node, from `node` up, a member list is written in.
fn container(node: &SyntaxNode) -> Option<SyntaxNode> {
    std::iter::once(node.clone())
        .chain(node.ancestors())
        .find(|n| {
            matches!(
                n.kind(),
                SyntaxKind::NodeBody
                    | SyntaxKind::ViewBlock
                    | SyntaxKind::ComponentDecl
                    | SyntaxKind::SystemDecl
                    | SyntaxKind::Block
                    | SyntaxKind::CompilationUnit
                    | SyntaxKind::Root
            )
        })
}

/// The node body `node` is in, if a node member stands there.
fn node_body(node: &SyntaxNode) -> Option<SyntaxNode> {
    container(node).filter(|n| n.kind() == SyntaxKind::NodeBody)
}

/// What can be written at the start of a member of `container`.
fn members_of(
    doc: &OpenDoc,
    root: &SyntaxNode,
    container: &SyntaxNode,
    at: usize,
    out: &mut Vec<Completion>,
) {
    match container.kind() {
        SyntaxKind::NodeBody => {
            properties(doc, root, container, out);
            nodes(doc, root, out);
            out.extend(keywords(NODE_MEMBERS));
        }
        SyntaxKind::ViewBlock => {
            nodes(doc, root, out);
            out.extend(keywords(VIEW_ITEMS));
        }
        SyntaxKind::ComponentDecl | SyntaxKind::SystemDecl => out.extend(keywords(MEMBERS)),
        SyntaxKind::Block => {
            values(doc, root, container, at, out);
            out.extend(keywords(STATEMENTS));
        }
        _ => out.extend(keywords(TOP_LEVEL)),
    }
}

fn keywords(words: &[&str]) -> Vec<Completion> {
    words
        .iter()
        .map(|w| Completion::new(*w, CompletionKind::Keyword))
        .collect()
}

/// The first name token directly under `node`.
fn name_of(node: &SyntaxNode) -> Option<SyntaxToken> {
    node.children_with_tokens()
        .into_iter()
        .filter_map(|e| e.as_token().cloned())
        .find(|t| matches!(t.kind(), SyntaxKind::Ident | SyntaxKind::RawIdent))
}

/// The top-level declarations of the document of `kind`.
fn top_level(root: &SyntaxNode, kind: SyntaxKind) -> impl Iterator<Item = SyntaxNode> {
    root.descendants()
        .into_iter()
        .filter(move |n| n.kind() == kind)
}

/// The type a node body's node is.
fn node_type(body: &SyntaxNode) -> Option<String> {
    let node = body.parent()?;
    let path = node
        .children()
        .into_iter()
        .find(|c| c.kind() == SyntaxKind::TypePath)?;
    let segment = path.children().into_iter().last()?;
    name_of(&segment).map(|t| t.text())
}

/// The component of this document named `name`.
fn component(root: &SyntaxNode, name: &str) -> Option<SyntaxNode> {
    top_level(root, SyntaxKind::ComponentDecl)
        .find(|c| name_of(c).is_some_and(|t| t.text() == name))
}

/// The members of `decl` of `kind`, by name, with their declaration's text after
/// the name as detail.
fn declared(decl: &SyntaxNode, kind: SyntaxKind) -> Vec<(String, SyntaxNode)> {
    decl.children()
        .into_iter()
        .filter(|m| m.kind() == kind)
        .filter_map(|m| Some((name_of(&m)?.text(), m)))
        .collect()
}

/// The properties a node of `body`'s type takes.
fn properties(doc: &OpenDoc, root: &SyntaxNode, body: &SyntaxNode, out: &mut Vec<Completion>) {
    let Some(ty) = node_type(body) else {
        return;
    };
    if let Some(own) = component(root, &ty) {
        for (name, decl) in declared(&own, SyntaxKind::InputDecl) {
            if is_primary(&decl) {
                continue;
            }
            let detail = type_text(&decl);
            out.push(property(name, detail));
        }
        return;
    }
    if doc.natives.widget(&ty).is_none() {
        return;
    }
    let catalog = Catalog::framework(doc.natives.clone());
    if let Ok(schema) = schema::query(&ty, &catalog) {
        for input in schema.inputs.into_iter().filter(|i| !i.primary) {
            out.push(property(input.name, Some(input.ty)));
        }
    }
}

fn property(name: String, ty: Option<String>) -> Completion {
    let snippet = format!("{name}: $0;");
    let completion = Completion::new(name, CompletionKind::Property).snippet(snippet);
    match ty {
        Some(ty) => completion.detail(ty),
        None => completion,
    }
}

/// The text of the type a declaration annotates, after its `:`.
fn type_text(decl: &SyntaxNode) -> Option<String> {
    decl.children()
        .into_iter()
        .find(|c| c.kind() == SyntaxKind::TypePath)
        .map(|t| t.text().trim().to_string())
}

/// The events a node of `body`'s type raises.
fn events(doc: &OpenDoc, root: &SyntaxNode, body: &SyntaxNode, out: &mut Vec<Completion>) {
    let Some(ty) = node_type(body) else {
        return;
    };
    if let Some(own) = component(root, &ty) {
        for (name, _) in declared(&own, SyntaxKind::EventDecl) {
            out.push(Completion::new(name, CompletionKind::Event));
        }
        return;
    }
    if doc.natives.widget(&ty).is_none() {
        return;
    }
    let catalog = Catalog::framework(doc.natives.clone());
    if let Ok(schema) = schema::query(&ty, &catalog) {
        for event in schema.events {
            let completion = Completion::new(event.name, CompletionKind::Event);
            out.push(match event.payload {
                Some(payload) => completion.detail(payload),
                None => completion,
            });
        }
    }
}

/// The widgets and this document's components, each a node to write.
fn nodes(doc: &OpenDoc, root: &SyntaxNode, out: &mut Vec<Completion>) {
    // A type with a primary property is written `Type(value)` first.
    let node = |name: &str, detail: &str, primary: bool| {
        let snippet = if primary {
            format!("{name}($0)")
        } else {
            format!("{name} {{\n\t$0\n}}")
        };
        Completion::new(name, CompletionKind::Class)
            .detail(detail)
            .snippet(snippet)
    };
    for entry in doc.natives.widgets() {
        let primary = entry.widget.primary().is_some();
        out.push(node(entry.widget.name, entry.library.path, primary));
    }
    for decl in top_level(root, SyntaxKind::ComponentDecl) {
        if let Some(name) = name_of(&decl) {
            let primary = declared(&decl, SyntaxKind::InputDecl)
                .iter()
                .any(|(_, input)| is_primary(input));
            out.push(node(&name.text(), "component", primary));
        }
    }
}

/// Whether the input `decl` is marked `@primary`, which its node's
/// `( value )` gives in place of a body binding.
fn is_primary(decl: &SyntaxNode) -> bool {
    let mut at = decl.prev_sibling();
    while let Some(attr) = at.filter(|n| n.kind() == SyntaxKind::Attribute) {
        let text: String = attr.text().split_whitespace().collect();
        if text == "@primary" {
            return true;
        }
        at = attr.prev_sibling();
    }
    false
}

/// The variants of the enum the value after `site`'s `:` expects (a
/// property's or a record field's type, or its `Option`), each of which a
/// bare name there stands for.
fn expected_variants(
    doc: &OpenDoc,
    root: &SyntaxNode,
    site: &SyntaxNode,
    out: &mut Vec<Completion>,
) {
    let Some(ty) = expected_type(doc, root, site) else {
        return;
    };
    let ty = ty
        .strip_prefix("Option<")
        .and_then(|t| t.strip_suffix('>'))
        .unwrap_or(&ty);
    let own =
        top_level(root, SyntaxKind::EnumDecl).find(|e| name_of(e).is_some_and(|n| n.text() == ty));
    let variants: Vec<String> = match own {
        Some(decl) => declared(&decl, SyntaxKind::EnumVariant)
            .into_iter()
            .map(|(name, _)| name)
            .collect(),
        None => viso_dsl::resolve::prelude_enum_variants(ty)
            .or_else(|| {
                let native = doc.natives.ty(ty)?;
                Some(native.ty.variants.iter().map(|v| v.to_string()).collect())
            })
            .unwrap_or_default(),
    };
    for variant in variants {
        out.push(Completion::new(variant, CompletionKind::EnumMember).detail(ty));
    }
}

/// The type the value of the property binding or record field `site`
/// expects, as source spells it.
fn expected_type(doc: &OpenDoc, root: &SyntaxNode, site: &SyntaxNode) -> Option<String> {
    let typed = |decl: &SyntaxNode, kind: SyntaxKind, name: &str| {
        declared(decl, kind)
            .into_iter()
            .find(|(n, _)| n == name)
            .and_then(|(_, d)| type_text(&d))
    };
    if site.kind() == SyntaxKind::RecordExprField {
        let field = name_of(site)?.text();
        let record = site.parent()?;
        let head = record
            .children_with_tokens()
            .into_iter()
            .filter_map(|e| e.as_token().cloned())
            .take_while(|t| t.kind() != SyntaxKind::LBrace)
            .filter(|t| matches!(t.kind(), SyntaxKind::Ident | SyntaxKind::RawIdent))
            .last()?
            .text();
        let decl = top_level(root, SyntaxKind::RecordDecl)
            .find(|r| name_of(r).is_some_and(|n| n.text() == head))?;
        return typed(&decl, SyntaxKind::RecordField, &field);
    }
    let property: String = site
        .children()
        .into_iter()
        .find(|c| c.kind() == SyntaxKind::PropertyPath)?
        .text()
        .split_whitespace()
        .collect();
    let owner = site.parent()?;
    let node = match owner.kind() {
        SyntaxKind::NodeBody => node_type(&owner)?,
        SyntaxKind::StyleDecl | SyntaxKind::StyleWhen => {
            let style = std::iter::once(owner.clone())
                .chain(owner.ancestors())
                .find(|n| n.kind() == SyntaxKind::StyleDecl)?;
            let target = style
                .children()
                .into_iter()
                .find(|c| c.kind() == SyntaxKind::TypePath)?;
            name_of(&target.children().into_iter().last()?)?.text()
        }
        _ => return None,
    };
    if let Some(own) = component(root, &node) {
        return typed(&own, SyntaxKind::InputDecl, &property);
    }
    doc.natives.widget(&node)?;
    let catalog = Catalog::framework(doc.natives.clone());
    schema::query(&node, &catalog)
        .ok()?
        .inputs
        .into_iter()
        .find(|i| i.name == property)
        .map(|i| i.ty)
}

/// The names an expression at `at` under `node` can read.
fn values(
    doc: &OpenDoc,
    root: &SyntaxNode,
    node: &SyntaxNode,
    at: usize,
    out: &mut Vec<Completion>,
) {
    locals(node, at, out);
    let scope = std::iter::once(node.clone())
        .chain(node.ancestors())
        .find(|n| matches!(n.kind(), SyntaxKind::ComponentDecl | SyntaxKind::SystemDecl));
    if let Some(component) = scope {
        for member in component.children() {
            let kind = match member.kind() {
                SyntaxKind::InputDecl
                | SyntaxKind::StateDecl
                | SyntaxKind::ComputedDecl
                | SyntaxKind::ResourceDecl => CompletionKind::Property,
                SyntaxKind::ActionDecl | SyntaxKind::FnDecl | SyntaxKind::TaskDecl => {
                    CompletionKind::Method
                }
                SyntaxKind::ConstDecl => CompletionKind::Constant,
                _ => continue,
            };
            if let Some(name) = name_of(&member) {
                let completion = Completion::new(name.text(), kind);
                out.push(match type_text(&member) {
                    Some(ty) => completion.detail(ty),
                    None => completion,
                });
            }
        }
    }
    for decl in root.children() {
        let kind = match decl.kind() {
            SyntaxKind::FnDecl => CompletionKind::Function,
            SyntaxKind::ConstDecl => CompletionKind::Constant,
            SyntaxKind::EnumDecl => CompletionKind::Enum,
            SyntaxKind::RecordDecl => CompletionKind::Struct,
            _ => continue,
        };
        if let Some(name) = name_of(&decl) {
            out.push(Completion::new(name.text(), kind));
        }
    }
    imports(doc, root, out);
    out.extend(keywords(VALUES));
}

/// The parameters and earlier `let`s in scope at `at` from `node` up.
fn locals(node: &SyntaxNode, at: usize, out: &mut Vec<Completion>) {
    for scope in std::iter::once(node.clone()).chain(node.ancestors()) {
        let bindings: Vec<SyntaxNode> = match scope.kind() {
            SyntaxKind::Block => scope
                .children()
                .into_iter()
                .filter(|s| {
                    s.kind() == SyntaxKind::LetStmt && s.text_range().end().to_usize() <= at
                })
                .collect(),
            SyntaxKind::ActionDecl
            | SyntaxKind::FnDecl
            | SyntaxKind::TaskDecl
            | SyntaxKind::EventHandler
            | SyntaxKind::ClosureExpr => scope
                .descendants()
                .into_iter()
                .filter(|p| {
                    matches!(p.kind(), SyntaxKind::Param | SyntaxKind::ClosureParam)
                        && p.parent().is_some_and(|l| {
                            l.parent()
                                .is_some_and(|d| d.text_range() == scope.text_range())
                        })
                })
                .collect(),
            SyntaxKind::ForStmt | SyntaxKind::ViewFor => vec![scope.clone()],
            _ => continue,
        };
        for binding in bindings {
            let names = binding
                .descendants_with_tokens()
                .into_iter()
                .filter_map(|e| e.as_token().cloned())
                .filter(|t| {
                    matches!(t.kind(), SyntaxKind::Ident | SyntaxKind::RawIdent)
                        && matches!(
                            t.parent().kind(),
                            SyntaxKind::IdentPattern
                                | SyntaxKind::BindingPattern
                                | SyntaxKind::Param
                                | SyntaxKind::ClosureParam
                        )
                });
            for name in names {
                out.push(Completion::new(name.text(), CompletionKind::Variable));
            }
        }
    }
}

/// The names this document's imports bring in: a library, a type or a module.
fn imports(doc: &OpenDoc, root: &SyntaxNode, out: &mut Vec<Completion>) {
    for import in top_level(root, SyntaxKind::ImportDecl) {
        let Some(path) = import
            .children()
            .into_iter()
            .find(|c| c.kind() == SyntaxKind::ModulePath)
        else {
            continue;
        };
        let path = path.text().trim().to_string();
        let items = declared(&import, SyntaxKind::ImportItem);
        if items.is_empty() {
            let alias = rename(&import).unwrap_or_else(|| last_segment(&path).to_string());
            out.push(Completion::new(alias, CompletionKind::Module).detail(path));
            continue;
        }
        for (name, item) in items {
            let alias = rename(&item).unwrap_or(name.clone());
            let full = format!("{path}::{name}");
            let kind = if doc.natives.function(&full).is_some() {
                CompletionKind::Function
            } else if name.starts_with(char::is_uppercase) {
                CompletionKind::Class
            } else {
                CompletionKind::Module
            };
            out.push(Completion::new(alias, kind).detail(full));
        }
    }
}

/// The name a `RenameClause` under `node` gives.
fn rename(node: &SyntaxNode) -> Option<String> {
    let clause = node
        .children()
        .into_iter()
        .find(|c| c.kind() == SyntaxKind::RenameClause)?;
    name_of(&clause).map(|t| t.text())
}

fn last_segment(path: &str) -> &str {
    path.rsplit("::").next().unwrap_or(path)
}

/// The types a type position takes.
fn types(root: &SyntaxNode, out: &mut Vec<Completion>) {
    for name in Ty::BUILTIN_NAMES {
        out.push(Completion::new(*name, CompletionKind::Struct).detail("builtin"));
    }
    for (name, params) in Ty::STRUCTURAL_HEADS {
        let holes: Vec<String> = (1..=*params).map(|i| format!("${i}")).collect();
        out.push(
            Completion::new(*name, CompletionKind::Struct)
                .detail("builtin")
                .snippet(format!("{name}<{}>", holes.join(", "))),
        );
    }
    for (kind, as_kind) in [
        (SyntaxKind::RecordDecl, CompletionKind::Struct),
        (SyntaxKind::EnumDecl, CompletionKind::Enum),
        (SyntaxKind::TypeAliasDecl, CompletionKind::Struct),
        (SyntaxKind::ComponentDecl, CompletionKind::Class),
    ] {
        for decl in top_level(root, kind) {
            if let Some(name) = name_of(&decl) {
                out.push(Completion::new(name.text(), as_kind));
            }
        }
    }
    for import in top_level(root, SyntaxKind::ImportDecl) {
        for (name, _) in declared(&import, SyntaxKind::ImportItem) {
            if name.starts_with(char::is_uppercase) {
                out.push(Completion::new(name, CompletionKind::Class));
            }
        }
    }
}

/// The path written before a trailing `::`: its segments, first to last.
fn path_before(tokens: &[SyntaxToken]) -> Vec<String> {
    let mut segments = Vec::new();
    let mut rest = tokens;
    while let Some((last, init)) = rest.split_last() {
        if !matches!(last.kind(), SyntaxKind::Ident | SyntaxKind::RawIdent)
            && !last.kind().is_keyword()
        {
            break;
        }
        segments.push(last.text());
        match init.split_last() {
            Some((sep, init)) if sep.kind() == SyntaxKind::ColonColon => rest = init,
            _ => break,
        }
    }
    segments.reverse();
    segments
}

/// The library roots an `import` names first.
fn roots(doc: &OpenDoc, out: &mut Vec<Completion>) {
    let mut seen: Vec<&str> = Vec::new();
    for library in doc.natives.libraries() {
        let root = library.path.split("::").next().unwrap_or(library.path);
        if !seen.contains(&root) {
            seen.push(root);
            out.push(Completion::new(root, CompletionKind::Module));
        }
    }
}

/// What follows `path::`: a library's sub-libraries, functions and types, a
/// type's associated functions, an enum's variants.
fn path_members(
    doc: &OpenDoc,
    root: &SyntaxNode,
    path: &[String],
    import: bool,
    out: &mut Vec<Completion>,
) {
    let Some((head, rest)) = path.split_first() else {
        return;
    };
    if !import && rest.is_empty() {
        for decl in top_level(root, SyntaxKind::EnumDecl) {
            if name_of(&decl).is_some_and(|t| t.text() == *head) {
                for (name, _) in declared(&decl, SyntaxKind::EnumVariant) {
                    out.push(Completion::new(name, CompletionKind::EnumMember));
                }
                return;
            }
        }
    }
    let base = if import {
        head.clone()
    } else {
        alias_target(root, head).unwrap_or_else(|| head.clone())
    };
    let full = std::iter::once(base)
        .chain(rest.iter().cloned())
        .collect::<Vec<_>>()
        .join("::");
    let natives = &doc.natives;
    let prefix = format!("{full}::");
    let mut subs: Vec<&str> = Vec::new();
    for library in natives.libraries() {
        if let Some(tail) = library.path.strip_prefix(&prefix) {
            let next = tail.split("::").next().unwrap_or(tail);
            if !subs.contains(&next) {
                subs.push(next);
                out.push(Completion::new(next, CompletionKind::Module));
            }
        }
    }
    let owner = natives.ty(&full).map(|t| t.id);
    let signatures = schema::list(
        schema::ListKind::Natives,
        &Catalog::framework(natives.clone()),
    );
    for function in natives.functions() {
        let Some(name) = function.path.strip_prefix(&prefix) else {
            continue;
        };
        if name.contains("::") || function.is_method(natives) || function.owner != owner {
            continue;
        }
        let detail = signatures
            .iter()
            .find(|hit| hit.path == *function.path)
            .map(|hit| hit.detail.clone());
        let completion =
            Completion::new(name, CompletionKind::Function).snippet(format!("{name}($0)"));
        out.push(match detail {
            Some(detail) => completion.detail(detail),
            None => completion,
        });
    }
    if let Some(ty) = natives.ty(&full) {
        for variant in ty.ty.variants {
            out.push(Completion::new(*variant, CompletionKind::EnumMember));
        }
    }
    for ty in natives.types() {
        if let Some(name) = ty.path.strip_prefix(&prefix)
            && !name.contains("::")
        {
            let kind = if ty.ty.is_enum() {
                CompletionKind::Enum
            } else {
                CompletionKind::Class
            };
            out.push(Completion::new(name, kind));
        }
    }
}

/// The path the import that binds `alias` names.
fn alias_target(root: &SyntaxNode, alias: &str) -> Option<String> {
    for import in top_level(root, SyntaxKind::ImportDecl) {
        let path = import
            .children()
            .into_iter()
            .find(|c| c.kind() == SyntaxKind::ModulePath)?
            .text()
            .trim()
            .to_string();
        let items = declared(&import, SyntaxKind::ImportItem);
        if items.is_empty() {
            let name = rename(&import).unwrap_or_else(|| last_segment(&path).to_string());
            if name == alias {
                return Some(path);
            }
        }
        for (name, item) in items {
            if rename(&item).unwrap_or(name.clone()) == alias {
                return Some(format!("{path}::{name}"));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source_map::SourceMap;

    /// The labels offered where `|` stands in `source`.
    fn labels(source: &str) -> Vec<(String, CompletionKind)> {
        let at = source.find('|').expect("a cursor");
        let text = source.replacen('|', "", 1);
        let mut map = SourceMap::new();
        let id = map.open(text, &["m"], None);
        let doc = map.get(id).unwrap();
        complete(doc, TextSize::new(at as u32))
            .into_iter()
            .map(|c| (c.label, c.kind))
            .collect()
    }

    fn has(items: &[(String, CompletionKind)], label: &str, kind: CompletionKind) -> bool {
        items.iter().any(|(l, k)| l == label && *k == kind)
    }

    const HEAD: &str = "import viso::text;\n\
        component Counter {\n\
        \x20   input step: I32 = 1;\n\
        \x20   state count = 0;\n\
        \x20   event changed(to: I32);\n";

    fn counter(rest: &str) -> String {
        format!("{HEAD}{rest}\n}}\n")
    }

    #[test]
    fn a_node_body_offers_its_widgets_properties_and_children() {
        let items = labels(&counter("    view { Column { g| } }"));
        assert!(has(&items, "gap", CompletionKind::Property), "{items:?}");
        assert!(has(&items, "Text", CompletionKind::Class));
        assert!(has(&items, "Counter", CompletionKind::Class));
        assert!(has(&items, "on", CompletionKind::Keyword));
        let after = labels(&counter("    view { Column { gap: 8dp; | } }"));
        assert!(has(&after, "gap", CompletionKind::Property), "{after:?}");
    }

    #[test]
    fn after_on_come_the_widgets_events() {
        let items = labels(&counter("    view { Button { on | } }"));
        assert!(has(&items, "click", CompletionKind::Event), "{items:?}");
        assert!(!has(&items, "text", CompletionKind::Property));
    }

    #[test]
    fn a_same_file_components_inputs_and_events_are_offered() {
        let items = labels(&counter("    view { Counter { | } }"));
        assert!(has(&items, "step", CompletionKind::Property), "{items:?}");
        let events = labels(&counter("    view { Counter { on | } }"));
        assert!(has(&events, "changed", CompletionKind::Event), "{events:?}");
    }

    #[test]
    fn an_expression_reads_members_locals_and_imports() {
        let items = labels(&counter(
            "    action bump(by: I32) { let twice = by * 2; count += | }",
        ));
        assert!(has(&items, "count", CompletionKind::Property), "{items:?}");
        assert!(has(&items, "step", CompletionKind::Property));
        assert!(has(&items, "bump", CompletionKind::Method));
        assert!(has(&items, "by", CompletionKind::Variable));
        assert!(has(&items, "twice", CompletionKind::Variable));
        assert!(has(&items, "text", CompletionKind::Module));
        let top = labels(&format!(
            "fn double(x: I32) -> I32 {{ x * 2 }}\n{}",
            counter("    computed c: I32 = |;")
        ));
        assert!(has(&top, "double", CompletionKind::Function), "{top:?}");
        let binding = labels(&counter("    view { Text(|) }"));
        assert!(
            has(&binding, "count", CompletionKind::Property),
            "{binding:?}"
        );
    }

    #[test]
    fn a_library_path_offers_its_functions() {
        let items = labels(&counter("    computed loud: String = text::|;"));
        assert!(has(&items, "upper", CompletionKind::Function), "{items:?}");
        let import = labels("import viso::|;\n");
        assert!(has(&import, "text", CompletionKind::Module), "{import:?}");
        let root = labels("import |;\n");
        assert!(has(&root, "viso", CompletionKind::Module), "{root:?}");
    }

    #[test]
    fn a_type_position_offers_types() {
        let items = labels(&counter("    state label: |"));
        assert!(has(&items, "String", CompletionKind::Struct), "{items:?}");
        assert!(has(&items, "List", CompletionKind::Struct));
        assert!(!has(&items, "count", CompletionKind::Property));
    }

    #[test]
    fn declaration_starts_offer_keywords() {
        let members = labels(&counter("    |"));
        assert!(
            has(&members, "action", CompletionKind::Keyword),
            "{members:?}"
        );
        assert!(has(&members, "view", CompletionKind::Keyword));
        let top = labels("component A { view { Text {} } }\n|");
        assert!(has(&top, "component", CompletionKind::Keyword), "{top:?}");
        assert!(!has(&top, "action", CompletionKind::Keyword));
    }

    #[test]
    fn a_property_or_field_expecting_an_enum_offers_its_variants() {
        let items = labels(&counter("    view { Row { align: | } }"));
        assert!(
            has(&items, "center", CompletionKind::EnumMember),
            "{items:?}"
        );
        assert!(has(&items, "count", CompletionKind::Property));
        let sizing = labels(&counter("    view { Text(\"a\") { width: f| } }"));
        assert!(
            has(&sizing, "fill", CompletionKind::EnumMember),
            "{sizing:?}"
        );
        let style = labels("style S for Row {\n    justify: |\n}\n");
        assert!(has(&style, "end", CompletionKind::EnumMember), "{style:?}");
        let field = labels(
            "enum Tone { calm; loud }\nrecord Look { tone: Tone = Tone::calm }\nconst L: Look = Look { tone: | };\n",
        );
        assert!(has(&field, "loud", CompletionKind::EnumMember), "{field:?}");
        let input = labels(&format!(
            "enum Tone {{ calm; loud }}\ncomponent B {{ input tone: Tone = Tone::calm; view {{ Column {{}} }} }}\n{}",
            counter("    view { B { tone: | } }")
        ));
        assert!(has(&input, "calm", CompletionKind::EnumMember), "{input:?}");
    }

    #[test]
    fn an_enums_variants_follow_its_name() {
        let items = labels("enum Mode { Light, Dark }\nconst M: Mode = Mode::|;\n");
        assert!(has(&items, "Dark", CompletionKind::EnumMember), "{items:?}");
    }
}
