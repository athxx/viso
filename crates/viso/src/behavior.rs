//! `viso::__behavior!`: the form of a view's behavior a `view!` expansion keeps.

/// The form of a view's behavior the expansion of `view!` keeps, as its
/// module's bytes and the module compiled to Rust: in a release build, and
/// in any build without the `vm` feature, the compiled code, run in place of
/// the interpreter, and the module without bytecode beside it; a debug build
/// with the interpreter keeps the bytecode it can hot-reload, and compiles
/// none of the Rust. The form not kept is compiled out, its bytes with it.
#[doc(hidden)]
#[macro_export]
#[cfg(any(not(debug_assertions), not(feature = "vm")))]
macro_rules! __behavior {
    (bytecode: $bytecode:block, compiled: $compiled:block, native: { $($code:tt)* } $(,)?) => {
        ($compiled, ::core::option::Option::Some({ $($code)* }))
    };
}

#[doc(hidden)]
#[macro_export]
#[cfg(all(debug_assertions, feature = "vm"))]
macro_rules! __behavior {
    (bytecode: $bytecode:block, compiled: $compiled:block, native: { $($code:tt)* } $(,)?) => {
        ($bytecode, ::core::option::Option::None)
    };
}

/// The mount a `view!` expansion keeps: in a `web-dom` build its offer of
/// the view to the page, which builds no node; everywhere else the nodes it
/// builds.
#[doc(hidden)]
#[macro_export]
#[cfg(feature = "web-dom")]
macro_rules! __view_mount {
    (page: { $($page:tt)* } nodes: { $($nodes:tt)* } $(,)?) => {
        { $($page)* }
    };
}

#[doc(hidden)]
#[macro_export]
#[cfg(not(feature = "web-dom"))]
macro_rules! __view_mount {
    (page: { $($page:tt)* } nodes: { $($nodes:tt)* } $(,)?) => {
        { $($nodes)* }
    };
}
