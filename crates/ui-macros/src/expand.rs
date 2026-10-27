//! The three entry points' expansions over the shared frontend.
//!
//! Each form hands its source to one `viso_dsl::frontend` function, reports every
//! error diagnostic at the place the author wrote it, and mounts the result:
//!
//! - `ui!` expands to a builder closure that mounts a stateless view, the Rust
//!   components it names mounting their own;
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
use std::sync::Arc;

use proc_macro2::{Literal, Span, TokenStream};
use quote::{format_ident, quote};
use std::io::Read as _;
use syn::{Ident, LitStr};
use viso_dsl::aot::{ShaderLang, pack_brush};
use viso_dsl::assets::{
    ASSET_ERROR, AssetCatalog, AssetFormat, AssetIssue, AssetKind, ResourceId, normalize_path,
};
use viso_dsl::frontend::{self, Compiled, Origin, Source, SourceKind};
use viso_dsl::hir::{ConstValue, HirComponent};
use viso_dsl::native_package::{self, NativePackage};
use viso_dsl::resolve::SymbolId;
use viso_dsl::schema::Natives;
use viso_dsl::syntax::{LineIndex, TextRange, TextSize};
use viso_dsl::view_behavior::{MountError, UNMOUNTED_HANDLER, ViewBehavior, view_behavior};

use crate::emit::{Named, aligned_bytes, aligned_file, emit_view, overlay_dependency};
use crate::package;
use crate::source_text::SourceText;

pub fn ui(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let source = SourceText::new(input);
    let expansion = (|| {
        let compiled = frontend::compile_fragment(&source.text);
        Report::Inline(&source, TextSize::ZERO).check(&compiled)?;
        let report = Report::Inline(&source, compiled.base);
        let Mounted {
            allocations, root, ..
        } = mount(&compiled, &report, None, None, &[], &[])?;
        Ok(quote! {
            |cx: &mut ::viso::__private::viso_ui::BuildCx<'_>| -> ::viso::__private::viso_ui::Handle {
                #allocations
                (#root).0
            }
        })
    })();
    finish(expansion, true)
}

