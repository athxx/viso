//! The resolution pass — what each name refers to.
//!
//! Given the [`ModuleGraph`](super::ModuleGraph) and the source units behind it, the
//! resolver answers *reference questions*, not *type questions* (that is Slice M).
//! For each module it:
//!
//! 1. builds a [`SymbolTable`] of the module's top-level declarations, minting a
//!    durable [`SymbolId`] per declaration and recording `export` visibility, and
//!    reports a within-namespace name clash as a collision diagnostic;
//! 2. builds the module's **import environment** — module renames (`import a as b;`)
//!    and selective items (`import a::{x, y as z};`) — mapping each local name to the
//!    exported symbol it refers to, reporting an import of a non-exported or missing
//!    name as [`E2001`](super::ResolveErrorKind::UnresolvedModule);
//! 3. walks the module resolving name uses: a type path's head resolves against the
//!    type namespace (locally, then imports); a value/property path head resolves
//!    against the local scope stack, the enclosing component's members, the value
//!    namespace and imports; an `on <event>` name resolves against the events of the
//!    component the node instantiates, and an `emit` against those of the enclosing
//!    component. A handler's payload pattern binds for its body. View-local `node` names, `for`-pattern
//!    bindings, and `let`/parameter names open local scopes whose uses resolve to a
//!    [`LocalSlot`](super::scope::LocalSlot).
//!
//! Everything is cold-path (AGENTS section 7.2): resolution runs once per build.

use crate::ast::{
    AstNode, Block, CompilationUnit, ComponentDecl, EventHandler, Expr, Item, Member, NamedNode,
    NodeBody, PathExpr, PropertyBinding, SystemDecl, TypePath, ViewBlock, ViewFor, ViewIf,
    ViewItem,
};
use crate::diag::{Applicability, Diagnostic, Fix, Related, TextEdit};
use crate::hir::Ty;
use crate::syntax::SyntaxNode;
use crate::syntax::span::{TextRange, TextSize};

use super::module::{ModuleGraph, NativeBinding, ResolveErrorKind, SourceUnit};
use super::name::{NameId, NameInterner};
use super::prelude::Prelude;
use super::scope::{LocalSlot, ModuleSymbol, Namespace, ScopeStack, SymbolTable};
use super::suggest;
use super::symbol::{SymbolId, SymbolIdentity, SymbolKind, fingerprint};
use viso_behavior::native::{NativeId, Natives};

/// What a name use resolves to.
///
/// `Hash`/`Ord` let tooling key an index by resolution target (both payloads —
/// [`SymbolId`] and [`LocalSlot`] — are themselves hashable and ordered); the
/// compiler proper compares by equality only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Resolution {
    /// A durable declaration identity, in this module or an imported one.
    Symbol(SymbolId),
    /// A lexically-bound local (a `let`, parameter, `for` pattern, or `node` name).
    Local(LocalSlot),
    /// A registered native function, method or handle type (a value path's head
    /// carries the whole path it names, `text::upper` or `Stopwatch::start`).
    Native(NativeId),
    /// The adaptive environment `env` a view reads, typed `Environment`: the View
    /// execution domain's context binding, which no declaration provides.
    Env,
    /// The `theme` a view reads, typed `Theme`: the other context binding.
    Theme,
}

impl Resolution {
    /// The type a type-position name resolving here denotes: a declaration's
    /// nominal type or a native handle type. A local names no type.
    pub fn nominal(self) -> Option<Ty> {
        match self {
            Resolution::Symbol(id) => Some(Ty::named(id)),
            Resolution::Native(id) => Some(Ty::Native(id)),
            Resolution::Local(_) | Resolution::Env | Resolution::Theme => None,
        }
    }
}

/// One resolved name use: the source span it occupies and what it resolves to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedRef {
    /// The span of the resolved name token.
    pub range: TextRange,
    /// The resolution target.
    pub to: Resolution,
}

/// The definition site of one module symbol: its id paired with the span of the
/// declaration's name token.
///
/// The resolver mints a [`SymbolId`] from a declaration's *canonical identity*
/// (never its position), so the id alone cannot locate the declaration in source.
/// Recording the name-token span here — captured at the single mint site, no
/// second tree walk — is what lets goto-definition and rename find and rewrite the
/// definition. This is a cold-path tooling aid (AGENTS 7.2); the compiler proper
/// does not consult it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SymbolDecl {
    /// The declaration's durable identity.
    pub id: SymbolId,
    /// The span of the declaration's name token.
    pub name_range: TextRange,
    /// Where `export ` goes to export the declaration (its keyword, after any
    /// attributes); `None` for one already exported and for a member.
    pub export_at: Option<TextSize>,
}

/// The result of resolving one module: its symbol table, its resolved references,
/// and any diagnostics gathered along the way.
pub struct ResolvedModule {
    /// The module's own top-level declarations by name and namespace.
    pub table: SymbolTable,
    /// Every name use the pass resolved, in source order.
    pub refs: Vec<ResolvedRef>,
    /// The definition site (name-token span) of every module symbol this module
    /// declares, in declaration order. The goto/rename target for a [`SymbolId`].
    pub decls: Vec<SymbolDecl>,
    /// Diagnostics from this module's resolution.
    pub errors: Vec<Diagnostic>,
}

/// A module's import environment.
#[derive(Default)]
struct ImportEnv {
    /// Local name → the symbol it imports.
    bindings: std::collections::HashMap<NameId, ImportBinding>,
    /// The local names of imports that name nothing, already reported, so their
    /// uses raise nothing more.
    unresolved: std::collections::HashSet<NameId>,
    /// Local name → the full path of the native library, function or type it
    /// imports or the module declares.
    natives: std::collections::HashMap<NameId, String>,
    /// `(owner, local name)` → the full path of the native a component's or
    /// system's member declaration declares.
    member_natives: std::collections::HashMap<(NameId, NameId), String>,
}

/// One import binding: a local name mapped to the exported symbol it names.
struct ImportBinding {
    /// The symbol the imported name resolves to.
    symbol: SymbolId,
    /// The namespace the imported symbol lives in.
    namespace: Namespace,
}

/// Resolves every module in `graph`, returning one [`ResolvedModule`] per graph node
/// in graph (sorted module-path) order.
///
/// `package` is the package identity mixed into every [`SymbolId`]; `units` supplies
/// the parse trees (matched to graph modules by module-path text). A module with no
/// matching unit resolves to an empty result.
pub fn resolve(
    graph: &ModuleGraph,
    units: &[SourceUnit],
    interner: &mut NameInterner,
    package: &str,
) -> Vec<ResolvedModule> {
    let prelude = Prelude::load(interner);
    // First pass: every module's public symbol table, so cross-module imports can be
    // resolved before any module body is walked.
    let mut tables: Vec<SymbolTable> = Vec::with_capacity(graph.modules().len());
    let mut all_decls: Vec<Vec<SymbolDecl>> = Vec::with_capacity(graph.modules().len());
    let mut early_errors: Vec<Vec<Diagnostic>> = Vec::with_capacity(graph.modules().len());
    for gm in graph.modules() {
        let module_text = gm.path.display(interner);
        let cu = unit_for(units, &module_text, interner);
        let (table, decls, errors) =
            build_symbol_table(cu.as_ref(), package, &module_text, interner);
        tables.push(table);
        all_decls.push(decls);
        early_errors.push(errors);
    }

    // Second pass: resolve each module's bodies against its own table plus imports,
    // with every component's member table in the package in view (an imported
    // component's events are part of its interface).
    let members: MemberTables<'_> = tables.iter().flat_map(SymbolTable::member_tables).collect();
    let mut passes = Vec::with_capacity(graph.modules().len());
    for (i, gm) in graph.modules().iter().enumerate() {
        let module_text = gm.path.display(interner);
        let cu = unit_for(units, &module_text, interner);
        let mut errors = std::mem::take(&mut early_errors[i]);
        let imports = build_import_env(
            cu.as_ref(),
            &gm.natives,
            graph,
            &tables,
            &all_decls,
            interner,
            &mut errors,
        );
        let mut pass = ModulePass {
            table: &tables[i],
            prelude: &prelude.module.table,
            members: &members,
            owner: None,
            owner_name: None,
            type_scopes: Vec::new(),
            decl_at: std::collections::HashMap::new(),
            node: None,
            decls: &all_decls[i],
            imports: &imports,
            natives: graph.natives(),
            interner,
            refs: Vec::new(),
            errors,
            scopes: ScopeStack::new(),
            // The component frontend has declarations/imports; a genuinely missing
            // user type is a real error here.
            defer_unresolved_types: false,
            in_view: false,
            owner_slots: Vec::new(),
        };
        pass.decl_at = all_decls[i].iter().map(|d| (d.name_range, d.id)).collect();
        if let Some(cu) = &cu {
            pass.resolve_unit(cu);
        }
        let ModulePass { refs, errors, .. } = pass;
        passes.push((refs, errors));
    }
    drop(members);
    tables
        .into_iter()
        .zip(all_decls)
        .zip(passes)
        .map(|((table, decls), (refs, errors))| ResolvedModule {
            table,
            refs,
            decls,
            errors,
        })
        .collect()
}

/// Resolves a single self-contained unit (no imports, no prelude) as the module
/// `module_text` of `package`.
pub(super) fn resolve_standalone(
    cu: &CompilationUnit,
    package: &str,
    module_text: &str,
    interner: &mut NameInterner,
) -> ResolvedModule {
    let (table, decls, errors) = build_symbol_table(Some(cu), package, module_text, interner);
    let members: MemberTables<'_> = table.member_tables().collect();
    let empty = SymbolTable::new();
    let imports = ImportEnv::default();
    let natives = Natives::new();
    let mut pass = ModulePass {
        table: &table,
        prelude: &empty,
        members: &members,
        owner: None,
        owner_name: None,
        type_scopes: Vec::new(),
        decl_at: std::collections::HashMap::new(),
        node: None,
        decls: &decls,
        imports: &imports,
        natives: &natives,
        interner,
        refs: Vec::new(),
        errors,
        scopes: ScopeStack::new(),
        defer_unresolved_types: false,
        in_view: false,
        owner_slots: Vec::new(),
    };
    pass.decl_at = decls.iter().map(|d| (d.name_range, d.id)).collect();
    pass.resolve_unit(cu);
    let ModulePass { refs, errors, .. } = pass;
    drop(members);
    ResolvedModule {
        table,
        refs,
        decls,
        errors,
    }
}

/// Every component's and system's member table in a package, by the owner's symbol.
type MemberTables<'t> = std::collections::HashMap<SymbolId, &'t SymbolTable>;

/// The compilation unit for a module path text, if a unit with that path parsed.
///
/// The graph and the units share one interner, so each unit's module path renders to
/// the same `::`-joined text the graph keys modules by — a direct string match.
fn unit_for(
    units: &[SourceUnit],
    module_text: &str,
    interner: &NameInterner,
) -> Option<CompilationUnit> {
    units
        .iter()
        .find(|u| u.path.display(interner) == module_text)
        .and_then(compilation_unit_of)
}

/// The typed compilation unit of a source unit, if its root casts.
fn compilation_unit_of(unit: &SourceUnit) -> Option<CompilationUnit> {
    CompilationUnit::cast(SyntaxNode::new_root(unit.parse.root.clone()))
}

/// Builds a module's public symbol table from its top-level declarations.
///
/// Alongside the table it returns one [`SymbolDecl`] per minted symbol — the
/// name-token span of each declaration, in declaration order — captured at the
/// mint site so tooling (goto/rename) can locate a definition from its
/// [`SymbolId`] without a second walk.
fn build_symbol_table(
    cu: Option<&CompilationUnit>,
    package: &str,
    module_text: &str,
    interner: &mut NameInterner,
) -> (SymbolTable, Vec<SymbolDecl>, Vec<Diagnostic>) {
    let mut out = SymbolTableBuild::default();
    let Some(cu) = cu else {
        return out.into_parts();
    };
    for item in cu.items() {
        let (decl, exported) = match item {
            Item::Export(e) => match e.declaration() {
                Some(inner) => (inner, true),
                None => continue,
            },
            other => (other, false),
        };
        let Some((name_tok, kind, ns)) = decl_identity(&decl) else {
            continue;
        };
        let name = interner.intern(&name_tok.text());
        let text = interner.text(name).unwrap_or_default().to_owned();
        let id = fingerprint(SymbolIdentity {
            package,
            module_path: module_text,
            kind,
            decl_path: &text,
        });
        let symbol = ModuleSymbol { id, exported };
        out.define(None, name, ns, symbol, &name_tok);
        if !exported && let Some(last) = out.decls.last_mut() {
            last.export_at = Some(keyword_start(decl.syntax()));
        }
        // A component's/system's members (state, computed, input, event, and the
        // callables) go in the owner's own member table, fingerprinted under the
        // owner's name: two components may each declare a `count`.
        if let Item::Component(_) | Item::System(_) | Item::Template(_) = decl {
            define_members(&decl, id, &text, package, module_text, interner, &mut out);
        }
        if let Item::Trait(t) = &decl {
            define_trait_members(t, id, &text, package, module_text, interner, &mut out);
        }
    }
    let mint = |kind: SymbolKind, path: &str| {
        fingerprint(SymbolIdentity {
            package,
            module_path: module_text,
            kind,
            decl_path: path,
        })
    };
    for item in cu.items() {
        let decl = match item {
            Item::Export(e) => match e.declaration() {
                Some(inner) => inner,
                None => continue,
            },
            other => other,
        };
        mint_unnamed(&decl, &mint, &mut out);
    }
    out.into_parts()
}

