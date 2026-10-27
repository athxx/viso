//! `#[derive(NativeData)]`: a Rust struct as a record `.vs` reads, or a Rust
//! enum as a schema enum `.vs` builds and matches.
//!
//! For a struct of named fields, each a `NativeValue`, and its path in a
//! native library (`#[native(path = "api::Profile")]`), the derive emits:
//!
//! 1. `NativeValue`: the type is `SchemaTy::Value(path)`, a value the
//!    aggregate of its fields in declaration order;
//! 2. `NativeData`: its path, its fields as schema parameters, and its
//!    `NativeType::record`, named the path's last segment.
//!
//! For an enum, whose variants are units or carry `NativeValue` fields by
//! position or by name, the type is `SchemaTy::Enum(path)`: a unit variant's
//! value is its index, a payload variant's the aggregate of its fields tagged
//! with its index. `.vs` names each variant in snake case (`UserProfile` is
//! `user_profile`) unless `#[native(name = "..")]` on it says otherwise, and
//! a positional field by its index. `FIELDS` is empty and `TYPE` is a
//! `NativeType::enumeration_of`.
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
        Data::Enum(e) => return enumeration(name, e, &path, &short, &n),
        Data::Union(_) => {
            return Err(syn::Error::new(
                input.span(),
                "NativeData derives for a struct or an enum",
            ));
        }
    };

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

/// The impls for the enum `name`: see the module docs.
fn enumeration(
    name: &syn::Ident,
    data: &syn::DataEnum,
    path: &LitStr,
    short: &str,
    n: &Path,
) -> syn::Result<TokenStream> {
    if data.variants.is_empty() {
        return Err(syn::Error::new(
            name.span(),
            "a schema enum has a variant: `.vs` holds one of them",
        ));
    }
    let mut names = Vec::new();
    let mut payloads = Vec::new();
    let mut from = Vec::new();
    let mut into = Vec::new();
    for (tag, variant) in data.variants.iter().enumerate() {
        let tag = u32::try_from(tag).expect("an enum has fewer than 2^32 variants");
        let ident = &variant.ident;
        let mut schema = None::<LitStr>;
        for attr in variant.attrs.iter().filter(|a| a.path().is_ident("native")) {
            attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("name") {
                    schema = Some(meta.value()?.parse()?);
                    Ok(())
                } else {
                    Err(meta.error("expected `name = \"..\"`"))
                }
            })?;
        }
        names.push(schema.map_or_else(|| snake(&ident.to_string()), |s| s.value()));
        let tys: Vec<_> = variant.fields.iter().map(|f| &f.ty).collect();
        let slots: Vec<_> = (0..tys.len())
            .map(|i| syn::Ident::new(&format!("field{i}"), Span::call_site()))
            .collect();
        let tag_lit = tag as i64;
        match &variant.fields {
            Fields::Unit => {
                payloads.push(quote! { #n::VariantFields::UNIT });
                from.push(quote! {
                    #n::Value::Int(#tag_lit) => ::core::option::Option::Some(Self::#ident),
                });
                into.push(quote! { Self::#ident => #n::Value::Int(#tag_lit), });
            }
            Fields::Unnamed(_) => {
                let params = tys.iter().enumerate().map(|(i, ty)| {
                    let field = i.to_string();
                    quote! { #n::Param { name: #field, ty: <#ty as #n::NativeValue>::TY } }
                });
                payloads.push(quote! { #n::VariantFields::tuple(&[#(#params),*]) });
                from.push(quote! {
                    #n::Value::Agg(agg) if agg.tag == #tag => {
                        let [#(#slots),*] = &agg.fields[..] else {
                            return ::core::option::Option::None;
                        };
                        ::core::option::Option::Some(Self::#ident(
                            #(<#tys as #n::NativeValue>::from_value(#slots)?,)*
                        ))
                    }
                });
                into.push(quote! {
                    Self::#ident(#(#slots),*) => #n::Value::variant(#tag, [
                        #(<#tys as #n::NativeValue>::into_value(#slots),)*
                    ]),
                });
            }
            Fields::Named(named) => {
                let idents: Vec<_> = named
                    .named
                    .iter()
                    .filter_map(|f| f.ident.as_ref())
                    .collect();
                let params = idents.iter().zip(&tys).map(|(ident, ty)| {
                    let field = ident.to_string().trim_start_matches("r#").to_owned();
                    quote! { #n::Param { name: #field, ty: <#ty as #n::NativeValue>::TY } }
                });
                payloads.push(quote! { #n::VariantFields::record(&[#(#params),*]) });
                from.push(quote! {
                    #n::Value::Agg(agg) if agg.tag == #tag => {
                        let [#(#slots),*] = &agg.fields[..] else {
                            return ::core::option::Option::None;
                        };
                        ::core::option::Option::Some(Self::#ident {
                            #(#idents: <#tys as #n::NativeValue>::from_value(#slots)?,)*
                        })
                    }
                });
                into.push(quote! {
                    Self::#ident { #(#idents: #slots),* } => #n::Value::variant(#tag, [
                        #(<#tys as #n::NativeValue>::into_value(#slots),)*
                    ]),
                });
            }
        }
    }
    Ok(quote! {
        impl #n::NativeValue for #name {
            const TY: #n::SchemaTy = #n::SchemaTy::Enum(#path);

            fn from_value(value: &#n::Value) -> ::core::option::Option<Self> {
                match value {
                    #(#from)*
                    _ => ::core::option::Option::None,
                }
            }

            fn into_value(self) -> #n::Value {
                match self {
                    #(#into)*
                }
            }
        }

        impl #n::NativeData for #name {
            const PATH: &'static str = #path;
            const FIELDS: &'static [#n::Param] = &[];
            const TYPE: #n::NativeType = #n::NativeType::enumeration_of(
                #short,
                &[#(#names),*],
                &[#(#payloads),*],
            );
        }
    })
}

/// `name` in snake case: `UserProfile` is `user_profile`.
fn snake(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 4);
    for (i, c) in name.chars().enumerate() {
        if c.is_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.extend(c.to_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}
