# ADR 0041 — Release native lowering of view behavior

- Status: Accepted
- Date: 2026-10-17 (accepted 2026-10-18)

## Context

`ui!`, `component!` and `view!` are build-time entry points, but a view's
handlers run on the behavior VM in every target, release included
(ADR 0037). `ViewHost` keeps a VM instance per component, mirrors its states
into the UI state store, and is shared as `Rc<RefCell<ViewHost>>` by the
handlers of every node of the view. A release artifact therefore pays for:

- the interpreter in the binary;
- one dispatch through the VM per handler run;
- two copies of every DSL state.

Architecture §2.3 promises "dynamic in development, AOT in release". The VM
path keeps the semantics in one place, but rustc never sees the behavior code,
so it cannot inline or constant-fold it.

## Decision

- The AOT emitter lowers handlers, binding evaluators, computed bodies and
  effect bodies to Rust source. The source is compiled with the app. The UI
  IR asset keeps structure and tables only: templates, binding tables, and
  handler tables of `EvaluatorId`s.
- A release `ViewHost` runs no interpreter: the generated code runs on the
  VM's runtime shell (transactions, budgets, faults, memo, tasks), and a
  scalar state mirrored into a UI cell is copied only when a cell changed.
- Dev keeps the interpreter, so hot reload can still swap behavior. Dev and
  release share one handler-table layout, so a patch only replaces table
  entries.
- The interpreter is linked only in Dev artifacts, for packages that declare
  a dynamic capability, and for sandbox/AI preview. `check-profiles` rejects
  it in the embedded feature sets.
- `tests/differential/` runs every DSL conformance fixture on the VM and on the
  generated code. It compares the state write sequence and the dirty set per
  transaction. A mismatch is a compiler bug.

## Consequences

- Release binaries lose the interpreter and the state mirror. Handler cost
  becomes ordinary Rust.
- The emitter gains a Rust code generator that must preserve the VM's
  semantics for overflow, the undo log and runtime faults (`E7101`–`E7105`).
  Faults in generated code surface through the same `Fault` type.
- `release_handler_native_vs_vm` measures the difference. ADR 0037's release
  path is superseded once this ADR is accepted.

## Implementation

- **Evaluator ids are chunk indices.** Every handler, region entry, shown
  value, computed, effect, state initializer, function and closure is a chunk
  of the behavior module; the handler table maps an entry to its chunk in Dev
  and release alike, so a hot-reload patch still replaces table entries only.
  `viso_behavior::aot::lower_module` lowers every chunk to one Rust function
  and emits a `NATIVE: NativeCode` table (chunk → function, plus each task's
  resumable state machine) tied to the module's build hash.
  `Vm::install_native` refuses code from another build.
- **Tasks are resumable state machines.** A task chunk lowers to
  `fn(vm, instance, depth, &mut TaskFrame) -> Result<Poll, FaultKind>`: its
  registers live in the frame, each await is a block boundary, and a call
  into another task nests a child frame. `start_task`/`resume_task` drive the
  frame; a suspended native task holds the frame where the interpreter holds
  its fiber.
- **The VM shell stays; the interpreter goes.** The generated code calls back
  into the same `Vm` for what is not code: the transaction and its undo log,
  fuel, depth and memory budgets, fault locations (`rt::at` reports the
  bytecode pc, so a fault reads identically), computed memoization, natives,
  events and task starts. The interpreter is the `interpreter` feature of
  `viso-behavior` (`vm` of `viso-view` and `viso`); without it a chunk with
  no compiled code faults `E7105` and nothing interprets. The facade's `vm`
  is not a default feature: the CLI adds it to dev builds only, so a release
  or shipping artifact links no interpreter unless the app asks for it
  (`check-profiles` and `check-release-absence` hold this).
- **Where the native code comes from.** `view!` lowers its module at macro
  expansion and embeds the code beside the module bytes; `viso::__behavior!`
  keeps it in a release build and in any build without `vm`, with the module
  as `Module::compiled` leaves it (constant pools and read sets, no
  bytecode), and drops it (compiling none of it) in a debug build that
  hot-reloads the full bytecode. A view package carries bytecode:
  `viso_dsl::aot::package_native_code` lowers it for the app to compile,
  `viso_dsl::aot::compile_package` strips the bytecode that code replaces,
  and `ViewLoad::native` hands the code to the loader.
- **The state mirror.** A component's rich states (strings, lists, records)
  have no UI cell, so the instance keeps its state vector; a scalar state
  mirrored into a UI cell is still copied, but the copy is gated: the
  `StateStore` epoch rises with every cell change, and a host whose cells and
  instance revision are as they were at its last sync copies nothing. A
  dispatch costs O(1) in the mirrors, not O(states).
- **Differential tests.** `crates/dsl/tests/differential.rs` runs fixtures
  (`differential/*.vs`, lowered into the checked-in `differential/*.rs`,
  `VISO_BLESS=1` to refresh) on the interpreter and the native code side by
  side. Every call, task start, resume and `done` handler must give the same
  value or fault (kind, location, message), states, revision, dirty slots,
  events, starts and cost — over a table of edge values (overflow, division,
  NaN and infinities, casts), exhausted budgets, rollbacks, and tasks whose
  awaited work fails.
- **Bench.** `cargo bench -p viso-dsl --bench behavior_native`
  (`release_handler_native_vs_vm/{vm,native}/{action,fib,loop}`).
