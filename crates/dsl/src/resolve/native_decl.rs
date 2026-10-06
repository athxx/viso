//! Handwritten `native` declarations (§47): signatures a `.vs` module states
//! for natives a Rust library implements, as interface stubs and in tests.
//!
//! A declaration names the native at its module's path: `native fn hash(..)`
//! in module `util` of package `app` is `app::util::hash`, and one in
//! `component Board` is `app::util::Board::hash`. Its types are schema types:
//! `Bool`, `I64`, `F32`, `F64`, `Duration`, `String`, `()`, `List<T>`,
//! `Option<T>`, a registered native type, or a `native type` declared beside
//! it.
//!
//! When the registry has the declaration's library, the declaration is
//! checked against it now: the path must be registered with the same kind and
//! signature, and a `requires` clause must name its capabilities (`E6101`).
//! Otherwise the declaration's own schema enters the registry the module
//! compiles against, so calls type, effect-check and lower to native imports
//! as any native's; the host links the module to the Rust library later, where
//! a missing path or another signature is `E6101` again.
//!
//! A declared schema is interned for the process: the registry holds
//! `'static` schemas, and each distinct declaration is allocated once however
//! often its module recompiles.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::{Mutex, OnceLock};

use viso_behavior::Value;
use viso_behavior::native::{
    NativeCx, NativeError, NativeFunction, NativeKind, NativeLibrary, NativeType, Natives, Param,
    SchemaTy,
};

use crate::ast::TypePath;
use crate::ast::{AstNode, Item, Member, NativeDecl, NativeDeclKind};
use crate::diag::Diagnostic;
use crate::syntax::{SyntaxKind, SyntaxNode, SyntaxToken, TextRange};

use super::module::{NativeBinding, ResolveErrorKind, SourceUnit};
use super::name::NameInterner;

/// The diagnostic code of a declaration that does not fit the schema.
const CODE: &str = "E6101";

/// What a package's native declarations add to its compilation.
pub(super) struct Declared {
    /// The registry with the declared libraries the given one lacks, when
    /// there are any.
    pub(super) natives: Option<Natives>,
    /// Each module's declared names, in module order.
    pub(super) bindings: Vec<Vec<NativeBinding>>,
    /// The problems found, each with the index of the module it points into.
    pub(super) errors: Vec<(usize, Diagnostic)>,
}

/// Checks the native declarations of `units` (in module order) against
/// `natives`; `imports` are each module's native import bindings, which a
/// declaration's types may name.
pub(super) fn declare(
    units: &[&SourceUnit],
    imports: &[Vec<NativeBinding>],
    interner: &NameInterner,
    natives: &Natives,
    package: &str,
) -> Declared {
    let mut declared = Declared {
        natives: None,
        bindings: Vec::with_capacity(units.len()),
        errors: Vec::new(),
    };
    for (index, unit) in units.iter().enumerate() {
        let module = unit.path.display(interner);
        let base = if package.is_empty() {
            module
        } else {
            format!("{package}::{module}")
        };
        let mut cx = ModuleCx {
            natives,
            imports: &imports[index],
            types: HashMap::new(),
            bindings: Vec::new(),
            errors: Vec::new(),
            stubs: Vec::new(),
        };
        if let Some(cu) = unit.compilation_unit() {
            let items: Vec<Item> = cu
                .items()
                .filter_map(|item| match item {
                    Item::Export(e) => e.declaration(),
                    other => Some(other),
                })
                .collect();
            let top: Vec<NativeDecl> = items
                .iter()
                .filter_map(|i| match i {
                    Item::Native(d) => Some(d.clone()),
                    _ => None,
                })
                .collect();
            let taken = item_names(&items);
            cx.group(&base, None, &top, &taken);
            for item in &items {
                let (name, members): (_, Vec<Member>) = match item {
                    Item::Component(c) => (c.name(), c.members().collect()),
                    Item::System(s) => (s.name(), s.members().collect()),
                    _ => continue,
                };
                let Some(name) = name else {
                    continue;
                };
                let decls: Vec<NativeDecl> = members
                    .iter()
                    .filter_map(|m| match m {
                        Member::Native(d) => Some(d.clone()),
                        _ => None,
                    })
                    .collect();
                let taken = member_names(&members);
                let owner = name.text().to_string();
                cx.group(&format!("{base}::{owner}"), Some(owner), &decls, &taken);
            }
        }
        for (library, at) in cx.stubs {
            let registry = declared.natives.get_or_insert_with(|| natives.clone());
            if let Err(conflict) = registry.register(library) {
                declared
                    .errors
                    .push((index, Diagnostic::error(CODE, at, conflict.message)));
            }
        }
        declared
            .errors
            .extend(cx.errors.into_iter().map(|e| (index, e)));
        declared.bindings.push(cx.bindings);
    }
    declared
}

