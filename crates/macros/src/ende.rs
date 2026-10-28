//! `#[derive(Encode, Decode)]`: a plain message's codec, its fields in
//! declaration order.
//!
//! - A struct writes each field in turn; a tuple or unit struct the same.
//! - An enum writes its variant's tag as a byte, then the variant's fields.
//!   Tags count up from 0 in declaration order, as discriminants do;
//!   `#[ende(tag = N)]` fixes a variant's, and the next count up from it. A
//!   tag the decoder does not know is `DecodeError::Malformed` at the tag.
//! - A field writes as its type's `Encode` does, or:
//!   - `#[ende(varint)]`: its integers as varints (`EncodeVarint`), in a
//!     tuple, list, array or `Option` of them too;
//!   - `#[ende(max = N)]`: a list whose decoder reads a count of at most
//!     `N`, `DecodeError::TooMany` past it;
//!   - `#[ende(with = path)]`: through `path::encode(&field, enc)` and
//!     `path::decode(dec)`.
//!
//! The output names `::viso_ende`; `#[ende(crate = path)]` names another.

use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::spanned::Spanned;
use syn::{Data, DeriveInput, Expr, Fields, Ident, Path};

/// What the output names the codec crate.
fn krate(input: &DeriveInput) -> syn::Result<Path> {
    let mut krate = None;
    for attr in input.attrs.iter().filter(|a| a.path().is_ident("ende")) {
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("crate") {
                krate = Some(meta.value()?.parse()?);
                Ok(())
            } else {
                Err(meta.error("expected `crate = path`"))
            }
        })?;
    }
    Ok(krate.unwrap_or_else(|| syn::parse_quote!(::viso_ende)))
}

/// How a field is written.
#[derive(Default)]
struct Field {
    varint: bool,
    max: Option<Expr>,
    with: Option<Path>,
}

fn field(attrs: &[syn::Attribute]) -> syn::Result<Field> {
    let mut field = Field::default();
    for attr in attrs.iter().filter(|a| a.path().is_ident("ende")) {
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("varint") {
                field.varint = true;
            } else if meta.path.is_ident("max") {
                field.max = Some(meta.value()?.parse()?);
            } else if meta.path.is_ident("with") {
                field.with = Some(meta.value()?.parse()?);
            } else {
                return Err(meta.error("expected `varint`, `max = N` or `with = path`"));
            }
            Ok(())
        })?;
        if field.with.is_some() && (field.varint || field.max.is_some()) {
            return Err(syn::Error::new(
                attr.span(),
                "a field written `with` a codec takes no `varint` or `max`",
            ));
        }
    }
    Ok(field)
}

/// A variant's fixed tag.
fn tag(attrs: &[syn::Attribute]) -> syn::Result<Option<u8>> {
    let mut tag = None;
    for attr in attrs.iter().filter(|a| a.path().is_ident("ende")) {
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("tag") {
                let value: syn::LitInt = meta.value()?.parse()?;
                tag = Some(value.base10_parse()?);
                Ok(())
            } else {
                Err(meta.error("expected `tag = N`"))
            }
        })?;
    }
    Ok(tag)
}

/// The fields' bindings, in order: their names, or `f0`, `f1`, … for a
/// tuple's.
fn bindings(fields: &Fields) -> Vec<Ident> {
    fields
        .iter()
        .enumerate()
        .map(|(at, f)| f.ident.clone().unwrap_or_else(|| format_ident!("f{at}")))
        .collect()
}