/// The `::`-free text identifying an impl among its module's: `impl Target` or
/// `impl Trait for Target`, without whitespace.
pub(crate) fn impl_path(decl: &crate::ast::ImplDecl) -> String {
    let squash = |n: &SyntaxNode| -> String {
        n.text()
            .to_string()
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect()
    };
    let target = decl.target().map(|t| squash(&t)).unwrap_or_default();
    match decl.trait_path() {
        Some(t) => format!("impl {} for {target}", squash(t.syntax())),
        None => format!("impl {target}"),
    }
}

/// The name tokens of the generic parameters `node` declares, in order.
pub(crate) fn generic_param_names(node: &SyntaxNode) -> Vec<crate::syntax::SyntaxToken> {
    use crate::syntax::SyntaxKind;
    node.children()
        .into_iter()
        .filter(|c| c.kind() == SyntaxKind::GenericParams)
        .flat_map(|g| g.children())
        .filter(|g| g.kind() == SyntaxKind::GenericParam)
        .filter_map(|g| ident_tokens(&g).into_iter().next())
        .collect()
}

/// The first token of `kind` directly under `node`.
fn keyword(
    node: &SyntaxNode,
    kind: crate::syntax::SyntaxKind,
) -> Option<crate::syntax::SyntaxToken> {
    node.children_with_tokens()
        .into_iter()
        .filter_map(|e| e.as_token().cloned())
        .find(|t| t.kind() == kind)
}

/// Mints the symbols a declaration has that no table names: its generic
/// parameters, a trait's and an impl's `Self`, and an impl's members, each
/// declared at its name (`Self` at the `trait`/`impl` keyword).
fn mint_unnamed(
    decl: &Item,
    mint: &dyn Fn(SymbolKind, &str) -> SymbolId,
    out: &mut SymbolTableBuild,
) {
    use crate::syntax::SyntaxKind;
    let generics = |path: &str, node: &SyntaxNode, out: &mut SymbolTableBuild| {
        for name in generic_param_names(node) {
            let id = mint(SymbolKind::TypeParam, &format!("{path}::<{}>", name.text()));
            out.mint(id, &name);
        }
    };
    let callables =
        |node: &SyntaxNode| -> Vec<(crate::syntax::SyntaxToken, SyntaxNode, SymbolKind)> {
            node.children()
                .into_iter()
                .filter_map(|m| {
                    let kind = match m.kind() {
                        SyntaxKind::FnDecl => SymbolKind::Function,
                        SyntaxKind::ActionDecl => SymbolKind::Action,
                        SyntaxKind::TaskDecl => SymbolKind::Task,
                        SyntaxKind::ConstDecl => SymbolKind::Const,
                        SyntaxKind::AssocTypeDecl => SymbolKind::AssocType,
                        _ => return None,
                    };
                    Some((ident_tokens(&m).into_iter().next()?, m, kind))
                })
                .collect()
        };
    let syntax = decl.syntax();
    match decl {
        Item::Impl(i) => {
            let path = impl_path(i);
            generics(&path, syntax, out);
            if let Some(kw) = keyword(syntax, SyntaxKind::ImplKw) {
                out.mint(mint(SymbolKind::TypeParam, &format!("{path}::Self")), &kw);
            }
            let mut seen: std::collections::HashMap<String, TextRange> =
                std::collections::HashMap::new();
            for (name, member, kind) in callables(syntax) {
                let text = name.text().to_string();
                let member_path = format!("{path}::{text}");
                if let Some(first) = seen.insert(text.clone(), name.text_range()) {
                    let mut error = ResolveErrorKind::DuplicateName
                        .to_diagnostic(Some(name.text_range()), &text);
                    error
                        .related
                        .push(Related::new(first, format!("`{text}` is declared here")));
                    out.errors.push(error);
                    continue;
                }
                out.mint(mint(kind, &member_path), &name);
                generics(&member_path, &member, out);
            }
        }
        Item::Trait(t) => {
            let Some(name) = t.name() else {
                return;
            };
            let path = name.text().to_string();
            generics(&path, syntax, out);
            if let Some(kw) = keyword(syntax, SyntaxKind::TraitKw) {
                out.mint(mint(SymbolKind::TypeParam, &format!("{path}::Self")), &kw);
            }
            for (member_name, member, _) in callables(syntax) {
                generics(&format!("{path}::{}", member_name.text()), &member, out);
            }
        }
        Item::Component(_) | Item::System(_) | Item::Template(_) => {
            let Some(name) = ident_tokens(syntax).into_iter().next() else {
                return;
            };
            let path = name.text().to_string();
            generics(&path, syntax, out);
            for child in syntax.children() {
                if matches!(
                    child.kind(),
                    SyntaxKind::FnDecl | SyntaxKind::ActionDecl | SyntaxKind::TaskDecl
                ) && let Some(member) = ident_tokens(&child).into_iter().next()
                {
                    generics(&format!("{path}::{}", member.text()), &child, out);
                }
            }
        }
        Item::Record(_)
        | Item::Enum(_)
        | Item::TypeAlias(_)
        | Item::Fn(_)
        | Item::Action(_)
        | Item::Task(_) => {
            if let Some(name) = ident_tokens(syntax).into_iter().next() {
                generics(&name.text(), syntax, out);
            }
        }
        _ => {}
    }
}

/// Defines a trait's members into its member table, each fingerprinted under
/// the trait's name: its callables, associated consts and associated types.
fn define_trait_members(
    decl: &crate::ast::TraitDecl,
    owner_id: SymbolId,
    owner: &str,
    package: &str,
    module_text: &str,
    interner: &mut NameInterner,
    out: &mut SymbolTableBuild,
) {
    use crate::ast::AssocItem;
    out.table.members_mut(owner_id);
    for member in decl.members() {
        let Some(name_tok) = member.name() else {
            continue;
        };
        let (kind, ns) = match member {
            AssocItem::Fn(_) => (SymbolKind::Function, Namespace::Value),
            AssocItem::Action(_) => (SymbolKind::Action, Namespace::Value),
            AssocItem::Task(_) => (SymbolKind::Task, Namespace::Value),
            AssocItem::Const(_) => (SymbolKind::Const, Namespace::Value),
            AssocItem::Type(_) => (SymbolKind::AssocType, Namespace::Type),
        };
        let name = interner.intern(&name_tok.text());
        let decl_path = format!("{owner}::{}", name_tok.text());
        let id = fingerprint(SymbolIdentity {
            package,
            module_path: module_text,
            kind,
            decl_path: &decl_path,
        });
        let symbol = ModuleSymbol {
            id,
            exported: false,
        };
        out.define(Some(owner_id), name, ns, symbol, &name_tok);
    }
}

/// The in-progress output of [`build_symbol_table`]: the module symbol table, the
/// declaration name-token spans (which goto/rename tooling keys on), and the collision
/// diagnostics. Threaded as one sink through the member-defining pass.
#[derive(Default)]
struct SymbolTableBuild {
    table: SymbolTable,
    decls: Vec<SymbolDecl>,
    /// The raw source spelling of each entry in `decls`, index for index, so a
    /// collision can tell a true duplicate from an NFC-equal respelling.
    spellings: Vec<String>,
    errors: Vec<Diagnostic>,
}

impl SymbolTableBuild {
    /// Defines `symbol`, declared by `name_tok`, in the module table or, for a member,
    /// in its `owner`'s member table, reporting a collision within that table: `E1101`
    /// when the earlier declaration is spelled differently but normalizes alike,
    /// `E2002` when it is the same spelling.
    fn define(
        &mut self,
        owner: Option<SymbolId>,
        name: NameId,
        ns: Namespace,
        symbol: ModuleSymbol,
        name_tok: &crate::syntax::SyntaxToken,
    ) {
        let spelling = name_tok.text().to_string();
        let range = name_tok.text_range();
        self.decls.push(SymbolDecl {
            id: symbol.id,
            name_range: range,
            export_at: None,
        });
        self.spellings.push(spelling.clone());
        let table = match owner {
            Some(owner) => self.table.members_mut(owner),
            None => &mut self.table,
        };
        let Err(existing) = table.define(name, ns, symbol) else {
            return;
        };
        let earlier = self
            .decls
            .iter()
            .zip(&self.spellings)
            .find(|(decl, _)| decl.id == existing.id);
        let kind = match earlier {
            Some((_, first)) if *first != spelling => ResolveErrorKind::NormalizationConflict,
            _ => ResolveErrorKind::DuplicateName,
        };
        let mut error = kind.to_diagnostic(Some(range), &spelling);
        if let Some((decl, first)) = earlier {
            error.related.push(Related::new(
                decl.name_range,
                format!("`{first}` is declared here"),
            ));
        }
        self.errors.push(error);
    }

    /// Records `id`, declared at `name_tok`, without naming it in any table.
    fn mint(&mut self, id: SymbolId, name_tok: &crate::syntax::SyntaxToken) {
        self.decls.push(SymbolDecl {
            id,
            name_range: name_tok.text_range(),
            export_at: None,
        });
        self.spellings.push(name_tok.text().to_string());
    }

    fn into_parts(self) -> (SymbolTable, Vec<SymbolDecl>, Vec<Diagnostic>) {
        (self.table, self.decls, self.errors)
    }
}

/// Defines a component's or system's members into its member table (created even
/// when it declares none), each fingerprinted under the owner's name.
fn define_members(
    decl: &Item,
    owner_id: SymbolId,
    owner: &str,
    package: &str,
    module_text: &str,
    interner: &mut NameInterner,
    out: &mut SymbolTableBuild,
) {
    let members: Vec<crate::ast::Member> = match decl {
        Item::Component(c) => c.members().collect(),
        Item::System(s) => s.members().collect(),
        Item::Template(t) => t.as_component().members().collect(),
        _ => return,
    };
    out.table.members_mut(owner_id);
    // A template's parameters are its inputs.
    let params = match decl {
        Item::Template(t) => t.params(),
        _ => Vec::new(),
    };
    for name_tok in params.iter().filter_map(|p| p.name()) {
        let name = interner.intern(&name_tok.text());
        let member_text = interner.text(name).unwrap_or_default();
        let decl_path = format!("{owner}::{member_text}");
        let id = fingerprint(SymbolIdentity {
            package,
            module_path: module_text,
            kind: SymbolKind::Input,
            decl_path: &decl_path,
        });
        let symbol = ModuleSymbol {
            id,
            exported: false,
        };
        out.define(Some(owner_id), name, Namespace::Value, symbol, &name_tok);
    }
    for member in members {
        let Some((name_tok, kind, ns)) = member_identity(&member) else {
            continue;
        };
        let name = interner.intern(&name_tok.text());
        let member_text = interner.text(name).unwrap_or_default();
        let decl_path = format!("{owner}::{member_text}");
        let id = fingerprint(SymbolIdentity {
            package,
            module_path: module_text,
            kind,
            decl_path: &decl_path,
        });
        let symbol = ModuleSymbol {
            id,
            exported: false,
        };
        out.define(Some(owner_id), name, ns, symbol, &name_tok);
    }
}

/// The name token, symbol kind, and namespace of a component/system member, or `None`
/// for a member that mints no module symbol: the `view` block itself, and a `slot`,
/// which `fill` and `SlotOutlet` name within the component's schema.
fn member_identity(
    member: &crate::ast::Member,
) -> Option<(crate::syntax::SyntaxToken, SymbolKind, Namespace)> {
    use crate::ast::Member;
    let triple = match member {
        Member::Input(d) => (d.name()?, SymbolKind::Input, Namespace::Value),
        Member::State(d) => (d.name()?, SymbolKind::State, Namespace::Value),
        Member::Computed(d) => (d.name()?, SymbolKind::Computed, Namespace::Value),
        Member::Event(d) => (d.name()?, SymbolKind::Event, Namespace::Event),
        Member::Fn(d) => (d.name()?, SymbolKind::Function, Namespace::Value),
        Member::Action(d) => (d.name()?, SymbolKind::Action, Namespace::Value),
        Member::Task(d) => (d.name()?, SymbolKind::Task, Namespace::Value),
        Member::Resource(d) => (d.name()?, SymbolKind::Resource, Namespace::Value),
        Member::Const(d) => (d.name()?, SymbolKind::Const, Namespace::Value),
        // A native declaration binds a native path, not a module symbol.
        Member::Slot(_) | Member::View(_) | Member::Effect(_) | Member::Native(_) => return None,
    };
    Some(triple)
}

