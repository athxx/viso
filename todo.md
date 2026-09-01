# Viso — TODO

> Working memory so progress survives interruption. Update as sections land.
> Authority order: accepted ADR > architecture doc > AGENTS.md > crate docs > convention.
> Standing constraints (do not drop):
> - No Makepad-keyword comments in code. No `§`+number section symbols in code/comments; strip any you pass.
> - Reference `/Users/x/code/makepad` code when designing a subsystem; don't invent alone.
> - Performance-first: prefer SIMD + zerocopy; unsafe allowed (with `SAFETY:` note).
> - Commit per section; keep changes focused.
> - Verify: `cargo xtask check-deps` (stays 13 crates) + build + clippy `-D warnings` + fmt + test; headless integration where UI; real-machine Metal when shaders change.

---

## Phase 3 — retained widget tree + reactive state

### Slice A — Component → Node → Flex → Paint (DONE)
- [x] `Component`/`BuildCx`, `NodeStore` SoA, generational `NodeArena`.
- [x] Two-pass Flex measure/layout; `paint_tree`.
- [x] Facade drives real retained tree through the frame phases.
- ADR 0003 (retained widget tree Flex slice). Committed.

### Slice B — reactive state + dirty true-propagation + incremental recompute
Spec: `docs/superpowers/specs/2026-09-01-reactive-state-dirty-propagation-design.md` (complete, no supplement needed).
Three focused commits:

#### B1 — dirty layered propagation + incremental recompute (no State dep) (DONE)
- [x] `NodeStore::mark_dirty(id, class)` — per-class layered propagation up the `parent` chain to each class's boundary (rules table in spec). Idempotent bit-or, stop at boundary, O(tree depth), zero alloc.
- [x] `NodeStore::dirty(id) -> DirtyClass`, `NodeStore::clear_dirty()`, `any_dirty()`.
- [x] Facade `relayout_and_paint` drives incremental `relayout_dirty` (measure+layout, only invalidated subtrees) + `repaint_dirty` (paint rebuilt only when a paint class is pending); clean subtrees skipped.
- [x] `FrameRecompute { measured, laid_out, painted }` counter, read before Submit under `VISO_FRAME_TRACE`.
- [x] Wire `PostFrameCleanup` → `clear_dirty`; seed root dirty on launch/resize.
- [x] Headless tests (`crates/viso/tests/incremental_dirty.rs`, 5): idle=0 recompute; LAYOUT-only re-places just that subtree; PAINT-only never bubbles LAYOUT/MEASURE to ancestors; MEASURE bubbles through flexible ancestor and stops below a fixed-on-both-axes container.
- [x] Strip `§`/Makepad refs from every file touched (dirty.rs, node.rs, component.rs, layout.rs, paint.rs, context.rs, style.rs, ui lib.rs, facade lib.rs).
- [x] Commit. (997f9e0)

#### B2 — State + StateStore + BindingTable + per-frame transactions (DONE)
- [x] `crates/ui/src/state.rs`: `StateId` (generational), `StateStore` SoA (values + generation + free list + frame pending write-set), `StateValue` scalar set (i32/f32/bool/color), `alloc/get/set/free/is_live/take_pending/has_pending`. No-op writes schedule nothing; pending dedupes.
- [x] `crates/ui/src/binding.rs`: `Binding { node, class }`, `BindingTable` (index-aligned to StateId; `bind`/`for_state`, folds same-node edges, contiguous runs regardless of registration order), plus dynamic region (`bind_dynamic`/`dynamic_for_state`) for B3.
- [x] `NodeStore::flush_state_transactions(changed, bindings)` turns each changed id into targeted `mark_dirty` via static + dynamic edges.
- [x] `UpdateCx` promoted to a real context holding `states`/`bindings`/`nodes` + `get`/`set`/`bindings`; deliberately no `mark_dirty` escape hatch.
- [x] Facade `FlushStateTransactions` phase: `take_pending` into a reused buffer → `flush_state_transactions`; one pass/frame, empty transaction inert.
- [x] First-write signal: `RuntimeCx::request_state_flush(window)` sets a flag + issues the beat; scheduler reads `state_dirty_requested()` back at launch and after each frame, folding `RedrawReason::StateDirty` into its reasons. Zero-CPU-when-idle preserved.
- [x] Tests drive the reactive stores directly through public `viso::ui` (no new facade write API needed this slice).
- [x] Headless tests (`crates/viso/tests/reactive_flush.rs`, 4): write→flush dirties only bound nodes with bound classes (paint-only stays local); many writes collapse into one flush; idle transaction recomputes nothing; MEASURE binding bubbles through a flexible ancestor. Plus 5 state + 5 binding unit tests.
- [x] Strip `§`/Makepad refs from every file touched (scheduler.rs, context.rs runtime + ui, state.rs, binding.rs, component.rs, facade lib.rs, ui lib.rs).
- [x] Commit.

#### B3 — Computed + Effect
- [ ] `Computed` (pure; runtime dep collection via flush-context cursor, not thread-local; read-only state access enforces purity). First eval registers dynamic bindings; on dep change re-eval; if result changed, mark downstream dirty.
- [ ] `Effect` (lifecycle: dependency restart runs prior cleanup then re-runs body; cleanup closure; cancellation on node free/unmount). Synchronous this slice.
- [ ] Headless tests: State → Computed re-eval → downstream dirty; Effect dep change runs cleanup + re-run; node unmount runs effect cleanup.
- [ ] Commit.

### Wrap-up for Slice B
- [ ] Append ADR: hybrid dep tracking (compile-time binding table fast path + runtime dynamic fallback), centralized SoA StateStore + StateId, per-class layered per-node dirty (paint-only never bubbles layout), per-frame one-shot flush, incremental recompute. Record tradeoff vs Makepad's coarse redraw_id + area-compare manual redraw.
- [ ] Update architecture doc reactive/dirty/incremental sections.

---

## Open design note — driver → scheduler StateDirty channel
`RuntimeCx` currently exposes only `request_redraw(window)` (a platform beat), not
`add(RedrawReason::StateDirty)`. The scheduler adds reasons itself from raw events;
a `RedrawRequested` beat drains whatever reasons are pending. Cleanest fit for B2:
have the facade's first write call `request_redraw` (the beat) and add a narrow
`RuntimeCx` method to record `StateDirty` so the scheduler's decide/idle bookkeeping
stays honest. Decide the exact seam when implementing B2, keeping the zero-CPU-when-idle
contract intact.