/// The value names a module's top-level declarations take.
fn item_names(items: &[Item]) -> HashSet<String> {
    items
        .iter()
        .filter_map(|item| match item {
            Item::Fn(d) => d.name(),
            Item::Action(d) => d.name(),
            Item::Task(d) => d.name(),
            Item::Const(d) => d.name(),
            _ => None,
        })
        .map(|t| t.text().to_string())
        .collect()
}

/// The value names a component's or system's members take.
fn member_names(members: &[Member]) -> HashSet<String> {
    members
        .iter()
        .filter_map(|member| match member {
            Member::Input(d) => d.name(),
            Member::State(d) => d.name(),
            Member::Computed(d) => d.name(),
            Member::Fn(d) => d.name(),
            Member::Action(d) => d.name(),
            Member::Task(d) => d.name(),
            Member::Resource(d) => d.name(),
            _ => None,
        })
        .map(|t| t.text().to_string())
        .collect()
}

/// One module's declarations being checked.
struct ModuleCx<'a> {
    natives: &'a Natives,
    imports: &'a [NativeBinding],
    /// The schema type each declared `native type` name stands for, the
    /// module's own and then each owner's, by `(owner, name)`.
    types: HashMap<(Option<String>, String), SchemaTy>,
    bindings: Vec<NativeBinding>,
    errors: Vec<Diagnostic>,
    /// The declared libraries the registry lacks, each with its first
    /// declaration's span.
    stubs: Vec<(&'static NativeLibrary, TextRange)>,
}

