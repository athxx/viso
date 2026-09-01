# ADR 0001 — Phase 0 Architecture Contract

- Status: Accepted
- Date: 2026-08-31

## Context

Viso is a clean-slate reimplementation of makepad following
`Viso_Architecture_and_Migration.md`. Phase 0 (§65) fixes the target
architecture contract *before* any legacy implementation is ported, so every
later phase has an objective "exit criterion" to check against.

## Decision

The following are frozen for the duration of the migration:

1. **Facade.** The sole public entry point is `viso::run::<App>()`; ordinary
   apps depend only on the `viso` crate (AGENTS §3.1).
2. **Crate DAG.** Twelve internal crates with one-way dependencies
   (§10). Enforced by `cargo xtask check-deps` as an allowlist — any edge not
   explicitly permitted fails CI. Forbidden edges (§10.1) such as
   `platform → ui`, `gpu → ui`, `ui → widgets`, `dsl → widgets` are therefore
   rejected by construction.
3. **Hot-path contract.** Steady-state frame paths target 0 unnecessary heap
   allocations, 0 string property lookups, 0 per-node global HashMap lookups,
   0 `Rc/RefCell` node traversal, 0 UI-main-thread mutex locks, 0 per-node
   backend virtual dispatch, 0 full-tree rebuild for a local state update
   (AGENTS §7.1).
4. **Node identity.** Retained tree over a generational
   `NodeId { index: u32, generation: u32 }` arena; stale handles are
   detectable (§14). Not `Rc<RefCell<Box<dyn Widget>>>`.
5. **Frame phases.** The twelve ordered phases of §11.2, each paired with a
   purpose-specific context (`AppCx`/`EventCx`/`LayoutCx`/…, §11.3).
6. **Dirty classes.** STRUCTURE / STYLE / MEASURE / LAYOUT / TRANSFORM /
   PAINT / HIT_TEST / SEMANTICS (§11); no single coarse `dirty = true`.
7. **`.vs`** is the sole canonical DSL source extension (AGENTS §1).
8. **Renderer primitive contract.** `Quad`/`GlyphRun`/`Image`/`Path`/`Mesh`/
   `Layer`; compact integer batch keys; UI never owns backend submission
   (§16).
9. **GPU instance ABI.** Host structs and GPU instance data are separate; the
   `GpuInstance` marker (later a validating derive) replaces the implicit
   "everything after field X is GPU memory" assumption (§18).

## Consequences

- The minimal shell compiles and runs with zero makepad runtime types
  (Phase 0 exit criterion).
- Dependency direction is machine-checked (Phase 0 exit criterion).
- Characterization/benchmark infrastructure is scaffolded (`benches/`,
  `tests/characterization/`) and filled with real baselines against makepad
  as those scenarios are ported.
- Migration policy: *migrate semantics, not architecture*. `makepad/` is a
  read-only behavior/perf reference; `viso-ext/` is out of scope.
