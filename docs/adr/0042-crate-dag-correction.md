# ADR 0042 — Crate DAG correction: compiler off the runtime stack

- Status: Accepted (2026-10-29); the tooling split is [ADR 0057](0057-shared-tooling-crate.md)
- Date: 2026-10-17

## Context

- `viso-gpu -> viso-runtime` and `viso-text -> viso-gpu` are allowed and
  declared, but neither crate's source references the other. Because of these
  edges, runtime sits below gpu, and pure-CPU text links GPU backends.
- `viso-dsl -> viso-ui` (plus `-> viso-view`) makes `viso-lsp` and
  `viso check` transitively link render, gpu, runtime and platform. The
  compiler only needs schema data and the IR types.
- The hot reload engine in `viso-dsl` includes the commit step, which mutates
  retained runtime state. That is apply-side work.
- `viso_render::primitive::{Rect, Point}` duplicate the `viso-math` types.
  `viso-ui` may not depend on `viso-math` at all.
- The platform accessibility seam carries `accesskit::TreeUpdate`. The
  Viso-owned adapters (iOS, Web) then have to go through a third-party format.

## Decision

- Remove `viso-gpu -> viso-runtime` and `viso-text -> viso-gpu`. Forbid both
  directions in `check-deps`.
- Add `viso-schema`, a `no_std + alloc` crate that depends only on
  `viso-ende`. It holds the data that crosses the compile boundary: widget,
  property, event and slot schemas and the standard widget table; `DirtyClass`;
  state identity and values; role, interaction, layout and env vocabulary;
  resource identity; `StableId`; value-type schemas and retyping; the AOT and
  view package formats. It holds no algorithms beyond the codecs and the small
  pure functions that define the data's meaning. The runtime crates re-export
  it at their existing paths.
- Add `viso-dev-protocol` for the CLI ↔ dev runtime link: the framed wire
  messages, the patch bundle they carry, and the inspection / profile /
  headless data a running app reports. Both sides depend on it; neither links
  the other. Runtime-side collection stays in `viso-view`.
- `viso-dsl` depends on `viso-schema`, `viso-dev-protocol`, `viso-ende`,
  `viso-behavior` and `viso-shader`. It drops `viso-ui` and `viso-view`
  (integration tests may still dev-depend on them).
- Hot reload is split. Plan, diff and migrate stay in `viso-dsl` and produce a
  patch bundle. Commit lives only in the dev module of `viso-view`.
- `viso-cli` drops `viso-view`. Asset checks use `viso-image` (raster decode)
  and `viso-svg` (parse only).
- `check-deps` gains a transitive-closure rule: the normal-dependency closures
  of `viso-dsl`, `viso-lsp`, `viso-ui-macros` and `viso-cli` contain no
  runtime, platform or gpu. Failures print the offending path.
- Add `viso-gpu-abi` for the GPU ABI description types (`InstanceSchema`,
  `ShaderCode`, `BlendMode`, resource descriptors, typed ids). `viso-gpu`
  re-exports it; `viso-shader` depends on it instead of `viso-gpu`.
- Add `viso-image` for CPU raster decode and snapshot images. `viso-render`
  and `viso-cli` share it.
- `viso-svg` becomes a pure parser on `viso-math`. SVG lowering to render
  primitives moves to `viso_render::svg`, so the edge is now
  `viso-render -> viso-svg`.
- `viso-math` owns the geometry types. Add `ui -> math` and
  `widgets -> math`; delete the duplicates in `viso-render`.
- `viso-shader` is the single owner of Shader IR
  (`viso_shader::program::Program`). `viso-dsl` parses and checks the shader
  declarations of a `.vs` module with the rest of the language (shared CST,
  resolver and diagnostics) and lowers them into that IR; validation and
  codegen belong to `viso-shader`.
- The platform accessibility seam carries a Viso-owned `SemanticsDelta`.
  AccessKit conversion lives inside the adapters that use AccessKit.

## Consequences

- LSP and `viso check` build without any GPU or windowing code, and the
  compiler can run in CI images without graphics stacks.
- `shader -> gpu`, `svg -> render`, `dsl -> ui`, `dsl -> view` and
  `cli -> view` are forbidden edges.
- Moving the schema data into `viso-schema` is a mechanical move. It lands as a separate change before any semantic change
  (AGENTS §40).

## As built

- `viso-dsl` also depends on `viso-diag`, `viso-lexer` and `viso-math`; `cargo xtask check-deps` holds the DAG.
