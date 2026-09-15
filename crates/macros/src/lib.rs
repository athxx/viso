//! `viso-macros` — compile-time code generation (§9).
//!
//! Hosts Viso's proc macros. Implemented so far:
//! - [`macro@GpuPod`] — derive an explicit, validated GPU instance layout
//!   for a `#[repr(C)]` struct (§18, §32): named per-field offsets instead of an
//!   implicit trailing-memory convention.
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
