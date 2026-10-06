//! Structured edits (§143): a change named by what it does and where, by
//! [`SymbolId`] and [`SyntaxId`] instead of by line and column, so a tool or an
//! AI agent cannot land an edit on a line that has drifted since it looked.
//!
//! [`Document::apply`] turns one [`StructuredEdit`] into [`TextEdit`]s laid out
//! the way the formatter would (four-space indentation, one member per line,
//! following the surrounding text), applies them, and recompiles: an edit that
//! introduces an error the source did not already have is refused with that
//! error ([`EditError::Rejected`]), so every accepted edit is a checked one.
//!
//! A [`SyntaxId`] names a view structure node by the declaration that holds it
//! and the path down to it, each step a kind and an ordinal among the siblings
//! of that kind. It survives every edit outside its declaration and every edit
//! inside it that adds no sibling of the same kind before it on its path.
//!
//! Cold-path tooling throughout (AGENTS 7.2): one edit reparses and compiles the
//! file twice.

use std::cell::OnceCell;
use std::fmt;
use std::str::FromStr;

use crate::diag::{Diagnostic, Severity, TextEdit};
use crate::frontend::{Origin, compile_file};
use crate::resolve::{
    ModuleGraph, ModulePath, NameInterner, SourceUnit, SymbolDecl, SymbolId, resolve,
};
use crate::syntax::{
    Entry, SyntaxElement, SyntaxKind, SyntaxNode, TextRange, TextSize, parse_entry, tokenize,
};

/// One indentation level, the formatter's.
const INDENT: &str = "    ";

/// The node kinds a [`SyntaxId`] path steps through: the view's structure, its
/// nodes' property bindings and handlers.
const ADDRESSABLE: &[SyntaxKind] = &[
    SyntaxKind::ViewDecl,
    SyntaxKind::AnonymousNode,
    SyntaxKind::NamedNode,
    SyntaxKind::PartNode,
    SyntaxKind::TemplateUse,
    SyntaxKind::PartOverride,
    SyntaxKind::PartReplace,
    SyntaxKind::FillClause,
    SyntaxKind::ViewIf,
    SyntaxKind::ViewFor,
    SyntaxKind::ViewMatch,
    SyntaxKind::ViewMatchArm,
    SyntaxKind::PropertyBinding,
    SyntaxKind::TwoWayBinding,
    SyntaxKind::EventHandler,
];

/// The view structure items: what a node body or view block nests.
const STRUCTURE: &[SyntaxKind] = &[
    SyntaxKind::AnonymousNode,
    SyntaxKind::NamedNode,
    SyntaxKind::PartNode,
    SyntaxKind::TemplateUse,
    SyntaxKind::ViewIf,
    SyntaxKind::ViewFor,
    SyntaxKind::ViewMatch,
];

/// The declarations that carry a [`SymbolId`] through their name token.
const DECLARATIONS: &[SyntaxKind] = &[
    SyntaxKind::ComponentDecl,
    SyntaxKind::SystemDecl,
    SyntaxKind::TemplateDecl,
    SyntaxKind::RecordDecl,
    SyntaxKind::EnumDecl,
    SyntaxKind::TraitDecl,
    SyntaxKind::ShaderDecl,
    SyntaxKind::ThemeDecl,
    SyntaxKind::StyleDecl,
    SyntaxKind::ConstDecl,
    SyntaxKind::TypeAliasDecl,
    SyntaxKind::FnDecl,
    SyntaxKind::ActionDecl,
    SyntaxKind::TaskDecl,
    SyntaxKind::InputDecl,
    SyntaxKind::StateDecl,
    SyntaxKind::ComputedDecl,
    SyntaxKind::EventDecl,
    SyntaxKind::SlotDecl,
    SyntaxKind::EffectDecl,
    SyntaxKind::ResourceDecl,
    SyntaxKind::NativeDecl,
];

/// One step of a [`SyntaxId`] path: the `ordinal`-th addressable child of kind
/// `kind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Step {
    /// The node kind, one of the view's structure, binding or handler kinds.
    pub kind: SyntaxKind,
    /// Its index among the addressable children of its parent of that kind.
    pub ordinal: u32,
}

/// The durable address of a view node: its owning declaration's symbol and the
/// path down from it. Text form `<symbol>/<Kind>.<ordinal>/...`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SyntaxId {
    /// The declaration holding the node.
    pub owner: SymbolId,
    /// The steps from the declaration to the node; empty names the declaration.
    pub path: Vec<Step>,
}

impl fmt::Display for SyntaxId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.owner)?;
        for step in &self.path {
            write!(f, "/{:?}.{}", step.kind, step.ordinal)?;
        }
        Ok(())
    }
}

impl FromStr for SyntaxId {
    type Err = ();

    fn from_str(text: &str) -> Result<Self, ()> {
        let mut parts = text.split('/');
        let owner = parts.next().ok_or(())?.parse()?;
        let path = parts
            .map(|part| {
                let (kind, ordinal) = part.split_once('.').ok_or(())?;
                let kind = *ADDRESSABLE
                    .iter()
                    .find(|k| format!("{k:?}") == kind)
                    .ok_or(())?;
                Ok(Step {
                    kind,
                    ordinal: ordinal.parse().map_err(|_| ())?,
                })
            })
            .collect::<Result<_, ()>>()?;
        Ok(SyntaxId { owner, path })
    }
}

