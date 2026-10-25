# ADR 0051 — Native packages

- Status: Accepted
- Date: 2026-10-25

## Context

`.vs` code calls Rust through native libraries (ADR 0036), but only the standard ones:
the compiler, the `view!` family, the CLI, the LSP and the runtime all resolve and link
against `Natives::standard()`. Capabilities an app needs and that are not UI live in
extension crates (`viso-ext`) and in the app's own Rust, not in the framework, so `.vs`
code cannot reach them. The spec already writes `requires { network::http }` and calls
domain natives (`Api::profile(id)`) from a task.

An extension's native library is Rust statics with function pointers. The runtime can
link them, but the compiler cannot load them: a proc macro, the CLI and the LSP compile
`.vs` without building the app's Rust. The handwritten `native` declarations of §47.2
cannot carry the schema either: they declare no methods or properties on a native type.

## Decision

A **native package** is a Cargo package that carries native libraries for `.vs` code.

- Its manifest declares them:

  ```toml
  [package.metadata.viso.natives]
  schema = "natives.json"   # the libraries' schema, beside the manifest
  library = "NATIVES"       # the `&'static [&'static NativeLibrary]` they are, in the crate
  feature = "dsl"           # the feature that builds them, where one does
  ```

- The schema is the libraries in the `viso-natives` JSON format (version 1) that
  `viso_dsl::native_package::write` writes: every field of every function and type but
  the implementation. The package's own tests write it from its Rust libraries and fail
  when it differs, so it never drifts from them. Traits and widgets are not carried yet.
- `viso_dsl::native_package::discover(manifest)` finds the native packages among the
  app crate's **direct normal dependencies**, their feature enabled, through
  `cargo metadata`, once per manifest and lock file for the process. `read` interns each
  schema as stubs; `registry` is the standard libraries and theirs, a path declared twice
  being `E6101`.
- `view!` and `component!` compile against the standard libraries first and discover
  only when a path resolves to nothing (`E2001`), so an app without native packages runs
  no `cargo metadata`; `ui!` imports nothing. A compiled source records the registry it
  resolved against, and its behavior links against that at build time too. The
  expansion names the Rust libraries of the packages the module calls
  (`::app_api::NATIVES`) and hands them to the view's host, which links against the
  standard libraries and these (`Natives::shared`, one registry per set of libraries for
  the process). The game a view starts links as its host does; a release package loads
  with the same libraries (`ViewLoad::libraries`). An app writes no registration code; a
  native package it does not depend on directly is not found, since its crate could not
  be named.
- A dev session's patch links against the libraries the live view's host linked: an
  edit cannot reach a package the build did not link, which needs a rebuild, as a new
  Rust dependency does.
- `viso check`, `export` and `dev`, and the LSP, compile as the macros do
  (`native_package::link_with`): against the standard libraries until a path resolves to
  nothing, then against the packages of the crate the source belongs to (the
  `Cargo.toml` beside `Viso.toml`, or the nearest above the file), found once per
  manifest and lock file and taken at once by later compiles. `viso schema` lists what a
  project can call, so it discovers up front. A discovery that fails is a note on each
  `E2001` it leaves.
- Capabilities stay as they are: a package's functions declare theirs
  (`network.http`), the app's `Viso.toml` grants them (`[package] capabilities`), the
  grant travels in the module, and a call outside it faults with `E6103`. The `view!`
  and `component!` expansions make `Viso.toml` a compile dependency, so a changed grant
  rebuilds them.

Alternatives rejected:

- Global registration at startup (`linkme`/`inventory`): link-section tricks are not
  available on every target, and the compiler still needs the schema.
- Handwritten `.vs` declarations shipped by the package: they cannot declare methods or
  properties, and the app would have to import another package's `.vs` sources, which
  the module graph does not do.
- Reading the package's Rust source for its `native!` invocations: fragile, and a
  macro-built schema is not plain data until the crate compiles.

## Consequences

- An extension's DSL surface is ordinary native library code plus one generated file;
  the framework gains no knowledge of it.
- A proc-macro expansion may run `cargo metadata` once per crate compilation when its
  `.vs` imports a library the standard registry lacks.
- A package whose Rust library and schema disagree links with `E6101` at startup, and
  fails its own tests before that.
- An app's own natives live in a crate of their own in its workspace, a native package
  like any other: the crate that compiles the views cannot generate the schema they
  compile against.
- The network is no DSL library. `.vs` calls the app's domain natives (`api::profile`);
  URLs, auth, retries, decoding and error kinds stay in their Rust, which uses
  `viso-net`. A raw HTTP surface would hand `.vs` untyped text, move transport concerns
  into the view layer and widen what generated or third-party `.vs` can do, so
  `viso-net` carries none.