/// The name token, symbol kind, and namespace of a top-level declaration, or `None`
/// for a form that mints no module symbol (imports are not items; Advanced items
/// carry no typed identity yet).
fn decl_identity(item: &Item) -> Option<(crate::syntax::SyntaxToken, SymbolKind, Namespace)> {
    let triple = match item {
        Item::Component(d) => (d.name()?, SymbolKind::Component, Namespace::Type),
        Item::Template(d) => (d.name()?, SymbolKind::Template, Namespace::Type),
        Item::System(d) => (d.name()?, SymbolKind::System, Namespace::Type),
        Item::Record(d) => (d.name()?, SymbolKind::Record, Namespace::Type),
        Item::Enum(d) => (d.name()?, SymbolKind::Enum, Namespace::Type),
        Item::TypeAlias(d) => (d.name()?, SymbolKind::TypeAlias, Namespace::Type),
        Item::Const(d) => (d.name()?, SymbolKind::Const, Namespace::Value),
        Item::Fn(d) => (d.name()?, SymbolKind::Function, Namespace::Value),
        Item::Action(d) => (d.name()?, SymbolKind::Action, Namespace::Value),
        Item::Task(d) => (d.name()?, SymbolKind::Task, Namespace::Value),
        Item::Shader(d) => (d.name()?, SymbolKind::Shader, Namespace::Type),
        // A theme is a constant `Theme` value; a style a constant reference
        // a node's `styles` list names.
        Item::Theme(d) => (d.name()?, SymbolKind::Const, Namespace::Value),
        Item::Style(d) => (d.name()?, SymbolKind::Style, Namespace::Value),
        Item::Trait(d) => (d.name()?, SymbolKind::Trait, Namespace::Type),
        Item::Export(_) | Item::Native(_) | Item::Impl(_) => return None,
    };
    Some(triple)
}

/// Where `export ` goes on the top-level declaration `node`: its first token that is
/// neither trivia nor part of an attribute.
fn keyword_start(node: &SyntaxNode) -> TextSize {
    use crate::syntax::SyntaxKind;
    node.children_with_tokens()
        .into_iter()
        .find(|el| match el.as_token() {
            Some(t) => !t.kind().is_trivia(),
            None => el.kind() != SyntaxKind::Attribute,
        })
        .map_or(node.text_range().start(), |el| el.text_range().start())
}

/// Builds a module's import environment: local name to the exported symbol it names.
///
/// The value names the language provides without a declaration: the `Option`
/// and `Result` constructors and the `format`/`untracked` intrinsics.
const BUILTIN_VALUES: &[&str] = &["Some", "None", "Ok", "Err", "format", "untracked"];

/// An item naming a declaration its module does not export is `E2001`, pointing at
/// the declaration and offering to export it; it still binds, so its uses type as
/// the declaration. An item naming nothing is `E2001` with the nearest exported
/// names, and binds nothing.
fn build_import_env(
    cu: Option<&CompilationUnit>,
    natives: &[NativeBinding],
    graph: &ModuleGraph,
    tables: &[SymbolTable],
    decls: &[Vec<SymbolDecl>],
    interner: &mut NameInterner,
    errors: &mut Vec<Diagnostic>,
) -> ImportEnv {
    let mut env = ImportEnv::default();
    for binding in natives {
        let local = interner.intern(&binding.local);
        match &binding.owner {
            Some(owner) => {
                let owner = interner.intern(owner);
                env.member_natives
                    .insert((owner, local), binding.path.clone());
            }
            None => {
                env.natives.insert(local, binding.path.clone());
            }
        }
    }
    let Some(cu) = cu else {
        return env;
    };
    for import in cu.imports() {
        let Some(path_node) = import.path() else {
            continue;
        };
        let target_text = type_or_module_path_text(path_node.syntax());
        let Some(idx) = graph.index_of(&target_text, interner) else {
            continue; // the module graph already reported this as E2001
        };
        let target_table = &tables[idx.as_usize()];
        let target_decls = &decls[idx.as_usize()];
        let target_path = graph.modules()[idx.as_usize()].path.display(interner);
        // `import a::{ x, y as z };` — selective items into the local environment.
        for item in import.items() {
            let Some(name_tok) = item.name() else {
                continue;
            };
            let orig = name_tok.text();
            let local_text = item
                .rename()
                .and_then(|r| r.name())
                .map(|t| t.text())
                .unwrap_or_else(|| orig.clone());
            let local = interner.intern(&local_text);
            let at = name_tok.text_range();
            let subject = format!("{target_path}::{orig}");
            let Some((symbol, namespace)) = lookup_import(target_table, interner, &orig) else {
                let mut diagnostic =
                    ResolveErrorKind::UnresolvedImport.to_diagnostic(Some(at), &subject);
                let suggestions =
                    nearest_exports(&orig, target_table, target_decls, &target_path, interner);
                suggest::attach(&mut diagnostic, at, &suggestions);
                errors.push(diagnostic);
                env.unresolved.insert(local);
                continue;
            };
            if !symbol.exported {
                let mut diagnostic =
                    ResolveErrorKind::PrivateImport.to_diagnostic(Some(at), &subject);
                if let Some(decl) = target_decls.iter().find(|d| d.id == symbol.id) {
                    diagnostic.related.push(Related::in_module(
                        target_path.as_str(),
                        decl.name_range,
                        "declared here without `export`",
                    ));
                    if let Some(export_at) = decl.export_at {
                        diagnostic.fixes.push(Fix {
                            title: format!("export `{orig}` from `{target_path}`"),
                            applicability: Applicability::MaybeIncorrect,
                            edits: vec![TextEdit::in_module(
                                target_path.as_str(),
                                TextRange::empty(export_at),
                                "export ",
                            )],
                        });
                    }
                }
                errors.push(diagnostic);
            }
            env.bindings.insert(
                local,
                ImportBinding {
                    symbol: symbol.id,
                    namespace,
                },
            );
        }
    }
    env
}

/// Looks a name up in a module's table across every namespace, preferring an
/// exported declaration.
fn lookup_import(
    table: &SymbolTable,
    interner: &mut NameInterner,
    name_text: &str,
) -> Option<(ModuleSymbol, Namespace)> {
    let name = interner.intern(name_text);
    let found = [Namespace::Type, Namespace::Value, Namespace::Event]
        .into_iter()
        .filter_map(|ns| Some((table.get(name, ns)?, ns)));
    let mut first = None;
    for (symbol, ns) in found {
        if symbol.exported {
            return Some((symbol, ns));
        }
        first.get_or_insert((symbol, ns));
    }
    first
}

/// The exported names of the module `path` (whose table and declarations are
/// `table` and `decls`) nearest to `text`.
fn nearest_exports<'a>(
    text: &str,
    table: &'a SymbolTable,
    decls: &'a [SymbolDecl],
    path: &'a str,
    interner: &'a NameInterner,
) -> Vec<suggest::Candidate<'a>> {
    let exported = [Namespace::Type, Namespace::Value, Namespace::Event]
        .into_iter()
        .flat_map(|ns| table.names(ns))
        .filter(|(_, symbol)| symbol.exported)
        .filter_map(|(name, symbol)| {
            Some(suggest::Candidate {
                name: interner.text(name)?,
                declared_at: decls
                    .iter()
                    .find(|d| d.id == symbol.id)
                    .map(|d| (Some(path), d.name_range)),
            })
        });
    suggest::nearest(text, exported)
}

/// The `::`-joined identifier text of a path-like syntax node (module path, type
/// path, or path expression), skipping `::` separators and generic arguments.
fn type_or_module_path_text(node: &SyntaxNode) -> String {
    use crate::syntax::SyntaxKind;
    let mut out = String::new();
    let mut first = true;
    for el in node.children_with_tokens() {
        if let Some(t) = el.as_token() {
            if matches!(t.kind(), SyntaxKind::Ident | SyntaxKind::RawIdent) {
                if !first {
                    out.push_str("::");
                }
                out.push_str(&t.text());
                first = false;
            }
        } else if let Some(child) = el.as_node() {
            // Type paths wrap each segment; recurse one level for the segment name.
            if child.kind() == SyntaxKind::TypePathSegment {
                for st in child.children_with_tokens() {
                    if let Some(t) = st.as_token()
                        && matches!(t.kind(), SyntaxKind::Ident | SyntaxKind::RawIdent)
                    {
                        if !first {
                            out.push_str("::");
                        }
                        out.push_str(&t.text());
                        first = false;
                        break;
                    }
                }
            }
        }
    }
    out
}

/// The identifier tokens directly under `node`, in order.
pub(crate) fn ident_tokens(node: &SyntaxNode) -> Vec<crate::syntax::SyntaxToken> {
    use crate::syntax::SyntaxKind;
    node.children_with_tokens()
        .into_iter()
        .filter_map(|e| e.as_token().cloned())
        .filter(|t| matches!(t.kind(), SyntaxKind::Ident | SyntaxKind::RawIdent))
        .collect()
}

/// The name of the adaptive environment a view reads.
const ENV: &str = "env";

/// The name of the theme a view reads.
const THEME: &str = "theme";

/// The code of a read of `env` or `theme` outside the View execution domain.
const ENV_OUTSIDE_VIEW: &str = "E2111";

/// The per-module resolution walk state.
struct ModulePass<'a> {
    table: &'a SymbolTable,
    /// The standard prelude's table, consulted after the module's own declarations and
    /// its imports.
    prelude: &'a SymbolTable,
    members: &'a MemberTables<'a>,
    /// The component or system whose body is being resolved.
    owner: Option<SymbolId>,
    /// Its name.
    owner_name: Option<NameId>,
    /// The generic parameters (and `Self`) in scope, innermost last.
    type_scopes: Vec<Vec<(NameId, SymbolId, bool)>>,
    /// The symbol declared at each name span, for the unnamed ones.
    decl_at: std::collections::HashMap<TextRange, SymbolId>,
    /// The component the innermost enclosing view node instantiates, whose events its
    /// handlers name.
    node: Option<SymbolId>,
    /// The declaration sites of `table`'s symbols, for nearest-name suggestions.
    decls: &'a [SymbolDecl],
    imports: &'a ImportEnv,
    /// The native registry native import paths resolve against.
    natives: &'a Natives,
    interner: &'a mut NameInterner,
    refs: Vec<ResolvedRef>,
    errors: Vec<Diagnostic>,
    scopes: ScopeStack,
    /// When set, an unresolved node/type name is treated as native/schema-provided
    /// and left undiagnosed instead of raising [`E2001`]. A bare `ui!` fragment has
    /// no compilation unit and no import mechanism, so its node types (`Column`, …)
    /// come from the native/widget schema exactly as value refs already do (see the
    /// deferred value-path head in [`ModulePass::resolve_value_path`]); the component
    /// frontend keeps `false` so a genuinely missing user type still surfaces.
    defer_unresolved_types: bool,
    /// Whether the walk is inside a view, the execution domain `env` is bound in.
    in_view: bool,
    /// The slots of the enclosing component or template, which a view's
    /// `SlotOutlet` names by value though no symbol declares them.
    owner_slots: Vec<String>,
}

impl<'a> ModulePass<'a> {
    /// Resolves a whole compilation unit's declarations.
    fn resolve_unit(&mut self, cu: &CompilationUnit) {
        for item in cu.items() {
            let decl = match item {
                Item::Export(e) => match e.declaration() {
                    Some(inner) => inner,
                    None => continue,
                },
                other => other,
            };
            let scoped = !matches!(decl, Item::Trait(_) | Item::Impl(_))
                && self.push_generics(decl.syntax(), None);
            match decl {
                Item::Component(c) => self.resolve_component(&c),
                Item::Template(t) => self.resolve_template(&t),
                Item::System(s) => self.resolve_system(&s),
                Item::Fn(f) => self.resolve_callable(f.params(), f.return_type(), f.body()),
                Item::Action(a) => self.resolve_callable(a.params(), a.return_type(), a.body()),
                Item::Task(t) => self.resolve_callable(t.params(), t.return_type(), t.body()),
                Item::Trait(t) => self.resolve_trait(&t),
                Item::Impl(i) => self.resolve_impl(&i),
                Item::Record(r)
                    if crate::ast::decl_attributes(r.syntax())
                        .iter()
                        .any(|(name, ..)| name == "shader_value") =>
                {
                    self.resolve_shader_types(r.syntax());
                }
                Item::Shader(s) => self.resolve_shader_types(s.syntax()),
                Item::Record(_) | Item::Enum(_) | Item::Const(_) | Item::TypeAlias(_) => {
                    self.resolve_body(decl.syntax());
                }
                Item::Theme(t) => {
                    if let Some(base) = t.base() {
                        self.resolve_named_value(&base);
                    }
                    for item in t.items() {
                        if let Some(value) = item.value() {
                            self.resolve_expr(&value);
                        }
                    }
                }
                Item::Style(s) => self.resolve_style(&s),
                Item::Native(n) => self.resolve_callable(n.params(), n.return_type(), None),
                _ => {}
            }
            if scoped {
                self.type_scopes.pop();
            }
        }
    }

    /// Opens a scope of the generic parameters `node` declares (and `Self`,
    /// declared at its `self_kw` keyword), resolving their bounds and defaults
    /// and its `where` clause in it. Returns whether a scope was opened.
    fn push_generics(
        &mut self,
        node: &SyntaxNode,
        self_kw: Option<crate::syntax::SyntaxKind>,
    ) -> bool {
        use crate::syntax::SyntaxKind;
        let mut scope = Vec::new();
        if let Some(kw) = self_kw.and_then(|k| keyword(node, k))
            && let Some(&symbol) = self.decl_at.get(&kw.text_range())
        {
            scope.push((self.interner.intern("Self"), symbol, false));
        }
        for name in generic_param_names(node) {
            if let Some(&symbol) = self.decl_at.get(&name.text_range()) {
                let is_const = keyword(&name.parent(), SyntaxKind::ConstKw).is_some();
                scope.push((self.interner.intern(&name.text()), symbol, is_const));
                self.refs.push(ResolvedRef {
                    range: name.text_range(),
                    to: Resolution::Symbol(symbol),
                });
            }
        }
        let clauses: Vec<SyntaxNode> = node
            .children()
            .into_iter()
            .filter(|c| {
                matches!(
                    c.kind(),
                    SyntaxKind::GenericParams | SyntaxKind::WhereClause
                )
            })
            .collect();
        if scope.is_empty() && clauses.is_empty() {
            return false;
        }
        self.type_scopes.push(scope);
        for clause in clauses {
            self.resolve_bounds_in(&clause);
        }
        true
    }

