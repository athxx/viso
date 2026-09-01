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
- [ ] Commit.

#### B2 — State + StateStore + BindingTable + per-frame transactions
- [ ] `crates/ui/src/state.rs`: `StateId` (generational), `StateStore` SoA (values + generation + free list + frame pending write-set), `StateValue` scalar set (i32/f32/bool/color), `alloc/get/set/take_pending/has_pending`.
- [ ] `crates/ui/src/binding.rs`: `Binding { node, class }`, `BindingTable` (index-aligned to StateId; `bind`/`for_state`), plus dynamic region for B3.
- [ ] `UpdateCx` gains `states`/`bindings`/`nodes` access + `set`/`get`.
- [ ] Facade `FlushStateTransactions` phase: consume pending write-set → `for_state` → `mark_dirty`.
- [ ] First-write signal: on first pending write, facade adds `StateDirty` reason + requests redraw. (Resolve driver→scheduler reason channel — see design note below.)
- [ ] Expose a public write entry for tests/examples.
- [ ] Headless tests: write State → flush → correct nodes dirtied; only dirty subtree recomputes; idle = zero recompute.
- [ ] Commit.

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