/// The pattern binding `fields` to `names`.
fn pattern(fields: &Fields, names: &[Ident]) -> TokenStream {
    match fields {
        Fields::Named(_) => quote! { { #(#names),* } },
        Fields::Unnamed(_) => quote! { ( #(#names),* ) },
        Fields::Unit => quote! {},
    }
}

/// The constructor of `fields`, each named in `names`, from `values`.
fn shape(fields: &Fields, names: &[Ident], values: &[TokenStream]) -> TokenStream {
    match fields {
        Fields::Named(_) => quote! { { #(#names: #values),* } },
        Fields::Unnamed(_) => quote! { ( #(#values),* ) },
        Fields::Unit => quote! {},
    }
}

fn encode_field(n: &Path, how: &Field, value: &Ident) -> TokenStream {
    match (&how.with, how.varint) {
        (Some(with), _) => quote! { #with::encode(#value, enc); },
        (None, true) => quote! { #n::EncodeVarint::encode_varint(#value, enc); },
        (None, false) => quote! { #n::Encode::encode(#value, enc); },
    }
}

fn decode_field(n: &Path, how: &Field) -> TokenStream {
    match (&how.with, how.varint, &how.max) {
        (Some(with), ..) => quote! { #with::decode(dec)? },
        (None, true, Some(max)) => quote! { dec.read_vec_varint(#max)? },
        (None, false, Some(max)) => quote! { dec.read_vec(#max)? },
        (None, true, None) => quote! { #n::DecodeVarint::decode_varint(dec)? },
        (None, false, None) => quote! { #n::Decode::decode(dec)? },
    }
}

/// Each field's codec, checked.
fn fields(fields: &Fields) -> syn::Result<Vec<Field>> {
    fields.iter().map(|f| field(&f.attrs)).collect()
}

/// The generics with `bound` on each type parameter.
fn bounded(input: &DeriveInput, bound: TokenStream) -> syn::Generics {
    let mut generics = input.generics.clone();
    for param in generics.type_params_mut() {
        param.bounds.push(syn::parse_quote!(#bound));
    }
    generics
}

pub fn derive_encode(input: DeriveInput) -> syn::Result<TokenStream> {
    let n = krate(&input)?;
    let name = &input.ident;
    let generics = bounded(&input, quote!(#n::Encode));
    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();
    let body = match &input.data {
        Data::Struct(data) => {
            let names = bindings(&data.fields);
            let hows = fields(&data.fields)?;
            let writes = names
                .iter()
                .zip(&hows)
                .map(|(name, how)| encode_field(&n, how, name));
            let pattern = pattern(&data.fields, &names);
            quote! {
                let Self #pattern = self;
                #(#writes)*
            }
        }
        Data::Enum(data) => {
            let tags = tags(data)?;
            let arms = data.variants.iter().zip(tags).map(|(variant, tag)| {
                let names = bindings(&variant.fields);
                let hows = fields(&variant.fields)?;
                let writes = names
                    .iter()
                    .zip(&hows)
                    .map(|(name, how)| encode_field(&n, how, name));
                let pattern = pattern(&variant.fields, &names);
                let ident = &variant.ident;
                Ok(quote! {
                    Self::#ident #pattern => {
                        enc.write_u8(#tag);
                        #(#writes)*
                    }
                })
            });
            let arms = arms.collect::<syn::Result<Vec<_>>>()?;
            if arms.is_empty() {
                quote! { match *self {} }
            } else {
                quote! { match self { #(#arms)* } }
            }
        }
        Data::Union(data) => {
            return Err(syn::Error::new(
                data.union_token.span,
                "`Encode` is derived for a struct or an enum",
            ));
        }
    };
    Ok(quote! {
        impl #impl_generics #n::Encode for #name #ty_generics #where_clause {
            #[allow(unused_variables)]
            fn encode(&self, enc: &mut #n::Encoder) {
                #body
            }
        }
    })
}

pub fn derive_decode(input: DeriveInput) -> syn::Result<TokenStream> {
    let n = krate(&input)?;
    let name = &input.ident;
    let generics = bounded(&input, quote!(#n::Decode));
    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();
    let body = match &input.data {
        Data::Struct(data) => {
            let names = bindings(&data.fields);
            let reads: Vec<_> = fields(&data.fields)?
                .iter()
                .map(|how| decode_field(&n, how))
                .collect();
            let value = shape(&data.fields, &names, &reads);
            quote! { ::core::result::Result::Ok(Self #value) }
        }
        Data::Enum(data) => {
            let tags = tags(data)?;
            let arms = data.variants.iter().zip(tags).map(|(variant, tag)| {
                let names = bindings(&variant.fields);
                let reads: Vec<_> = fields(&variant.fields)?
                    .iter()
                    .map(|how| decode_field(&n, how))
                    .collect();
                let value = shape(&variant.fields, &names, &reads);
                let ident = &variant.ident;
                Ok(quote! { #tag => Self::#ident #value, })
            });
            let arms = arms.collect::<syn::Result<Vec<_>>>()?;
            quote! {
                let offset = dec.position();
                ::core::result::Result::Ok(match dec.read_u8()? {
                    #(#arms)*
                    _ => return ::core::result::Result::Err(#n::DecodeError::Malformed { offset }),
                })
            }
        }
        Data::Union(data) => {
            return Err(syn::Error::new(
                data.union_token.span,
                "`Decode` is derived for a struct or an enum",
            ));
        }
    };
    Ok(quote! {
        impl #impl_generics #n::Decode for #name #ty_generics #where_clause {
            #[allow(unused_variables)]
            fn decode(dec: &mut #n::Decoder<'_>) -> ::core::result::Result<Self, #n::DecodeError> {
                #body
            }
        }
    })
}

/// Each variant's tag: its fixed one, or one past the variant's before it.
fn tags(data: &syn::DataEnum) -> syn::Result<Vec<u8>> {
    let mut tags: Vec<u8> = Vec::with_capacity(data.variants.len());
    let mut next: u16 = 0;
    for variant in &data.variants {
        let tag = match tag(&variant.attrs)? {
            Some(tag) => tag,
            None => u8::try_from(next).map_err(|_| {
                syn::Error::new(
                    variant.ident.span(),
                    "an enum's tag is a byte: past 255, fix the tags with `#[ende(tag = N)]`",
                )
            })?,
        };
        if tags.contains(&tag) {
            return Err(syn::Error::new(
                variant.ident.span(),
                format!("tag {tag} is another variant's"),
            ));
        }
        tags.push(tag);
        next = u16::from(tag) + 1;
    }
    Ok(tags)
}