    /// Resolves the types and trait bounds under a generic parameter list or a
    /// `where` clause: a path after a `:` is a bound, any other a type.
    fn resolve_bounds_in(&mut self, clause: &SyntaxNode) {
        use crate::syntax::SyntaxKind;
        let mut items: Vec<SyntaxNode> = vec![clause.clone()];
        if clause.kind() == SyntaxKind::GenericParams {
            items = clause.children();
        }
        for item in items {
            let mut after_colon = false;
            for element in item.children_with_tokens() {
                match element.kind() {
                    SyntaxKind::Colon => after_colon = true,
                    SyntaxKind::Eq | SyntaxKind::Comma => after_colon = false,
                    SyntaxKind::TypePath => {
                        if let Some(path) = element.as_node().cloned().and_then(TypePath::cast) {
                            if after_colon {
                                self.resolve_bound(&path);
                            } else {
                                self.resolve_type_path(&path);
                            }
                        }
                    }
                    _ => {
                        if let Some(node) = element.as_node() {
                            self.resolve_body(node);
                        }
                    }
                }
            }
        }
    }

    /// Resolves a trait: its supertraits and members, with `Self` and its
    /// generic parameters in scope.
    fn resolve_trait(&mut self, decl: &crate::ast::TraitDecl) {
        let scoped = self.push_generics(decl.syntax(), Some(crate::syntax::SyntaxKind::TraitKw));
        for bound in decl.supertraits() {
            self.resolve_bound(&bound);
        }
        for member in decl.members() {
            self.resolve_assoc(&member);
        }
        if scoped {
            self.type_scopes.pop();
        }
    }

    /// Resolves an impl: its trait, its target and its members, with `Self` and
    /// its generic parameters in scope.
    fn resolve_impl(&mut self, decl: &crate::ast::ImplDecl) {
        let scoped = self.push_generics(decl.syntax(), Some(crate::syntax::SyntaxKind::ImplKw));
        if let Some(path) = decl.trait_path() {
            self.resolve_bound(&path);
        }
        if let Some(target) = decl.target() {
            self.resolve_body(&target);
        }
        for member in decl.members() {
            self.resolve_assoc(&member);
        }
        if scoped {
            self.type_scopes.pop();
        }
    }

    /// Resolves one member of a trait or an impl.
    fn resolve_assoc(&mut self, member: &crate::ast::AssocItem) {
        use crate::ast::AssocItem;
        let scoped = self.push_generics(member.syntax(), None);
        match member {
            AssocItem::Fn(f) => self.resolve_callable(f.params(), f.return_type(), f.body()),
            AssocItem::Action(a) => self.resolve_callable(a.params(), a.return_type(), a.body()),
            AssocItem::Task(t) => self.resolve_callable(t.params(), t.return_type(), t.body()),
            AssocItem::Type(t) => {
                for bound in t.bounds() {
                    self.resolve_bound(&bound);
                }
                if let Some(value) = t.value() {
                    self.resolve_body(&value);
                }
            }
            AssocItem::Const(c) => self.resolve_body(c.syntax()),
        }
        if scoped {
            self.type_scopes.pop();
        }
    }

    /// Binds the type names under a shader or a `@shader_value` record, left
    /// undiagnosed: the built-in shader types have no declaration, and the
    /// shader checker reports a name that is neither one nor a record. Values
    /// in a shader body are scoped by the shader checker.
    fn resolve_shader_types(&mut self, node: &SyntaxNode) {
        use crate::syntax::SyntaxKind;
        for node in node.descendants() {
            match node.kind() {
                SyntaxKind::TypePath
                    if node.parent().map(|p| p.kind()) != Some(SyntaxKind::GenericArgs) =>
                {
                    if let Some(ty) = TypePath::cast(node) {
                        self.resolve_type_head(&ty, true);
                    }
                }
                SyntaxKind::RecordExpr => self.resolve_record_head(&node),
                _ => {}
            }
        }
    }

    fn resolve_component(&mut self, decl: &ComponentDecl) {
        self.resolve_owner(decl.name(), decl.members());
    }

    /// A template: its parameters' types and defaults, then its members as
    /// a component's, the parameters being its inputs.
    fn resolve_template(&mut self, decl: &crate::ast::TemplateDecl) {
        for param in decl.params() {
            if let Some(ty) = param.ty() {
                self.resolve_type_path(&ty);
            }
            if let Some(default) = param.default() {
                self.resolve_expr(&default);
            }
        }
        self.resolve_owner(decl.name(), decl.as_component().members());
    }

    fn resolve_system(&mut self, decl: &SystemDecl) {
        for bound in decl.implements() {
            self.resolve_bound(&bound);
        }
        for (_, _, names) in decl.ordering() {
            for name in names.into_iter().flatten() {
                self.resolve_ordered(&name);
            }
        }
        self.resolve_owner(decl.name(), decl.members());
    }

    /// Resolves a system an `@after`/`@before` names, in the type namespace
    /// (`E2001` with the nearest types otherwise).
    fn resolve_ordered(&mut self, name: &crate::syntax::SyntaxToken) {
        let text = name.text();
        let at = name.text_range();
        let interned = self.interner.intern(&text);
        if let Some(symbol) = self.type_symbol(interned) {
            self.refs.push(ResolvedRef {
                range: at,
                to: Resolution::Symbol(symbol),
            });
            return;
        }
        let mut diagnostic = ResolveErrorKind::UnresolvedType.to_diagnostic(Some(at), &text);
        let suggestions = self.nearest_types(&text);
        suggest::attach(&mut diagnostic, at, &suggestions);
        self.errors.push(diagnostic);
    }

    /// Resolves one `implements` bound: an imported native trait is recorded as
    /// [`Resolution::Native`], a path into a native import naming neither a
    /// trait nor a type is [`E2001`](ResolveErrorKind::UnresolvedNative) with
    /// the nearest traits, and anything else resolves as a type for lowering
    /// to reject.
    fn resolve_bound(&mut self, bound: &TypePath) {
        let Some(head) = bound.segments().next() else {
            return;
        };
        let name = self.interner.intern(&head.text());
        if self.type_symbol(name).is_some() {
            self.resolve_type_head(bound, false);
            return;
        }
        let Some(base) = self.native_base(name) else {
            self.resolve_type_head(bound, false);
            return;
        };
        let mut path = base.clone();
        let mut last = head.clone();
        for segment in bound.segments().skip(1) {
            path.push_str("::");
            path.push_str(&segment.text());
            last = segment;
        }
        if let Some(t) = self.natives.native_trait(&path) {
            self.refs.push(ResolvedRef {
                range: head.text_range(),
                to: Resolution::Native(t.id),
            });
            return;
        }
        if self.natives.ty(&path).is_some() {
            // A native type, which lowering rejects as a bound.
            self.resolve_type_head(bound, false);
            return;
        }
        let at = last.text_range();
        let mut diagnostic = ResolveErrorKind::UnresolvedNative.to_diagnostic(Some(at), &path);
        let parent = path.rsplit_once("::").map_or("", |(p, _)| p);
        let names: Vec<&str> = self
            .natives
            .traits()
            .iter()
            .filter_map(|t| t.path.strip_prefix(parent)?.strip_prefix("::"))
            .filter(|n| !n.contains("::"))
            .collect();
        let suggestions = suggest::nearest(
            &last.text(),
            names.into_iter().map(|name| suggest::Candidate {
                name,
                declared_at: None,
            }),
        );
        suggest::attach(&mut diagnostic, at, &suggestions);
        self.errors.push(diagnostic);
    }

    /// Resolves the members of the component or system named `name`, with its member
    /// table in scope.
    fn resolve_owner(
        &mut self,
        name: Option<crate::syntax::SyntaxToken>,
        members: impl Iterator<Item = Member>,
    ) {
        let owner_name = name.as_ref().map(|name| self.interner.intern(&name.text()));
        let owner = name.and_then(|name| {
            let name = self.interner.intern(&name.text());
            self.table.get(name, Namespace::Type).map(|s| s.id)
        });
        let members: Vec<Member> = members.collect();
        let slots = members
            .iter()
            .filter_map(|m| match m {
                Member::Slot(slot) => slot.name().map(|n| n.text().to_string()),
                _ => None,
            })
            .collect();
        let outer = std::mem::replace(&mut self.owner, owner);
        let outer_name = std::mem::replace(&mut self.owner_name, owner_name);
        let outer_slots = std::mem::replace(&mut self.owner_slots, slots);
        self.scopes.push();
        for member in members {
            self.resolve_member(member);
        }
        self.scopes.pop();
        self.owner = outer;
        self.owner_name = outer_name;
        self.owner_slots = outer_slots;
    }