/// One structured edit (§143). Code fields (types, expressions, statements,
/// nodes) are source text, laid out into place by the edit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StructuredEdit {
    /// `import <path>;` after the last import.
    AddImport {
        /// The imported path, e.g. `viso::time` or `app::ui::{Card, Badge}`.
        path: String,
    },
    /// An empty component at the end of the file.
    CreateComponent {
        /// Its name.
        name: String,
        /// Whether it is exported.
        export: bool,
    },
    /// `input <name>: <ty> (= <default>)?;` after the component's last input.
    AddInput {
        /// The component (or template, or system).
        component: SymbolId,
        /// The input's name.
        name: String,
        /// Its type.
        ty: String,
        /// Its default value.
        default: Option<String>,
    },
    /// `state <name>(: <ty>)? = <init>;` after the component's last state.
    AddState {
        /// The component (or system).
        component: SymbolId,
        /// The state's name.
        name: String,
        /// Its type, when not inferred from `init`.
        ty: Option<String>,
        /// Its initializer.
        init: String,
    },
    /// `action <name>(<params>) { <body> }` before the component's view.
    AddAction {
        /// The component.
        component: SymbolId,
        /// The action's name.
        name: String,
        /// Its parameter list, without parentheses.
        params: String,
        /// Its statements.
        body: String,
    },
    /// A view structure node as the `index`-th structure child of `parent`
    /// (a node, the view, or a `for`/`fill`/match arm), or its last when
    /// `index` is past the end.
    InsertNode {
        /// The parent.
        parent: SyntaxId,
        /// Its position among the parent's structure children.
        index: usize,
        /// The node's source, e.g. `Text { text: "hi"; }`.
        node: String,
    },
    /// `<property>: <value>;` on a node: the existing binding's value replaced,
    /// or a new binding after the node's last one.
    SetPropertyBinding {
        /// The node (or `override part`).
        node: SyntaxId,
        /// The property path, e.g. `color` or `padding.left`.
        property: String,
        /// The bound expression.
        value: String,
    },
    /// `on <event>(<pattern>)? { <body> }` after the node's bindings and
    /// handlers.
    AttachEventHandler {
        /// The node.
        node: SyntaxId,
        /// The event's name.
        event: String,
        /// The payload pattern, if the handler binds one.
        pattern: Option<String>,
        /// Its statements.
        body: String,
    },
    /// The node wrapped in `for <item> in <list> key <key> { .. }`.
    WrapInKeyedFor {
        /// The node.
        node: SyntaxId,
        /// The item pattern.
        item: String,
        /// The iterated list.
        list: String,
        /// The stable key of an item.
        key: String,
    },
    /// A `resource` loading through the task, declared after it, its
    /// `Resource<T, E>` read off the task's `Result<T, E>`; every plain
    /// `start <task>(..);` (no slot, no handlers) of the component is removed,
    /// the resource now loading it.
    ConvertTaskToResource {
        /// The task, a member of a component.
        task: SymbolId,
        /// The resource's name.
        name: String,
        /// The `load` call's arguments.
        args: Vec<String>,
        /// The resource's key expression.
        key: String,
    },
    /// `impl <trait_name> for <ty> { <body> }` at the end of the file.
    AddTraitImpl {
        /// The implemented trait.
        trait_name: String,
        /// The implementing type.
        ty: String,
        /// The impl's members.
        body: String,
    },
}

/// What an edit created, for chaining further edits onto it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Created {
    /// A declaration.
    Symbol(SymbolId),
    /// A view node.
    Syntax(SyntaxId),
}

/// An accepted edit.
#[derive(Debug, Clone)]
pub struct Applied {
    /// The text edits, in source order, none overlapping.
    pub edits: Vec<TextEdit>,
    /// The edited source.
    pub source: String,
    /// The declaration or node the edit created, if it created one.
    pub created: Option<Created>,
}

