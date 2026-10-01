//! The three entry points' expansions over the shared frontend.
//!
//! Each form hands its source to one `viso_dsl::frontend` function, reports every
//! error diagnostic at the place the author wrote it, and mounts the result:
//!
//! - `ui!` expands to a builder closure over `StateId`s the surrounding Rust scope
//!   supplies by name;
//! - `component!` expands to a struct of the component's `StateId`s and a `build`
//!   that allocates them and mounts the view;
//! - `view!` expands to a builder closure that allocates the file's component's
//!   states and mounts its view, and makes the `.vs` file a compile dependency.
//!
//! A component's states mount from constant initializers; a state no UI cell
//! holds (a string, a list) mounts as the revision cell its behavior tracks it
//! through, when the view has behavior to run against it. A view that reads an
//! `input` or `computed` is reported rather than mounted wrong.

use std::collections::{BTreeSet, HashMap};
use std::fmt::Display;
use std::path::{Path, PathBuf};

use proc_macro2::{Literal, Span, TokenStream};
use quote::{format_ident, quote};
use syn::{Ident, LitStr};
use viso_dsl::frontend::{self, Compiled, Origin, Source, SourceKind};
use viso_dsl::hir::{ConstValue, HirComponent};
use viso_dsl::resolve::SymbolId;
use viso_dsl::syntax::{LineIndex, TextRange};
use viso_dsl::view_behavior::{MountError, UNMOUNTED_HANDLER, ViewBehavior, view_behavior};

use crate::emit::emit_view;
use crate::package;
use crate::source_text::SourceText;

pub fn ui(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let source = SourceText::new(input);
    let report = Report::Inline(&source);
    let expansion = (|| {
        let compiled = frontend::compile_fragment(&source.text);
        report.check(&compiled)?;
        let idents = compiled
            .sources
            .iter()
            .filter_map(|source| Some((source.symbol, rust_ident(&source.name).ok()?)))
            .collect();
        view_behavior(&compiled).map_err(|errors| report.mount_errors(errors))?;
        let root = emit_view(&compiled.tree, &compiled.bindings, &idents, None, None)
            .map_err(|message| report.at(None, message))?;
        Ok(quote! {
            |cx: &mut ::viso_ui::BuildCx<'_>| -> ::viso_ui::Handle { #root }
        })
    })();
    finish(expansion, true)
}