pub fn component(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let source = SourceText::new(input);
    let report = Report::Inline(&source, TextSize::ZERO);
    let expansion = (|| {
        let root_dir = package::root()?;
        let origin = package::origin(&root_dir, package::invoking_file().as_deref())?;
        let profile = package::profile(&root_dir)?;
        let catalogs = profile_dependencies(&profile);
        let (compiled, packages) = compile_linked(&root_dir, &report, |natives| {
            frontend::compile_component_for(&source.text, &origin, natives, profile.target.clone())
        })?;
        report.check(&compiled)?;
        let Mounted {
            component,
            states,
            allocations,
            root,
            named,
        } = mount(
            &compiled,
            &report,
            None,
            asset_dir(&profile),
            &packages,
            &[],
        )?;
        let name = rust_ident(&component.schema.name)
            .map_err(|message| report.at(Some(component.source_origin), message))?;
        let imported = compiled
            .imported
            .iter()
            .filter_map(|module| module.path.as_deref()?.to_str());
        let nodes_name = format_ident!("{name}Nodes");
        let (node_fields, node_values) = named_nodes(&named, &report)?;
        let nodes_doc = format!("The handles of the `{name}` component's named nodes.");
        let doc = format!("The `{name}` component's state ids.");
        let fields = states
            .iter()
            .map(|(field, _)| quote! { pub #field: ::viso::__private::viso_ui::StateId });
        let values = states
            .iter()
            .map(|(field, local)| quote! { #field: #local });
        Ok(quote! {
            #catalogs
            #(const _: &str = ::core::include_str!(#imported);)*
            #[doc = #doc]
            #[derive(Debug, Clone, Copy, PartialEq, Eq)]
            pub struct #name {
                #(#fields,)*
            }

            #[doc = #nodes_doc]
            #[derive(Debug, Clone, Copy, PartialEq, Eq)]
            pub struct #nodes_name {
                #(#node_fields,)*
            }

            impl #name {
                /// Allocates the component's states and mounts its view, returning the
                /// state ids and the mounted view: its root and its named nodes.
                pub fn build(
                    cx: &mut ::viso::__private::viso_ui::BuildCx<'_>,
                ) -> (Self, ::viso::__private::viso_ui::MountedView<#nodes_name>) {
                    #allocations
                    let (__viso_root, __viso_named) = #root;
                    let __viso_view = ::viso::__private::viso_ui::MountedView {
                        root: __viso_root,
                        nodes: #nodes_name { #(#node_values,)* },
                    };
                    (Self { #(#values,)* }, __viso_view)
                }
            }

            impl ::viso::__private::viso_ui::Mount for #name {
                fn mount(cx: &mut ::viso::__private::viso_ui::BuildCx<'_>) -> ::viso::__private::viso_ui::Handle {
                    Self::build(cx).1.root
                }
            }
        })
    })();
    finish(expansion, false)
}

pub fn view(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let expansion = (|| {
        let ViewArgs { literal, inputs } = syn::parse(input)?;
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
        let profile = package::profile(&root_dir)?;
        let catalogs = profile_dependencies(&profile);
        let catalog = match &profile.catalogs {
            Some(c) => {
                let (source, dir) = (&c.source, &c.dir);
                quote! { ::core::option::Option::Some((#source, #dir)) }
            }
            None => quote! { ::core::option::Option::None },
        };
        // What the dev session compiles the file's edits with: the grants the
        // build checked it against.
        let granted = profile.target.capabilities.iter();
        let hash = Literal::u64_suffixed(viso_dsl::hotreload::source_hash(&text));
        let report = Report::File {
            path: &path,
            index: LineIndex::new(&text),
            span,
        };
        let (compiled, packages) = compile_linked(&root_dir, &report, |natives| {
            frontend::compile_file_for(&text, &origin, natives, profile.target.clone())
        })?;
        let statics = viso_dsl::aot::static_shape(&compiled.tree);
        // The asset directory the dev session compiles the file's edits
        // against, and what the build embedded, which a patch need not send.
        let assets = match asset_dir(&profile).and_then(Path::to_str) {
            Some(dir) => {
                let ids = compiled
                    .assets
                    .iter()
                    .map(|asset| Literal::u64_suffixed(asset.id.0));
                quote! {
                    ::core::option::Option::Some((
                        #dir,
                        &[#(::viso::__private::viso_ui::resource::ResourceId(#ids)),*],
                    ))
                }
            }
            None => quote! { ::core::option::Option::None },
        };
        let build = quote! {
            catalog: #catalog,
            assets: #assets,
            capabilities: &[#(#granted),*],
            source_hash: #hash,
            statics: &[#(#statics),*],
        };
        report.check(&compiled)?;
        let Mounted {
            allocations,
            root,
            named,
            ..
        } = mount(
            &compiled,
            &report,
            Some((dependency, &origin, build)),
            asset_dir(&profile),
            &packages,
            &inputs,
        )?;
        let (node_fields, node_values) = named_nodes(&named, &report)?;
        // The modules the file imports are compile dependencies too: editing
        // one rebuilds every view that reads it.
        let imported = compiled
            .imported
            .iter()
            .filter_map(|module| module.path.as_deref()?.to_str());
        Ok(quote! {
            {
                const _: &str = ::core::include_str!(#dependency);
                #(const _: &str = ::core::include_str!(#imported);)*
                #catalogs
                /// The handles of the view's named nodes.
                #[allow(dead_code)]
                #[derive(Debug, Clone, Copy, PartialEq, Eq)]
                struct __VisoNodes {
                    #(#node_fields,)*
                }
                move |cx: &mut ::viso::__private::viso_ui::BuildCx<'_>| -> ::viso::__private::viso_ui::MountedView<__VisoNodes> {
                    #allocations
                    let (__viso_root, __viso_named) = #root;
                    ::viso::__private::viso_ui::MountedView {
                        root: __viso_root,
                        nodes: __VisoNodes { #(#node_values,)* },
                    }
                }
            }
        })
    })();
    finish(expansion, true)
}

/// `view!`'s arguments: the `.vs` path, then the root component's inputs as
/// `name: value`.
struct ViewArgs {
    literal: LitStr,
    inputs: Vec<(Ident, syn::Expr)>,
}

impl syn::parse::Parse for ViewArgs {
    fn parse(input: syn::parse::ParseStream<'_>) -> syn::Result<Self> {
        let literal = input.parse()?;
        let mut inputs = Vec::new();
        while input.parse::<Option<syn::Token![,]>>()?.is_some() && !input.is_empty() {
            let name: Ident = input.parse()?;
            input.parse::<syn::Token![:]>()?;
            inputs.push((name, input.parse()?));
        }
        Ok(ViewArgs { literal, inputs })
    }
}

/// `asset!("images/logo.png")`: the file of the package's `assets/` the path
/// names, checked to exist and to be an image, an SVG or a video, as a
/// `viso_ui::Asset` embedding its bytes, or the file the build converted it
/// to ([`embedded_asset`](viso_dsl::assets::embedded_asset)), as an aligned
/// `static` — so editing it rebuilds the crate.
pub fn asset(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let expansion = (|| {
        let literal: LitStr = syn::parse(input)?;
        let span = literal.span();
        let fail = |message: String| syn::Error::new(span, format!("{ASSET_ERROR}: {message}"));
        let written = literal.value();
        let path = normalize_path(&written).ok_or_else(|| {
            fail(format!(
                "`{written}` is outside `assets/`: an asset path is relative to it and never \
                 climbs out"
            ))
        })?;
        let dir = package::asset_dir(&package::root()?)?;
        let file = dir.join(&path);
        let mut head = Vec::with_capacity(4096);
        let opened = std::fs::File::open(&file)
            .and_then(|f| std::io::Read::take(f, 4096).read_to_end(&mut head));
        if opened.is_err() || !file.is_file() {
            let nearest = AssetCatalog::scan(&dir)
                .ok()
                .and_then(|catalog| match catalog.lookup(&path) {
                    Err(AssetIssue::Missing { nearest }) => nearest,
                    _ => None,
                })
                .map(|nearest| format!("; did you mean `{nearest}`?"))
                .unwrap_or_default();
            return Err(fail(format!("`assets/{path}` does not exist{nearest}")));
        }
        let format = AssetFormat::sniff(&head).ok_or_else(|| {
            fail(format!(
                "`assets/{path}` is not an image, SVG, video or glTF file"
            ))
        })?;
        let (file, kind) = viso_dsl::assets::embedded_asset(&dir, &path, format);
        let file = file
            .to_str()
            .ok_or_else(|| fail(format!("the path of `assets/{path}` is not UTF-8")))?;
        let bytes = aligned_file(file);
        let overlay = overlay_dependency();
        let id = Literal::u64_suffixed(ResourceId::of(&path).0);
        let kind = match kind {
            AssetKind::Raster => quote! { Raster },
            AssetKind::Vector => quote! { Vector },
            AssetKind::Video => quote! { Video },
            AssetKind::Model => quote! { Model },
        };
        Ok(quote! {{
            #overlay
            ::viso::__private::viso_ui::Asset {
                id: ::viso::__private::viso_ui::resource::ResourceId(#id),
                kind: ::viso::__private::viso_ui::resource::AssetKind::#kind,
                source: ::viso::__private::viso_ui::AssetSource::Bytes(
                    ::viso::__private::viso_ui::AssetBytes::Static(#bytes),
                ),
            }
        }})
    })();
    finish(expansion, true)
}

/// Makes the `Viso.toml` and each catalog file a compile dependency of the
/// expansion, so a changed grant or message recompiles the forms that check
/// against it.
fn profile_dependencies(profile: &package::Profile) -> TokenStream {
    let catalogs = profile.catalogs.as_ref().map_or(&[][..], |c| &c.files[..]);
    let files = profile.manifest.iter().chain(catalogs);
    quote! { #(const _: &str = ::core::include_str!(#files);)* }
}

/// Where a form's diagnostics are reported.
enum Report<'a> {
    /// At the macro body token a diagnostic points at; a span of the text
    /// the body compiled as, which starts so far into it, is shifted back to
    /// the body's own offsets.
    Inline(&'a SourceText, TextSize),
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
            Report::Inline(source, base) => {
                let span = range.map_or_else(Span::call_site, |range| {
                    source.span_at(range.start().saturating_sub(*base))
                });
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

    /// Every error diagnostic of `compiled`, whose spans are the source's
    /// own, as one combined error.
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
    /// Evaluates to the root's `Handle` and the id of each of `named`.
    root: TokenStream,
    named: Vec<Named>,
}

/// `record` is the `.vs` file a `view!` mounts and the module identity it
/// compiles under, which the mount records for the development session; `None`
/// for a form that records no mount.
///
/// `assets` is the package's asset directory, which the view's assets are
/// embedded from; `packages` the native packages it was compiled against,
/// whose libraries its host links when it calls them.
fn mount<'a>(
    compiled: &'a Compiled,
    report: &Report<'_>,
    record: Option<(&str, &Origin, TokenStream)>,
    assets: Option<&Path>,
    packages: &[NativePackage],
    inputs: &[(Ident, syn::Expr)],
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
                // The cell the state's revisions tick in, which no restart
                // carries: the state's value is the behavior's.
                let local = format_ident!("__viso_t{}", tracked.len());
                allocations.extend(quote! {
                    let #local = cx.__macro_state(
                        ::viso::__private::viso_ui::StateValue::Int(0),
                        ::core::option::Option::None,
                    );
                });
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
        let (hi, lo) = (source.symbol.hi, source.symbol.lo);
        allocations.extend(quote! {
            let #local = cx.__macro_state(
                #value,
                ::core::option::Option::Some(::viso::__private::viso_ui::state::StateKey::from_parts(#hi, #lo)),
            );
        });
        idents.insert(source.symbol, local.clone());
        states.push((field, local));
    }
    let takes_inputs = record.is_some();
    let given = match mount_inputs(
        compiled,
        component,
        behavior.as_ref(),
        report,
        inputs,
        takes_inputs,
    ) {
        Ok(given) => given,
        Err(error) => {
            errors.push(error);
            Vec::new()
        }
    };
    for input in &given {
        allocations.extend(input.tokens.clone());
        idents.insert(input.symbol, input.cell.clone());
    }
    // An input with a default that `view!` is not given holds it: a cell that
    // never moves.
    if takes_inputs && let Some(behavior) = &behavior {
        let defaulted = behavior
            .module
            .component(&behavior.component)
            .map(|index| behavior.module.layout(index))
            .map(|layout| {
                layout
                    .inputs
                    .iter()
                    .zip(&layout.input_defaults)
                    .filter(|(_, default)| default.is_some())
                    .map(|(name, _)| name.to_string())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        for input in &component.schema.inputs {
            let Some(symbol) = input.meta.resolved_symbol else {
                continue;
            };
            if idents.contains_key(&symbol) || !defaulted.iter().any(|d| *d == *input.name) {
                continue;
            }
            let local = format_ident!("__viso_id{}", idents.len());
            allocations.extend(quote! {
                let #local = cx.__macro_state(
                    ::viso::__private::viso_ui::StateValue::Int(0),
                    ::core::option::Option::None,
                );
            });
            idents.insert(symbol, local);
        }
    }
    let mut unmounted = BTreeSet::new();
    for edge in compiled.bindings.static_edges() {
        let Some(source) = compiled.source(edge.source) else {
            continue;
        };
        // A binding through a computed or a function names the states beneath it,
        // so only a root input, which no caller supplies here, is left unmounted.
        // A `view!` reports an input it is not given that has no default
        // once, as such.
        if source.kind == SourceKind::Input
            && (!takes_inputs || behavior.is_none())
            && !idents.contains_key(&source.symbol)
            && unmounted.insert(source.symbol)
        {
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
        let mut embedded = embedded_assets(compiled, assets)
            .map_err(|message| report.at(component.schema.view, message))?;
        embedded.extend(
            embedded_shaders(behavior)
                .map_err(|message| report.at(component.schema.view, message))?,
        );
        let libraries = package_libraries(behavior, packages)
            .map_err(|message| report.at(component.schema.view, message))?;
        allocations.extend(host_tokens(
            behavior, &idents, &tracked, embedded, libraries, &given,
        ));
    }
    let record = record.map(|(file, origin, build)| {
        record_tokens(file, origin, build, &idents, behavior.is_some())
    });
    let kinds = |name: &str| {
        name != viso_dsl::schema::COMPONENT.name
            && compiled
                .natives
                .widgets()
                .iter()
                .any(|w| w.widget.name == name && w.library.path == STANDARD_WIDGETS)
    };
    let (root, named) = emit_view(
        &compiled.tree,
        &compiled.bindings,
        &idents,
        behavior.as_ref(),
        record,
        &kinds,
    )
    .map_err(|message| report.at(component.schema.view, message))?;
    Ok(Mounted {
        component,
        states,
        allocations,
        root,
        named,
    })
}

/// The library of the standard widgets, whose named nodes are handed out
/// typed.
const STANDARD_WIDGETS: &str = "viso::widgets";

/// The field of each named node in the struct of their handles, and its
/// value, read off `__viso_named`: `save_button: Handle<kind::Button>`.
fn named_nodes(
    named: &[Named],
    report: &Report<'_>,
) -> syn::Result<(Vec<TokenStream>, Vec<TokenStream>)> {
    let mut errors = Vec::new();
    let (mut fields, mut values) = (Vec::new(), Vec::new());
    for (index, node) in named.iter().enumerate() {
        let (field, kind) = match (rust_ident(&node.name), rust_ident(&node.kind)) {
            (Ok(field), Ok(kind)) => (field, kind),
            (Err(message), _) | (_, Err(message)) => {
                errors.push(report.at(Some(node.at), message));
                continue;
            }
        };
        let ty = quote! {
            ::viso::__private::viso_ui::Handle<::viso::__private::viso_ui::kind::#kind>
        };
        fields.push(quote! { pub #field: #ty });
        values.push(quote! {
            #field: ::viso::__private::viso_ui::Handle::__of_kind(
                __viso_named[#index].expect("a named node the view builds once is built"),
            )
        });
    }
    combine(errors)?;
    Ok((fields, values))
}

/// The `__viso_host` a view with behavior dispatches into: the component's
/// module, embedded as an aligned `static` and decoded once per thread,
/// borrowing it, mirrored into the states the expansion allocated, and
/// tracking the `tracked` ones through their revision cells. `viso::__behavior!`
/// keeps one form of the module: its bytecode, or — where it runs the module
/// compiled to Rust — that code and the module without bytecode.
fn host_tokens(
    behavior: &ViewBehavior,
    idents: &HashMap<SymbolId, Ident>,
    tracked: &BTreeSet<SymbolId>,
    assets: TokenStream,
    libraries: TokenStream,
    inputs: &[GivenInput],
) -> TokenStream {
    let given = inputs.iter().map(|input| {
        let (name, value) = (&input.name, &input.value);
        quote! { (#name, #value) }
    });
    let bytecode = aligned_bytes(&behavior.bytes);
    let compiled = aligned_bytes(&behavior.compiled_bytes());
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
    let native: TokenStream = behavior
        .native_code("::viso::__private::viso_behavior")
        .parse()
        .expect("the lowered module is Rust");
    quote! {
        let (__viso_module, __viso_native) = ::viso::__behavior! {
            bytecode: #bytecode,
            compiled: #compiled,
            native: {
                mod __viso_native { #native }
                &__viso_native::NATIVE
            },
        };
        let __viso_host = ::viso::__private::viso_view::__embedded(
            __viso_module,
            #component,
            __viso_native,
            #libraries,
            ::std::vec![#(#given),*],
        );
        {
            let mut __viso_view = __viso_host.borrow_mut();
            #(#mirrors)*
            #assets
        }
        ::viso::__private::viso_view::__load_persisted(cx, &__viso_host);
    }
}

/// The form compiled by `compile` as [`native_package::compile_linked`]
/// compiles it for the crate at `root`, with the native packages it was
/// compiled against.
fn compile_linked(
    root: &Path,
    report: &Report<'_>,
    compile: impl Fn(Arc<Natives>) -> Compiled,
) -> syn::Result<(Compiled, Arc<[NativePackage]>)> {
    let linked = native_package::compile_linked(Some(&root.join("Cargo.toml")), compile);
    if let Some(why) = linked.unlinked {
        let mut error = report.at(None, format!("the native packages are not known: {why}"));
        if let Err(errors) = report.check(&linked.made) {
            error.combine(errors);
        }
        return Err(error);
    }
    Ok((linked.made, linked.packages))
}

/// The libraries of each of `packages` the module calls, as the crate names
/// them: `&[::viso_net::NATIVES]`.
fn package_libraries(
    behavior: &ViewBehavior,
    packages: &[NativePackage],
) -> Result<TokenStream, String> {
    let natives = behavior.module.natives();
    let krate = std::env::var("CARGO_CRATE_NAME").unwrap_or_default();
    let mut libraries = Vec::new();
    for package in packages {
        if natives.iter().any(|import| package.holds(&import.path)) {
            let library = package.library_in(&krate);
            let path: TokenStream = library.parse().map_err(|_| {
                format!(
                    "the libraries of `{}` are not a Rust path: `{library}`",
                    package.name
                )
            })?;
            libraries.push(path);
        }
    }
    Ok(quote! { &[#(#libraries),*] })
}

/// The package's asset directory a form's profile indexes.
fn asset_dir(profile: &package::Profile) -> Option<&Path> {
    profile.target.assets.as_deref().map(AssetCatalog::dir)
}

/// The statement giving the host the bytes of every asset the view names —
/// each the file the build converted it to, or its source
/// ([`embedded_file`](viso_dsl::assets::embedded_file)) — embedded with
/// `include_bytes!` as an aligned `static`, so editing one rebuilds the form;
/// or nothing for a view naming none.
fn embedded_assets(compiled: &Compiled, dir: Option<&Path>) -> Result<TokenStream, String> {
    if compiled.assets.is_empty() {
        return Ok(TokenStream::new());
    }
    let Some(dir) = dir else {
        return Err("internal: a view names assets but its profile has no asset directory".into());
    };
    let mut entries = Vec::with_capacity(compiled.assets.len());
    for asset in &compiled.assets {
        let path = viso_dsl::assets::embedded_file(dir, &asset.path);
        let path = path
            .to_str()
            .ok_or_else(|| format!("the path of `assets/{}` is not UTF-8", asset.path))?;
        let bytes = aligned_file(path);
        let id = Literal::u64_suffixed(asset.id.0);
        entries.push(quote! {
            (
                ::viso::__private::viso_ui::resource::ResourceId(#id),
                ::viso::__private::viso_ui::AssetBytes::Static(#bytes),
            )
        });
    }
    let overlay = overlay_dependency();
    Ok(quote! {
        #overlay
        __viso_view.set_assets([#(#entries),*]);
    })
}

/// The statement giving the host its shader table, or nothing for a view
/// painting with none: each shader packed, at build time, in every backend's
/// language beside the reference interpreter's program, the target keeping
/// only its backend's.
fn embedded_shaders(behavior: &ViewBehavior) -> Result<TokenStream, String> {
    if behavior.shaders.is_empty() {
        return Ok(TokenStream::new());
    }
    let mut entries = Vec::with_capacity(behavior.shaders.len());
    for brush in &behavior.shaders {
        let pack = |lang: ShaderLang| {
            pack_brush(brush, &[lang, ShaderLang::None])
                .map(|bytes| aligned_bytes(&bytes))
                .map_err(|error| {
                    format!("internal: a shader the view paints with does not build: {error}")
                })
        };
        let (msl, hlsl, spirv, glsl, wgsl, none) = (
            pack(ShaderLang::Msl)?,
            pack(ShaderLang::Hlsl)?,
            pack(ShaderLang::SpirV)?,
            pack(ShaderLang::Glsl)?,
            pack(ShaderLang::Wgsl)?,
            pack(ShaderLang::None)?,
        );
        entries.push(quote! {
            ::viso::__private::viso_view::__embedded_shader(::viso::__private::viso_view::__per_backend!(
                msl: #msl,
                hlsl: #hlsl,
                spirv: #spirv,
                glsl: #glsl,
                wgsl: #wgsl,
                none: #none,
            ))
        });
    }
    Ok(quote! { __viso_view.set_shaders([#(#entries),*]); })
}

/// The fields of a `view!` mount record the expansion knows before the tree is
/// built: the file, the module identity, the build's catalogs and grants,
/// and the source's hash and static shape (`build`), every state cell
/// by its durable key, and the behavior host.
fn record_tokens(
    file: &str,
    origin: &Origin,
    build: TokenStream,
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
        quote! { (::viso::__private::viso_ui::state::StateKey::from_parts(#hi, #lo), #local) }
    });
    let host = if behavior {
        quote! { ::core::option::Option::Some(::std::rc::Rc::clone(&__viso_host)) }
    } else {
        quote! { ::core::option::Option::None }
    };
    quote! {
        file: #file,
        package: #package,
        module: [#(#module),*],
        language: #language,
        #build
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
        ConstValue::Bool(value) => quote! { ::viso::__private::viso_ui::StateValue::Bool(#value) },
        ConstValue::Int(value, _) => {
            let value = i32::try_from(*value).ok()?;
            quote! { ::viso::__private::viso_ui::StateValue::Int(#value) }
        }
        ConstValue::Float(value, _) => {
            let value = *value as f32;
            if !value.is_finite() {
                return None;
            }
            quote! { ::viso::__private::viso_ui::StateValue::Float(#value) }
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

/// A root input `view!` was given: the tokens mounting it, the local its cell
/// is in and the local holding the mounted input.
struct GivenInput {
    symbol: SymbolId,
    name: String,
    tokens: TokenStream,
    cell: Ident,
    value: Ident,
}

/// Mounts each of `inputs` as the root component's input of its name,
/// checked at compile time to be a Rust value of the input's type. A name the
/// component does not declare, an input of a type no Rust value gives, and an
/// input without a default the macro is not given (where it `takes_inputs`)
/// are errors.
fn mount_inputs(
    compiled: &Compiled,
    component: &HirComponent,
    behavior: Option<&ViewBehavior>,
    report: &Report<'_>,
    inputs: &[(Ident, syn::Expr)],
    takes_inputs: bool,
) -> syn::Result<Vec<GivenInput>> {
    let mut errors = Vec::new();
    let mut given = Vec::new();
    for (at, (name, value)) in inputs.iter().enumerate() {
        let text = name.to_string();
        let Some(input) = component.schema.inputs.iter().find(|i| i.name == text) else {
            errors.push(syn::Error::new(
                name.span(),
                format!("the view's component has no input `{text}`"),
            ));
            continue;
        };
        let Some(symbol) = input.meta.resolved_symbol else {
            continue;
        };
        let Some(ty) = input_ty(compiled, &input.meta.inferred_type) else {
            errors.push(report.at(
                Some(input.meta.source_origin),
                format!("input `{text}` has a type no Rust value gives a view"),
            ));
            continue;
        };
        let (local, cell) = (
            format_ident!("__viso_in{at}"),
            format_ident!("__viso_ic{at}"),
        );
        let view = quote! { ::viso::__private::viso_view };
        let mount = match ty {
            InputKind::Value(kind) => quote! {
                let #local = #view::__mount_input::<#view::input_kind::#kind, _>(cx, #value);
            },
            InputKind::Navigator(route) => {
                let expect = format_ident!("__VisoRoute{at}");
                quote! {
                    struct #expect;
                    impl #view::__Route for #expect {
                        const PATH: &'static str = #route;
                    }
                    let #local = #view::__mount_navigator::<#expect, _>(cx, #value);
                }
            }
        };
        let tokens = quote! {
            #mount
            let #cell = #local.__cell();
        };
        given.push(GivenInput {
            symbol,
            name: text,
            tokens,
            cell,
            value: local,
        });
    }
    // An input without a default needs a value, where the form takes them.
    if let Some(behavior) = behavior.filter(|_| takes_inputs)
        && let Some(index) = behavior.module.component(&behavior.component)
    {
        let layout = behavior.module.layout(index);
        for (slot, default) in layout.input_defaults.iter().enumerate() {
            let name = &layout.inputs[slot];
            if default.is_none() && !given.iter().any(|g| *g.name == **name) {
                let declared = component
                    .schema
                    .inputs
                    .iter()
                    .find(|i| *i.name == **name)
                    .map(|i| i.meta.source_origin);
                errors.push(report.at(
                    declared,
                    format!(
                        "input `{name}` has no default: give it as `view!(\"..\", {name}: value)`"
                    ),
                ));
            }
        }
    }
    combine(errors)?;
    Ok(given)
}

/// What a Rust value given for an input is checked against.
enum InputKind {
    /// A value of the `viso_view::input_kind` marker it names.
    Value(Ident),
    /// A router view over the native enum at this path.
    Navigator(String),
}

/// What a Rust value of an input of type `ty` must be, or `None` for a type
/// no Rust value gives a view.
fn input_ty(compiled: &Compiled, ty: &viso_dsl::hir::Ty) -> Option<InputKind> {
    use viso_dsl::hir::Ty;
    let kind = |name: &str| Some(InputKind::Value(format_ident!("{name}")));
    match ty {
        Ty::Bool => kind("Bool"),
        Ty::I32 => kind("I32"),
        Ty::F32 => kind("F32"),
        Ty::I64 => kind("I64"),
        Ty::F64 => kind("F64"),
        Ty::String => kind("String"),
        Ty::Native(id, args) => {
            let navigator = viso_dsl::schema::NativeId::of(viso_dsl::schema::NAVIGATOR_PATH);
            let [Ty::Named(route, _)] = args.as_slice() else {
                return None;
            };
            if *id != navigator {
                return None;
            }
            let path = compiled
                .natives
                .types()
                .iter()
                .find(|t| t.ty.is_enum() && viso_dsl::resolve::native_enum(&t.path) == *route)?
                .path
                .to_string();
            Some(InputKind::Navigator(path))
        }
        _ => None,
    }
}