impl ModuleCx<'_> {
    fn error(&mut self, at: &SyntaxNode, message: impl Into<String>) {
        self.errors
            .push(Diagnostic::error(CODE, at.text_range(), message));
    }

    /// Checks the declarations `decls` of the library at `library`, declared
    /// in `owner` (none at the top level) beside the value names `taken`.
    fn group(
        &mut self,
        library: &str,
        owner: Option<String>,
        decls: &[NativeDecl],
        taken: &HashSet<String>,
    ) {
        let registered = self.natives.library(library);
        let mut seen: HashSet<String> = HashSet::new();
        let mut functions: Vec<NativeFunction> = Vec::new();
        let mut types: Vec<NativeType> = Vec::new();
        // Types first, so the functions beside them may name them.
        let (type_decls, fn_decls): (Vec<&NativeDecl>, Vec<&NativeDecl>) = decls
            .iter()
            .partition(|d| d.kind() == Some(NativeDeclKind::Type));
        for decl in type_decls.into_iter().chain(fn_decls) {
            let (Some(kind), Some(name)) = (decl.kind(), decl.name()) else {
                continue;
            };
            let text = name.text().to_string();
            let at = name.text_range();
            if !seen.insert(text.clone()) || taken.contains(&text) {
                self.errors
                    .push(ResolveErrorKind::DuplicateName.to_diagnostic(Some(at), &text));
                continue;
            }
            let path = format!("{library}::{text}");
            let syntax = decl.syntax();
            if let Some(generics) = decl.generic_params() {
                self.error(
                    &generics,
                    "a native schema is not generic: declare one native per type",
                );
                continue;
            }
            if let Some(clause) = child(syntax, SyntaxKind::WhereClause) {
                self.error(&clause, "a native declaration takes no `where` clause");
                continue;
            }
            if kind == NativeDeclKind::Type {
                if let Some(bound) = child(syntax, SyntaxKind::TypePath) {
                    self.error(
                        &bound,
                        "a native type's traits are its schema's: declare it without bounds",
                    );
                    continue;
                }
                let ty = match self.natives.ty(&path) {
                    Some(entry) => schema_of_type(entry.ty, &entry.path),
                    None if registered.is_some() => {
                        self.missing(&name, library, &text, "type");
                        continue;
                    }
                    None => {
                        types.push(NativeType::new(intern_str(&text), &[]));
                        SchemaTy::Handle(intern_str(&path))
                    }
                };
                self.types.insert((owner.clone(), text.clone()), ty);
                self.bind(&owner, text, path);
                continue;
            }
            let Some(function) = self.function(decl, kind, &text, &owner) else {
                continue;
            };
            match self.natives.function(&path) {
                Some(entry) => {
                    if entry.function.kind != function.kind
                        || entry.function.signature() != function.signature()
                    {
                        let mut diagnostic = Diagnostic::error(
                            CODE,
                            at,
                            format!(
                                "`{path}` is registered with another signature: `{:?}`",
                                entry.function
                            ),
                        );
                        diagnostic
                            .notes
                            .push(format!("declared here as `{function:?}`"));
                        self.errors.push(diagnostic);
                        continue;
                    }
                    if decl.capability_clause().is_some() {
                        let declared: BTreeSet<&str> =
                            function.capabilities.iter().copied().collect();
                        let schema: BTreeSet<&str> =
                            entry.function.capabilities.iter().copied().collect();
                        if declared != schema {
                            let names: Vec<String> =
                                schema.iter().map(|c| format!("`{c}`")).collect();
                            self.errors.push(Diagnostic::error(
                                CODE,
                                at,
                                format!(
                                    "`{path}` requires {} in its schema",
                                    if names.is_empty() {
                                        "no capability".to_owned()
                                    } else {
                                        names.join(", ")
                                    }
                                ),
                            ));
                            continue;
                        }
                    }
                }
                None if registered.is_some() => {
                    self.missing(&name, library, &text, "function");
                    continue;
                }
                None => functions.push(function),
            }
            self.bind(&owner, text, path);
        }
        if (!functions.is_empty() || !types.is_empty())
            && let Some(first) = decls.first()
        {
            let at = first.syntax().text_range();
            self.stubs
                .push((intern_library(library, functions, types), at));
        }
    }

    fn bind(&mut self, owner: &Option<String>, local: String, path: String) {
        self.bindings.push(NativeBinding {
            local,
            path,
            owner: owner.clone(),
        });
    }

    fn missing(&mut self, name: &SyntaxToken, library: &str, text: &str, what: &str) {
        self.errors.push(Diagnostic::error(
            CODE,
            name.text_range(),
            format!("the registered library `{library}` declares no {what} `{text}`"),
        ));
    }

    /// The schema a callable declaration states, or `None` after reporting
    /// what does not fit.
    fn function(
        &mut self,
        decl: &NativeDecl,
        kind: NativeDeclKind,
        name: &str,
        owner: &Option<String>,
    ) -> Option<NativeFunction> {
        let mut params = Vec::new();
        let mut ok = true;
        for param in decl.params() {
            let syntax = param.syntax();
            if syntax
                .children_with_tokens()
                .into_iter()
                .any(|e| e.kind() == SyntaxKind::Eq)
            {
                self.error(syntax, "a native parameter has no default");
                ok = false;
                continue;
            }
            let Some(param_name) = param.name() else {
                ok = false;
                continue;
            };
            match type_node(syntax).map(|ty| self.schema_ty(&ty, owner)) {
                Some(Some(ty)) => params.push(Param {
                    name: intern_str(&param_name.text()),
                    ty,
                }),
                Some(None) => ok = false,
                None => {
                    self.error(syntax, "a native parameter is declared with its type");
                    ok = false;
                }
            }
        }
        let ret = match decl.return_type().and_then(|r| type_node(r.syntax())) {
            Some(ty) => self.schema_ty(&ty, owner),
            None => Some(SchemaTy::Unit),
        };
        let (Some(ret), true) = (ret, ok) else {
            return None;
        };
        let capabilities: BTreeSet<String> = decl
            .capability_clause()
            .map(|clause| {
                clause
                    .capabilities()
                    .map(|path| {
                        path.segments()
                            .map(|t| t.text().to_string())
                            .collect::<Vec<_>>()
                            .join(".")
                    })
                    .filter(|c| !c.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        let kind = match kind {
            NativeDeclKind::Fn | NativeDeclKind::Type => NativeKind::Fn,
            NativeDeclKind::Action => NativeKind::Action,
            NativeDeclKind::Task => NativeKind::Task,
        };
        let capabilities: Vec<&'static str> = capabilities.iter().map(|c| intern_str(c)).collect();
        Some(
            NativeFunction::new(intern_str(name), kind, intern_params(params), ret, unlinked)
                .requires(intern_capabilities(capabilities)),
        )
    }

    /// The schema type the annotation `node` names in `owner`, or `None` after
    /// reporting one that is no schema type.
    fn schema_ty(&mut self, node: &SyntaxNode, owner: &Option<String>) -> Option<SchemaTy> {
        let found = match node.kind() {
            SyntaxKind::TupleType if node.children().is_empty() => Some(SchemaTy::Unit),
            SyntaxKind::TypePath => self.path_ty(node, owner),
            _ => None,
        };
        if found.is_none() {
            self.error(
                node,
                format!(
                    "`{}` is no native schema type: use `Bool`, `I64`, `F32`, `F64`, \
                     `Duration`, `String`, `()`, `List<T>`, `Option<T>` or a native type",
                    node.text().to_string().trim()
                ),
            );
        }
        found
    }

    fn path_ty(&mut self, node: &SyntaxNode, owner: &Option<String>) -> Option<SchemaTy> {
        let segments: Vec<SyntaxNode> = node
            .children()
            .into_iter()
            .filter(|n| n.kind() == SyntaxKind::TypePathSegment)
            .collect();
        let names: Vec<String> = TypePath::cast(node.clone())
            .map(|p| p.segments().map(|t| t.text().to_string()).collect())
            .unwrap_or_default();
        let args: Vec<SyntaxNode> = segments
            .last()
            .and_then(|s| child(s, SyntaxKind::GenericArgs))
            .map(|g| {
                g.children()
                    .into_iter()
                    .filter(|n| matches!(n.kind(), SyntaxKind::TypePath | SyntaxKind::TupleType))
                    .collect()
            })
            .unwrap_or_default();
        let [name] = &names[..] else {
            return self.native_ty(&names.join("::"));
        };
        let wrapped = |cx: &mut Self, wrap: fn(&'static SchemaTy) -> SchemaTy| match &args[..] {
            [arg] => cx.schema_ty(arg, owner).map(|t| wrap(intern_ty(t))),
            _ => None,
        };
        let simple = |ty: SchemaTy| args.is_empty().then_some(ty);
        match name.as_str() {
            "Bool" => simple(SchemaTy::Bool),
            "I64" => simple(SchemaTy::I64),
            "F32" => simple(SchemaTy::F32),
            "F64" => simple(SchemaTy::F64),
            "Duration" => simple(SchemaTy::Duration),
            "String" => simple(SchemaTy::String),
            "Unit" => simple(SchemaTy::Unit),
            "List" => wrapped(self, SchemaTy::List),
            "Option" => wrapped(self, SchemaTy::Option),
            _ if !args.is_empty() => None,
            _ => {
                let local = |o: Option<String>| self.types.get(&(o, name.clone())).copied();
                if let Some(ty) = local(owner.clone()).or_else(|| local(None)) {
                    return Some(ty);
                }
                let imported = self
                    .imports
                    .iter()
                    .find(|b| b.local == *name)
                    .map(|b| b.path.clone())?;
                self.native_ty(&imported)
            }
        }
    }

    /// The registered native type at `path`.
    fn native_ty(&self, path: &str) -> Option<SchemaTy> {
        let entry = self.natives.ty(path)?;
        Some(schema_of_type(entry.ty, &entry.path))
    }
}

/// The schema type a value of the native type `ty` at `path` has.
fn schema_of_type(ty: &NativeType, path: &str) -> SchemaTy {
    let path = intern_str(path);
    if ty.is_enum() {
        SchemaTy::Enum(path)
    } else if ty.value {
        SchemaTy::Value(path)
    } else {
        SchemaTy::Handle(path)
    }
}

/// The first child node of `node` of `kind`.
fn child(node: &SyntaxNode, kind: SyntaxKind) -> Option<SyntaxNode> {
    node.children().into_iter().find(|n| n.kind() == kind)
}

/// The type annotation among the children of `node`.
fn type_node(node: &SyntaxNode) -> Option<SyntaxNode> {
    node.children()
        .into_iter()
        .find(|n| matches!(n.kind(), SyntaxKind::TypePath | SyntaxKind::TupleType))
}

/// A declared native's implementation until the host links the Rust one; a
/// linked module never calls it.
fn unlinked(_: &mut NativeCx<'_>, _: &[Value]) -> Result<Value, NativeError> {
    Err(NativeError::new(
        "a declared native runs only once its library is linked",
    ))
}

/// The process's interned declaration schemas.
#[derive(Default)]
struct Interned {
    strs: HashSet<&'static str>,
    tys: HashMap<SchemaTy, &'static SchemaTy>,
    params: HashMap<Vec<Param>, &'static [Param]>,
    capabilities: HashMap<Vec<&'static str>, &'static [&'static str]>,
    libraries: HashMap<String, &'static NativeLibrary>,
}

fn interned<T>(f: impl FnOnce(&mut Interned) -> T) -> T {
    static INTERNED: OnceLock<Mutex<Interned>> = OnceLock::new();
    let mut guard = INTERNED
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    f(&mut guard)
}

fn intern_str(s: &str) -> &'static str {
    interned(|i| match i.strs.get(s) {
        Some(s) => s,
        None => {
            let s: &'static str = Box::leak(s.into());
            i.strs.insert(s);
            s
        }
    })
}

fn intern_ty(ty: SchemaTy) -> &'static SchemaTy {
    interned(|i| *i.tys.entry(ty).or_insert_with(|| Box::leak(Box::new(ty))))
}

fn intern_params(params: Vec<Param>) -> &'static [Param] {
    interned(|i| match i.params.get(&params) {
        Some(p) => p,
        None => {
            let leaked: &'static [Param] = Box::leak(params.clone().into_boxed_slice());
            i.params.insert(params, leaked);
            leaked
        }
    })
}