    /// The native path `name` binds to: a native the enclosing component or
    /// system declares, then one the module imports or declares.
    fn native_base(&self, name: NameId) -> Option<&'a String> {
        let imports: &'a ImportEnv = self.imports;
        self.owner_name
            .and_then(|owner| imports.member_natives.get(&(owner, name)))
            .or_else(|| imports.natives.get(&name))
    }

    /// Looks `name` up among the members of `owner`.
    fn member(&self, owner: Option<SymbolId>, name: NameId, ns: Namespace) -> Option<SymbolId> {
        let table = self.members.get(&owner?)?;
        table.get(name, ns).map(|s| s.id)
    }

    /// The component a view node of type `ty` instantiates, if it names one.
    fn instantiated(&mut self, ty: Option<TypePath>) -> Option<SymbolId> {
        let ty = ty?;
        let segments: Vec<_> = ty.segments().collect();
        let [head] = segments.as_slice() else {
            return None;
        };
        let name = self.interner.intern(&head.text());
        let symbol = self.type_symbol(name)?;
        self.members.contains_key(&symbol).then_some(symbol)
    }

    /// Resolves an `emit`'s event against the enclosing component's events, then its
    /// arguments.
    fn resolve_emit(&mut self, stmt: &SyntaxNode) {
        if let Some(event) = ident_tokens(stmt).into_iter().next() {
            let name = self.interner.intern(&event.text());
            if let Some(symbol) = self.member(self.owner, name, Namespace::Event) {
                self.refs.push(ResolvedRef {
                    range: event.text_range(),
                    to: Resolution::Symbol(symbol),
                });
            }
        }
        self.resolve_children(stmt);
    }

    fn resolve_member(&mut self, member: Member) {
        match member {
            Member::View(v) => {
                if let Some(block) = v.block() {
                    self.in_view = true;
                    self.resolve_view_block(&block);
                    self.in_view = false;
                }
            }
            Member::Computed(c) => {
                if let Some(ty) = c.ty() {
                    self.resolve_type_path(&ty);
                }
                if let Some(body) = c.body() {
                    self.resolve_expr(&body);
                }
            }
            Member::State(s) => {
                if let Some(ty) = s.ty() {
                    self.resolve_type_path(&ty);
                }
                if let Some(init) = s.initializer() {
                    self.resolve_expr(&init);
                }
            }
            Member::Input(i) => {
                if let Some(ty) = i.ty() {
                    self.resolve_type_path(&ty);
                }
                if let Some(def) = i.default() {
                    self.resolve_expr(&def);
                }
            }
            Member::Fn(f) => {
                let scoped = self.push_generics(f.syntax(), None);
                self.resolve_callable(f.params(), f.return_type(), f.body());
                if scoped {
                    self.type_scopes.pop();
                }
            }
            Member::Native(n) => self.resolve_callable(n.params(), n.return_type(), None),
            Member::Action(a) => {
                let scoped = self.push_generics(a.syntax(), None);
                self.resolve_callable(a.params(), a.return_type(), a.body());
                if scoped {
                    self.type_scopes.pop();
                }
            }
            Member::Task(t) => {
                let scoped = self.push_generics(t.syntax(), None);
                self.resolve_callable(t.params(), t.return_type(), t.body());
                if scoped {
                    self.type_scopes.pop();
                }
            }
            Member::Effect(e) => {
                if let Some(deps) = e.deps() {
                    for dep in deps.exprs() {
                        self.resolve_expr(&dep);
                    }
                }
                // The run policy is a path into `EffectRun`, checked by name.
                if let Some(body) = e.body() {
                    self.scopes.push();
                    self.resolve_body(body.syntax());
                    self.scopes.pop();
                }
            }
            Member::Resource(r) => {
                if let Some(ty) = r.ty() {
                    self.resolve_type_path(&ty);
                }
                // A policy and the scope are paths into `ResourcePolicy` and
                // `ResourceScope`, checked by name.
                for item in r.items() {
                    use crate::syntax::SyntaxKind;
                    if matches!(
                        item.kind(),
                        SyntaxKind::ResourceLoad | SyntaxKind::ResourceKey
                    ) && let Some(value) = item.value()
                    {
                        self.resolve_expr(&value);
                    }
                }
            }
            Member::Const(c) => {
                if let Some(ty) = c.ty() {
                    self.resolve_type_path(&ty);
                }
                if let Some(value) = c.value() {
                    self.resolve_expr(&value);
                }
            }
            // An event's parameter types name types like any other.
            Member::Event(e) => {
                use crate::syntax::SyntaxKind;
                for param in e.syntax().children() {
                    if param.kind() != SyntaxKind::EventParam {
                        continue;
                    }
                    for ty in param.children() {
                        match ty.kind() {
                            SyntaxKind::TypePath => {
                                if let Some(path) = TypePath::cast(ty) {
                                    self.resolve_type_path(&path);
                                }
                            }
                            k if k.is_type() => self.resolve_body(&ty),
                            _ => {}
                        }
                    }
                }
            }
            Member::Slot(_) => {}
        }
    }

    fn resolve_callable(
        &mut self,
        params: Vec<crate::ast::Param>,
        returns: Option<crate::ast::ReturnType>,
        body: Option<Block>,
    ) {
        if let Some(ty) = returns {
            self.resolve_body(ty.syntax());
        }
        self.scopes.push();
        for p in params {
            if let Some(tok) = p.self_token() {
                let name = self.interner.intern("self");
                let slot = self.scopes.bind(name);
                self.refs.push(ResolvedRef {
                    range: tok.text_range(),
                    to: Resolution::Local(slot),
                });
            }
            if let Some(tok) = p.name() {
                let name = self.interner.intern(&tok.text());
                let slot = self.scopes.bind(name);
                self.refs.push(ResolvedRef {
                    range: tok.text_range(),
                    to: Resolution::Local(slot),
                });
            }
            if let Some(ty) = p.ty() {
                self.resolve_type_path(&ty);
            }
        }
        if let Some(body) = body {
            self.resolve_block(&body);
        }
        self.scopes.pop();
    }

    fn resolve_block(&mut self, block: &Block) {
        self.resolve_body(block.syntax());
    }

    /// Resolves the value paths under `node`, opening a lexical scope at each
    /// block, closure, `for` and match arm, and binding `let`, closure-parameter,
    /// `for` and arm patterns in it so later uses resolve to a [`LocalSlot`]. A
    /// `let` binds after its initializer, so the initializer still sees the outer
    /// name it shadows.
    fn resolve_body(&mut self, node: &SyntaxNode) {
        use crate::syntax::SyntaxKind;
        match node.kind() {
            SyntaxKind::PathExpr => {
                if let Some(path) = PathExpr::cast(node.clone()) {
                    self.resolve_value_path(&path);
                }
            }
            SyntaxKind::TypePath => {
                if let Some(ty) = TypePath::cast(node.clone()) {
                    self.resolve_type_path(&ty);
                }
            }
            SyntaxKind::DynType => {
                for bound in node.children().into_iter().filter_map(TypePath::cast) {
                    self.resolve_bound(&bound);
                }
            }
            SyntaxKind::Pattern => self.resolve_pattern_types(node),
            SyntaxKind::EmitStmt => self.resolve_emit(node),
            SyntaxKind::RecordExpr => {
                self.resolve_record_head(node);
                self.resolve_children(node);
            }
            SyntaxKind::RecordExprField => {
                // A shorthand field `{ x }` reads the value named `x`.
                match node.first_child() {
                    Some(value) => self.resolve_body(&value),
                    None => {
                        if let Some(name) = ident_tokens(node).into_iter().next() {
                            self.resolve_value_token(&name);
                        }
                    }
                }
            }
            SyntaxKind::Block | SyntaxKind::MatchArm | SyntaxKind::ClosureExpr => {
                self.scopes.push();
                self.resolve_children(node);
                self.scopes.pop();
            }
            // Resolved with the scope of the parameters they declare.
            SyntaxKind::GenericParams | SyntaxKind::WhereClause => {}
            // A `start` handler's payload pattern binds for its block.
            SyntaxKind::StartSuccess | SyntaxKind::StartError => {
                self.scopes.push();
                self.bind_patterns(node);
                for child in node.children() {
                    if child.kind() == SyntaxKind::Block {
                        self.resolve_body(&child);
                    }
                }
                self.scopes.pop();
            }
            // A task policy names a standard policy, not a value in scope.
            SyntaxKind::StartPolicy => {}
            SyntaxKind::ClosureParam => {
                for child in node.children() {
                    if child.kind() != SyntaxKind::Pattern {
                        self.resolve_body(&child);
                    }
                }
                self.bind_patterns(node);
            }
            SyntaxKind::LetStmt => {
                for child in node.children() {
                    if child.kind() != SyntaxKind::Pattern {
                        self.resolve_body(&child);
                    }
                }
                self.bind_patterns(node);
            }
            SyntaxKind::ForStmt => {
                for child in node.children() {
                    if !matches!(child.kind(), SyntaxKind::Pattern | SyntaxKind::Block) {
                        self.resolve_body(&child);
                    }
                }
                self.scopes.push();
                self.bind_patterns(node);
                for child in node.children() {
                    if child.kind() == SyntaxKind::Block {
                        self.resolve_body(&child);
                    }
                }
                self.scopes.pop();
            }
            _ => self.resolve_children(node),
        }
    }

    fn resolve_children(&mut self, node: &SyntaxNode) {
        use crate::syntax::SyntaxKind;
        for child in node.children() {
            self.resolve_body(&child);
            // An arm's pattern binds for its guard and body.
            if node.kind() == SyntaxKind::MatchArm && child.kind() == SyntaxKind::Pattern {
                self.bind_pattern(&child);
            }
        }
    }

    /// Resolves the type heads of every direct `Pattern` child of `node` and binds
    /// its names in the innermost scope.
    fn bind_patterns(&mut self, node: &SyntaxNode) {
        use crate::syntax::SyntaxKind;
        for child in node.children() {
            if child.kind() == SyntaxKind::Pattern {
                self.resolve_pattern_types(&child);
                self.bind_pattern(&child);
            }
        }
    }

    fn bind_pattern(&mut self, pattern: &SyntaxNode) {
        let Some(pattern) = crate::ast::Pattern::cast(pattern.clone()) else {
            return;
        };
        for tok in pattern.bindings() {
            let name = self.interner.intern(&tok.text());
            let slot = self.scopes.bind(name);
            self.refs.push(ResolvedRef {
                range: tok.text_range(),
                to: Resolution::Local(slot),
            });
        }
    }

    /// Resolves the type heads a pattern names (`S::busy(n)`, `P { x, .. }`,
    /// `S::idle`), without diagnosing an unknown head: `Some`/`Ok`/`Err` and
    /// native types have no declaration.
    fn resolve_pattern_types(&mut self, pattern: &SyntaxNode) {
        use crate::syntax::SyntaxKind;
        for node in pattern.descendants() {
            match node.kind() {
                SyntaxKind::TypePath
                    if node.parent().map(|p| p.kind()) != Some(SyntaxKind::GenericArgs) =>
                {
                    if let Some(ty) = TypePath::cast(node) {
                        self.resolve_type_head(&ty, true);
                    }
                }
                SyntaxKind::QualifiedVariantPattern => {
                    // `S::idle` / `a::S::done`: the enum is the next-to-last segment.
                    let segments = ident_tokens(&node);
                    if let Some(head) = segments.len().checked_sub(2).map(|i| &segments[i]) {
                        self.resolve_type_token(head);
                    }
                }
                _ => {}
            }
        }
    }

    /// Resolves the type a record literal names by its head token (`P { .. }`,
    /// `a::P { .. }`), or the enum of a record-variant literal (`S::done { .. }`).
    fn resolve_record_head(&mut self, record: &SyntaxNode) {
        let heads = ident_tokens(record);
        let Some(last) = heads.last() else {
            return;
        };
        if !self.resolve_type_token(last)
            && let Some(owner) = heads.len().checked_sub(2).map(|i| &heads[i])
        {
            self.resolve_type_token(owner);
        }
    }

    /// Resolves `head` in the type namespace, reporting whether it named a type.
    fn resolve_type_token(&mut self, head: &crate::syntax::SyntaxToken) -> bool {
        let name = self.interner.intern(&head.text());
        let symbol = self.type_symbol(name);
        if let Some(symbol) = symbol {
            self.refs.push(ResolvedRef {
                range: head.text_range(),
                to: Resolution::Symbol(symbol),
            });
        }
        symbol.is_some()
    }

    fn resolve_view_block(&mut self, block: &ViewBlock) {
        self.scopes.push();
        for item in block.items() {
            self.resolve_view_item(item);
        }
        self.scopes.pop();
    }

    fn resolve_view_item(&mut self, item: ViewItem) {
        match item {
            ViewItem::Named(n) => self.resolve_named_node(&n),
            ViewItem::Anonymous(a) => {
                if let Some(ty) = a.ty() {
                    self.resolve_node_type(&ty);
                }
                if let Some(body) = a.body() {
                    let node = self.instantiated(a.ty());
                    self.resolve_node_body(&body, node);
                }
            }
            ViewItem::Property(p) => self.resolve_property(&p),
            ViewItem::Handler(h) => self.resolve_handler(&h),
            ViewItem::For(f) => self.resolve_for(&f),
            ViewItem::If(i) => self.resolve_if(&i),
            ViewItem::Match(m) => {
                // The scrutinee is read once, in the enclosing scope.
                if let Some(scrutinee) = m.scrutinee() {
                    self.resolve_expr(&scrutinee);
                }
                // Each arm's pattern binds for its guard and body.
                for arm in m.arms() {
                    self.scopes.push();
                    if let Some(pattern) = arm.pattern() {
                        self.resolve_pattern_types(pattern.syntax());
                        self.bind_pattern(pattern.syntax());
                    }
                    if let Some(guard) = arm.guard() {
                        self.resolve_expr(&guard);
                    }
                    if let Some(body) = arm.body() {
                        self.resolve_view_block(&body);
                    }
                    self.scopes.pop();
                }
            }
            ViewItem::TwoWayBinding(b) => {
                // The source head is a value of the enclosing scope, left unresolved
                // when it is a host name; each `[index]` is an expression.
                if let Some(source) = b.source() {
                    if let Some(head) = source.segments().next() {
                        self.resolve_value_token(&head);
                    }
                    for index in source.syntax().children() {
                        if let Some(index) = crate::ast::Expr::cast(index) {
                            self.resolve_expr(&index);
                        }
                    }
                }
                // A converter no declaration names is a native one.
                if let Some(ty) = b.using_ty() {
                    self.resolve_type_head(&ty, true);
                }
            }
            ViewItem::Fill(fill) => {
                // Fill content belongs to the view that writes it, not to the node
                // whose slot it fills.
                if let Some(body) = fill.body() {
                    let node = self.node.take();
                    self.resolve_view_block(&body);
                    self.node = node;
                }
            }
            ViewItem::Part(part) => {
                // Like a named node's, a part's name binds a slot for later
                // siblings; callers name it in `override`/`replace part`.
                if let Some(tok) = part.name() {
                    let name = self.interner.intern(&tok.text());
                    let slot = self.scopes.bind(name);
                    self.refs.push(ResolvedRef {
                        range: tok.text_range(),
                        to: Resolution::Local(slot),
                    });
                }
                if let Some(ty) = part.ty() {
                    self.resolve_node_type(&ty);
                }
                if let Some(body) = part.body() {
                    let instantiated = self.instantiated(part.ty());
                    self.resolve_node_body(&body, instantiated);
                }
            }
            ViewItem::Use(u) => {
                if let Some(ty) = u.ty() {
                    self.resolve_node_type(&ty);
                }
                for (_, value) in u.args() {
                    self.resolve_expr(&value);
                }
                if let Some(body) = u.body() {
                    let instantiated = self.instantiated(u.ty());
                    self.resolve_node_body(&body, instantiated);
                }
            }
            ViewItem::Override(o) => {
                // The bindings are the caller's, on a node of the callee.
                let node = self.node.take();
                for member in o.members() {
                    self.resolve_view_item(member);
                }
                self.node = node;
            }
            ViewItem::Replace(r) => {
                if let Some(body) = r.body() {
                    let node = self.node.take();
                    self.resolve_view_block(&body);
                    self.node = node;
                }
            }
        }
    }

    fn resolve_named_node(&mut self, node: &NamedNode) {
        // The node's local name binds a slot usable by later siblings.
        if let Some(tok) = node.name() {
            let name = self.interner.intern(&tok.text());
            let slot = self.scopes.bind(name);
            self.refs.push(ResolvedRef {
                range: tok.text_range(),
                to: Resolution::Local(slot),
            });
        }
        if let Some(ty) = node.ty() {
            self.resolve_node_type(&ty);
        }
        if let Some(body) = node.body() {
            let instantiated = self.instantiated(node.ty());
            self.resolve_node_body(&body, instantiated);
        }
    }

    /// Resolves a node body whose node instantiates the component `node`, if any.
    fn resolve_node_body(&mut self, body: &NodeBody, node: Option<SymbolId>) {
        let outer = std::mem::replace(&mut self.node, node);
        for member in body.members() {
            self.resolve_view_item(member);
        }
        self.node = outer;
    }

    fn resolve_property(&mut self, binding: &PropertyBinding) {
        if let Some(value) = binding.value() {
            self.resolve_expr(&value);
        }
    }

    /// Resolves a handler's event against the events of the component its node
    /// instantiates, then binds its payload pattern for its body.
    fn resolve_handler(&mut self, handler: &EventHandler) {
        if let Some(evt) = handler.event() {
            let name = self.interner.intern(&evt.text());
            if let Some(symbol) = self.member(self.node, name, Namespace::Event) {
                self.refs.push(ResolvedRef {
                    range: evt.text_range(),
                    to: Resolution::Symbol(symbol),
                });
            }
            // An unknown event is left unresolved rather than an error here: a
            // handler may bind a standard or widget event the resolver has no view
            // of; typing reports one a component does not have.
        }
        self.scopes.push();
        if let Some(payload) = handler.payload() {
            self.resolve_pattern_types(payload.syntax());
            self.bind_pattern(payload.syntax());
        }
        if let Some(body) = handler.body() {
            self.resolve_block(&body);
        }
        self.scopes.pop();
    }

    fn resolve_for(&mut self, for_item: &ViewFor) {
        // The iterable is evaluated in the outer scope — it cannot see the loop
        // pattern it is about to bind, so resolve it before pushing the loop scope.
        if let Some(iterable) = for_item.iterable() {
            self.resolve_expr(&iterable);
        }
        self.scopes.push();
        // Bind the loop pattern's names (the pattern precedes `in`).
        if let Some(pat) = for_item.pattern() {
            self.resolve_pattern_types(pat.syntax());
            for tok in pat.bindings() {
                let name = self.interner.intern(&tok.text());
                let slot = self.scopes.bind(name);
                self.refs.push(ResolvedRef {
                    range: tok.text_range(),
                    to: Resolution::Local(slot),
                });
            }
        }
        // The key is evaluated per item, so it resolves in the loop scope — it may
        // read the loop pattern (`key item.id`).
        if let Some(key) = for_item.key() {
            self.resolve_expr(&key);
        }
        if let Some(body) = for_item.body() {
            self.resolve_view_block(&body);
        }
        self.scopes.pop();
    }

    /// Resolves an `if / else if / else` region: each arm's condition (in the
    /// enclosing scope) and then-block, chaining through the `else` branch.
    fn resolve_if(&mut self, view_if: &ViewIf) {
        if let Some(condition) = view_if.condition() {
            self.resolve_expr(&condition);
        }
        if let Some(then_block) = view_if.then_block() {
            self.resolve_view_block(&then_block);
        }
        match view_if.else_branch() {
            Some(crate::ast::ElseBranch::If(nested)) => self.resolve_if(&nested),
            Some(crate::ast::ElseBranch::Block(block)) => self.resolve_view_block(&block),
            None => {}
        }
    }

    /// Resolves an expression, descending into it and resolving each path head.
    fn resolve_expr(&mut self, expr: &Expr) {
        self.resolve_body(expr.syntax());
    }

    /// Resolves a single-segment type path naming a value declaration: a
    /// theme's base, a style's base. A qualified or unbound one is left for
    /// lowering to report.
    fn resolve_named_value(&mut self, path: &TypePath) {
        let segments: Vec<_> = path.segments().collect();
        if let [name] = &segments[..] {
            self.resolve_value_token(name);
        }
    }

    /// Resolves a style: its target as a node type, its bases as styles, and
    /// its property values as a view's, which read `theme`. The selectors
    /// name members of the target, which lowering checks against its schema.
    fn resolve_style(&mut self, style: &crate::ast::StyleDecl) {
        if let Some(target) = style.target() {
            self.resolve_node_type(&target);
        }
        for base in style.bases() {
            self.resolve_named_value(&base);
        }
        let in_view = std::mem::replace(&mut self.in_view, true);
        let values = style
            .bindings()
            .chain(style.whens().flat_map(|w| w.bindings().collect::<Vec<_>>()));
        for binding in values {
            if let Some(value) = binding.value() {
                self.resolve_expr(&value);
            }
        }
        self.in_view = in_view;
    }

    /// Resolves the head segment of a value/property path: local scope first, then
    /// the module value namespace, then imports.
    fn resolve_value_path(&mut self, path: &PathExpr) {
        let Some(head) = path.segments().next() else {
            return;
        };
        use crate::syntax::SyntaxKind;
        // `self` is a method's receiver; `Self::..` names the impl's or the
        // trait's type. Neither is a module symbol.
        if head.kind() == SyntaxKind::SelfValueKw {
            let name = self.interner.intern("self");
            if let Some(slot) = self.scopes.lookup(name) {
                self.refs.push(ResolvedRef {
                    range: head.text_range(),
                    to: Resolution::Local(slot),
                });
            }
            return;
        }
        if head.kind() == SyntaxKind::SelfTypeKw {
            self.resolve_type_token(&head);
            return;
        }
        if self.resolve_value_token(&head) {
            return;
        }
        let name = self.interner.intern(&head.text());
        if let Some(base) = self.native_base(name) {
            let rest: Vec<_> = path.segments().skip(1).collect();
            self.resolve_native(&head, base, &rest, false);
            return;
        }
        // `S::idle` names a variant of the type `S`: a qualified head that is no
        // value resolves in the type namespace.
        if path.segments().nth(1).is_some() {
            self.resolve_type_token(&head);
            return;
        }
        let text = head.text();
        let builtin = BUILTIN_VALUES.contains(&text.as_str()) || self.owner_slots.contains(&text);
        if !builtin && !self.defer_unresolved_types && !self.imports.unresolved.contains(&name) {
            self.unresolved_value(&head);
        }
    }

    /// Reports a value name nothing declares (`E2001`), with the nearest names
    /// of the enclosing component and the module as fixes.
    fn unresolved_value(&mut self, head: &crate::syntax::SyntaxToken) {
        let text = head.text();
        let at = head.text_range();
        let mut diagnostic =
            Diagnostic::error("E2001", at, format!("no value named `{text}` is in scope"));
        let declared_at = |id| {
            self.decls
                .iter()
                .find(|d| d.id == id)
                .map(|d| (None, d.name_range))
        };
        let owner = self
            .owner
            .and_then(|o| self.members.get(&o))
            .into_iter()
            .flat_map(|t| t.names(Namespace::Value));
        let candidates: Vec<suggest::Candidate<'_>> = owner
            .chain(self.table.names(Namespace::Value))
            .filter_map(|(name, symbol)| {
                Some(suggest::Candidate {
                    name: self.interner.text(name)?,
                    declared_at: declared_at(symbol.id),
                })
            })
            .collect();
        let suggestions = suggest::nearest(&text, candidates);
        suggest::attach(&mut diagnostic, at, &suggestions);
        self.errors.push(diagnostic);
    }

    /// Resolves the path `head::rest..` whose head imports the native `base`:
    /// the function or schema enum variant (or, for a type position, handle
    /// type) it names is recorded on `head` as [`Resolution::Native`]; a path
    /// naming none is
    /// [`E2001`](ResolveErrorKind::UnresolvedNative) at its first unknown
    /// segment, with the nearest registered names.
    fn resolve_native(
        &mut self,
        head: &crate::syntax::SyntaxToken,
        base: &str,
        rest: &[crate::syntax::SyntaxToken],
        ty: bool,
    ) {
        let mut path = base.to_owned();
        let mut last = head.clone();
        let value = |natives: &Natives, path: &str| {
            natives
                .function(path)
                .map(|f| f.id)
                .or_else(|| natives.variant(path).map(|_| NativeId::of(path)))
        };
        for segment in rest {
            let found = if ty {
                self.natives.ty(&path).is_some()
            } else {
                value(self.natives, &path).is_some()
            };
            if found {
                break;
            }
            path.push_str("::");
            path.push_str(&segment.text());
            last = segment.clone();
        }
        let id = if ty {
            self.natives.ty(&path).map(|t| t.id)
        } else {
            value(self.natives, &path)
        };
        if let Some(id) = id {
            self.refs.push(ResolvedRef {
                range: head.text_range(),
                to: Resolution::Native(id),
            });
            return;
        }
        let at = last.text_range();
        let mut diagnostic = ResolveErrorKind::UnresolvedNative.to_diagnostic(Some(at), &path);
        let parent = path.rsplit_once("::").map_or("", |(p, _)| p);
        let variants: Vec<&str> = self
            .natives
            .ty(parent)
            .map_or(&[][..], |t| t.ty.variants)
            .to_vec();
        let names: Vec<&str> = self
            .natives
            .functions()
            .iter()
            .map(|f| &*f.path)
            .chain(self.natives.types().iter().map(|t| &*t.path))
            .filter_map(|p| p.strip_prefix(parent)?.strip_prefix("::"))
            .filter(|n| !n.contains("::"))
            .chain(variants)
            .collect();
        let suggestions = suggest::nearest(
            &last.text(),
            names.into_iter().map(|name| suggest::Candidate {
                name,
                declared_at: None,
            }),
        );
        suggest::attach(&mut diagnostic, at, &suggestions);
        self.errors.push(diagnostic);
    }

    /// Resolves one value name token: local scope first, then the enclosing
    /// component's members, the module value namespace, imports, and the prelude.
    /// Returns whether it resolved.
    fn resolve_value_token(&mut self, head: &crate::syntax::SyntaxToken) -> bool {
        let name = self.interner.intern(&head.text());
        let const_param = || {
            self.type_scopes.iter().rev().find_map(|scope| {
                scope
                    .iter()
                    .find(|(n, _, is_const)| *n == name && *is_const)
                    .map(|(_, s, _)| *s)
            })
        };
        let to = if let Some(slot) = self.scopes.lookup(name) {
            Resolution::Local(slot)
        } else if let Some(symbol) = const_param() {
            Resolution::Symbol(symbol)
        } else if let Some(symbol) = self.member(self.owner, name, Namespace::Value) {
            Resolution::Symbol(symbol)
        } else if let Some(sym) = self.table.get(name, Namespace::Value) {
            Resolution::Symbol(sym.id)
        } else if let Some(binding) = self.imports.bindings.get(&name) {
            Resolution::Symbol(binding.symbol)
        } else if let Some(sym) = self.standard(name, Namespace::Value) {
            Resolution::Symbol(sym)
        } else if head.text() == ENV {
            if !self.in_view {
                self.errors.push(Diagnostic::error(
                    ENV_OUTSIDE_VIEW,
                    head.text_range(),
                    "`env` is read only in a view: its items, property bindings, \
                     conditions and handlers; pass the value a state or function needs"
                        .to_string(),
                ));
            }
            Resolution::Env
        } else if head.text() == THEME {
            if !self.in_view {
                self.errors.push(Diagnostic::error(
                    ENV_OUTSIDE_VIEW,
                    head.text_range(),
                    "`theme` is read only in a view and a style; pass the value a state or \
                     function needs"
                        .to_string(),
                ));
            }
            Resolution::Theme
        } else {
            // Possibly a native/schema name; not diagnosed at this layer.
            return false;
        };
        self.refs.push(ResolvedRef {
            range: head.text_range(),
            to,
        });
        true
    }

    /// Resolves the head segment of a type path against the type namespace, then
    /// imports; an unresolved user type is [`E2001`].
    fn resolve_type_path(&mut self, ty: &TypePath) {
        self.resolve_type_head(ty, self.defer_unresolved_types);
    }

    /// Resolves a view node's type. A node type no declaration or import names is a
    /// native/widget schema type (`Column`, `Leaf`, …), in a component's view exactly
    /// as in a fragment, so it is left for the schema to check rather than raising
    /// [`E2001`].
    fn resolve_node_type(&mut self, ty: &TypePath) {
        self.resolve_type_head(ty, true);
    }

    fn resolve_type_head(&mut self, ty: &TypePath, defer_unresolved: bool) {
        use crate::syntax::SyntaxKind;
        // Generic arguments (`List<P>`) name types too.
        for segment in ty.syntax().children() {
            for generic in segment.children() {
                if generic.kind() != SyntaxKind::GenericArgs {
                    continue;
                }
                for arg in generic.children() {
                    match arg.kind() {
                        SyntaxKind::TypePath => {
                            if let Some(arg) = TypePath::cast(arg) {
                                self.resolve_type_head(&arg, defer_unresolved);
                            }
                        }
                        SyntaxKind::TupleType
                        | SyntaxKind::DynType
                        | SyntaxKind::ConstGenericArg => self.resolve_body(&arg),
                        _ => {}
                    }
                }
            }
        }
        // A function type (`Fn(A) -> R`) holds its parameter and result
        // types directly.
        for child in ty.syntax().children() {
            match child.kind() {
                SyntaxKind::TypePath => {
                    if let Some(inner) = TypePath::cast(child) {
                        self.resolve_type_head(&inner, defer_unresolved);
                    }
                }
                SyntaxKind::TupleType | SyntaxKind::ArrayType | SyntaxKind::DynType => {
                    self.resolve_body(&child)
                }
                _ => {}
            }
        }
        let Some(head) = ty.segments().next() else {
            return;
        };
        let text = head.text();
        let name = self.interner.intern(&text);
        if let Some(symbol) = self.type_symbol(name) {
            self.refs.push(ResolvedRef {
                range: head.text_range(),
                to: Resolution::Symbol(symbol),
            });
            return;
        }
        if let Some(base) = self.native_base(name) {
            let rest: Vec<_> = ty.segments().skip(1).collect();
            self.resolve_native(&head, base, &rest, true);
            return;
        }
        // `Float` is no type: the one diagnostic for it, wherever a type is
        // written, is raised here.
        if text == "Float" {
            let err = crate::hir::TypeError::FloatRemoved;
            let mut diagnostic = Diagnostic::error(err.code(), head.text_range(), err.message());
            for width in ["F64", "F32"] {
                diagnostic.fixes.push(Fix {
                    title: format!("use `{width}`"),
                    applicability: Applicability::MaybeIncorrect,
                    edits: vec![TextEdit::new(head.text_range(), width)],
                });
            }
            self.errors.push(diagnostic);
            return;
        }
        // Built-in/native types (Int, Text, Color, ...) are provided by schema, not
        // by a user declaration, so only a name that looks user-defined is flagged.
        // In a fragment there is no compilation unit to declare it and no import, so
        // the name is a native/schema widget type — deferred, never diagnosed here.
        if is_user_type_name(&text) && !defer_unresolved && !self.imports.unresolved.contains(&name)
        {
            let at = head.text_range();
            let mut diagnostic = ResolveErrorKind::UnresolvedType.to_diagnostic(Some(at), &text);
            let suggestions = self.nearest_types(&text);
            suggest::attach(&mut diagnostic, at, &suggestions);
            self.errors.push(diagnostic);
        }
    }

    /// The type `name` names: this module's declaration, then a type import, then the
    /// prelude's.
    fn type_symbol(&self, name: NameId) -> Option<SymbolId> {
        self.type_scopes
            .iter()
            .rev()
            .find_map(|scope| {
                scope
                    .iter()
                    .find(|(n, _, is_const)| *n == name && !is_const)
                    .map(|(_, s, _)| *s)
            })
            .or_else(|| self.table.get(name, Namespace::Type).map(|s| s.id))
            .or_else(|| {
                self.imports
                    .bindings
                    .get(&name)
                    .filter(|b| b.namespace == Namespace::Type)
                    .map(|b| b.symbol)
            })
            .or_else(|| self.standard(name, Namespace::Type))
    }

    /// The prelude's exported symbol `name` in `ns`.
    fn standard(&self, name: NameId, ns: Namespace) -> Option<SymbolId> {
        self.prelude
            .get(name, ns)
            .filter(|s| s.exported)
            .map(|s| s.id)
    }

    /// The type-namespace names in scope nearest to `text`: this module's own type
    /// declarations (with their declaration spans), its type imports and the prelude's
    /// types.
    fn nearest_types(&self, text: &str) -> Vec<suggest::Candidate<'_>> {
        let declared_at = |id| {
            self.decls
                .iter()
                .find(|d| d.id == id)
                .map(|d| (None, d.name_range))
        };
        let own = self
            .table
            .names(Namespace::Type)
            .filter_map(|(name, symbol)| {
                Some(suggest::Candidate {
                    name: self.interner.text(name)?,
                    declared_at: declared_at(symbol.id),
                })
            });
        let imported = self
            .imports
            .bindings
            .iter()
            .filter(|(_, binding)| binding.namespace == Namespace::Type)
            .filter_map(|(&name, _)| {
                Some(suggest::Candidate {
                    name: self.interner.text(name)?,
                    declared_at: None,
                })
            });
        let standard = self
            .prelude
            .names(Namespace::Type)
            .filter(|(_, symbol)| symbol.exported)
            .filter_map(|(name, _)| {
                Some(suggest::Candidate {
                    name: self.interner.text(name)?,
                    declared_at: None,
                })
            });
        suggest::nearest(text, own.chain(imported).chain(standard))
    }
}