/// Why an edit was refused.
#[derive(Debug, Clone)]
pub enum EditError {
    /// No declaration of the file has the symbol.
    UnknownSymbol(SymbolId),
    /// The path names no node of the file.
    UnknownSyntax(SyntaxId),
    /// The target is not the kind of declaration or node the edit applies to.
    WrongTarget(&'static str),
    /// What the edit adds is already there.
    Exists(String),
    /// The edited source has errors the source did not have.
    Rejected(Vec<Diagnostic>),
}

impl fmt::Display for EditError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EditError::UnknownSymbol(id) => write!(f, "no declaration has the symbol {id}"),
            EditError::UnknownSyntax(id) => write!(f, "no node is at {id}"),
            EditError::WrongTarget(expected) => write!(f, "the target is not {expected}"),
            EditError::Exists(what) => write!(f, "{what} is already there"),
            EditError::Rejected(errors) => {
                write!(f, "the edit introduces {} error(s)", errors.len())?;
                for e in errors {
                    write!(f, "; {}: {}", e.code, e.message)?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for EditError {}

/// One source file, parsed and resolved, that structured edits address.
pub struct Document {
    source: String,
    origin: Origin,
    root: SyntaxNode,
    decls: Vec<SymbolDecl>,
    diagnostics: OnceCell<Vec<Diagnostic>>,
}

/// An edit's text edits, before they are checked, and where in the edited
/// source the node it creates starts: `(edit index, offset in its replacement)`.
struct Plan {
    edits: Vec<TextEdit>,
    focus: Option<(usize, usize)>,
}

impl Plan {
    /// The edits in source order, and the focus re-indexed to match.
    fn sorted(mut self) -> (Vec<TextEdit>, Option<(usize, usize)>) {
        let focus = self
            .focus
            .map(|(i, within)| (self.edits[i].range.start(), within));
        self.edits.sort_by_key(|e| e.range.start());
        let focus = focus.and_then(|(start, within)| {
            let index = self
                .edits
                .iter()
                .position(|e| e.range.start() == start && !e.replacement.is_empty())?;
            Some((index, within))
        });
        (self.edits, focus)
    }
}

impl Plan {
    fn one(edit: TextEdit, focus: usize) -> Plan {
        Plan {
            edits: vec![edit],
            focus: Some((0, focus)),
        }
    }
}

impl Document {
    /// Parses and resolves `source` as the module `origin` names.
    pub fn new(source: impl Into<String>, origin: &Origin) -> Document {
        let source = source.into();
        let parse = parse_entry(&tokenize(&source), &source, Entry::CompilationUnit);
        let root = SyntaxNode::new_root(parse.root.clone());
        let mut interner = NameInterner::new();
        let segments: Vec<&str> = origin.module.iter().map(String::as_str).collect();
        let path = ModulePath::intern(&mut interner, &segments);
        let units = vec![SourceUnit::new(path, parse)];
        let graph = ModuleGraph::build(&units, &interner);
        let decls = resolve(&graph, &units, &mut interner, &origin.package)
            .pop()
            .map(|m| m.decls)
            .unwrap_or_default();
        Document {
            source,
            origin: origin.clone(),
            root,
            decls,
            diagnostics: OnceCell::new(),
        }
    }

    /// The source text.
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Every diagnostic of the file, of every stage, compiled once.
    pub fn diagnostics(&self) -> &[Diagnostic] {
        self.diagnostics
            .get_or_init(|| compile_file(&self.source, &self.origin).diagnostics)
    }

    /// The symbol of the declaration at `path`, outermost first, e.g.
    /// `["Counter", "count"]` for the state `count` of the component `Counter`.
    pub fn symbol(&self, path: &[&str]) -> Option<SymbolId> {
        let (last, outer) = path.split_last()?;
        self.decls
            .iter()
            .filter(|d| self.text(d.name_range) == *last)
            .find(|d| {
                let Some(node) = self.decl_node(d) else {
                    return false;
                };
                let names: Vec<&str> = node
                    .ancestors()
                    .skip(1)
                    .filter_map(|n| self.decl_named(&n))
                    .map(|d| self.text(d.name_range))
                    .collect();
                names.iter().rev().eq(outer.iter())
            })
            .map(|d| d.id)
    }

    /// The innermost declaration containing `offset`.
    pub fn symbol_at(&self, offset: TextSize) -> Option<SymbolId> {
        let node = self.node_at(offset)?;
        node.ancestors().find_map(|n| self.symbol_of(&n))
    }

    /// The address of the innermost view node containing `offset`.
    pub fn syntax_id_at(&self, offset: TextSize) -> Option<SyntaxId> {
        let node = self.node_at(offset)?;
        let node = node.ancestors().find(|n| ADDRESSABLE.contains(&n.kind()))?;
        self.syntax_id_of(&node)
    }

    /// The edits whose every argument follows from the declaration at
    /// `offset`, each titled and already checked to apply: today a task
    /// returning `Result` becomes a resource loading it by its own parameters,
    /// named after it without a `fetch_`/`load_`/`get_` prefix.
    pub fn suggestions_at(&self, offset: TextSize) -> Vec<(String, StructuredEdit)> {
        let Some(task) = self
            .node_at(offset)
            .and_then(|n| n.ancestors().find(|a| a.kind() == SyntaxKind::TaskDecl))
        else {
            return Vec::new();
        };
        let Some(decl) = self.decl_named(&task) else {
            return Vec::new();
        };
        let task_name = self.text(decl.name_range);
        let stem = ["fetch_", "load_", "get_"]
            .iter()
            .find_map(|p| task_name.strip_prefix(p))
            .filter(|s| !s.is_empty());
        let name = stem.map_or_else(|| format!("{task_name}_data"), str::to_string);
        let args: Vec<String> = task
            .children()
            .into_iter()
            .find(|c| c.kind() == SyntaxKind::ParamList)
            .map(|list| {
                list.children()
                    .iter()
                    .filter_map(|p| self.decl_named_param(p))
                    .collect()
            })
            .unwrap_or_default();
        let key = match args.as_slice() {
            [] => "()".to_string(),
            [one] => one.clone(),
            many => format!("({})", many.join(", ")),
        };
        let edit = StructuredEdit::ConvertTaskToResource {
            task: decl.id,
            name: name.clone(),
            args,
            key,
        };
        if self.apply(&edit).is_err() {
            return Vec::new();
        }
        vec![(
            format!("Convert task `{task_name}` to resource `{name}`"),
            edit,
        )]
    }

    /// The name of a parameter node.
    fn decl_named_param(&self, param: &SyntaxNode) -> Option<String> {
        if param.kind() != SyntaxKind::Param {
            return None;
        }
        name_token(param)
    }

    /// The source range of the node `id` addresses.
    pub fn range_of(&self, id: &SyntaxId) -> Option<TextRange> {
        self.find(id).map(|n| n.text_range())
    }

    /// Plans `edit`, applies it, and checks the result.
    pub fn apply(&self, edit: &StructuredEdit) -> Result<Applied, EditError> {
        let (edits, focus) = self.plan(edit)?.sorted();
        let source = splice(&self.source, &edits);
        let edited = Document::new(source, &self.origin);
        let introduced = introduced(self.diagnostics(), edited.diagnostics());
        if !introduced.is_empty() {
            return Err(EditError::Rejected(introduced));
        }
        let created = focus.and_then(|(index, within)| {
            let shift: i64 = edits[..index]
                .iter()
                .map(|e| e.replacement.len() as i64 - e.range.len().to_usize() as i64)
                .sum();
            let at = edits[index].range.start().to_usize() as i64 + shift + within as i64;
            edited.created_at(TextSize::new(at as u32))
        });
        Ok(Applied {
            edits,
            source: edited.source.clone(),
            created,
        })
    }

    fn plan(&self, edit: &StructuredEdit) -> Result<Plan, EditError> {
        match edit {
            StructuredEdit::AddImport { path } => self.add_import(path),
            StructuredEdit::CreateComponent { name, export } => {
                let export = if *export { "export " } else { "" };
                let text = format!(
                    "{export}component {name} {{\n{INDENT}view {{\n{INDENT}{INDENT}Column {{}}\n{INDENT}}}\n}}"
                );
                Ok(self.append_item(&text))
            }
            StructuredEdit::AddInput {
                component,
                name,
                ty,
                default,
            } => {
                let default = default
                    .as_ref()
                    .map_or_else(String::new, |d| format!(" = {d}"));
                self.add_member(
                    *component,
                    Rank::Input,
                    &format!("input {name}: {ty}{default};"),
                )
            }
            StructuredEdit::AddState {
                component,
                name,
                ty,
                init,
            } => {
                let ty = ty.as_ref().map_or_else(String::new, |t| format!(": {t}"));
                self.add_member(
                    *component,
                    Rank::State,
                    &format!("state {name}{ty} = {init};"),
                )
            }
            StructuredEdit::AddAction {
                component,
                name,
                params,
                body,
            } => {
                let text = format!("action {name}({params}) {}", block(body));
                self.add_member(*component, Rank::Action, &text)
            }
            StructuredEdit::InsertNode {
                parent,
                index,
                node,
            } => self.insert_node(parent, *index, node),
            StructuredEdit::SetPropertyBinding {
                node,
                property,
                value,
            } => self.set_property(node, property, value),
            StructuredEdit::AttachEventHandler {
                node,
                event,
                pattern,
                body,
            } => {
                let target = self.find_or(node)?;
                let body_node = container_of(&target).ok_or(EditError::WrongTarget("a node"))?;
                let pattern = pattern
                    .as_ref()
                    .map_or_else(String::new, |p| format!("({p})"));
                let text = format!("on {event}{pattern} {}", block(body));
                let after = children_of(&body_node, |k| {
                    matches!(
                        k,
                        SyntaxKind::PropertyBinding
                            | SyntaxKind::TwoWayBinding
                            | SyntaxKind::EventHandler
                    )
                })
                .pop();
                Ok(self.insert_in(&body_node, after.map_or(At::Start, At::After), &text))
            }
            StructuredEdit::WrapInKeyedFor {
                node,
                item,
                list,
                key,
            } => {
                let target = self.find_or(node)?;
                if !STRUCTURE.contains(&target.kind()) {
                    return Err(EditError::WrongTarget("a view structure node"));
                }
                let indent = line_indent(&self.source, target.text_range().start());
                let inner = shifted(&self.source, &target);
                let text = format!(
                    "for {item} in {list} key {key} {{\n{indent}{INDENT}{inner}\n{indent}}}"
                );
                Ok(Plan::one(TextEdit::new(target.text_range(), text), 0))
            }
            StructuredEdit::ConvertTaskToResource {
                task,
                name,
                args,
                key,
            } => self.task_to_resource(*task, name, args, key),
            StructuredEdit::AddTraitImpl {
                trait_name,
                ty,
                body,
            } => Ok(self.append_item(&format!("impl {trait_name} for {ty} {}", block(body)))),
        }
    }

    fn add_import(&self, path: &str) -> Result<Plan, EditError> {
        let wanted: String = path.split_whitespace().collect();
        let imports: Vec<SyntaxNode> = self
            .root
            .children()
            .into_iter()
            .filter(|n| n.kind() == SyntaxKind::ImportDecl)
            .collect();
        for import in &imports {
            let text: String = import.text().split_whitespace().collect();
            if text.trim_start_matches("import").trim_end_matches(';') == wanted {
                return Err(EditError::Exists(format!("`import {path};`")));
            }
        }
        let text = format!("import {path};");
        if let Some(last) = imports.last() {
            let at = TextRange::empty(last.text_range().end());
            return Ok(Plan::one(TextEdit::new(at, format!("\n{text}")), 1));
        }
        match self.root.children().first() {
            Some(first) => {
                let at = TextRange::empty(first.text_range().start());
                Ok(Plan::one(TextEdit::new(at, format!("{text}\n\n")), 0))
            }
            None => {
                let at = TextRange::empty(end_of(&self.source));
                let lead = if self.source.trim().is_empty() || self.source.ends_with('\n') {
                    ""
                } else {
                    "\n"
                };
                Ok(Plan::one(
                    TextEdit::new(at, format!("{lead}{text}\n")),
                    lead.len(),
                ))
            }
        }
    }

    /// `text` as a new top-level item after the last one.
    fn append_item(&self, text: &str) -> Plan {
        match self.root.children().last() {
            Some(last) => {
                let at = TextRange::empty(last.text_range().end());
                Plan::one(TextEdit::new(at, format!("\n\n{text}")), 2)
            }
            None => {
                let at = TextRange::empty(end_of(&self.source));
                let lead = if self.source.trim().is_empty() {
                    ""
                } else {
                    "\n"
                };
                Plan::one(TextEdit::new(at, format!("{lead}{text}\n")), lead.len())
            }
        }
    }

    fn add_member(&self, owner: SymbolId, rank: Rank, text: &str) -> Result<Plan, EditError> {
        let decl = self.decl_of(owner)?;
        if !matches!(
            decl.kind(),
            SyntaxKind::ComponentDecl | SyntaxKind::SystemDecl | SyntaxKind::TemplateDecl
        ) {
            return Err(EditError::WrongTarget("a component, system or template"));
        }
        let members = members_of(&decl);
        let at = match members
            .iter()
            .rev()
            .find(|m| Rank::of(m.kind()).is_some_and(|r| r <= rank))
        {
            Some(after) => At::After(after.clone()),
            None => At::Start,
        };
        Ok(self.insert_in(&decl, at, text))
    }

    fn insert_node(&self, parent: &SyntaxId, index: usize, node: &str) -> Result<Plan, EditError> {
        let target = self.find_or(parent)?;
        let body = container_of(&target).ok_or(EditError::WrongTarget("a node or view"))?;
        let items = children_of(&body, |k| STRUCTURE.contains(&k));
        let at = match items.get(index) {
            Some(before) => At::Before(before.clone()),
            None => match items.last() {
                Some(last) => At::After(last.clone()),
                None => At::End,
            },
        };
        Ok(self.insert_in(&body, at, node))
    }

    fn set_property(
        &self,
        node: &SyntaxId,
        property: &str,
        value: &str,
    ) -> Result<Plan, EditError> {
        let target = self.find_or(node)?;
        let body = container_of(&target).ok_or(EditError::WrongTarget("a node"))?;
        let wanted: String = property.split_whitespace().collect();
        let bindings = children_of(&body, |k| {
            matches!(k, SyntaxKind::PropertyBinding | SyntaxKind::TwoWayBinding)
        });
        for binding in &bindings {
            if binding.kind() != SyntaxKind::PropertyBinding {
                continue;
            }
            let children = binding.children();
            let Some(path) = children
                .iter()
                .find(|c| c.kind() == SyntaxKind::PropertyPath)
            else {
                continue;
            };
            let text: String = path.text().split_whitespace().collect();
            if text != wanted {
                continue;
            }
            let Some(old) = children
                .iter()
                .find(|c| c.kind() != SyntaxKind::PropertyPath)
            else {
                continue;
            };
            return Ok(Plan {
                edits: vec![TextEdit::new(old.text_range(), value)],
                focus: None,
            });
        }
        let text = format!("{property}: {value};");
        let at = bindings.last().cloned().map_or(At::Start, At::After);
        Ok(self.insert_in(&body, at, &text))
    }

    fn task_to_resource(
        &self,
        task: SymbolId,
        name: &str,
        args: &[String],
        key: &str,
    ) -> Result<Plan, EditError> {
        let decl = self.decl_of(task)?;
        if decl.kind() != SyntaxKind::TaskDecl {
            return Err(EditError::WrongTarget("a task"));
        }
        let owner = decl
            .parent()
            .filter(|p| p.kind() == SyntaxKind::ComponentDecl)
            .ok_or(EditError::WrongTarget("a task declared in a component"))?;
        let (ok, err) =
            result_args(&decl).ok_or(EditError::WrongTarget("a task returning `Result<T, E>`"))?;
        let task_name = self
            .decl_named(&decl)
            .map_or("", |d| self.text(d.name_range));
        let text = format!(
            "resource {name}: Resource<{ok}, {err}> {{\n{INDENT}load = {task_name}({});\n{INDENT}key = {key};\n}}",
            args.join(", ")
        );
        let mut plan = self.insert_in(&owner, At::After(decl.clone()), &text);
        for start in owner
            .descendants()
            .into_iter()
            .filter(|n| n.kind() == SyntaxKind::StartStmt)
        {
            let children = start.children();
            let plain = !children
                .iter()
                .any(|c| matches!(c.kind(), SyntaxKind::StartSlot | SyntaxKind::StartHandlers));
            let calls_task = children
                .iter()
                .find(|c| c.kind() == SyntaxKind::CallExpr)
                .and_then(|call| call.first_child())
                .is_some_and(|head| {
                    head.kind() == SyntaxKind::PathExpr && head.text().trim() == task_name
                });
            if plain && calls_task {
                plan.edits.push(TextEdit::new(
                    line_span(&self.source, start.text_range()),
                    "",
                ));
            }
        }
        Ok(plan)
    }

    /// `text` placed in the braced `container` at `at`, laid out on its own
    /// line at the indentation of its siblings.
    fn insert_in(&self, container: &SyntaxNode, at: At, text: &str) -> Plan {
        let source = &self.source;
        let (open, close) = braces(container);
        let outer = line_indent(source, open);
        let inner = format!("{outer}{INDENT}");
        let laid = |indent: &str| indented(text, indent);
        match at {
            At::After(sibling) => {
                let start = sibling.text_range().start();
                let indent = if same_line(source, open, start) {
                    inner.clone()
                } else {
                    line_indent(source, start)
                };
                let lead = format!("\n{indent}");
                Plan::one(
                    TextEdit::new(
                        TextRange::empty(sibling.text_range().end()),
                        format!("{lead}{}", laid(&indent)),
                    ),
                    lead.len(),
                )
            }
            At::Before(sibling) => {
                let start = sibling.text_range().start();
                if same_line(source, open, start) {
                    let lead = format!("\n{inner}");
                    return Plan::one(
                        TextEdit::new(
                            TextRange::empty(start),
                            format!("{lead}{}{lead}", laid(&inner)),
                        ),
                        lead.len(),
                    );
                }
                let indent = line_indent(source, start);
                Plan::one(
                    TextEdit::new(
                        TextRange::empty(start),
                        format!("{}\n{indent}", laid(&indent)),
                    ),
                    0,
                )
            }
            At::Start | At::End => {
                let members = members_of(container);
                let sibling = match at {
                    At::Start => members.first(),
                    _ => members.last(),
                };
                match (sibling, at) {
                    (Some(first), At::Start) => {
                        self.insert_in(container, At::Before(first.clone()), text)
                    }
                    (Some(last), _) => self.insert_in(container, At::After(last.clone()), text),
                    (None, _) => {
                        let lead = format!("\n{inner}");
                        Plan::one(
                            TextEdit::new(
                                TextRange::new(open + TextSize::new(1), close),
                                format!("{lead}{}\n{outer}", laid(&inner)),
                            ),
                            lead.len(),
                        )
                    }
                }
            }
        }
    }

    fn find_or(&self, id: &SyntaxId) -> Result<SyntaxNode, EditError> {
        self.find(id)
            .ok_or_else(|| EditError::UnknownSyntax(id.clone()))
    }

    fn find(&self, id: &SyntaxId) -> Option<SyntaxNode> {
        let mut at = self.decl_of(id.owner).ok()?;
        for step in &id.path {
            at = addressable_children(&at)
                .into_iter()
                .filter(|n| n.kind() == step.kind)
                .nth(step.ordinal as usize)?;
        }
        Some(at)
    }

    fn syntax_id_of(&self, node: &SyntaxNode) -> Option<SyntaxId> {
        let mut path = Vec::new();
        let mut at = node.clone();
        loop {
            if let Some(owner) = self.symbol_of(&at) {
                path.reverse();
                return Some(SyntaxId { owner, path });
            }
            let parent = at
                .ancestors()
                .skip(1)
                .find(|n| ADDRESSABLE.contains(&n.kind()) || self.symbol_of(n).is_some())?;
            let ordinal = addressable_children(&parent)
                .into_iter()
                .filter(|n| n.kind() == at.kind())
                .position(|n| n == at)?;
            path.push(Step {
                kind: at.kind(),
                ordinal: ordinal as u32,
            });
            at = parent;
        }
    }

    /// The symbol `node` declares, if it is a declaration.
    fn symbol_of(&self, node: &SyntaxNode) -> Option<SymbolId> {
        self.decl_named(node).map(|d| d.id)
    }

    /// The declaration `node` is: the one whose name is a token of its own.
    fn decl_named(&self, node: &SyntaxNode) -> Option<&SymbolDecl> {
        if !DECLARATIONS.contains(&node.kind()) {
            return None;
        }
        node.children_with_tokens()
            .into_iter()
            .find_map(|e| match e {
                SyntaxElement::Token(t) => {
                    self.decls.iter().find(|d| d.name_range == t.text_range())
                }
                SyntaxElement::Node(_) => None,
            })
    }

    fn decl_of(&self, id: SymbolId) -> Result<SyntaxNode, EditError> {
        self.decls
            .iter()
            .find(|d| d.id == id)
            .and_then(|d| self.decl_node(d))
            .ok_or(EditError::UnknownSymbol(id))
    }

    fn decl_node(&self, decl: &SymbolDecl) -> Option<SyntaxNode> {
        self.node_at(decl.name_range.start())
            .filter(|n| DECLARATIONS.contains(&n.kind()))
    }

    /// The innermost node whose range contains `offset`.
    fn node_at(&self, offset: TextSize) -> Option<SyntaxNode> {
        let mut at = self.root.clone();
        'down: loop {
            for child in at.children() {
                let range = child.text_range();
                if range.start() <= offset && offset < range.end() {
                    at = child;
                    continue 'down;
                }
            }
            return Some(at);
        }
    }

    /// The declaration or view node starting at `offset`.
    fn created_at(&self, offset: TextSize) -> Option<Created> {
        let node = self.node_at(offset)?;
        let start = node
            .ancestors()
            .filter(|n| n.text_range().start() == offset)
            .collect::<Vec<_>>();
        if let Some(symbol) = start.iter().find_map(|n| {
            let decl = if n.kind() == SyntaxKind::ExportDecl {
                n.children().into_iter().next()?
            } else {
                n.clone()
            };
            self.symbol_of(&decl)
        }) {
            return Some(Created::Symbol(symbol));
        }
        start
            .iter()
            .find(|n| ADDRESSABLE.contains(&n.kind()))
            .and_then(|n| self.syntax_id_of(n))
            .map(Created::Syntax)
    }

    fn text(&self, range: TextRange) -> &str {
        &self.source[range.as_usize()]
    }
}

/// Where in a braced container an insertion goes.
enum At {
    After(SyntaxNode),
    Before(SyntaxNode),
    Start,
    End,
}

/// The canonical member order an added member follows: after the last member
/// that ranks at or before it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Rank {
    Input,
    State,
    Action,
}

impl Rank {
    fn of(kind: SyntaxKind) -> Option<Rank> {
        Some(match kind {
            SyntaxKind::InputDecl | SyntaxKind::SlotDecl | SyntaxKind::EventDecl => Rank::Input,
            SyntaxKind::StateDecl => Rank::State,
            SyntaxKind::ViewDecl => return None,
            _ => Rank::Action,
        })
    }
}

/// The errors of `after` not among those of `before`, counted by code and
/// message so that an error's moving does not count as a new one.
fn introduced(before: &[Diagnostic], after: &[Diagnostic]) -> Vec<Diagnostic> {
    let mut old: Vec<(&str, &str)> = before
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .map(|d| (d.code, d.message.as_str()))
        .collect();
    after
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .filter(
            |d| match old.iter().position(|o| *o == (d.code, d.message.as_str())) {
                Some(i) => {
                    old.swap_remove(i);
                    false
                }
                None => true,
            },
        )
        .cloned()
        .collect()
}

/// `source` with `edits` (sorted, disjoint) applied.
fn splice(source: &str, edits: &[TextEdit]) -> String {
    let mut out = String::with_capacity(source.len());
    let mut at = 0;
    for edit in edits {
        let range = edit.range.as_usize();
        out.push_str(&source[at..range.start]);
        out.push_str(&edit.replacement);
        at = range.end;
    }
    out.push_str(&source[at..]);
    out
}

/// A statement block of `body`, empty as `{}`.
fn block(body: &str) -> String {
    if body.trim().is_empty() {
        return "{}".to_string();
    }
    format!("{{\n{INDENT}{}\n}}", indented(body, INDENT))
}

/// `text` with its common leading indentation removed and blank edges trimmed.
fn dedent(text: &str) -> String {
    let lines: Vec<&str> = text.trim_matches('\n').lines().collect();
    let common = lines
        .iter()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.len() - l.trim_start().len())
        .min()
        .unwrap_or(0);
    lines
        .iter()
        .map(|l| {
            if l.trim().is_empty() {
                ""
            } else {
                &l[common..]
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// `text` dedented, every line after the first indented by `indent` (the
/// first goes where the caller already is).
fn indented(text: &str, indent: &str) -> String {
    let text = dedent(text);
    let mut out = String::with_capacity(text.len());
    for (i, line) in text.lines().enumerate() {
        if i > 0 {
            out.push('\n');
            if !line.is_empty() {
                out.push_str(indent);
            }
        }
        out.push_str(line);
    }
    out
}

/// The text of `node` with one more level of indentation on each line after
/// its first, string literals left as written.
fn shifted(source: &str, node: &SyntaxNode) -> String {
    let range = node.text_range().as_usize();
    let strings: Vec<std::ops::Range<usize>> = node
        .descendants_with_tokens()
        .into_iter()
        .filter_map(|e| match e {
            SyntaxElement::Token(t)
                if matches!(
                    t.kind(),
                    SyntaxKind::StringLiteral | SyntaxKind::RawStringLiteral
                ) =>
            {
                Some(t.text_range().as_usize())
            }
            _ => None,
        })
        .collect();
    let mut out = String::new();
    for (i, ch) in source[range.clone()].char_indices() {
        out.push(ch);
        let at = range.start + i;
        if ch == '\n' && !strings.iter().any(|s| s.contains(&at)) {
            let next = source[at + 1..range.end].chars().next();
            if next.is_some_and(|c| c != '\n') {
                out.push_str(INDENT);
            }
        }
    }
    out
}

/// The leading whitespace of the line holding `offset`.
fn line_indent(source: &str, offset: TextSize) -> String {
    let at = offset.to_usize();
    let start = source[..at].rfind('\n').map_or(0, |i| i + 1);
    source[start..]
        .chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .collect()
}

fn same_line(source: &str, a: TextSize, b: TextSize) -> bool {
    let (a, b) = (
        a.to_usize().min(b.to_usize()),
        a.to_usize().max(b.to_usize()),
    );
    !source[a..b].contains('\n')
}

/// `range` widened to its whole line when nothing else is on it, else to the
/// whitespace after it.
fn line_span(source: &str, range: TextRange) -> TextRange {
    let (start, end) = (range.start().to_usize(), range.end().to_usize());
    let line_start = source[..start].rfind('\n').map_or(0, |i| i + 1);
    let line_end = source[end..].find('\n').map_or(source.len(), |i| end + i);
    let alone =
        source[line_start..start].trim().is_empty() && source[end..line_end].trim().is_empty();
    if alone && line_end < source.len() {
        return TextRange::new(size(line_start), size(line_end + 1));
    }
    let trailing = source[end..].chars().take_while(|c| *c == ' ').count();
    TextRange::new(range.start(), size(end + trailing))
}

fn size(at: usize) -> TextSize {
    TextSize::new(at as u32)
}

fn end_of(source: &str) -> TextSize {
    size(source.len())
}

/// The offsets of the `{` and `}` delimiting `container`'s members.
fn braces(container: &SyntaxNode) -> (TextSize, TextSize) {
    let mut open = None;
    let mut close = container.text_range().end();
    for element in container.children_with_tokens() {
        if let SyntaxElement::Token(t) = element {
            match t.kind() {
                SyntaxKind::LBrace if open.is_none() => open = Some(t.text_range().start()),
                SyntaxKind::RBrace => close = t.text_range().start(),
                _ => {}
            }
        }
    }
    (open.unwrap_or(container.text_range().start()), close)
}

/// The child nodes between `container`'s braces.
fn members_of(container: &SyntaxNode) -> Vec<SyntaxNode> {
    let (open, close) = braces(container);
    container
        .children()
        .into_iter()
        .filter(|n| n.text_range().start() > open && n.text_range().end() <= close)
        .collect()
}

/// The members of `container` whose kind `keep` accepts.
fn children_of(container: &SyntaxNode, keep: impl Fn(SyntaxKind) -> bool) -> Vec<SyntaxNode> {
    members_of(container)
        .into_iter()
        .filter(|n| keep(n.kind()))
        .collect()
}

/// The braced body a node's children, bindings and handlers go in.
fn container_of(node: &SyntaxNode) -> Option<SyntaxNode> {
    if node.kind() == SyntaxKind::PartOverride {
        return Some(node.clone());
    }
    node.children()
        .into_iter()
        .find(|c| matches!(c.kind(), SyntaxKind::NodeBody | SyntaxKind::ViewBlock))
}

/// The nearest addressable descendants of `node`, in source order.
fn addressable_children(node: &SyntaxNode) -> Vec<SyntaxNode> {
    let mut out = Vec::new();
    fn walk(node: &SyntaxNode, out: &mut Vec<SyntaxNode>) {
        for child in node.children() {
            if ADDRESSABLE.contains(&child.kind()) {
                out.push(child);
            } else if !DECLARATIONS.contains(&child.kind()) {
                walk(&child, out);
            }
        }
    }
    walk(node, &mut out);
    out
}

fn name_token(node: &SyntaxNode) -> Option<String> {
    node.children_with_tokens()
        .into_iter()
        .find_map(|e| match e {
            SyntaxElement::Token(t)
                if matches!(t.kind(), SyntaxKind::Ident | SyntaxKind::RawIdent) =>
            {
                Some(t.text())
            }
            _ => None,
        })
}

/// The `T` and `E` of a task's `-> Result<T, E>`.
fn result_args(task: &SyntaxNode) -> Option<(String, String)> {
    let ret = task
        .children()
        .into_iter()
        .find(|c| c.kind() == SyntaxKind::ReturnType)?;
    let ty = ret.first_child()?;
    let segments = ty.children();
    let [segment] = segments.as_slice() else {
        return None;
    };
    if name_token(segment).as_deref() != Some("Result") {
        return None;
    }
    let generics = segment
        .children()
        .into_iter()
        .find(|c| c.kind() == SyntaxKind::GenericArgs)?;
    let args: Vec<String> = generics
        .children()
        .iter()
        .map(|a| a.text().trim().to_string())
        .collect();
    let [ok, err] = args.as_slice() else {
        return None;
    };
    Some((ok.clone(), err.clone()))
}
