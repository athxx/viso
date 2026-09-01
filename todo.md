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
Design (locked, after reading the makepad reference — which has NO reactive
derivation system: it uses redraw_id generation + explicit invalidation queue +
draw-cache memo; Effect-equivalent is imperative handle_* hooks + apply-reload
re-eval + async drop→GC cancellation; Animator = NextFrame self-requeue loop
that snapshots start values, restarts on state switch, stops when done). Viso
deliberately diverges to compiled/tracked fine-grained reactivity; we take the
*semantics* (re-eval only on dep change, propagate only if result changed;
dep-change restart = cleanup-then-rerun; unmount cleanup) not the coarse model.
New file `crates/ui/src/reactive.rs` (Computed + Effect are one subsystem).
Since makepad has no reactive-derivation system to preserve, we chose the better
fine-grained design outright rather than fitting a coarse model: compiled binding
fast path + runtime `DepCursor` fallback, and — decisively — Computed and Effect
wake by DIFFERENT routes because their outputs differ in kind (see below).
- [x] `Computed` (pure; runtime dep collection via a `DepCursor` passed into eval — not thread-local; a read-only `ComputeCx` exposes `get` only, so purity is enforced by the type, no `set`). SoA `ComputedStore` keyed by generational `ComputedId`: cached last `StateValue` + dep set. `eval` records deps → refreshes them into `ComputedStore`'s own `StateId -> Vec<ComputedId>` reverse index (NOT `bind_dynamic` — the wrap-up dropped that route because the binding flush dirties dynamic edges unconditionally, bypassing the memo boundary). Returns `changed`. `wake_computed` re-evals affected derivations and marks the downstream node dirty ONLY when the value changed (the memo boundary in the wake). A Computed's output *is* a node dirty class.
- [x] `Effect` (lifecycle: SoA `EffectStore` keyed by generational `EffectId`; each holds dep set + body + prior `cleanup: Option<Cleanup>` + owning `NodeId`). Dependency restart = prior cleanup THEN re-run body. Cancellation runs cleanup on `cancel`/`cancel_for_node` (node free = unmount). Synchronous this slice; no timers/async yet.
- [x] DESIGN DIVERGENCE (deliberate, better-than-coarse): an Effect has NO dirty class — its output is the side effect — so routing it through the 8-bit `DirtyClass` would overload a bit meaning "recompute layout/paint". Instead `EffectStore` owns a compact reverse index `StateId -> Vec<EffectId>`, rebuilt every run (a dropped dep stops waking; a new dep starts). The flush hands the frame's changed ids to `EffectStore::wake`, which dedupes and re-runs each affected effect once. `DirtyClass` stays a clean eight, one meaning each. `wake` reuses a scratch buffer → zero steady-state alloc.
- [x] Unit tests (`reactive.rs`, 9): computed first-eval registers deps + reports change; re-eval on dep change; unchanged result propagates nothing (memo boundary, step function); freed computed is stale; effect dep change runs cleanup THEN re-run (ordered log); effect only wakes on its own deps; two changed deps re-run once; node unmount runs cleanup + drops from reverse index; effect that stops reading a dep stops being woken by it.
- [x] No `§`/Makepad refs in any file touched (reactive.rs, ui lib.rs).
- [x] Wire into the facade flush phase (Computed wake + Effect wake): `AppDriver` owns `computeds`/`effects`; `FlushStateTransactions` drains pending once and fans it wake_computed → binding flush → effects.wake. Integration test `crates/viso/tests/reactive_wiring.rs` asserts that order. `cancel_for_node` hook comment left at the future structural-teardown site.
- [x] Commit.

### Wrap-up for Slice B
- [x] Append ADR: `docs/adr/0005-reactive-state-dirty-incremental.md` — hybrid dep tracking (compile-time binding table fast path + runtime dynamic fallback), centralized SoA StateStore + StateId, memo-gated Computed + scoped Effect each on their own reverse index, per-class layered per-node dirty (paint-only never bubbles layout), per-frame one-shot flush, incremental recompute. Records tradeoff vs Makepad's coarse redraw_id + area-compare manual redraw.
- [x] Update architecture doc reactive/dirty/incremental sections: appended "As built (Slice B)" notes to §18.1 (hybrid binding), §19 (DirtyMask u16 sketch → shipped DirtyClass u8/8 classes), §21 (Computed/Effect reverse-index wake), each pointing at ADR 0005.

---

## Phase 4 — Input / Style / Semantics

