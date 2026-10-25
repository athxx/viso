//! `#[derive(NativeData)]`: a Rust struct as a record `.vs` reads.
//!
//! For a struct of named fields, each a `NativeValue`, and its path in a
//! native library (`#[native(path = "api::Profile")]`), the derive emits:
//!
//! 1. `NativeValue`: the type is `SchemaTy::Value(path)`, a value the
//!    aggregate of its fields in declaration order;
//! 2. `NativeData`: its path, its fields as schema parameters, and its
//!    `NativeType::record`, named the path's last segment.
//!
//! The output names `::viso::native::...`; `#[native(crate = path)]` names
//! another module re-exporting the same items.

use proc_macro2::{Span, TokenStream};
use quote::quote;
use syn::spanned::Spanned;
use syn::{Data, DeriveInput, Fields, LitStr, Path};

/// Entry point for the derive; see the module docs.
pub fn derive(input: DeriveInput) -> TokenStream {
    match expand(&input) {
        Ok(tokens) => tokens,
        Err(e) => e.to_compile_error(),
    }
}

fn expand(input: &DeriveInput) -> syn::Result<TokenStream> {
    let name = &input.ident;
    if !input.generics.params.is_empty() {
        return Err(syn::Error::new(
            input.generics.span(),
            "a native record has no generic parameters: its schema is one type",
        ));
    }
    let fields = match &input.data {
        Data::Struct(s) => match &s.fields {
            Fields::Named(named) => &named.named,
            _ => {
                return Err(syn::Error::new(
                    input.span(),
                    "a native record has named fields: `.vs` reads them by name",
                ));
            }
        },
        _ => {
            return Err(syn::Error::new(
                input.span(),
                "NativeData derives for a struct; a schema enum is a `NativeType::enumeration`",
            ));
        }
    };
    let mut path: Option<LitStr> = None;
    let mut krate: Option<Path> = None;
    for attr in input.attrs.iter().filter(|a| a.path().is_ident("native")) {
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("path") {
                path = Some(meta.value()?.parse()?);
                Ok(())
            } else if meta.path.is_ident("crate") {
                krate = Some(meta.value()?.parse()?);
                Ok(())
            } else {
                Err(meta.error("expected `path = \"..\"` or `crate = ..`"))
            }
        })?;
    }
    let Some(path) = path else {
        return Err(syn::Error::new(
            Span::call_site(),
            "a native record names its path: `#[native(path = \"api::Profile\")]`",
        ));
    };
    let full = path.value();
    let short = match full.rsplit_once("::") {
        Some((library, short)) if !library.is_empty() && !short.is_empty() => short.to_owned(),
        _ => {
            return Err(syn::Error::new(
                path.span(),
                "a native record's path is its library's and its name: `api::Profile`",
            ));
        }
    };
    let n = krate.unwrap_or_else(|| syn::parse_quote!(::viso::native));

    let idents: Vec<_> = fields.iter().filter_map(|f| f.ident.as_ref()).collect();
    let tys: Vec<_> = fields.iter().map(|f| &f.ty).collect();
    let names: Vec<String> = idents
        .iter()
        .map(|i| i.to_string().trim_start_matches("r#").to_owned())
        .collect();
    let slots: Vec<_> = (0..idents.len())
        .map(|i| syn::Ident::new(&format!("field{i}"), Span::call_site()))
        .collect();

    Ok(quote! {
        impl #n::NativeValue for #name {
            const TY: #n::SchemaTy = #n::SchemaTy::Value(#path);

            fn from_value(value: &#n::Value) -> ::core::option::Option<Self> {
                let [#(#slots),*] = value.fields()? else {
                    return ::core::option::Option::None;
                };
                ::core::option::Option::Some(#name {
                    #(#idents: <#tys as #n::NativeValue>::from_value(#slots)?,)*
                })
            }

            fn into_value(self) -> #n::Value {
                #n::Value::record([
                    #(<#tys as #n::NativeValue>::into_value(self.#idents),)*
                ])
            }
        }

        impl #n::NativeData for #name {
            const PATH: &'static str = #path;
            const FIELDS: &'static [#n::Param] = &[
                #(#n::Param { name: #names, ty: <#tys as #n::NativeValue>::TY },)*
            ];
            const TYPE: #n::NativeType =
                #n::NativeType::record(#short, <Self as #n::NativeData>::FIELDS, &[]);
        }
    })
}