pub fn component(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let source = SourceText::new(input);
    let report = Report::Inline(&source);
    let expansion = (|| {
        let root_dir = package::root()?;
        let origin = package::origin(&root_dir, package::invoking_file().as_deref())?;
        let compiled = frontend::compile_component(&source.text, &origin);
        report.check(&compiled)?;
        let Mounted {
            component,
            states,
            allocations,
            root,
        } = mount(&compiled, &report, None)?;
        let name = rust_ident(&component.schema.name)
            .map_err(|message| report.at(Some(component.source_origin), message))?;
        let doc = format!("The `{name}` component's state ids.");
        let fields = states
            .iter()
            .map(|(field, _)| quote! { pub #field: ::viso_ui::StateId });
        let values = states
            .iter()
            .map(|(field, local)| quote! { #field: #local });
        Ok(quote! {
            #[doc = #doc]
            #[derive(Debug, Clone, Copy, PartialEq, Eq)]
            pub struct #name {
                #(#fields,)*
            }

            impl #name {
                /// Allocates the component's states and mounts its view, returning the
                /// state ids and the root handle.
                pub fn build(cx: &mut ::viso_ui::BuildCx<'_>) -> (Self, ::viso_ui::Handle) {
                    #allocations
                    let __viso_root = #root;
                    (Self { #(#values,)* }, __viso_root)
                }
            }
        })
    })();
    finish(expansion, false)
}

pub fn view(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let expansion = (|| {
        let literal: LitStr = syn::parse(input)?;
        let span = literal.span();
        let root_dir = package::root()?;
        let base = package::invoking_file()
            .and_then(|file| file.parent().map(Path::to_path_buf))
            .unwrap_or_else(|| root_dir.clone());
        let path = base.join(literal.value());
        let text = std::fs::read_to_string(&path).map_err(|error| {
            syn::Error::new(span, format!("cannot read `{}`: {error}", path.display()))
        })?;
        let path = canonical(&path);
        let dependency = path
            .to_str()
            .ok_or_else(|| syn::Error::new(span, "the `.vs` path is not UTF-8"))?;
        let origin = package::origin(&canonical(&root_dir), Some(&path))?;
        let compiled = frontend::compile_file(&text, &origin);
        let report = Report::File {
            path: &path,
            index: LineIndex::new(&text),
            span,
        };
        report.check(&compiled)?;
        let Mounted {
            allocations, root, ..
        } = mount(&compiled, &report, Some((dependency, &origin)))?;
        Ok(quote! {
            {
                const _: &str = ::core::include_str!(#dependency);
                |cx: &mut ::viso_ui::BuildCx<'_>| -> ::viso_ui::Handle {
                    #allocations
                    #root
                }
            }
        })
    })();
    finish(expansion, true)
}

/// Where a form's diagnostics are reported.
enum Report<'a> {
    /// At the macro body token a diagnostic points at.
    Inline(&'a SourceText),
    /// At the `view!` path literal, prefixed with the `.vs` file's line and column.
    File {
        path: &'a Path,
        index: LineIndex,
        span: Span,
    },
}

impl Report<'_> {
    fn at(&self, range: Option<TextRange>, message: impl Display) -> syn::Error {
        match self {
            Report::Inline(source) => {
                let span =
                    range.map_or_else(Span::call_site, |range| source.span_at(range.start()));
                syn::Error::new(span, message)
            }
            Report::File { path, index, span } => {
                let location = match range {
                    Some(range) => {
                        let at = index.line_col_scalar(range.start());
                        format!("{}:{}:{}", path.display(), at.line + 1, at.column + 1)
                    }
                    None => path.display().to_string(),
                };
                syn::Error::new(*span, format!("{location}: {message}"))
            }
        }
    }

    /// Every handler that does not mount, as one combined error.
    fn mount_errors(&self, errors: Vec<MountError>) -> syn::Error {
        let errors = errors
            .into_iter()
            .map(|error| self.at(error.at, format!("{UNMOUNTED_HANDLER}: {}", error.message)));
        combine(errors).expect_err("a failed mount reports at least one error")
    }

    /// Every error diagnostic of `compiled`, as one combined error.
    fn check(&self, compiled: &Compiled) -> syn::Result<()> {
        let errors = compiled.errors().map(|diagnostic| {
            let message = format!("{}: {}", diagnostic.code, diagnostic.message);
            self.at(Some(diagnostic.primary), message)
        });
        combine(errors)
    }
}

/// A component's view, mounted over states it allocates.
struct Mounted<'a> {
    component: &'a HirComponent,
    /// `(field, local)` per state, in declaration order: the state's Rust name and
    /// the local its `StateId` is allocated into.
    states: Vec<(Ident, Ident)>,
    allocations: TokenStream,
    root: TokenStream,
}

/// `record` is the `.vs` file a `view!` mounts and the module identity it
/// compiles under, which the mount records for the development session; `None`
/// for a form that records no mount.
fn mount<'a>(
    compiled: &'a Compiled,
    report: &Report<'_>,
    record: Option<(&str, &Origin)>,
) -> syn::Result<Mounted<'a>> {
    let Some(component) = &compiled.component else {
        return Err(report.at(None, "the frontend produced no component to mount"));
    };
    let behavior = view_behavior(compiled).map_err(|errors| report.mount_errors(errors))?;
    let mut errors = Vec::new();
    let mut idents = HashMap::new();
    let mut tracked = BTreeSet::new();
    let mut states = Vec::new();
    let mut allocations = TokenStream::new();
    for source in &compiled.sources {
        let SourceKind::State { initial, .. } = &source.kind else {
            continue;
        };
        let declared = declared_at(component, source);
        let Some(value) = initial.as_ref().and_then(state_value) else {
            if let Some(behavior) = &behavior
                && behavior.slot(source.symbol).is_some()
            {
                let local = format_ident!("__viso_t{}", tracked.len());
                allocations
                    .extend(quote! { let #local = cx.state(::viso_ui::StateValue::Int(0)); });
                idents.insert(source.symbol, local);
                tracked.insert(source.symbol);
                continue;
            }
            errors.push(report.at(
                declared,
                format!(
                    "state `{}` needs a constant `bool`, `i32` or `f32` initializer to mount",
                    source.name
                ),
            ));
            continue;
        };
        let field = match rust_ident(&source.name) {
            Ok(field) => field,
            Err(message) => {
                errors.push(report.at(declared, message));
                continue;
            }
        };
        let local = format_ident!("__viso_s{}", states.len());
        allocations.extend(quote! { let #local = cx.state(#value); });
        idents.insert(source.symbol, local.clone());
        states.push((field, local));
    }
    let mut unmounted = BTreeSet::new();
    for edge in compiled.bindings.static_edges() {
        let Some(source) = compiled.source(edge.source) else {
            continue;
        };
        // A binding through a computed or a function names the states beneath it,
        // so only a root input, which no caller supplies here, is left unmounted.
        if source.kind == SourceKind::Input && unmounted.insert(source.symbol) {
            errors.push(report.at(
                declared_at(component, source),
                format!(
                    "the view reads input `{}`; a mounted view reads only states",
                    source.name
                ),
            ));
        }
    }
    combine(errors)?;
    if let Some(behavior) = &behavior {
        allocations.extend(host_tokens(behavior, &idents, &tracked));
    }
    let record =
        record.map(|(file, origin)| record_tokens(file, origin, &idents, behavior.is_some()));
    let root = emit_view(
        &compiled.tree,
        &compiled.bindings,
        &idents,
        behavior.as_ref(),
        record,
    )
    .map_err(|message| report.at(component.schema.view, message))?;
    Ok(Mounted {
        component,
        states,
        allocations,
        root,
    })
}

/// The `__viso_host` a view with behavior dispatches into: the component's
/// module, embedded as bytes and decoded once per thread, mirrored into the
/// states the expansion allocated, and tracking the `tracked` ones through
/// their revision cells.
fn host_tokens(
    behavior: &ViewBehavior,
    idents: &HashMap<SymbolId, Ident>,
    tracked: &BTreeSet<SymbolId>,
) -> TokenStream {
    let bytes = Literal::byte_string(&behavior.bytes);
    let component = &behavior.component;
    let mirrors = behavior.slots.iter().filter_map(|(symbol, slot)| {
        let local = idents.get(symbol)?;
        let slot = *slot as usize;
        Some(if tracked.contains(symbol) {
            quote! { __viso_view.track(#slot, #local); }
        } else {
            quote! { __viso_view.mirror(#slot, #local); }
        })
    });
    quote! {
        let __viso_host = ::viso_view::__embedded(#bytes, #component);
        {
            let mut __viso_view = __viso_host.borrow_mut();
            #(#mirrors)*
        }
    }
}

/// The fields of a `view!` mount record the expansion knows before the tree is
/// built: the file and its source, the module identity, every state cell by its
/// durable key, and the behavior host.
fn record_tokens(
    file: &str,
    origin: &Origin,
    idents: &HashMap<SymbolId, Ident>,
    behavior: bool,
) -> TokenStream {
    let package = &origin.package;
    let module = &origin.module;
    let language = match &origin.language {
        Some(language) => quote! { ::core::option::Option::Some(#language) },
        None => quote! { ::core::option::Option::None },
    };
    let mut cells: Vec<(&SymbolId, &Ident)> = idents.iter().collect();
    cells.sort_unstable_by_key(|(symbol, _)| (symbol.hi, symbol.lo));
    let cells = cells.into_iter().map(|(symbol, local)| {
        let (hi, lo) = (symbol.hi, symbol.lo);
        quote! { (::viso_ui::state::StateKey::from_parts(#hi, #lo), #local) }
    });
    let host = if behavior {
        quote! { ::core::option::Option::Some(::std::rc::Rc::clone(&__viso_host)) }
    } else {
        quote! { ::core::option::Option::None }
    };
    quote! {
        file: #file,
        source: ::core::include_str!(#file),
        package: #package,
        module: [#(#module),*],
        language: #language,
        cells: [#(#cells),*],
        host: #host,
    }
}

/// The declaration span of one of the component's sources.
fn declared_at(component: &HirComponent, source: &Source) -> Option<TextRange> {
    let schema = &component.schema;
    let symbol = Some(source.symbol);
    let states = schema.states.iter().map(|state| &state.meta);
    let inputs = schema.inputs.iter().map(|input| &input.meta);
    let computeds = schema.computeds.iter().map(|computed| &computed.meta);
    states
        .chain(inputs)
        .chain(computeds)
        .find(|meta| meta.resolved_symbol == symbol)
        .map(|meta| meta.source_origin)
}

/// A state initializer as the `StateValue` the store starts from.
fn state_value(initial: &ConstValue) -> Option<TokenStream> {
    Some(match initial {
        ConstValue::Bool(value) => quote! { ::viso_ui::StateValue::Bool(#value) },
        ConstValue::Int(value, _) => {
            let value = i32::try_from(*value).ok()?;
            quote! { ::viso_ui::StateValue::Int(#value) }
        }
        ConstValue::Float(value, _) => {
            let value = *value as f32;
            if !value.is_finite() {
                return None;
            }
            quote! { ::viso_ui::StateValue::Float(#value) }
        }
        ConstValue::Str(_) => return None,
    })
}

/// A DSL name as a Rust identifier, raw when it is a Rust keyword.
fn rust_ident(name: &str) -> Result<Ident, String> {
    syn::parse_str::<Ident>(name)
        .or_else(|_| syn::parse_str::<Ident>(&format!("r#{name}")))
        .map_err(|_| format!("`{name}` is not usable as a Rust identifier"))
}

fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn combine(errors: impl IntoIterator<Item = syn::Error>) -> syn::Result<()> {
    let mut errors = errors.into_iter();
    let Some(mut first) = errors.next() else {
        return Ok(());
    };
    for error in errors {
        first.combine(error);
    }
    Err(first)
}

/// The expansion, or its errors. An expression-position form wraps several
/// `compile_error!`s in a block so the expansion stays one expression.
fn finish(expansion: syn::Result<TokenStream>, expression: bool) -> proc_macro::TokenStream {
    match expansion {
        Ok(tokens) => tokens.into(),
        Err(error) => {
            let errors = error.into_compile_error();
            if expression {
                quote! { { #errors } }.into()
            } else {
                errors.into()
            }
        }
    }
}
