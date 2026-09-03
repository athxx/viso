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
//!    against the value namespace and the local scope stack; an `on <event>` name
//!    resolves against the event namespace. View-local `node` names, `for`-pattern
//!    bindings, and `let`/parameter names open local scopes whose uses resolve to a
//!    [`LocalSlot`](super::scope::LocalSlot).
//!
//! Everything is cold-path (AGENTS section 7.2): resolution runs once per build.

use crate::ast::{
    AstNode, Block, CompilationUnit, ComponentDecl, EventHandler, Expr, Item, Member, NamedNode,
    NodeBody, PathExpr, PropertyBinding, SystemDecl, TypePath, ViewBlock, ViewFor, ViewIf,
    ViewItem,
};
use crate::diag::Diagnostic;
use crate::syntax::SyntaxNode;
use crate::syntax::span::TextRange;

use super::module::{ModuleGraph, ResolveErrorKind, SourceUnit};
use super::name::{NameId, NameInterner};
use super::scope::{LocalSlot, ModuleSymbol, Namespace, ScopeStack, SymbolTable};
use super::symbol::{SymbolId, SymbolIdentity, SymbolKind, fingerprint};

/// What a name use resolves to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolution {
    /// A durable declaration identity, in this module or an imported one.
    Symbol(SymbolId),
    /// A lexically-bound local (a `let`, parameter, `for` pattern, or `node` name).
    Local(LocalSlot),
}

/// One resolved name use: the source span it occupies and what it resolves to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedRef {
    /// The span of the resolved name token.
    pub range: TextRange,
    /// The resolution target.
    pub to: Resolution,
}

/// The result of resolving one module: its symbol table, its resolved references,
/// and any diagnostics gathered along the way.
pub struct ResolvedModule {
    /// The module's own top-level declarations by name and namespace.
    pub table: SymbolTable,
    /// Every name use the pass resolved, in source order.
    pub refs: Vec<ResolvedRef>,
    /// Diagnostics from this module's resolution.
    pub errors: Vec<Diagnostic>,
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
    // First pass: every module's public symbol table, so cross-module imports can be
    // resolved before any module body is walked.
    let mut tables: Vec<SymbolTable> = Vec::with_capacity(graph.modules().len());
    let mut early_errors: Vec<Vec<Diagnostic>> = Vec::with_capacity(graph.modules().len());
    for gm in graph.modules() {
        let module_text = gm.path.display(interner);
        let cu = unit_for(units, &module_text, interner);
        let (table, errors) = build_symbol_table(cu.as_ref(), package, &module_text, interner);
        tables.push(table);
        early_errors.push(errors);
    }

    // Second pass: resolve each module's bodies against its own table plus imports.
    let mut resolved = Vec::with_capacity(graph.modules().len());
    for (i, gm) in graph.modules().iter().enumerate() {
        let module_text = gm.path.display(interner);
        let cu = unit_for(units, &module_text, interner);
        let imports = build_import_env(cu.as_ref(), graph, &tables, interner);
        let mut pass = ModulePass {
            table: &tables[i],
            imports: &imports,
            interner,
            refs: Vec::new(),
            errors: std::mem::take(&mut early_errors[i]),
            scopes: ScopeStack::new(),
            // The component frontend has declarations/imports; a genuinely missing
            // user type is a real error here.
            defer_unresolved_types: false,
        };
        if let Some(cu) = &cu {
            pass.resolve_unit(cu);
        }
        // Move `refs`/`errors` out of `pass` first: this drops the `&tables[i]`
        // borrow `pass.table` held, so the table can then be taken by value.
        let ModulePass { refs, errors, .. } = pass;
        resolved.push(ResolvedModule {
            table: std::mem::take(&mut tables[i]),
            refs,
            errors,
        });
    }
    resolved
}

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
fn build_symbol_table(
    cu: Option<&CompilationUnit>,
    package: &str,
    module_text: &str,
    interner: &mut NameInterner,
) -> (SymbolTable, Vec<Diagnostic>) {
    let mut table = SymbolTable::new();
    let mut errors = Vec::new();
    let Some(cu) = cu else {
        return (table, errors);
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
        let text = name_tok.text();
        let name = interner.intern(&text);
        let id = fingerprint(SymbolIdentity {
            package,
            module_path: module_text,
            kind,
            decl_path: &text,
        });
        let symbol = ModuleSymbol { id, exported };
        if let Err(_existing) = table.define(name, ns, symbol) {
            errors.push(
                ResolveErrorKind::AmbiguousModule.to_diagnostic(Some(name_tok.text_range()), &text),
            );
        }
        // A component's/system's members (state, computed, input, event, and the
        // callables) are named module symbols too: an intra-component reference such
        // as `computed x = count` resolves `count` to the member's symbol. Their
        // fingerprint is keyed by the enclosing declaration's name so two components
        // may each declare a `count` without colliding.
        if let Item::Component(_) | Item::System(_) = decl {
            define_members(
                &decl,
                &text,
                package,
                module_text,
                interner,
                &mut table,
                &mut errors,
            );
        }
    }
    (table, errors)
}

