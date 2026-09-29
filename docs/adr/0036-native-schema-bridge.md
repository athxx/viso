# ADR 0036 — Native schema bridge in the behavior crate

- Status: Proposed
- Date: 2026-09-28

## Context

`.vs` code calls native Rust functions (`text::upper`, `Stopwatch::start()`,
`clipboard::write_text`). The spec (§47, §95, AGENTS.md §21.5.4) requires that:

- application authors never duplicate native signatures in `.vs`;
- the compiler sees each function's effect kind (`fn`/`action`/`task`), thread domain,
  capabilities and handle ownership, so effect, capability and ownership errors are
  compile-time diagnostics;
- the runtime checks capabilities per call, meters native calls against a budget, and
  turns a native error or panic into a Runtime Fault, never a Rust panic;
- a module compiled against one schema cannot silently run against another.

The compiler (`viso-dsl`) and the executor (`viso-behavior`, ADR 0035) both need the same
schema. `viso-dsl` already depends on `viso-behavior`; the reverse is forbidden.

## Decision

The native schema and registry live in `viso-behavior::native`, as `'static` const data:

- `NativeLibrary { path, version, functions, types }`, `NativeFunction { name, kind,
  params, ret, capabilities, thread, deterministic, realtime_safe, cost, call }` and
  `NativeType { name, methods, ownership, thread }`. The `native!` macro builds a
  `NativeFunction` from a typed Rust closure; argument and return conversion goes through
  the `NativeValue` trait, and `NativeObject` types become `Obj<T>` handles.
- `Natives` indexes libraries by full path and `NativeId` (a stable hash of the path).
  Registering a second version of a path, or a name that collides, is a `SchemaConflict`
  (`E6101`). `Natives::standard()` holds `viso::text`, `viso::math`, `viso::time` and
  `viso::clipboard`.
- The compiler resolves native paths through the registry the module graph is built with
  (`compile_file_in`, `load_package_in`), types calls from the schema, maps kinds to
  effect classes, adds function capabilities to the caller's direct capability set, and
  reports `E6102` for a worker-domain call outside a `task` and for a borrowed handle
  stored anywhere but a local or a parameter.
- A compiled `Module` carries one `NativeImport { path, signature, params }` per distinct
  native it calls; `Op::Native` indexes the import table. `Vm::link(&Natives, grants)`
  binds every import or fails as a whole with `E6101` (unregistered path or different
  signature hash). An import whose capabilities are not granted still links and faults
  with `CapabilityDenied` (`E6103`) when called, so code that never calls it runs.
- Each call spends one unit of `Budget::native_calls` (`E7101` when exhausted). A native
  `Err` or a caught panic is `NativeFailure` (`E7106`); the transaction rolls back its VM
  state, but effects outside the VM are not undone.
- Host services a native needs (the clipboard) are installed on the VM as typed values in
  `Services`, so headless tests mock them.
- `viso_dsl::schema` answers `viso schema` queries from the same registry and the widget
  baseline and writes the §139 object; the CLI only picks the text or JSON form.

Alternatives rejected:

- A separate `viso-native` crate: nothing but `viso-behavior` and `viso-dsl` use the
  schema, and both already depend in this direction (AGENTS.md §3.3).
- A proc-macro schema generator: const data plus `macro_rules!` gives the same schema with
  no build-time dependency; a proc-macro can replace `native!` later without changing the
  schema types.
- String lookup at call time: the linked import table resolves every native once per
  link, so a call is an index and a function-pointer call.

## Consequences

- Worker-domain natives and `task` natives type-check and lower, but the interpreter runs
  only UI/any-domain calls; a worker call faults as unsupported until the task runtime
  exists.
- The thread check covers a function's own domain; a handle type's domain is recorded and
  reported by `viso schema` but not yet enforced on method calls.
- `catch_unwind` converts a native panic only when panics unwind; under `panic = "abort"`
  a native panic aborts the process.
- A project's own native libraries are registered in Rust; `viso schema` currently answers
  from the standard registry and the widget baseline, not from project components.