> **Numbering reconciliation (read once).** The architecture doc (Part XXV, §69–70) is
> the authority. The old "Phase 3" section above was an *execution* label that bundled
> the doc's Phase 3 (NodeArena — done as Slice A's `NodeStore`/arena), the doc's Phase 4
> *layout* (Slice A: Component→Node→Flex→Paint, i.e. doc-§69 items 5–6 box-constraints +
> Row/Column) and the whole doc Phase 5 Reactive (Slice B). Measured against the doc's
> exit criteria, Slice B satisfies **doc Phase 5** in full (§70: counter without `render()`,
> single-property targeted phases, one flush per transaction, profileable dep graph). The
> remaining unfinished doc-**Phase 4** work is **Input, Style token, Semantics** (§69 items
> 1–4, 9, 10). From here TODO section headers align to the doc's phase numbers so the two
> stop drifting. Doc-§69 items 7–8 (Scroll, VirtualList) and 11 (Grid/Adaptive) are layout
> containers that ride on Input's hit-test; they are deferred to a follow-up Phase 4 slice
> after the input→state→layout→paint→GPU chain is closed and interactive.

This closes the one break in the vertical chain: today `on_input` is empty, nothing is
hit-testable, and state can only change from code. Completing Input makes the first
interactive example (`01-counter`) real — a click routes to a target that `set`s state,
which the Slice B flush already turns into targeted invalidation.

Order follows the doc §69: **bounds/transform → hit test → pointer routing → keyboard/
focus/IME**, then style token, then semantics.

**Before each subsystem:** read the Makepad reference at `/Users/x/code/makepad` for that
subsystem (hit-test/finger/pointer, focus/key/next-frame, area/DrawList bounds) as a
*semantics* reference only, then design on Viso Node/State/Dirty contracts. Makepad's model
is coarse (event walk over the widget tree, area-compare); Viso takes the target-route
semantics (capture→target→bubble, focus ring, IME preedit) not the coarse walk.

### Slice C — Input: bounds/transform + hit test
Doc §69 items 1–2. World bounds are the prerequisite for hit-testing; Slice A resolves
local layout boxes but not accumulated world transform.
- [ ] World bounds/transform: accumulate parent transform down the tree into a per-node
      world rect (hot column on `NodeStore`, aligned to existing `bounds`). Recomputed only
      for the TRANSFORM/LAYOUT-dirty subtree (reuse the incremental relayout walk); a
      paint-only frame does not recompute world bounds.
- [ ] `HitTestTree`: given a point, return the topmost hit `NodeId`. Front-to-back
      descent using ancestry + world rect + a per-node HIT_TEST flag (opaque/pass-through/
      clip); respects paint/z order and clip rects. NOT a full-tree scan per event — descend
      only into children whose world rect contains the point (the §13 route: no whole-tree
      walk when a target route suffices).
- [ ] `HIT_TEST` dirty class already exists (one of the 8); wire world-bounds recompute so a
      TRANSFORM/LAYOUT change re-derives the hittable rect. No new dirty class.
- [ ] Headless tests: point over a leaf returns that leaf; overlapping siblings return the
      topmost; a point in padding/gap returns the container (or nothing if pass-through);
      a translated subtree hit-tests at its world position; clip rect excludes outside points.
- [ ] Strip `§`/Makepad refs from every file touched. Commit.

### Slice D — Input: pointer routing (capture → target → bubble)
Doc §69 item 3. The normalized-event → route pipeline (§13).
- [ ] Normalize platform pointer events (down/move/up/scroll) into a Viso `PointerEvent`
      (position in logical px, button, modifiers, phase) at the platform/scheduler seam —
      no Makepad `FingerDown`/`Cx` vocabulary in Viso code.
- [ ] Route: hit-test the target, then dispatch capture (root→target), target, bubble
      (target→root) along the ancestry chain. Pointer **capture**: a node can hold capture so
      subsequent moves/up route to it regardless of hit (drag). Handlers live on the
      Component; the route carries a phase-specific `EventCx` (per §6.4 — `get`/`set` state,
      request focus/capture; NOT layout or GPU).
- [ ] Handler registration: a Component declares pointer handlers; stored as compact
      per-node handler ids/edges (no per-event `dyn` walk of the whole tree, no string child
      lookup — §13/§45). A click that `set`s state flows into the existing Slice B flush.
- [ ] Facade: `on_input` stops being empty — normalize, route, and let a resulting `set`
      drive the next frame's flush. Wire the scheduler's input beat to routing.
- [ ] Headless tests (input tape, §66): a down+up on a leaf fires its click; capture
      redirects moves to the holder; bubble reaches an ancestor handler; a click that sets
      state produces exactly the bound node's dirty class next flush (end-to-end
      input→state→dirty). First `01-counter` interaction proven headless.
