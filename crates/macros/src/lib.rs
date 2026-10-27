//! `viso-macros` — compile-time code generation.
//!
//! Hosts Viso's proc macros. Implemented so far:
//! - [`macro@GpuPod`] — derive an explicit, validated GPU instance layout
//!   for a `#[repr(C)]` struct: named per-field offsets instead of an
//!   implicit trailing-memory convention.
//! - [`macro@NativeData`] — derive a native record (a Rust struct whose
//!   fields `.vs` reads) or schema enum (a Rust enum `.vs` builds and
//!   matches), with its schema.
//! - [`packaged_fonts!`] — scan `assets/fonts/` at build time and embed each
//!   face with the metadata the font manifest keeps, subset to the
//!   build's characters for an embedded target, its glyphs baked at the
//!   target's declared sizes.
//!
//! Planned: `#[component]`, state/binding metadata, `.vs` schema, static
//! template generation, compile-time diagnostics.
//!
//! This is a proc-macro crate whose one `viso-*` dependency is the leaf
//! `viso-glyph-a8`, which bakes glyphs with the rasterizer the text runtime
//! runs. The `GpuPod` derive emits code that names `viso_gpu::...` paths, but the
//! edge runs the other way — `viso-gpu` depends on `viso-macros` and re-exports
//! the derive, so downstream users only ever import it from `viso_gpu`.

use proc_macro::TokenStream;
use syn::{DeriveInput, parse_macro_input};

mod font_bake;
mod font_subset;
mod gpu_instance;
mod native_data;
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

/// Derive `viso::native::NativeData` and `NativeValue` for a struct of
/// named fields, each a native value: a record `.vs` reads, named by its
/// path in a native library; or for an enum.
///
/// ```ignore
/// #[derive(NativeData)]
/// #[native(path = "api::Profile")]
/// pub struct Profile {
///     pub name: String,
///     pub followers: i64,
/// }
/// ```
///
/// `Profile::TYPE` enters the library's `types`; a native returns a
/// `Profile` and `.vs` reads `profile.name`. `#[native(crate = path)]` names
/// the module the output refers to, `::viso::native` by default.
///
/// On an enum it derives a schema enum, each variant a unit or carrying
/// native values, named in snake case unless `#[native(name = "..")]`
/// renames it:
///
/// ```ignore
/// #[derive(NativeData)]
/// #[native(path = "mail::Route")]
/// pub enum Route {
///     Inbox,
///     Message { id: i64 },
/// }
/// ```
///
/// `.vs` writes `Route::inbox` and `Route::message { id: 5 }`, and matches
/// on them as on any enum.
#[proc_macro_derive(NativeData, attributes(native))]
pub fn derive_native_data(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    native_data::derive(input).into()
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
///
/// When the build sets `VISO_FONT_CHARSET` (comma-separated `ascii`, `latin`,
/// `greek`, `cyrillic`, `gb2312`, `U+XXXX` or `U+XXXX-YYYY`) or
/// `VISO_FONT_TEXT` (a UTF-8 file of the build's static text, relative to the
/// invoking crate's root unless absolute), each face is embedded as a
/// glyph-id-preserving subset holding those characters, and a character of
/// the text no packaged face maps is a compile error listing it.
///
/// When it sets `VISO_FONT_BAKE` (comma-separated sizes in pixels per em),
/// each face's drawable glyphs are rasterized at those sizes into compressed
/// A8 pages embedded beside it, which the runtime draws from instead of
/// rasterizing. `VISO_FONT_RASTERIZER=off` makes a face that cannot bake an
/// error, drops the outlines from the embedded fonts, and needs the facade
/// built without its `rasterizer` feature; a facade without it refuses an
/// expansion that does not bake every face. Every variable and the text file
/// are tracked: changing them rebuilds.
#[proc_macro]
pub fn packaged_fonts(input: TokenStream) -> TokenStream {
    let dir = if input.is_empty() {
        None
    } else {
        Some(parse_macro_input!(input as syn::LitStr))
    };
    packaged_fonts::expand(dir).into()
}
