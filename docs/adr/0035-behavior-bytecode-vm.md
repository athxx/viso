# ADR 0035 — Behavior bytecode VM in a leaf crate

- Status: Proposed
- Date: 2026-09-28

## Context

`.vs` behavior (`fn`, `action`, `computed`, state initializers, input defaults, closures)
lowers from typed HIR to a register-based Behavior IR (`crates/dsl/src/behavior`). The IR
is a compiler data structure: it carries HIR types, source spans and unsupported-body
reasons, and it cannot run. Development runtime, headless tests and hot reload need an
executor that:

- runs a warmed-up action with no allocation beyond what the action itself creates;
- enforces the §95.1 budgets and turns every failure into a structured Runtime Fault
  (§83) instead of a Rust panic;
- gives actions the §86 transaction semantics: one revision per outermost transaction,
  full rollback on fault;
- stays out of the dependency graph of `viso-runtime` and `viso-ui`, so pure Rust UI never
  pays for it (AGENTS.md §21.1).

## Decision

A new leaf crate `viso-behavior` (`crates/behavior`) owns the bytecode format, the
verifier and the interpreter. It depends on no workspace crate. `viso-dsl` depends on it
and lowers `Program` to a `viso_behavior::Module` (`Program::bytecode`). `viso-runtime`,
`viso-ui` and `viso-widgets` must not depend on it.

### Format

- A module is a list of chunks (one per IR function, same index) plus component layouts
  (state/input/event names, initializer and default chunks, members by name).
- A chunk has a fixed frame of registers; arguments arrive in `r0..`, closure captures in
  declared registers. A chunk whose body the compiler could not produce keeps its arity and
  a reason; calling it faults.
- Instructions are 8-byte `Op` values: three 16-bit registers or a register and a 32-bit
  immediate. Variable operand lists (calls, aggregates, paths, switch tables) live in a
  per-chunk `u32` operand table. Constants too large for an immediate go to a per-chunk
  pool. Each instruction keeps its source span in a parallel table (cold).
- `I64`/`F64` arithmetic and ordering have dedicated ops; other widths use one generic
  checked-arithmetic op tagged with operator and numeric type.
- Values are a 16-byte enum: `Nil`, `Int(i64)`, `Float(f64)`, and reference-counted
  strings, lists, aggregates and closures. `Bool`, `Char`, `Color`, `Unit` and unit enum
  variants are `Int`; `None` is `Nil`.

### Verification

`Module::new` checks every register, jump target, operand-table range, constant, chunk
reference and arity, state/input slot and event index, and that no body can run off its
end. The interpreter then indexes without re-checking operands for validity of shape;
value-kind mismatches still fault (`E7105`, internal) instead of panicking.

### Transactions

Writes to state happen in place. The first write to a slot in a transaction pushes the
old value to an undo log (tracked by a bitset). A successful outermost call raises the
revision by one if anything was written and marks the written slots dirty; a fault
restores every logged slot and drops the buffered events. Nested calls join the outer
transaction.

### Budgets and faults

Per outermost invocation: instruction fuel (one unit per executed op), call depth, and
bytes allocated by strings, lists, aggregates and closures. Every invocation starts with
a fresh budget. Fault codes:

- `E7101` instruction budget or call depth exceeded;
- `E7102` memory budget exceeded;
- `E7103` integer overflow, division by zero, shift out of range;
- `E7104` index out of bounds;
- `E7105` the function cannot run (compile errors), a required input is missing, or an
  internal invariant failed.

A fault carries its chunk and source span.

### Performance

Criterion benches in `crates/dsl/benches/behavior_vm.rs` (release): a counter increment
action ≈ 100 ns; a 1000-iteration integer loop (10 ops per iteration) ≈ 40 µs. Cold ops are
dispatched through a separate non-inlined function so the hot dispatch loop stays small.

## Consequences

- The DSL compiler and the interpreter evolve together through the `Module` format; the
  format is not a stable external ABI.
- Release AOT (ADR 0016) remains the release path; the VM serves development, headless
  tests and hot reload.
- Native calls, method calls and task bodies are not yet executable; they arrive with the
  typed native schema and later slices.
