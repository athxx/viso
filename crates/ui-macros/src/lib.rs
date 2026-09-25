//! The Rust-side entry points of the Viso DSL (AGENTS section 21.5): `ui!`,
//! `component!` and `view!`.
//!
//! A proc-macro crate is a compile-time dylib, so — unlike the leaf `viso-macros`
//! derive crate — it MAY carry an ordinary library dependency. This crate uses that
//! to run the shared `viso_dsl::frontend` at Rust compile time: the three forms
//! differ only in the grammar entry they parse and where their source comes from,
//! and share resolution, typed HIR, UI IR, Binding IR and diagnostics. The mounted
//! view is a static `viso_ui` builder expression: no runtime parse, no per-frame
//! rebuild (section 59).
//!
//! The emitted tokens name `::viso_ui::…` paths; this crate does not depend on
//! `viso-ui`. The facade `viso` re-exports the macros and depends on `viso-ui`, so
//! the emitted paths resolve at the call site.
//!
//! A compiler-known typed binding never silently falls back to dynamic tracking
//! (section 10.3): each reactive read becomes a static `cx.bind` edge.

mod emit;
mod expand;
mod package;
mod source_text;

use proc_macro::TokenStream;

/// A view fragment, mounted by a `|cx: &mut BuildCx| -> Handle` closure. Each
/// reactive name the fragment reads is a `StateId` in the surrounding Rust scope.
///
/// ```ignore
/// let build = ui! { Text { text: label; } };
/// ```
#[proc_macro]
pub fn ui(input: TokenStream) -> TokenStream {
    expand::ui(input)
}

/// One complete component. Expands to a struct of the component's `StateId`s, one
/// field per `state`, with `build(cx) -> (Self, Handle)` allocating the states and
/// mounting the view. The `component` keyword is optional; the module path derives
/// from the invoking file, the package and language version from `Viso.toml`.
///
/// ```ignore
/// component! { Counter { state count = 0; view { Text { text: count; } } } }
/// ```
#[proc_macro]
pub fn component(input: TokenStream) -> TokenStream {
    expand::component(input)
}

/// The component of a `.vs` file, resolved relative to the invoking file: its
/// exported component, or its only one. Expands to a `|cx: &mut BuildCx| -> Handle`
/// closure that allocates the component's states and mounts its view. The file is
/// a compile dependency, so editing it rebuilds the crate.
///
/// ```ignore
/// let build = view!("counter.vs");
/// ```
#[proc_macro]
pub fn view(input: TokenStream) -> TokenStream {
    expand::view(input)
}