/// Defines a component's or system's members into the module symbol table, each
/// fingerprinted under the owner's name so members of different owners stay distinct.
fn define_members(
    decl: &Item,
    owner: &str,
    package: &str,
    module_text: &str,
    interner: &mut NameInterner,
    table: &mut SymbolTable,
    errors: &mut Vec<Diagnostic>,
) {
    let members: Vec<crate::ast::Member> = match decl {
        Item::Component(c) => c.members().collect(),
        Item::System(s) => s.members().collect(),
        _ => return,
    };
    for member in members {
        let Some((name_tok, kind, ns)) = member_identity(&member) else {
            continue;
        };
        let member_text = name_tok.text();
        let name = interner.intern(&member_text);
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
        if let Err(_existing) = table.define(name, ns, symbol) {
            errors.push(
                ResolveErrorKind::AmbiguousModule
                    .to_diagnostic(Some(name_tok.text_range()), &member_text),
            );
        }
    }
}

/// The name token, symbol kind, and namespace of a component/system member, or `None`
/// for a member that mints no module symbol (the `view` block itself).
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
        Member::View(_) => return None,
    };
    Some(triple)
}

/// The name token, symbol kind, and namespace of a top-level declaration, or `None`
/// for a form that mints no module symbol (imports are not items; Advanced items
/// carry no typed identity yet).
fn decl_identity(item: &Item) -> Option<(crate::syntax::SyntaxToken, SymbolKind, Namespace)> {
    let triple = match item {
        Item::Component(d) => (d.name()?, SymbolKind::Component, Namespace::Type),
        Item::System(d) => (d.name()?, SymbolKind::System, Namespace::Type),
        Item::Record(d) => (d.name()?, SymbolKind::Record, Namespace::Type),
        Item::Enum(d) => (d.name()?, SymbolKind::Enum, Namespace::Type),
        Item::TypeAlias(d) => (d.name()?, SymbolKind::TypeAlias, Namespace::Type),
        Item::Const(d) => (d.name()?, SymbolKind::Const, Namespace::Value),
        Item::Fn(d) => (d.name()?, SymbolKind::Function, Namespace::Value),
        Item::Action(d) => (d.name()?, SymbolKind::Action, Namespace::Value),
        Item::Task(d) => (d.name()?, SymbolKind::Task, Namespace::Value),
        Item::Export(_) | Item::Advanced(_) => return None,
    };
    Some(triple)
}

/// Builds a module's import environment: local name to the exported symbol it names.
fn build_import_env(
    cu: Option<&CompilationUnit>,
    graph: &ModuleGraph,
    tables: &[SymbolTable],
    interner: &mut NameInterner,
) -> std::collections::HashMap<NameId, ImportBinding> {
    use std::collections::HashMap;
    let mut env: HashMap<NameId, ImportBinding> = HashMap::new();
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
            if let Some((symbol, ns)) = lookup_exported(target_table, interner, &orig) {
                let local = interner.intern(&local_text);
                env.insert(
                    local,
                    ImportBinding {
                        symbol,
                        namespace: ns,
                    },
                );
            }
        }
    }
    env
}