fn intern_capabilities(capabilities: Vec<&'static str>) -> &'static [&'static str] {
    interned(|i| match i.capabilities.get(&capabilities) {
        Some(c) => c,
        None => {
            let leaked: &'static [&'static str] =
                Box::leak(capabilities.clone().into_boxed_slice());
            i.capabilities.insert(capabilities, leaked);
            leaked
        }
    })
}

/// The declared library at `path` of `functions` and `types`, the same one
/// for the same declarations.
fn intern_library(
    path: &str,
    functions: Vec<NativeFunction>,
    types: Vec<NativeType>,
) -> &'static NativeLibrary {
    use std::fmt::Write as _;
    let mut key = format!("{path}|");
    for f in &functions {
        let _ = write!(key, "{f:?} requires {:?};", f.capabilities);
    }
    for t in &types {
        let _ = write!(key, "type {};", t.name);
    }
    let path = intern_str(path);
    interned(|i| {
        *i.libraries.entry(key).or_insert_with(|| {
            Box::leak(Box::new(NativeLibrary {
                path,
                version: 0,
                functions: Box::leak(functions.into_boxed_slice()),
                types: Box::leak(types.into_boxed_slice()),
                traits: &[],
                derives: &[],
                widgets: &[],
            }))
        })
    })
}
