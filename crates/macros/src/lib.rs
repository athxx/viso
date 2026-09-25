//! `viso-macros` — compile-time code generation (§9).
//!
//! Hosts Viso's proc macros. Implemented so far:
//! - [`macro@GpuPod`] — derive an explicit, validated GPU instance layout
//!   for a `#[repr(C)]` struct (§18, §32): named per-field offsets instead of an
//!   implicit trailing-memory convention.
//! - [`packaged_fonts!`] — scan `assets/fonts/` at build time and embed each
//!   face with the metadata the font manifest keeps (§3.1).
//!
//! Planned: `#[component]`, state/binding metadata, `.vs` schema, static
//! template generation, compile-time diagnostics.
//!
//! This is a proc-macro crate: a DAG leaf with no `viso-*` dependencies. The
//! `GpuPod` derive emits code that names `viso_gpu::...` paths, but the
//! edge runs the other way — `viso-gpu` depends on `viso-macros` and re-exports
//! the derive, so downstream users only ever import it from `viso_gpu`.

use proc_macro::TokenStream;
use syn::{DeriveInput, parse_macro_input};

mod gpu_instance;
mod packaged_fonts;

/// Derive [`viso_gpu::GpuPod`] for a `#[repr(C)]` struct of GPU
/// scalar/vector fields.
///
/// Generates `unsafe impl GpuPod` (with `STRIDE`), an inherent
/// `const LAYOUT: InstanceLayout` built from real `offset_of!` offsets, and an
/// inherent `validate_against(schema)` that cross-checks the layout against a
/// shader's declared [`viso_gpu::InstanceSchema`]. See `gpu_instance` for the
/// accepted field types and layout rules.
///
/// [`viso_gpu::GpuPod`]: ../viso_gpu/trait.GpuPod.html
/// [`viso_gpu::InstanceSchema`]: ../viso_gpu/struct.InstanceSchema.html
#[proc_macro_derive(GpuPod)]
pub fn derive_gpu_pod(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    gpu_instance::derive(input).into()
}

/// Package the application's `assets/fonts/` into the binary.
///
/// Expands to a `viso::fonts::PackagedFonts` describing every face in the
/// crate's `assets/fonts/` (or the directory named by an optional string
/// literal, relative to the crate root): each `.ttf` / `.otf` / `.ttc` / `.otc`
/// file is validated at build time, its faces' family, style, color and script
/// metadata extracted, and its bytes embedded. A file that does not parse is a
/// compile error naming it.
///
/// Cargo tracks each embedded file, so editing one rebuilds; adding or removing
/// a file needs a rebuild of the crate that invokes the macro (a `build.rs`
/// printing `cargo:rerun-if-changed=assets/fonts` makes that automatic).
#[proc_macro]
pub fn packaged_fonts(input: TokenStream) -> TokenStream {
    let dir = if input.is_empty() {
        None
    } else {
        Some(parse_macro_input!(input as syn::LitStr))
    };
    packaged_fonts::expand(dir).into()
}