/// The resolution of a bare `ui!` [`ViewFragment`] against a caller-supplied set of
/// reactive-source names (AGENTS section 21.5).
///
/// A `ui!` fragment has no surrounding component and no `CompilationUnit`: its
/// reactive sources are Rust `state`/signals captured into the builder closure, named
/// only by the caller. [`resolve_fragment`] runs the *same* view walker the component
/// frontend uses ([`ModulePass::resolve_view_item`]) so a fragment is checked
/// identically — node-name and loop-pattern locals bind to [`Resolution::Local`],
/// unknown names stay unresolved (native/schema, deferred), and each caller-named
/// reactive source resolves to a durable [`SymbolId`]. The `sources` map is what a
/// caller turns into a [`crate::hir::ReadEnv`] for the Binding IR / keys passes.
pub struct ResolvedFragment {
    /// Every name use the walk resolved, in source order — the `refs` table the
    /// Binding IR and keys passes read.
    pub refs: Vec<ResolvedRef>,
    /// The [`SymbolId`] minted for each reactive-source name, in the order the caller
    /// supplied them. A caller builds its [`crate::hir::ReadEnv`] from these ids.
    pub sources: Vec<SymbolId>,
    /// Diagnostics gathered resolving the fragment (an unresolved user type, etc.).
    pub errors: Vec<Diagnostic>,
}