/// Looks an exported name up in a module's table across every namespace.
fn lookup_exported(
    table: &SymbolTable,
    interner: &mut NameInterner,
    name_text: &str,
) -> Option<(SymbolId, Namespace)> {
    let name = interner.intern(name_text);
    for ns in [Namespace::Type, Namespace::Value, Namespace::Event] {
        if let Some(sym) = table.get(name, ns)
            && sym.exported
        {
            return Some((sym.id, ns));
        }
    }
    None
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

/// The per-module resolution walk state.
struct ModulePass<'a> {
    table: &'a SymbolTable,
    imports: &'a std::collections::HashMap<NameId, ImportBinding>,
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
}

impl ModulePass<'_> {
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
            match decl {
                Item::Component(c) => self.resolve_component(&c),
                Item::System(s) => self.resolve_system(&s),
                _ => {}
            }
        }
    }

    fn resolve_component(&mut self, decl: &ComponentDecl) {
        self.scopes.push();
        for member in decl.members() {
            self.resolve_member(member);
        }
        self.scopes.pop();
    }

    fn resolve_system(&mut self, decl: &SystemDecl) {
        self.scopes.push();
        for member in decl.members() {
            self.resolve_member(member);
        }
        self.scopes.pop();
    }

    fn resolve_member(&mut self, member: Member) {
        match member {
            Member::View(v) => {
                if let Some(block) = v.block() {
                    self.resolve_view_block(&block);
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
            Member::Fn(f) => self.resolve_callable(f.params(), f.body()),
            Member::Action(a) => self.resolve_callable(a.params(), a.body()),
            Member::Task(t) => self.resolve_callable(t.params(), t.body()),
            Member::Event(_) => {}
        }
    }

    fn resolve_callable(&mut self, params: Vec<crate::ast::Param>, body: Option<Block>) {
        self.scopes.push();
        for p in params {
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
        // Statement-level `let`/assignment binding is Slice-M territory for full
        // flow; here we resolve every value path the block contains against the
        // current scope so value references still resolve. A single descendant walk
        // visits each path head exactly once.
        use crate::syntax::SyntaxKind;
        for node in block.syntax().descendants() {
            if node.kind() == SyntaxKind::PathExpr
                && let Some(path) = PathExpr::cast(node)
            {
                self.resolve_value_path(&path);
            }
        }
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
                    self.resolve_type_path(&ty);
                }
                if let Some(body) = a.body() {
                    self.resolve_node_body(&body);
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
                for block in m
                    .syntax()
                    .descendants()
                    .into_iter()
                    .filter_map(ViewBlock::cast)
                {
                    self.resolve_view_block(&block);
                }
            }
            ViewItem::TwoWayBinding(_) | ViewItem::Fill(_) => {}
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
            self.resolve_type_path(&ty);
        }
        if let Some(body) = node.body() {
            self.resolve_node_body(&body);
        }
    }

    fn resolve_node_body(&mut self, body: &NodeBody) {
        for member in body.members() {
            self.resolve_view_item(member);
        }
    }

    fn resolve_property(&mut self, binding: &PropertyBinding) {
        if let Some(value) = binding.value() {
            self.resolve_expr(&value);
        }
    }

    fn resolve_handler(&mut self, handler: &EventHandler) {
        if let Some(evt) = handler.event() {
            let name = self.interner.intern(&evt.text());
            if let Some(sym) = self.table.get(name, Namespace::Event) {
                self.refs.push(ResolvedRef {
                    range: evt.text_range(),
                    to: Resolution::Symbol(sym.id),
                });
            }
            // An unknown event is left unresolved rather than an error here: a
            // handler may bind a native/schema event the resolver has no view of.
        }
        self.scopes.push();
        if let Some(body) = handler.body() {
            self.resolve_block(&body);
        }
        self.scopes.pop();
    }

    fn resolve_for(&mut self, for_item: &ViewFor) {
        use crate::syntax::SyntaxKind;
        // The iterable is evaluated in the outer scope — it cannot see the loop
        // pattern it is about to bind, so resolve it before pushing the loop scope.
        if let Some(iterable) = for_item.iterable() {
            self.resolve_expr(&iterable);
        }
        self.scopes.push();
        // Bind the loop pattern's identifiers (the pattern precedes `in`).
        if let Some(pat) = for_item
            .syntax()
            .children()
            .into_iter()
            .find(|n| n.kind() == SyntaxKind::Pattern)
        {
            for tok in pat.descendants_with_tokens().into_iter().filter_map(|e| {
                e.as_token()
                    .filter(|t| matches!(t.kind(), SyntaxKind::Ident | SyntaxKind::RawIdent))
                    .cloned()
            }) {
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
        use crate::syntax::SyntaxKind;
        // Resolve the head of every path expression anywhere within `expr`.
        // `descendants()` already yields `expr` itself first, so no separate prefix.
        for node in expr.syntax().descendants() {
            if node.kind() == SyntaxKind::PathExpr
                && let Some(path) = PathExpr::cast(node)
            {
                self.resolve_value_path(&path);
            }
        }
    }

    /// Resolves the head segment of a value/property path: local scope first, then
    /// the module value namespace, then imports.
    fn resolve_value_path(&mut self, path: &PathExpr) {
        let Some(head) = path.segments().next() else {
            return;
        };
        use crate::syntax::SyntaxKind;
        // `self`/`Self` heads are not module symbols; leave them unresolved.
        if matches!(
            head.kind(),
            SyntaxKind::SelfValueKw | SyntaxKind::SelfTypeKw
        ) {
            return;
        }
        let text = head.text();
        let name = self.interner.intern(&text);
        if let Some(slot) = self.scopes.lookup(name) {
            self.refs.push(ResolvedRef {
                range: head.text_range(),
                to: Resolution::Local(slot),
            });
            return;
        }
        if let Some(sym) = self.table.get(name, Namespace::Value) {
            self.refs.push(ResolvedRef {
                range: head.text_range(),
                to: Resolution::Symbol(sym.id),
            });
            return;
        }
        if let Some(binding) = self.imports.get(&name) {
            self.refs.push(ResolvedRef {
                range: head.text_range(),
                to: Resolution::Symbol(binding.symbol),
            });
        }
        // Otherwise: possibly a native/schema name; not diagnosed at this layer.
    }

    /// Resolves the head segment of a type path against the type namespace, then
    /// imports; an unresolved user type is [`E2001`].
    fn resolve_type_path(&mut self, ty: &TypePath) {
        let Some(head) = ty.segments().next() else {
            return;
        };
        let text = head.text();
        let name = self.interner.intern(&text);
        if let Some(sym) = self.table.get(name, Namespace::Type) {
            self.refs.push(ResolvedRef {
                range: head.text_range(),
                to: Resolution::Symbol(sym.id),
            });
            return;
        }
        if let Some(binding) = self.imports.get(&name)
            && binding.namespace == Namespace::Type
        {
            self.refs.push(ResolvedRef {
                range: head.text_range(),
                to: Resolution::Symbol(binding.symbol),
            });
            return;
        }
        // Built-in/native types (Int, Text, Color, ...) are provided by schema, not
        // by a user declaration, so only a name that looks user-defined is flagged.
        // In a fragment there is no compilation unit to declare it and no import, so
        // the name is a native/schema widget type — deferred, never diagnosed here.
        if is_user_type_name(&text) && !self.defer_unresolved_types {
            self.errors.push(
                ResolveErrorKind::UnresolvedModule.to_diagnostic(Some(head.text_range()), &text),
            );
        }
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

    let imports = std::collections::HashMap::new();
    let mut pass = ModulePass {
        table: &table,
        imports: &imports,
        interner,
        refs: Vec::new(),
        errors: Vec::new(),
        scopes: ScopeStack::new(),
        // A fragment's node types are native/schema-provided (no imports, no unit),
        // so an unresolved PascalCase name defers instead of raising E2001.
        defer_unresolved_types: true,
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
/// built-in schema names. A conservative approximation until the native schema lands
/// (Slice deferred): only PascalCase names outside a small built-in set are flagged
/// so a missing user type surfaces while built-ins stay quiet.
fn is_user_type_name(text: &str) -> bool {
    const BUILTINS: &[&str] = &[
        "Int", "Float", "Text", "Bool", "Color", "Unit", "Vec2", "Vec3", "Vec4", "List", "Map",
        "Option", "Self",
    ];
    let starts_upper = text.chars().next().is_some_and(|c| c.is_uppercase());
    starts_upper && !BUILTINS.contains(&text)
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
            .find_map(|m| m.table.get(count, Namespace::Value))
            .expect("count is a value symbol")
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