- [ ] Strip `§`/Makepad refs from every file touched. Commit.

### Slice E — Input: keyboard / focus / IME
Doc §69 item 4.
- [ ] Focus: a single focused `NodeId` per window; focus ring traversal (next/prev in DOM/
      tab order over focusable nodes), programmatic focus request via `EventCx`. Focus change
      is its own targeted invalidation (PAINT for focus ring; SEMANTICS later).
- [ ] Keyboard: normalized key events routed to the focused node with capture/target/bubble
      (same route machinery as pointer, target chosen by focus not hit-test).
- [ ] IME: preedit/commit events routed to the focused node; a text-input-shaped handler
      surface (composition string + caret) even before a real text widget — enough for
      `07-text-input` later. Keep IME plumbing in platform→facade normalized form.
- [ ] Headless tests: tab moves focus in order and wraps; a key event reaches the focused
      node's handler and bubbles; focus change dirties only the two nodes' focus-ring paint;
      an IME preedit/commit sequence routes to the focused node.
- [ ] Strip `§`/Makepad refs from every file touched. Commit.

### Slice F — Style token
Doc §69 item 9 (§14). Compile source style names to IDs; no runtime string lookup on the
hot path.
- [ ] `TokenId`/`StyleId` interning: semantic token namespaces (`color.*`, `spacing.*`,
      `radius.*`, `typography.*`, `elevation.*`, `motion.*`) compiled to compact ids. A theme
      is a token→value table; resolution is an id index, not a string map (§14/§29).
- [ ] Node style references a resolved token id, not a literal, so a theme swap re-resolves
      without touching node structure. A token change dirties only STYLE/PAINT of nodes bound
      to it (reuse the binding/dirty machinery; a token is a state-like source).
- [ ] A normal frame with no token change recomputes no style cascade (§14 exit).
- [ ] Headless tests: token resolves to value; theme swap re-resolves bound nodes only;
      unrelated node untouched; token change dirties STYLE/PAINT not LAYOUT.
- [ ] Strip `§`/Makepad refs from every file touched. Commit.

### Slice G — Semantics
Doc §69 item 10 (§15). Accessibility tree generated from the Node model, incrementally.
- [ ] `SemanticsTree` derived from nodes: role, label, state (focused/checked/…), bounds.
      Generated from the Node model (§69 exit: "semantics 从 Node 模型天然生成"), not a
      parallel hand-maintained tree.
- [ ] Incremental: the existing SEMANTICS dirty class drives re-derivation of only changed
      subtrees; a focus/label/role change dirties SEMANTICS on that node.
- [ ] Interactive nodes (the ones with pointer/key handlers from Slices D/E) carry default
      semantics so a keyboard/AT path exists (§15).
- [ ] Headless semantics-snapshot tests (§35/§66): tree shape + roles for the demo scene;
      a focus change updates only that node's semantics; a label change re-derives one node.
- [ ] Strip `§`/Makepad refs from every file touched. Commit.

### Wrap-up for Phase 4
- [ ] `01-counter` example: a real interactive counter (button click → `set` → bound label
      repaints), the first end-to-end interactive Viso app. Headless input-tape test asserts
      the click drives exactly the counter's dirty class.
- [ ] ADR 0006 — Input & focus routing (target/capture/bubble, focus/IME, hit-test world
      bounds) + style-token resolution + semantics derivation. Records the §13 target-route
      divergence from Makepad's event-walk and the §14 id-not-string style contract.
- [ ] Reconcile architecture doc §69 with "As built" notes pointing at ADR 0006; confirm
      Phase 4 exit criteria (§69) except deferred Scroll/VirtualList/Grid.
- [ ] Deferred to a follow-up Phase 4 slice (tracked, not this pass): Scroll (§69 item 7),
      VirtualList (item 8), Grid/Adaptive (item 11).

---

## Open design note — driver → scheduler StateDirty channel
`RuntimeCx` currently exposes only `request_redraw(window)` (a platform beat), not
`add(RedrawReason::StateDirty)`. The scheduler adds reasons itself from raw events;
a `RedrawRequested` beat drains whatever reasons are pending. Cleanest fit for B2:
have the facade's first write call `request_redraw` (the beat) and add a narrow
`RuntimeCx` method to record `StateDirty` so the scheduler's decide/idle bookkeeping
stays honest. Decide the exact seam when implementing B2, keeping the zero-CPU-when-idle
contract intact.