/// Resolves a bare `ui!` [`ViewFragment`]'s items, treating `sources` as the reactive
/// state names captured from the surrounding Rust scope (AGENTS section 21.5).
///
/// Each source name is seeded into a fresh value-namespace [`SymbolTable`] with a
/// durable [`SymbolId`] (a [`SymbolKind::State`] fingerprint over `package`, an empty
/// module path, and the source name), so a property value reading that name resolves
/// to [`Resolution::Symbol`] — exactly as a component `state` read would. Imports are
/// empty (a fragment has none), so any other free name is left unresolved for the
/// caller's `ReadEnv` to treat as non-reactive. This reuses the component view walker
/// verbatim; the fragment path adds no second set of resolution rules.
pub fn resolve_fragment(
    fragment: &crate::ast::ViewFragment,
    sources: &[&str],
    interner: &mut NameInterner,
    package: &str,
) -> ResolvedFragment {
    // Seed one value-namespace symbol per reactive source. A duplicate name keeps the
    // first (the table reports the clash) so `source_ids` stays 1:1 with `sources`.
    let mut table = SymbolTable::new();
    let mut source_ids = Vec::with_capacity(sources.len());
    for name in sources {
        let id = fingerprint(SymbolIdentity {
            package,
            module_path: "",
            kind: SymbolKind::State,
            decl_path: name,
        });
        let name_id = interner.intern(name);
        let _ = table.define(
            name_id,
            Namespace::Value,
            ModuleSymbol {
                id,
                exported: false,
            },
        );
        source_ids.push(id);
    }

    let imports = ImportEnv::default();
    let members = MemberTables::new();
    let prelude = SymbolTable::new();
    let natives = Natives::new();
    let mut pass = ModulePass {
        table: &table,
        prelude: &prelude,
        members: &members,
        owner: None,
        owner_name: None,
        type_scopes: Vec::new(),
        decl_at: std::collections::HashMap::new(),
        node: None,
        decls: &[],
        imports: &imports,
        natives: &natives,
        interner,
        refs: Vec::new(),
        errors: Vec::new(),
        scopes: ScopeStack::new(),
        // A fragment's node types are native/schema-provided (no imports, no unit),
        // so an unresolved PascalCase name defers instead of raising E2001.
        defer_unresolved_types: true,
        owner_slots: Vec::new(),
        // A fragment is a view.
        in_view: true,
    };
    // A fragment's items are top-level (no `ViewBlock` wrapper); open one scope for
    // node-name / loop-pattern locals, matching `resolve_view_block`.
    pass.scopes.push();
    for item in fragment.items() {
        pass.resolve_view_item(item);
    }
    pass.scopes.pop();

    ResolvedFragment {
        refs: pass.refs,
        sources: source_ids,
        errors: pass.errors,
    }
}

/// Whether a type name is a user-defined type (uppercase-initial) not covered by the
/// built-in names. A conservative approximation until the native schema lands: only
/// PascalCase names that are neither a scalar the typer knows
/// ([`Ty::from_builtin_name`], which also reports the removed `Float` itself) nor a
/// structural head are flagged, so a missing user type surfaces while built-ins stay
/// quiet.
fn is_user_type_name(text: &str) -> bool {
    const STRUCTURAL: &[&str] = &[
        "Int",
        "Text",
        "Vec2",
        "Vec3",
        "Vec4",
        "List",
        "Map",
        "Option",
        "Result",
        "Range",
        "RangeInclusive",
        "Resource",
        "ResourceState",
        "Self",
    ];
    let starts_upper = text.chars().next().is_some_and(|c| c.is_uppercase());
    let builtin = !matches!(Ty::from_builtin_name(text), Ok(None));
    starts_upper && !builtin && !STRUCTURAL.contains(&text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolve::ModulePath;
    use crate::syntax::{grammar::parse, tokenize};

    fn parse_unit(src: &str) -> crate::syntax::grammar::Parse {
        parse(&tokenize(src), src)
    }

    fn unit(interner: &mut NameInterner, path: &[&str], src: &str) -> SourceUnit {
        SourceUnit::new(ModulePath::intern(interner, path), parse_unit(src))
    }

    fn resolve_all(units: Vec<SourceUnit>, interner: &mut NameInterner) -> Vec<ResolvedModule> {
        let graph = ModuleGraph::build(&units, interner);
        resolve(&graph, &units, interner, "app")
    }

    #[test]
    fn a_cross_module_exported_type_resolves_to_its_symbol() {
        let mut interner = NameInterner::new();
        let lib = unit(&mut interner, &["lib"], "export record Point { x: Int; }");
        let app = unit(
            &mut interner,
            &["app"],
            "import lib::{ Point }; component A { input origin: Point; view { } }",
        );
        let mods = resolve_all(vec![lib, app], &mut interner);
        // The `lib` symbol table exports `Point` in the type namespace.
        let point = interner.intern("Point");
        let lib_mod = mods
            .iter()
            .find(|m| m.table.get(point, Namespace::Type).is_some());
        assert!(lib_mod.is_some(), "lib exports Point as a type");
        // The `app` module resolves its `Point` type reference to lib's symbol.
        let lib_point = lib_mod
            .unwrap()
            .table
            .get(point, Namespace::Type)
            .unwrap()
            .id;
        let resolved_to_lib = mods
            .iter()
            .flat_map(|m| m.refs.iter())
            .any(|r| r.to == Resolution::Symbol(lib_point));
        assert!(
            resolved_to_lib,
            "app's `Point` type use resolves to lib's exported symbol"
        );
    }

    #[test]
    fn an_import_alias_rebinds_the_local_name() {
        let mut interner = NameInterner::new();
        let lib = unit(&mut interner, &["lib"], "export record Point { x: Int; }");
        let app = unit(
            &mut interner,
            &["app"],
            "import lib::{ Point as P }; component A { input origin: P; view { } }",
        );
        let mods = resolve_all(vec![lib, app], &mut interner);
        let point = interner.intern("Point");
        let lib_point = mods
            .iter()
            .find_map(|m| m.table.get(point, Namespace::Type))
            .expect("lib exports Point")
            .id;
        let resolved = mods
            .iter()
            .flat_map(|m| m.refs.iter())
            .any(|r| r.to == Resolution::Symbol(lib_point));
        assert!(resolved, "the aliased `P` resolves to lib's `Point` symbol");
    }

    #[test]
    fn an_unresolved_user_type_is_e2001() {
        let mut interner = NameInterner::new();
        let app = unit(
            &mut interner,
            &["app"],
            "component A { input value: Missing; view { } }",
        );
        let mods = resolve_all(vec![app], &mut interner);
        assert!(
            mods.iter()
                .flat_map(|m| m.errors.iter())
                .any(|d| d.code == "E2001" && d.message.contains("`Missing`")),
            "an unknown PascalCase type is E2001"
        );
    }

    #[test]
    fn prelude_types_resolve_and_are_suggested() {
        let mut interner = NameInterner::new();
        let src = "component A { input at: Point; input miss: Pointt; view { } }";
        let app = unit(&mut interner, &["app"], src);
        let mods = resolve_all(vec![app], &mut interner);
        let point = interner.intern("Point");
        let prelude = Prelude::load(&mut interner);
        let standard = prelude
            .module
            .table
            .get(point, Namespace::Type)
            .expect("Point")
            .id;
        assert!(
            mods[0]
                .refs
                .iter()
                .any(|r| r.to == Resolution::Symbol(standard))
        );
        let errors: Vec<_> = mods[0].errors.iter().map(|d| d.code).collect();
        assert_eq!(errors, ["E2001"]);
        let fix = &mods[0].errors[0].fixes[0].edits[0];
        assert_eq!(fix.replacement, "Point");
    }

    #[test]
    fn an_unresolved_value_is_e2001_with_the_nearest_names() {
        let mut interner = NameInterner::new();
        let src = "component A { state count = 0; slot body: Slot<Node>; \
                   action go() { count = cont + 1; let x = Some(format(\"{count}\")); } \
                   view { SlotOutlet { slot: body; } } }";
        let app = unit(&mut interner, &["app"], src);
        let mods = resolve_all(vec![app], &mut interner);
        let errors: Vec<_> = mods
            .iter()
            .flat_map(|m| m.errors.iter())
            .filter(|d| d.code == "E2001")
            .collect();
        // Builtins and the component's slots are no unresolved names.
        assert_eq!(errors.len(), 1, "{errors:#?}");
        assert!(errors[0].message.contains("`cont`"), "{errors:#?}");
        let replacements: Vec<_> = errors[0]
            .fixes
            .iter()
            .map(|f| f.edits[0].replacement.as_str())
            .collect();
        assert_eq!(replacements, ["count"]);
    }

    #[test]
    fn an_unresolved_type_suggests_the_nearest_type_names() {
        let mut interner = NameInterner::new();
        let lib = unit(&mut interner, &["lib"], "export record Bag { x: Int; }");
        let src = "import lib::{ Bag }; record Badge { x: Int; } const Bade = 1; \
                   component A { input value: Badg; view { } }";
        let app = unit(&mut interner, &["app"], src);
        let mods = resolve_all(vec![lib, app], &mut interner);
        let error = mods
            .iter()
            .flat_map(|m| m.errors.iter())
            .find(|d| d.code == "E2001")
            .expect("`Badg` is E2001");
        // The value `Bade` is as near but lives in the wrong namespace.
        let replacements: Vec<_> = error
            .fixes
            .iter()
            .map(|f| (f.applicability, f.edits[0].replacement.as_str()))
            .collect();
        assert_eq!(
            replacements,
            [
                (crate::diag::Applicability::MaybeIncorrect, "Badge"),
                (crate::diag::Applicability::MaybeIncorrect, "Bag"),
            ]
        );
        assert!(
            error
                .fixes
                .iter()
                .all(|f| &src[range(f.edits[0].range)] == "Badg")
        );
        // Only the local declaration has a span in this file.
        let related: Vec<_> = error.related.iter().map(|r| &src[range(r.range)]).collect();
        assert_eq!(related, ["Badge"]);
    }

    fn range(r: TextRange) -> std::ops::Range<usize> {
        r.start().to_u32() as usize..r.end().to_u32() as usize
    }

    #[test]
    fn an_import_of_a_private_declaration_offers_to_export_it() {
        let mut interner = NameInterner::new();
        let lib_src = "@deprecated\nrecord Point { x: Int; }";
        let lib = unit(&mut interner, &["lib"], lib_src);
        let src = "import lib::{ Point }; component A { input origin: Point; view { } }";
        let app = unit(&mut interner, &["app"], src);
        let mods = resolve_all(vec![lib, app], &mut interner);
        let errors: Vec<_> = mods.iter().flat_map(|m| m.errors.iter()).collect();
        assert_eq!(errors.len(), 1, "{errors:?}");
        let error = errors[0];
        assert_eq!(error.code, "E2001");
        assert!(error.message.contains("not exported"), "{}", error.message);
        assert_eq!(&src[range(error.primary)], "Point");
        let related = &error.related[0];
        assert_eq!(related.module.as_deref(), Some("lib"));
        assert_eq!(&lib_src[range(related.range)], "Point");
        let edit = &error.fixes[0].edits[0];
        assert_eq!(edit.module.as_deref(), Some("lib"));
        let keyword = lib_src.find("record").unwrap();
        assert_eq!(range(edit.range), keyword..keyword);
        assert_eq!(edit.replacement, "export ");
        // The import still binds, so its use raises nothing more.
        let point = interner.intern("Point");
        let id = mods
            .iter()
            .find_map(|m| m.table.get(point, Namespace::Type))
            .unwrap()
            .id;
        assert!(
            mods.iter()
                .flat_map(|m| m.refs.iter())
                .any(|r| r.to == Resolution::Symbol(id))
        );
    }

    #[test]
    fn an_import_of_a_missing_name_suggests_the_modules_exports() {
        let mut interner = NameInterner::new();
        let lib_src = "export record Point { x: Int; } record Pont2 { x: Int; }";
        let lib = unit(&mut interner, &["lib"], lib_src);
        let src = "import lib::{ Pont }; component A { input origin: Pont; view { } }";
        let app = unit(&mut interner, &["app"], src);
        let mods = resolve_all(vec![lib, app], &mut interner);
        let errors: Vec<_> = mods.iter().flat_map(|m| m.errors.iter()).collect();
        // One error at the import; the use of the name it failed to bind is quiet.
        assert_eq!(errors.len(), 1, "{errors:?}");
        let error = errors[0];
        assert_eq!(error.code, "E2001");
        let at = src.find("Pont").unwrap();
        assert_eq!(range(error.primary), at..at + 4);
        // Only exported names are offered, each declared in `lib`.
        let replacements: Vec<_> = error
            .fixes
            .iter()
            .map(|f| f.edits[0].replacement.as_str())
            .collect();
        assert_eq!(replacements, ["Point"]);
        assert_eq!(error.fixes[0].edits[0].module, None);
        assert_eq!(error.related[0].module.as_deref(), Some("lib"));
        assert_eq!(&lib_src[range(error.related[0].range)], "Point");
    }

    #[test]
    fn a_namespace_collision_is_reported() {
        let mut interner = NameInterner::new();
        // Two records named `Dup` collide in the type namespace.
        let app = unit(
            &mut interner,
            &["app"],
            "record Dup { a: Int; } record Dup { b: Int; }",
        );
        let mods = resolve_all(vec![app], &mut interner);
        assert!(
            mods.iter()
                .flat_map(|m| m.errors.iter())
                .any(|d| d.code == "E2002" && d.message.contains("`Dup`")),
            "a repeated type name in one module is a collision"
        );
    }

    #[test]
    fn an_nfc_equal_respelling_is_a_normalization_conflict() {
        let mut interner = NameInterner::new();
        // `café` composed and decomposed: one name after NFC, two spellings.
        let src = "const caf\u{e9} = 1; const cafe\u{301} = 2; fn f() -> I64 { cafe\u{301} }";
        let app = unit(&mut interner, &["app"], src);
        let mods = resolve_all(vec![app], &mut interner);
        let errors: Vec<_> = mods.iter().flat_map(|m| m.errors.iter()).collect();
        let conflict = errors
            .iter()
            .find(|d| d.code == "E1101")
            .unwrap_or_else(|| panic!("no E1101 in {errors:?}"));
        assert!(!errors.iter().any(|d| d.code == "E2002"), "{errors:?}");
        let first = conflict.related[0].range;
        let at = first.start().to_u32() as usize..first.end().to_u32() as usize;
        assert_eq!(&src[at], "caf\u{e9}");
        // The same spelling twice stays a plain duplicate.
        let mut interner = NameInterner::new();
        let app = unit(&mut interner, &["app"], "const x = 1; const x = 2;");
        let mods = resolve_all(vec![app], &mut interner);
        let codes: Vec<_> = mods
            .iter()
            .flat_map(|m| m.errors.iter())
            .map(|d| d.code)
            .collect();
        assert_eq!(codes, ["E2002"]);
    }

    #[test]
    fn a_view_local_node_name_resolves_to_a_local_slot() {
        let mut interner = NameInterner::new();
        let app = unit(
            &mut interner,
            &["app"],
            "component A { view { Row { for item in items key item.id { Text { text: item; } } } } }",
        );
        let mods = resolve_all(vec![app], &mut interner);
        // `item` (the for-binding) and its use both appear as local resolutions.
        let has_local = mods
            .iter()
            .flat_map(|m| m.refs.iter())
            .any(|r| matches!(r.to, Resolution::Local(_)));
        assert!(
            has_local,
            "the `for` pattern binding resolves to a local slot"
        );
    }

    #[test]
    fn a_component_entry_resolves_its_state_reference() {
        let mut interner = NameInterner::new();
        let app = unit(
            &mut interner,
            &["app"],
            "component Counter { state count = 0; computed doubled = count; view { } }",
        );
        let mods = resolve_all(vec![app], &mut interner);
        let count = interner.intern("count");
        let count_sym = mods
            .iter()
            .find_map(|m| {
                m.table
                    .member_tables()
                    .find_map(|(_, t)| t.get(count, Namespace::Value))
            })
            .expect("count is a member value symbol")
            .id;
        let resolved = mods
            .iter()
            .flat_map(|m| m.refs.iter())
            .any(|r| r.to == Resolution::Symbol(count_sym));
        assert!(
            resolved,
            "`computed doubled = count` resolves `count` to its state symbol"
        );
    }

    #[test]
    fn members_are_scoped_to_their_owner() {
        let mut interner = NameInterner::new();
        let src = "const n = 1;
            component A { state n = 0; event changed(v: I64); computed m = n; view { } }
            component B { state n = 0; event changed(v: I64); view { } }
            component C { computed k = n; view { A { on changed(ev) { let x = ev; } } } }";
        let app = unit(&mut interner, &["app"], src);
        let mods = resolve_all(vec![app], &mut interner);
        let m = &mods[0];
        assert!(
            m.errors.is_empty(),
            "no collision across owners: {:?}",
            m.errors
        );
        let n = interner.intern("n");
        let changed = interner.intern("changed");
        let [a, b, c] = ["A", "B", "C"].map(|name| {
            let name = interner.intern(name);
            m.table.get(name, Namespace::Type).expect("component").id
        });
        let members = |owner| m.table.members(owner).expect("member table");
        let a_n = members(a).get(n, Namespace::Value).expect("A.n").id;
        let b_n = members(b).get(n, Namespace::Value).expect("B.n").id;
        assert_ne!(a_n, b_n);
        assert!(members(c).get(n, Namespace::Value).is_none());
        let const_n = m.table.get(n, Namespace::Value).expect("const n").id;
        let to = |range| m.refs.iter().find(|r| r.range == range).map(|r| r.to);
        let use_in = |prefix: &str, name: &str| {
            let start = src.find(prefix).expect("prefix") + prefix.len() - name.len();
            let start = start as u32;
            to(crate::syntax::TextRange::new(
                start.into(),
                (start + name.len() as u32).into(),
            ))
        };
        // A member shadows the module `const`; outside any owner of `n`, the `const`.
        assert_eq!(use_in("computed m = n", "n"), Some(Resolution::Symbol(a_n)));
        assert_eq!(
            use_in("computed k = n", "n"),
            Some(Resolution::Symbol(const_n))
        );
        // `on changed` on an `A` node names `A`'s event; its payload binds for the body.
        let a_changed = members(a)
            .get(changed, Namespace::Event)
            .expect("A.changed")
            .id;
        assert_eq!(
            use_in("on changed", "changed"),
            Some(Resolution::Symbol(a_changed))
        );
        assert!(matches!(
            use_in("let x = ev", "ev"),
            Some(Resolution::Local(_))
        ));
    }

    #[test]
    fn emit_names_an_event_of_its_component() {
        let mut interner = NameInterner::new();
        let src =
            "component A { event changed(v: I64); action go() { emit changed(1); } view { } }";
        let app = unit(&mut interner, &["app"], src);
        let mods = resolve_all(vec![app], &mut interner);
        let m = &mods[0];
        let a = m
            .table
            .get(interner.intern("A"), Namespace::Type)
            .expect("A")
            .id;
        let changed = interner.intern("changed");
        let event = m
            .table
            .members(a)
            .and_then(|t| t.get(changed, Namespace::Event));
        let event = event.expect("A.changed").id;
        let start = src.find("emit changed").expect("emit") as u32 + 5;
        let range = crate::syntax::TextRange::new(start.into(), (start + 7).into());
        assert!(
            m.refs
                .iter()
                .any(|r| r.range == range && r.to == Resolution::Symbol(event))
        );
    }

    // --- `ui!` fragment resolution ------------------------------------------

    /// Parses `src` as a bare `ui!` view fragment.
    fn fragment(src: &str) -> crate::ast::ViewFragment {
        use crate::ast::AstNode;
        use crate::syntax::grammar::{Entry, parse_entry};
        let root = crate::syntax::SyntaxNode::new_root(
            parse_entry(&tokenize(src), src, Entry::ViewFragment).root,
        );
        crate::ast::ViewFragment::cast(root).expect("a ViewFragment root")
    }

    #[test]
    fn a_fragment_property_reading_a_source_resolves_to_its_symbol() {
        let mut interner = NameInterner::new();
        let frag = fragment("Text { text: label; }");
        let out = resolve_fragment(&frag, &["label"], &mut interner, "<ui!>");

        assert_eq!(out.sources.len(), 1, "one seeded reactive source");
        assert!(out.errors.is_empty(), "a bare source read is not an error");
        // The `label` value read resolves to the minted state symbol.
        let label = out.sources[0];
        assert!(
            out.refs.iter().any(|r| r.to == Resolution::Symbol(label)),
            "the `label` read resolves to its seeded symbol"
        );
    }

    #[test]
    fn a_fragment_source_symbol_matches_a_state_fingerprint() {
        // The minted id is a `State`-kind fingerprint over the synthetic package and
        // the source name — stable and independent of resolution order.
        let mut interner = NameInterner::new();
        let frag = fragment("Text { text: label; }");
        let out = resolve_fragment(&frag, &["label"], &mut interner, "<ui!>");
        let expected = fingerprint(SymbolIdentity {
            package: "<ui!>",
            module_path: "",
            kind: SymbolKind::State,
            decl_path: "label",
        });
        assert_eq!(out.sources[0], expected);
    }

    #[test]
    fn a_fragment_loop_pattern_binds_a_local_not_a_source() {
        let mut interner = NameInterner::new();
        let frag = fragment("for item in items key item.id { Row { } }");
        // `items` is a reactive source; `item` is a loop local.
        let out = resolve_fragment(&frag, &["items"], &mut interner, "<ui!>");
        let items = out.sources[0];
        assert!(
            out.refs.iter().any(|r| r.to == Resolution::Symbol(items)),
            "the iterable `items` resolves to its source symbol"
        );
        assert!(
            out.refs
                .iter()
                .any(|r| matches!(r.to, Resolution::Local(_))),
            "the loop pattern `item` binds a local slot"
        );
    }

    #[test]
    fn a_fragment_free_name_is_left_unresolved() {
        // A name that is neither a seeded source nor a local is left unresolved for
        // the caller's ReadEnv to treat as non-reactive — no diagnostic.
        let mut interner = NameInterner::new();
        let frag = fragment("Text { text: helper; }");
        let out = resolve_fragment(&frag, &[], &mut interner, "<ui!>");
        assert!(out.sources.is_empty(), "no sources seeded");
        assert!(
            out.errors.is_empty(),
            "an unresolved free name is not an error here"
        );
        assert!(
            !out.refs
                .iter()
                .any(|r| matches!(r.to, Resolution::Symbol(_))),
            "no free name resolves to a symbol"
        );
    }
}
